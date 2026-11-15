//! 会话管理：本地 PTY / 远程 SSH channel 两种后端、vt100 屏幕状态、状态机与通知去抖，以及 GUI 消息分发。

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{Read, Write},
    num::NonZeroU16,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use proto::{ClaudeState, ClaudeStatus, ClientMsg, Config, FsSource, HostSource, RecentItem, ServerMsg, SessionInfo, Status, Target};
use tokio::sync::{mpsc::UnboundedSender, oneshot};

use crate::{claude, config, fs, history, hub, notify, now_ms, recent, saved, ssh, stats};

pub type Shared = Arc<Mutex<State>>;

/// 交互提示的回复：(输入值, 是否保存)，None 表示取消
pub type PromptAnswer = Option<(String, bool)>;

const DEFAULT_MSG: &str = "会话需要你的关注";
/// 最近一次 Claude hook 事件后这段时间内，OSC9/BEL 不再通知（hook 已通知，避免重复）
const HOOK_SUPPRESS: Duration = Duration::from_secs(5);
/// 未收到 SubagentStop 的子代理登记超过这段时间即作废（仅在 Stop 不带 background_tasks 的旧版 Claude Code 上用到）
const AGENT_EXPIRE: Duration = Duration::from_secs(30 * 60);

#[derive(Default)]
pub struct State {
    next_id: u32,
    /// 用户配置，启动时从文件加载
    pub config: Config,
    pub sessions: BTreeMap<u32, Session>,
    /// 最近关闭的会话目录，启动时从文件加载
    pub recent: Vec<RecentItem>,
    /// 当前接入的 GUI（同时只服务一个）
    pub gui: Option<Gui>,
    /// SSH 连接池
    pub pool: ssh::Pool,
    next_req: u64,
    /// 等待 GUI 回复的交互提示；GUI 断开或被顶替时清空（发送端被丢弃即视为取消）
    prompts: HashMap<u64, oneshot::Sender<PromptAnswer>>,
    /// 等待恢复的远程会话（连接可能需要交互认证，等 GUI 首次接入时再连）
    pending: Vec<saved::Saved>,
    /// 管理服务器（hub）客户端
    pub hub: hub::Hub,
}

pub struct Gui {
    pub conn: u64,
    tx: UnboundedSender<String>,
    /// 已 attach 的会话，只给这些会话推 output
    attached: HashSet<u32>,
    /// GUI 上报的激活会话与窗口焦点
    active: Option<u32>,
    focused: bool,
}

pub struct Session {
    pub info: SessionInfo,
    backend: Backend,
    /// 文件树后端：本地磁盘 / 远程连接的 SFTP
    fs: fs::Backend,
    parser: vt100::Parser<Signals>,
    /// 通知去抖：200ms 内合并 OSC9 与 BEL，通知后 1 秒内静默
    pending_msg: Option<String>,
    timer_armed: bool,
    muted_until: Instant,
    /// 最近一次收到 Claude hook 事件的时间；有 Claude 状态或刚收到过事件时抑制 OSC9/BEL 通知
    hook_at: Option<Instant>,
    /// Claude 子代理跟踪
    agents: Agents,
    /// 会话中的 Claude Code 对话，daemon 重启后据此 `claude --resume`
    resume: Option<saved::Resume>,
}

/// Claude 子代理跟踪：running 为 SubagentStart 登记、SubagentStop 移除的子代理（id → 登记时间）；
/// bg 为主代理 Stop 时仍在后台运行的子代理数，此后每个 SubagentStop 减一
#[derive(Default)]
struct Agents {
    running: HashMap<String, Instant>,
    bg: u32,
}

/// 会话 I/O 后端
pub enum Backend {
    Local {
        master: Box<dyn MasterPty + Send>,
        writer: Box<dyn Write + Send>,
        killer: Box<dyn ChildKiller + Send + Sync>,
    },
    /// 命令交给该 channel 的异步任务执行，持锁时不 await
    Remote(UnboundedSender<ssh::Cmd>),
}

impl Backend {
    fn write(&mut self, data: &[u8]) {
        match self {
            // 必须一次 write_all：拆成多次小写入时 ConPTY 收到碎片序列不会识别（CPR 回复）
            Backend::Local { writer, .. } => {
                let _ = writer.write_all(data);
                let _ = writer.flush();
            }
            Backend::Remote(tx) => {
                let _ = tx.send(ssh::Cmd::Data(data.to_vec()));
            }
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        match self {
            Backend::Local { master, .. } => {
                let _ = master.resize(pty_size(cols, rows));
            }
            Backend::Remote(tx) => {
                let _ = tx.send(ssh::Cmd::Resize(cols, rows));
            }
        }
    }

    fn close(&mut self) {
        match self {
            Backend::Local { killer, .. } => {
                let _ = killer.kill();
            }
            Backend::Remote(tx) => {
                let _ = tx.send(ssh::Cmd::Close);
            }
        }
    }
}

/// vt100 回调：vte 状态机已区分 OSC 终止符 BEL 与独立 BEL，且跨 read 块保持状态
#[derive(Default)]
struct Signals {
    fired: bool,
    msg: Option<String>,
    /// 收到光标位置查询（CSI 6 n）时记录当时的光标（0 起始的行、列）
    cpr: Option<(u16, u16)>,
    /// 推出屏幕的历史行（hub 接管时发送；每块输出后不取走）
    history: history::History,
}

impl AsMut<history::History> for Signals {
    fn as_mut(&mut self) -> &mut history::History {
        &mut self.history
    }
}

impl vt100::Callbacks for Signals {
    fn on_scroll(&mut self, row: vt100::capture::RowContents<'_>, alternate: bool) {
        self.history.push(row, alternate);
    }

    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.fired = true;
    }

    /// vte 按 ';' 切分 OSC 参数：`9;text` → ["9", "text"...]，`9;4;...` 是任务栏进度，忽略
    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        match params {
            [b"9", b"4", ..] => {}
            [b"9", rest @ ..] => {
                self.fired = true;
                self.msg = Some(String::from_utf8_lossy(&rest.join(&b';')).into_owned());
            }
            _ => {}
        }
    }

    /// ConPTY（INHERIT_CURSOR）启动时发 CSI 6 n 并阻塞等待回复，无 GUI 接入时需由 daemon 代答
    fn unhandled_csi(&mut self, screen: &mut vt100::Screen, i1: Option<u8>, _: Option<u8>, params: &[&[u16]], c: char) {
        if c == 'n' && i1.is_none() && params == [&[6u16][..]] {
            self.cpr = Some(screen.cursor_position());
        }
    }
}

impl Gui {
    pub fn new(conn: u64, tx: UnboundedSender<String>) -> Self {
        Self { conn, tx, attached: HashSet::new(), active: None, focused: false }
    }

    pub fn send(&self, msg: &ServerMsg) {
        if let Ok(json) = serde_json::to_string(msg) {
            let _ = self.tx.send(json + "\n");
        }
    }
}

impl State {
    pub fn send(&self, msg: &ServerMsg) {
        if let Some(g) = &self.gui {
            g.send(msg);
        }
    }

    pub fn push_sessions(&self) {
        let sessions = self.sessions.values().map(|s| s.info.clone()).collect();
        self.send(&ServerMsg::Sessions { sessions });
        // hub 常驻连接据此防抖上报（未启用时无人等待，只留一个许可）
        self.hub.changed.notify_one();
    }

    pub fn push_recent(&self) {
        self.send(&ServerMsg::Recent { items: self.recent.clone() });
    }

    /// 把未关闭的会话（含等待恢复的远程会话）写入 sessions.json
    pub fn save_sessions(&self) {
        let live = self.sessions.values().map(|s| saved::Saved {
            name: s.info.name.clone(),
            cwd: s.info.cwd.clone(),
            command: s.info.command.clone(),
            host_id: s.info.host_id.clone(),
            claude: s.resume.clone(),
        });
        saved::save(&live.chain(self.pending.iter().cloned()).collect::<Vec<_>>());
    }

    /// 更换 / 断开 GUI：发给旧 GUI 的未回复提示全部视为取消
    pub fn set_gui(&mut self, gui: Option<Gui>) {
        self.gui = gui;
        self.prompts.clear();
    }

    /// 有交互提示在等 GUI 回复（连接测试据此暂停计时）
    pub fn prompting(&self) -> bool {
        !self.prompts.is_empty()
    }

    /// 登记一个交互提示（覆盖 msg 中的 req_id）并发给 GUI；没有 GUI 连接时返回 None
    pub fn prompt(&mut self, mut msg: ServerMsg) -> Option<oneshot::Receiver<PromptAnswer>> {
        self.gui.as_ref()?;
        self.next_req += 1;
        let req = self.next_req;
        if let ServerMsg::Prompt { req_id, .. } = &mut msg {
            *req_id = req;
        }
        let (tx, rx) = oneshot::channel();
        self.prompts.insert(req, tx);
        self.send(&msg);
        Some(rx)
    }

    fn send_config(&self) {
        let hosts = config::all_hosts(&self.config);
        let ssh_hosts = config::ssh_config_hosts();
        let password_saved = ssh::password_saved(&ssh::Keyring, &hosts);
        self.send(&ServerMsg::Config {
            config: self.config.clone(),
            shells: config::detect_shells(),
            hosts,
            ssh_hosts,
            password_saved,
            hub_token_saved: hub::token_saved(),
        });
        self.send(&self.hub.status_msg());
    }
}

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v).unwrap_or(NonZeroU16::MIN)
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }
}

/// 处理一条 GUI 消息；返回 false 表示该连接已被新连接顶替，应断开
pub fn dispatch(state: &Shared, conn: u64, msg: ClientMsg) -> bool {
    let mut guard = state.lock().unwrap();
    let st = &mut *guard;
    if !st.gui.as_ref().is_some_and(|g| g.conn == conn) {
        return false;
    }
    match msg {
        ClientMsg::List => st.push_sessions(),
        ClientMsg::Create { target: Target::Remote { host_id, cwd }, cols, rows, .. } => {
            let cwd = cwd.filter(|c| !c.trim().is_empty());
            match config::all_hosts(&st.config).into_iter().find(|h| h.id == host_id) {
                // 连接 / 认证可能要等用户输入，放到异步任务中，不阻塞消息分发
                Some(host) => drop(tokio::spawn(ssh::create(state.clone(), host, cwd, cols, rows))),
                None => st.send(&ServerMsg::Error { message: format!("未找到主机 {host_id}") }),
            }
        }
        ClientMsg::Create { target: Target::Local { cwd }, cols, rows, command } => {
            // 自动模式每次新建时现场探测，安装 PowerShell 7 后无需重启 daemon
            let shell = command.unwrap_or_else(|| config::resolve_shell(&st.config.shell));
            // 未指定目录（“+ 新建 → 本地”）时进用户主目录
            let cwd = match cwd.trim() {
                "" => config::home().map(|h| h.display().to_string()).unwrap_or(cwd),
                _ => cwd,
            };
            // 启动 PTY 较慢，不持锁
            drop(guard);
            let result = spawn(state, &shell, cwd, cols, rows);
            let st = state.lock().unwrap();
            match result {
                Ok(id) => {
                    st.push_sessions();
                    st.send(&ServerMsg::Activate { id });
                }
                Err(e) => {
                    log!("启动 shell 失败（{shell}）：{e}");
                    st.send(&ServerMsg::Error { message: format!("启动 shell 失败（{shell}）：{e}") });
                }
            }
        }
        ClientMsg::Attach { id } => {
            let Some(s) = st.sessions.get(&id) else { return true };
            let data = B64.encode(s.parser.screen().state_formatted());
            st.send(&ServerMsg::Snapshot { id, data });
            if let Some(g) = st.gui.as_mut() {
                g.attached.insert(id);
            }
        }
        ClientMsg::Input { id, data } => input(st, id, data.as_bytes()),
        ClientMsg::Resize { id, cols, rows } => resize(st, id, cols, rows),
        ClientMsg::Rename { id, name } => {
            let Some(s) = st.sessions.get_mut(&id) else { return true };
            s.info.name = name;
            st.push_sessions();
            st.save_sessions();
        }
        ClientMsg::Close { id } => {
            let removed = remove(st, id);
            // 先释放锁再 kill/drop：drop master 会关闭 ConPTY，可能需要读线程（要抢锁）排空输出
            drop(guard);
            if let Some(s) = removed {
                s.close();
            }
        }
        ClientMsg::Focus { active_id, window_focused } => {
            if let Some(g) = st.gui.as_mut() {
                g.active = active_id;
                g.focused = window_focused;
            }
        }
        ClientMsg::GetConfig => st.send_config(),
        ClientMsg::SetConfig { mut config } => {
            // 丢弃 ~/.ssh/config 中已不存在的 alias 的覆盖项
            let ssh = config::ssh_config_hosts();
            config.hosts.retain(|h| h.source == HostSource::Tether || ssh.iter().any(|s| s.id == h.id));
            match config::save(&config) {
                Ok(()) => {
                    // 主机账户变化 / 删除时同步凭据管理器中保存的密码
                    let old = config::all_hosts_with(&st.config, ssh.clone());
                    ssh::migrate_credentials(&ssh::Keyring, &old, &config::all_hosts_with(&config, ssh));
                    // hub 连接设置变化，或已停在 failed（401 / 被替换等）时，保存即按新设置重连；只改允许接管不重连
                    let link = |h: &proto::HubConfig| (h.enabled, h.address.clone(), h.allow_insecure);
                    let hub_changed = link(&st.config.hub) != link(&config.hub) || matches!(st.hub.status_msg(), ServerMsg::HubStatus { state: proto::HubState::Failed, .. });
                    st.config = config;
                    if !st.config.hub.allow_takeover {
                        hub::release_all(st, Some("disabled"));
                    }
                    if hub_changed {
                        hub::restart(state, st);
                    }
                    st.send_config();
                }
                Err(e) => {
                    log!("保存配置失败：{e}");
                    st.send(&ServerMsg::Error { message: format!("保存配置失败：{e}") });
                }
            }
        }
        ClientMsg::SetHostPassword { host_id, password } => {
            let Some(host) = config::all_hosts(&st.config).into_iter().find(|h| h.id == host_id) else {
                st.send(&ServerMsg::Error { message: format!("未找到主机 {host_id}") });
                return true;
            };
            if let Err(e) = ssh::set_password(&ssh::Keyring, &host, password.as_ref().map(|p| p.0.as_str())) {
                log!("保存密码失败：{e}");
                st.send(&ServerMsg::Error { message: format!("保存密码失败：{e}") });
            }
            st.send_config();
        }
        ClientMsg::PromptReply { req_id, value, save } => {
            if let Some(tx) = st.prompts.remove(&req_id) {
                let _ = tx.send(value.map(|v| (v, save)));
            }
        }
        ClientMsg::FsList { req_id, session_id, path } => fs_spawn(state, st, req_id, session_id, fs::Op::List(path)),
        ClientMsg::FsMkdir { req_id, session_id, path } => fs_spawn(state, st, req_id, session_id, fs::Op::Mkdir(path)),
        ClientMsg::FsCreateFile { req_id, session_id, path } => {
            fs_spawn(state, st, req_id, session_id, fs::Op::CreateFile(path))
        }
        ClientMsg::FsRename { req_id, session_id, from, to } => {
            fs_spawn(state, st, req_id, session_id, fs::Op::Rename(from, to))
        }
        ClientMsg::FsDelete { req_id, session_id, path } => fs_spawn(state, st, req_id, session_id, fs::Op::Delete(path)),
        ClientMsg::FsOpen { req_id, session_id, path, mode } => {
            fs_spawn(state, st, req_id, session_id, fs::Op::Open(path, mode))
        }
        ClientMsg::FsRead { req_id, session_id, path } => fs_spawn(state, st, req_id, session_id, fs::Op::Read(path)),
        ClientMsg::Upload { req_id, session_id, local_paths, dest_dir } => {
            fs_spawn(state, st, req_id, session_id, fs::Op::Upload(local_paths, dest_dir))
        }
        ClientMsg::Download { req_id, session_id, paths, local_dir } => {
            fs_spawn(state, st, req_id, session_id, fs::Op::Download(paths, local_dir))
        }
        ClientMsg::FsCopy { req_id, session_id, source, items, dest_dir } => {
            let Some(dest) = st.sessions.get(&session_id).map(|s| s.fs.clone()) else {
                st.send(&ServerMsg::FsResult { req_id, error: Some("会话已不存在".into()) });
                return true;
            };
            // 源按主机定位：源会话关闭后只要主机仍可连接（池中连接复用，或重新连接）即可粘贴
            let host = match source {
                FsSource::Local => None,
                FsSource::Remote { host_id } => match config::all_hosts(&st.config).into_iter().find(|h| h.id == host_id) {
                    Some(h) => Some(h),
                    None => {
                        st.send(&ServerMsg::FsResult { req_id, error: Some(format!("未找到源主机 {host_id}")) });
                        return true;
                    }
                },
            };
            drop(tokio::spawn(fs::copy(state.clone(), req_id, dest, host, items, dest_dir)));
        }
        ClientMsg::OpenUrl { url } => drop(tokio::spawn(fs::open_url(state.clone(), url))),
        ClientMsg::RemoveRecent { target } => {
            if recent::remove(&mut st.recent, &target) {
                st.push_recent();
            }
        }
        ev @ ClientMsg::ClaudeEvent { .. } => {
            drop(guard);
            claude_event(state, ev);
        }
        ClientMsg::GetClaudeHooks => st.send(&claude::status_msg(None)),
        ClientMsg::SetClaudeHooks { enabled } => {
            let result = claude::settings_path().ok_or_else(|| "未找到用户目录".to_string()).and_then(|p| {
                let r = if enabled { claude::hook_command().and_then(|c| claude::enable(&p, &c)) } else { claude::disable(&p) };
                log!("Claude 集成{}（{}）：{r:?}", if enabled { "启用" } else { "移除" }, p.display());
                r
            });
            st.send(&claude::status_msg(result.err()));
        }
        ClientMsg::PlaySound => notify::play(),
        ClientMsg::Hello { version, proto } => {
            log!("GUI 版本 {version}（协议 {proto}），daemon 版本 {}", env!("CARGO_PKG_VERSION"));
            st.send(&ServerMsg::Hello { version: env!("CARGO_PKG_VERSION").into(), proto: proto::PROTO_VERSION });
        }
        ClientMsg::HostStats { req_id, session_id } => match st.sessions.get(&session_id) {
            Some(s) => drop(tokio::spawn(stats::run(state.clone(), req_id, s.fs.clone()))),
            None => st.send(&ServerMsg::HostStats { req_id, stats: None, error: Some("会话已不存在".into()) }),
        },
        ClientMsg::TestHost { req_id, host, password, use_saved } => {
            let password = match password {
                Some(p) => Some(p.0),
                None if use_saved => {
                    let old = config::all_hosts(&st.config).into_iter().find(|h| h.id == host.id);
                    ssh::saved_password(&ssh::Keyring, old.as_ref(), &host)
                }
                None => None,
            };
            drop(tokio::spawn(ssh::test(state.clone(), req_id, host, password)));
        }
        ClientMsg::SetHubToken { token } => {
            match hub::set_token(token.as_ref().map(|t| t.0.as_str())) {
                Ok(()) => log!("hub token 已{}", if token.is_some() { "保存" } else { "清除" }),
                Err(e) => {
                    log!("保存 hub token 失败：{e}");
                    st.send(&ServerMsg::Error { message: format!("保存 hub token 失败：{e}") });
                }
            }
            if st.config.hub.enabled {
                hub::restart(state, st);
            }
            st.send_config();
        }
        ClientMsg::HubReclaim { id } => hub::reclaim(st, id),
        ClientMsg::TestHub { req_id, address, allow_insecure, token, use_saved } => {
            let token = match token {
                Some(t) if !t.0.trim().is_empty() => Some(t.0.trim().to_string()),
                _ if use_saved => hub::saved_token(),
                _ => None,
            };
            drop(tokio::spawn(hub::test(state.clone(), req_id, address, allow_insecure, token)));
        }
        // 先落盘会话列表再退出：GUI 重连时拉起（可能是更新后的）tetherd.exe，按 sessions.json 恢复会话
        ClientMsg::Shutdown => {
            st.save_sessions();
            log!("收到 Shutdown，保存 {} 个会话后退出", st.sessions.len() + st.pending.len());
            std::process::exit(0);
        }
    }
    true
}

/// 写入会话（本地 GUI 与 hub 接管共用）：更新活动时间，等待中的状态回到运行中
pub fn input(st: &mut State, id: u32, data: &[u8]) {
    let Some(s) = st.sessions.get_mut(&id) else { return };
    s.backend.write(data);
    s.info.last_activity_at = now_ms();
    let mut changed = s.info.status == Status::Waiting;
    if changed {
        s.info.status = Status::Running;
    }
    // 等待确认时用户作答（按键）后 Claude 继续处理；主代理已结束、只剩后台子代理时回到后台运行中
    if let Some(c) = s.info.claude.as_mut().filter(|c| c.state == ClaudeStatus::Waiting) {
        *c = match s.agents.bg {
            0 => claude_state(ClaudeStatus::Thinking, None, None),
            n => claude_state(ClaudeStatus::Background, None, Some(n)),
        };
        changed = true;
    }
    if changed {
        st.push_sessions();
    }
}

/// 调整会话尺寸（本地 GUI 与 hub 接管共用，以最后一次为准）
pub fn resize(st: &mut State, id: u32, cols: u16, rows: u16) {
    let Some(s) = st.sessions.get_mut(&id) else { return };
    s.backend.resize(cols, rows);
    s.parser.set_size(nz(rows), nz(cols));
}

/// hub 接管用：当前尺寸 (cols, rows) 与屏幕快照（base64，同 GUI Attach）；会话不存在或已退出时为 None
pub fn snapshot(st: &State, id: u32) -> Option<(u16, u16, String)> {
    let s = st.sessions.get(&id).filter(|s| s.info.status != Status::Exited)?;
    let (rows, cols) = s.parser.screen().size();
    Some((cols.get(), rows.get(), B64.encode(s.parser.screen().state_formatted())))
}

/// hub 接管用（能力 history）：推出屏幕的历史（base64，按协议拼装，浏览器在 snapshot 之前写入）
pub fn history(st: &State, id: u32) -> Option<String> {
    let s = st.sessions.get(&id)?;
    Some(B64.encode(s.parser.callbacks().history.render(s.parser.screen())))
}

/// 本地会话（非 SSH）；会话不存在为 false
pub fn is_local(st: &State, id: u32) -> bool {
    st.sessions.get(&id).is_some_and(|s| s.info.host.is_none())
}

/// 把文件路径作为输入写入会话（网页贴图）：含空格时加双引号；会话开启 bracketed paste 时按粘贴包裹；不加回车
pub fn paste_path(st: &mut State, id: u32, path: &str) {
    let Some(s) = st.sessions.get(&id) else { return };
    let path = if path.contains(' ') { format!("\"{path}\"") } else { path.to_string() };
    let data = if s.parser.screen().bracketed_paste() { format!("\x1b[200~{path}\x1b[201~") } else { path };
    input(st, id, data.as_bytes());
}

/// 关闭会话（本地 GUI 与 hub 网页共用）：移出列表、记入最近目录、正被接管时发 detached exited，推送并保存会话列表。
/// 返回被移除的会话，调用方须在释放锁之后调用其 close
pub fn remove(st: &mut State, id: u32) -> Option<Session> {
    let removed = st.sessions.remove(&id);
    if let Some(g) = st.gui.as_mut() {
        g.attached.remove(&id);
    }
    // 先推 Recent 再推 Sessions：GUI 收到空列表显示空状态时已包含刚关闭的这条
    if let Some(s) = &removed {
        recent::push(&mut st.recent, &s.info);
        st.push_recent();
        if s.info.taken_over {
            st.hub.detached(id, "exited");
        }
    }
    st.push_sessions();
    st.save_sessions();
    removed
}

impl Session {
    /// 结束会话后端：本地会话结束进程（drop 时关闭 ConPTY），SSH 会话关闭通道；不可持 State 锁调用
    pub fn close(mut self) {
        self.backend.close();
    }
}

/// 按会话取文件后端，交给异步任务执行（不持锁 await）
fn fs_spawn(state: &Shared, st: &State, req_id: u64, session_id: u32, op: fs::Op) {
    match st.sessions.get(&session_id) {
        Some(s) => drop(tokio::spawn(fs::run(state.clone(), req_id, s.fs.clone(), op))),
        None => st.send(&ServerMsg::FsResult { req_id, error: Some("会话已不存在".into()) }),
    }
}

/// 启动配置中的 shell，登记会话并起读线程、等待线程
fn spawn(state: &Shared, shell: &str, cwd: String, cols: u16, rows: u16) -> Result<u32, String> {
    let argv = config::split_command(shell);
    let (program, args) = argv.split_first().ok_or("默认 shell 未配置")?;
    let pair = native_pty_system().openpty(pty_size(cols, rows)).map_err(|e| e.to_string())?;
    // 先分配 id：写入环境变量供会话内的 `tetherd hook` 回报（启动失败时该 id 作废，不影响后续）
    let id = alloc_id(state);

    // 环境变量默认继承当前进程
    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    cmd.cwd(&cwd);
    cmd.env("TERM", "xterm-256color");
    // xterm.js 支持真彩色；不声明的话 Claude Code 等程序会降级到 256 色
    cmd.env("COLORTERM", "truecolor");
    // Claude Code hook（`tetherd hook`）据此把事件发回本 daemon（dev / 安装版各走自己的管道）
    cmd.env("TETHER_SESSION", id.to_string());
    cmd.env("TETHER_PIPE", claude::pipe_env());

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let killer = child.clone_killer();

    let name = cwd.split(['\\', '/']).rfind(|s| !s.is_empty()).unwrap_or(cwd.as_str()).to_string();
    let info = new_info(name, cwd, shell.into(), None);
    register_with_id(state, id, info, Backend::Local { master: pair.master, writer, killer }, fs::Backend::Local, cols, rows);

    // 读线程：会话被关闭（ConPTY 关闭）后读到 EOF 结束
    let out_state = state.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if !on_output(&out_state, id, &buf[..n]) {
                break;
            }
        }
    });

    // 等待线程：子进程退出后标记 exited
    let exit_state = state.clone();
    thread::spawn(move || {
        let _ = child.wait();
        on_exit(&exit_state, id);
    });

    Ok(id)
}

/// 构造新会话的元信息，id 由 register 分配
pub fn new_info(name: String, cwd: String, command: String, host: Option<String>) -> SessionInfo {
    let now = now_ms();
    SessionInfo { id: 0, name, cwd, command, status: Status::Running, started_at: now, last_activity_at: now, host, host_id: None, claude: None, taken_over: false }
}

/// 分配 id 并登记会话
pub fn register(state: &Shared, info: SessionInfo, backend: Backend, fs: fs::Backend, cols: u16, rows: u16) -> u32 {
    let id = alloc_id(state);
    register_with_id(state, id, info, backend, fs, cols, rows);
    id
}

fn alloc_id(state: &Shared) -> u32 {
    let mut st = state.lock().unwrap();
    st.next_id += 1;
    st.next_id
}

/// 以预先分配的 id 登记会话
fn register_with_id(state: &Shared, id: u32, mut info: SessionInfo, backend: Backend, fs: fs::Backend, cols: u16, rows: u16) {
    info.id = id;
    let mut st = state.lock().unwrap();
    st.sessions.insert(id, new_session(info, backend, fs, cols, rows));
    st.save_sessions();
}

fn new_session(info: SessionInfo, backend: Backend, fs: fs::Backend, cols: u16, rows: u16) -> Session {
    Session {
        info,
        backend,
        fs,
        parser: vt100::Parser::new_with_callbacks(nz(rows), nz(cols), 0, Signals::default()),
        pending_msg: None,
        timer_armed: false,
        muted_until: Instant::now(),
        hook_at: None,
        agents: Agents::default(),
        resume: None,
    }
}

/// 测试用：登记一个远程后端的会话（命令发到返回的接收端），不写 sessions.json
#[cfg(test)]
pub fn test_session(st: &mut State, cols: u16, rows: u16) -> (u32, tokio::sync::mpsc::UnboundedReceiver<ssh::Cmd>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    st.next_id += 1;
    let id = st.next_id;
    let mut info = new_info("test".into(), "C:\\".into(), "pwsh".into(), None);
    info.id = id;
    st.sessions.insert(id, new_session(info, Backend::Remote(tx), fs::Backend::Local, cols, rows));
    (id, rx)
}

/// daemon 启动时恢复 sessions.json 中的会话：本地会话立即重建，有 Claude 对话的在其目录启动并在 shell 就绪后
/// `claude --resume`；远程会话留给 restore_remote
pub fn restore(state: &Shared) {
    for item in saved::load() {
        if item.host_id.is_some() {
            state.lock().unwrap().pending.push(item);
            continue;
        }
        let cwd = item.claude.as_ref().map_or(&item.cwd, |c| &c.cwd).clone();
        match spawn(state, &item.command, cwd, 120, 30) {
            Ok(id) => {
                if let Some(s) = state.lock().unwrap().sessions.get_mut(&id) {
                    s.info.name = item.name;
                    s.resume = item.claude.clone();
                }
                if let Some(cmd) = item.claude.as_ref().and_then(saved::resume_command) {
                    type_when_ready(state, id, cmd);
                }
            }
            Err(e) => log!("恢复会话失败（{} @ {}）：{e}", item.command, item.cwd),
        }
    }
    state.lock().unwrap().save_sessions();
}

/// GUI 首次接入时连接等待恢复的远程会话（结果以 Sessions / Error 告知 GUI）
pub fn restore_remote(state: &Shared, st: &mut State) {
    for item in std::mem::take(&mut st.pending) {
        let host_id = item.host_id.unwrap_or_default();
        match config::all_hosts(&st.config).into_iter().find(|h| h.id == host_id) {
            Some(host) => drop(tokio::spawn(ssh::create(state.clone(), host, Some(item.cwd), 120, 30))),
            None => log!("恢复远程会话失败：未找到主机 {host_id}"),
        }
    }
}

/// 恢复的会话：等 shell 有输出后静默 500ms（提示符已显示）再输入命令，最多等 15 秒
fn type_when_ready(state: &Shared, id: u32, cmd: String) {
    let state = state.clone();
    let start = Instant::now();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(100));
        let mut st = state.lock().unwrap();
        let Some(s) = st.sessions.get_mut(&id).filter(|s| s.info.status != Status::Exited) else { return };
        let output = s.info.last_activity_at > s.info.started_at;
        if (output && now_ms().saturating_sub(s.info.last_activity_at) >= 500) || start.elapsed() > Duration::from_secs(15) {
            s.backend.write(cmd.as_bytes());
            return;
        }
    });
}

/// 收到会话输出后的统一处理（本地读线程与远程异步任务共用）：喂 vt100、CPR 代答、检测通知信号、推给已 attach 的 GUI。
/// 会话已不存在（被关闭）时返回 false
pub fn on_output(state: &Shared, id: u32, data: &[u8]) -> bool {
    let mut guard = state.lock().unwrap();
    let st = &mut *guard;
    let Some(s) = st.sessions.get_mut(&id) else { return false };
    history::process(&mut s.parser, data);
    s.info.last_activity_at = now_ms();
    let mut sig = std::mem::take(s.parser.callbacks_mut());
    s.parser.callbacks_mut().history = std::mem::take(&mut sig.history);
    let gui_attached = st.gui.as_ref().is_some_and(|g| g.attached.contains(&id));
    let taken = s.info.taken_over;
    // 有 GUI 接入时由 xterm 回复，避免重复应答（hub 网页端不回复终端查询）
    if let (Some((row, col)), false) = (sig.cpr, gui_attached) {
        // 先格式化成完整字节串再一次写入：write! 会拆成多次小写入，ConPTY 收到碎片序列不会识别
        s.backend.write(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
    }
    if sig.fired {
        signal(state, s, sig.msg);
    }
    if let Some(g) = &st.gui
        && g.attached.contains(&id)
    {
        g.send(&ServerMsg::Output { id, data: B64.encode(data) });
    }
    if taken {
        st.hub.output(id, data);
    }
    true
}

/// 子进程退出 / channel 结束：标记 exited，会话保留在列表中直到用户关闭
pub fn on_exit(state: &Shared, id: u32) {
    let mut guard = state.lock().unwrap();
    let st = &mut *guard;
    if let Some(s) = st.sessions.get_mut(&id)
        && s.info.status != Status::Exited
    {
        s.info.status = Status::Exited;
        s.info.claude = None;
        s.agents = Agents::default();
        if std::mem::take(&mut s.info.taken_over) {
            st.hub.detached(id, "exited");
        }
        st.push_sessions();
    }
}

/// 收到 OSC9/BEL：首个信号起 200ms 定时，期间的信号合并；通知后 1 秒内同会话不再通知
fn signal(state: &Shared, s: &mut Session, msg: Option<String>) {
    if Instant::now() < s.muted_until || hooked(s) {
        return;
    }
    if msg.is_some() {
        s.pending_msg = msg;
    }
    if s.timer_armed {
        return;
    }
    s.timer_armed = true;
    let (state, id) = (state.clone(), s.info.id);
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        fire(&state, id);
    });
}

fn fire(state: &Shared, id: u32) {
    let mut guard = state.lock().unwrap();
    let st = &mut *guard;
    let Some(s) = st.sessions.get_mut(&id) else { return };
    s.timer_armed = false;
    s.muted_until = Instant::now() + Duration::from_secs(1);
    let message = s.pending_msg.take().filter(|m| !m.is_empty()).unwrap_or_else(|| DEFAULT_MSG.into());
    // 去抖期间收到了 hook 事件：交给 hook 通知
    if hooked(s) {
        return;
    }
    let title = s.info.name.clone();
    if s.info.status == Status::Running {
        s.info.status = Status::Waiting;
        st.push_sessions();
    }
    let toast = attention(st, id, &message);
    drop(guard);
    if toast {
        notify::toast(state, id, &title, &message);
    }
}

/// 该会话的通知由 Claude hook 接管：有 Claude 状态，或刚收到过 hook 事件（如 SessionEnd 前后的响铃）
fn hooked(s: &Session) -> bool {
    s.info.claude.is_some() || s.hook_at.is_some_and(|t| t.elapsed() < HOOK_SUPPRESS)
}

/// 告知 GUI 该会话有通知（侧栏提示圈）；返回是否需要弹系统通知：
/// 没有 GUI，或 GUI 的激活会话 + 窗口焦点不是该会话时弹
fn attention(st: &State, id: u32, message: &str) -> bool {
    match &st.gui {
        Some(g) => {
            g.send(&ServerMsg::Notify { id, message: message.into() });
            !(g.focused && g.active == Some(id))
        }
        None => true,
    }
}

/// Claude Code hook 事件（`ClientMsg::ClaudeEvent`）：更新会话的 Claude 状态并推送；Notification / Stop 按与 OSC9 相同的条件弹通知
pub fn claude_event(state: &Shared, ev: ClientMsg) {
    let ClientMsg::ClaudeEvent { session: id, event, .. } = &ev else { return };
    let id = *id;
    let mut guard = state.lock().unwrap();
    let st = &mut *guard;
    let Some(s) = st.sessions.get_mut(&id) else { return };
    if s.info.status == Status::Exited {
        return;
    }
    s.hook_at = Some(Instant::now());
    let resume = next_resume(s.resume.as_ref(), &s.info.cwd, &ev);
    if resume != s.resume {
        s.resume = resume;
        st.save_sessions();
    }
    let Some(s) = st.sessions.get_mut(&id) else { return };
    let prev = s.info.claude.as_ref().map(|c| c.state);
    let Some(Step { next, message, body }) = step(prev, &mut s.agents, &ev) else { return };
    s.info.claude = next.map(|state| {
        let count = (state == ClaudeStatus::Background).then_some(s.agents.bg);
        claude_state(state, if state == ClaudeStatus::Waiting { message } else { None }, count)
    });
    // 与 OSC9 一致：需要关注时状态点变为 waiting；重新提交提示后回到 running
    if body.is_some() && s.info.status == Status::Running {
        s.info.status = Status::Waiting;
    } else if next == Some(ClaudeStatus::Thinking) && s.info.status == Status::Waiting {
        s.info.status = Status::Running;
    }
    let title = format!("{} · Claude", s.info.name);
    st.push_sessions();
    let Some(body) = body else { return };
    let toast = attention(st, id, &body);
    log!("Claude 事件：会话 #{id} {event}，{}", if toast { "弹出系统通知" } else { "会话在前台，不弹通知" });
    drop(guard);
    if toast {
        notify::toast(state, id, &title, &body);
    }
}

/// 由 hook 事件更新会话的 Claude 对话：session_id 变化（新对话 / /clear / /resume）时记录，连同当时目录（缺失时用会话目录）；
/// SessionEnd 时清除，但 reason 为 other（多为进程被结束，如关机）时保留以便恢复。
/// 事件带 permission_mode 时记为最近模式；出现过 bypassPermissions 则一直记着，直到新的 claude 进程：
/// session_id 变化时，SessionStart 的 source 为 clear / resume / compact（同一进程内换对话）沿用上一条的权限记录，其它情况重置
fn next_resume(cur: Option<&saved::Resume>, session_cwd: &str, ev: &ClientMsg) -> Option<saved::Resume> {
    let ClientMsg::ClaudeEvent { event, claude_session, cwd, reason, permission_mode, source, .. } = ev else {
        return cur.cloned();
    };
    let mut next = match claude_session {
        _ if event == "SessionEnd" && reason.as_deref() != Some("other") => return None,
        Some(id) if cur.is_none_or(|c| &c.id != id) => {
            let same_process = event == "SessionStart" && matches!(source.as_deref(), Some("clear" | "resume" | "compact"));
            let (permission_mode, bypass) = match cur {
                Some(c) if same_process => (c.permission_mode.clone(), c.bypass),
                _ => (None, false),
            };
            saved::Resume { id: id.clone(), cwd: cwd.clone().unwrap_or_else(|| session_cwd.into()), permission_mode, bypass }
        }
        _ => cur.cloned()?,
    };
    if let Some(mode) = permission_mode {
        next.bypass |= mode == "bypassPermissions";
        next.permission_mode = Some(mode.clone());
    }
    Some(next)
}

fn claude_state(state: ClaudeStatus, message: Option<String>, count: Option<u32>) -> ClaudeState {
    ClaudeState { state, message, count, at: now_ms() }
}

/// 状态机一步的结果：next 为新状态（None 表示清除），message 为 Notification 消息，body 为需要弹的通知内容
#[derive(Debug, PartialEq)]
struct Step {
    next: Option<ClaudeStatus>,
    message: Option<String>,
    body: Option<String>,
}

/// Claude 状态机（不含推送 / 通知，便于测试）。返回 None 表示忽略该事件（状态不变、不推送）。
/// 主代理 Stop 时仍有后台子代理在运行 → Background（不通知）；子代理全部结束后 Claude 会被重新唤起处理结果
/// （不经 UserPromptSubmit），因此转为 Thinking，等到下一次没有后台子代理的 Stop 才算 Done 并通知
fn step(prev: Option<ClaudeStatus>, a: &mut Agents, ev: &ClientMsg) -> Option<Step> {
    let ClientMsg::ClaudeEvent { event, message, notification_type, agent_id, agent_type, background, .. } = ev else {
        return None;
    };
    let set = |next| Some(Step { next, message: None, body: None });
    // Claude Code 内部代理（提示建议、/btw 等）的 agent_type 为空串，不算子代理
    let subagent = agent_type.as_deref().is_some_and(|t| !t.is_empty());
    match event.as_str() {
        "SessionStart" => {
            *a = Agents::default();
            set(Some(ClaudeStatus::Idle))
        }
        "SessionEnd" => {
            *a = Agents::default();
            set(None)
        }
        "UserPromptSubmit" => {
            // 子代理可能跨提示继续运行，running 保留；bg 到下次 Stop 时重新计算
            a.bg = 0;
            set(Some(ClaudeStatus::Thinking))
        }
        "SubagentStart" => {
            if let (true, Some(id)) = (subagent, agent_id) {
                a.running.insert(id.clone(), Instant::now());
            }
            None
        }
        "SubagentStop" => {
            if !subagent {
                return None;
            }
            if let Some(id) = agent_id {
                a.running.remove(id);
            }
            if a.bg == 0 {
                return None;
            }
            a.bg -= 1;
            match prev {
                // 最后一个后台子代理结束：Claude 将被唤起处理结果
                Some(ClaudeStatus::Background) if a.bg == 0 => set(Some(ClaudeStatus::Thinking)),
                Some(ClaudeStatus::Background) => set(Some(ClaudeStatus::Background)),
                // 等待确认等其它状态只更新计数
                _ => None,
            }
        }
        "Stop" => {
            // background_tasks 为权威数据：没有子代理时清空登记，防止漏掉的 SubagentStop 让会话一直卡住；
            // 旧版 Claude Code 不带该字段时按登记数计算，并丢弃过久未结束的登记
            a.bg = match background {
                Some(n) => {
                    if *n == 0 {
                        a.running.clear();
                    }
                    *n
                }
                None => {
                    a.running.retain(|_, t| t.elapsed() < AGENT_EXPIRE);
                    a.running.len() as u32
                }
            };
            if a.bg > 0 {
                return set(Some(ClaudeStatus::Background));
            }
            Some(Step { next: Some(ClaudeStatus::Done), message: None, body: Some("任务已完成".into()) })
        }
        "Notification" => {
            let message = message.clone().filter(|m| !m.trim().is_empty());
            match notification_type.as_deref() {
                // 空闲提醒（回复完成后一段时间）：已完成时 Stop 已通知过，后台运行中也不是在等用户，均保持不变
                Some("idle_prompt") if matches!(prev, Some(ClaudeStatus::Done | ClaudeStatus::Background)) => None,
                // 其余情况（Esc 中断后没有 Stop、刚启动未提问等）只是空闲，不算等待确认，也不通知
                Some("idle_prompt") => set(Some(ClaudeStatus::Idle)),
                // 不需要用户输入的通知类型
                Some("auth_success" | "elicitation_complete" | "elicitation_response") => None,
                // 含后台子代理的权限请求（显示在主会话中）
                _ => {
                    let body = message.clone().unwrap_or_else(|| "Claude 需要你的关注".into());
                    Some(Step { next: Some(ClaudeStatus::Waiting), message, body: Some(body) })
                }
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ClaudeStatus::*;

    /// 模拟一个会话的 Claude 状态：依次喂事件，记录状态与通知
    #[derive(Default)]
    struct Sim {
        state: Option<ClaudeStatus>,
        agents: Agents,
        toasts: Vec<String>,
    }

    impl Sim {
        fn feed(&mut self, event: &str, agent: Option<(&str, &str)>, background: Option<u32>) -> Option<ClaudeStatus> {
            let ev = ClientMsg::ClaudeEvent {
                session: 1,
                event: event.into(),
                message: None,
                notification_type: None,
                agent_id: agent.map(|a| a.0.into()),
                agent_type: agent.map(|a| a.1.into()),
                background,
                claude_session: None,
                cwd: None,
                reason: None,
                permission_mode: None,
                source: None,
            };
            if let Some(step) = step(self.state, &mut self.agents, &ev) {
                self.state = step.next;
                self.toasts.extend(step.body);
            }
            self.state
        }
        fn start(&mut self, id: &str) {
            self.feed("SubagentStart", Some((id, "general-purpose")), None);
        }
        fn stop_agent(&mut self, id: &str) -> Option<ClaudeStatus> {
            self.feed("SubagentStop", Some((id, "general-purpose")), Some(0))
        }
    }

    #[test]
    fn plain_turn_done() {
        let mut s = Sim::default();
        assert_eq!(s.feed("SessionStart", None, None), Some(Idle));
        assert_eq!(s.feed("UserPromptSubmit", None, None), Some(Thinking));
        assert_eq!(s.feed("Stop", None, Some(0)), Some(Done));
        assert_eq!(s.toasts.len(), 1);
    }

    #[test]
    fn foreground_subagents_do_not_delay_done() {
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        s.start("a");
        assert_eq!(s.stop_agent("a"), Some(Thinking));
        assert_eq!(s.feed("Stop", None, Some(0)), Some(Done));
        assert_eq!(s.toasts.len(), 1);
    }

    #[test]
    fn background_subagents_delay_done_until_next_stop() {
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        s.start("a");
        s.start("b");
        // 主代理结束，2 个后台子代理仍在运行：不通知
        assert_eq!(s.feed("Stop", None, Some(2)), Some(Background));
        assert_eq!(s.agents.bg, 2);
        assert_eq!(s.stop_agent("a"), Some(Background));
        assert_eq!(s.agents.bg, 1);
        // 最后一个结束：Claude 被唤起处理结果（无 UserPromptSubmit）
        assert_eq!(s.stop_agent("b"), Some(Thinking));
        assert!(s.toasts.is_empty());
        assert_eq!(s.feed("Stop", None, Some(0)), Some(Done));
        assert_eq!(s.toasts, ["任务已完成"]);
    }

    #[test]
    fn resumed_turn_with_remaining_subagent_stays_background() {
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        assert_eq!(s.feed("Stop", None, Some(2)), Some(Background));
        s.stop_agent("a");
        // Claude 处理第一个结果后再次 Stop，仍有 1 个在运行
        assert_eq!(s.feed("Stop", None, Some(1)), Some(Background));
        assert_eq!(s.stop_agent("b"), Some(Thinking));
        assert_eq!(s.feed("Stop", None, Some(0)), Some(Done));
        assert_eq!(s.toasts.len(), 1);
    }

    #[test]
    fn internal_agents_ignored() {
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        s.feed("Stop", None, Some(0));
        // 提示建议等内部代理：agent_type 为空串
        s.feed("SubagentStart", Some(("x", "")), None);
        assert_eq!(s.feed("SubagentStop", Some(("x", "")), Some(0)), Some(Done));
        assert!(s.agents.running.is_empty());
        assert_eq!(s.toasts.len(), 1);
    }

    #[test]
    fn subagent_permission_prompt_then_back_to_background() {
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        s.feed("Stop", None, Some(1));
        assert_eq!(s.feed("Notification", None, None), Some(Waiting));
        assert_eq!(s.toasts.len(), 1);
        // 空闲提醒在后台运行中不通知
        s.state = Some(Background);
        let idle = ClientMsg::ClaudeEvent {
            session: 1,
            event: "Notification".into(),
            message: None,
            notification_type: Some("idle_prompt".into()),
            agent_id: None,
            agent_type: None,
            background: None,
            claude_session: None,
            cwd: None,
            reason: None,
            permission_mode: None,
            source: None,
        };
        assert_eq!(step(s.state, &mut s.agents, &idle), None);
    }

    #[test]
    fn idle_prompt_means_idle_not_waiting() {
        let idle = ClientMsg::ClaudeEvent {
            session: 1,
            event: "Notification".into(),
            message: Some("Claude is waiting for your input".into()),
            notification_type: Some("idle_prompt".into()),
            agent_id: None,
            agent_type: None,
            background: None,
            claude_session: None,
            cwd: None,
            reason: None,
            permission_mode: None,
            source: None,
        };
        // Esc 中断后没有 Stop（停在 Thinking）、刚启动未提问（Idle）：空闲提醒转为 Idle，不通知
        for prev in [Some(Thinking), Some(Idle), Some(Waiting), None] {
            let mut a = Agents::default();
            assert_eq!(step(prev, &mut a, &idle), Some(Step { next: Some(Idle), message: None, body: None }));
        }
        // 已完成 / 后台运行中：保持原状态
        for prev in [Some(Done), Some(Background)] {
            let mut a = Agents::default();
            assert_eq!(step(prev, &mut a, &idle), None);
        }
    }

    #[test]
    fn missing_subagent_stop_cannot_stick() {
        // 有 background_tasks：Stop 报告 0 个子代理时清空漏掉的登记
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        s.start("lost");
        assert_eq!(s.feed("Stop", None, Some(0)), Some(Done));
        assert!(s.agents.running.is_empty());

        // 旧版（无 background_tasks）：按登记数计算，过期登记被丢弃
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        s.start("lost");
        assert_eq!(s.feed("Stop", None, None), Some(Background));
        s.agents.running.insert("lost".into(), Instant::now() - AGENT_EXPIRE - Duration::from_secs(1));
        s.feed("UserPromptSubmit", None, None);
        assert_eq!(s.feed("Stop", None, None), Some(Done));

        // 卡在 Background 时：新提示、SessionStart（/clear）、SessionEnd 都能恢复
        let mut s = Sim::default();
        s.feed("Stop", None, Some(1));
        assert_eq!(s.feed("UserPromptSubmit", None, None), Some(Thinking));
        assert_eq!(s.agents.bg, 0);
        s.feed("Stop", None, Some(1));
        assert_eq!(s.feed("SessionStart", None, None), Some(Idle));
        assert_eq!(s.agents.bg, 0);
        s.start("a");
        s.feed("Stop", None, None);
        assert_eq!(s.feed("SessionEnd", None, None), None);
        assert!(s.agents.running.is_empty() && s.agents.bg == 0);
        // 多余的 SubagentStop 不会让计数变负
        assert_eq!(s.stop_agent("zz"), None);
        assert_eq!(s.agents.bg, 0);
    }

    fn ev(event: &str, sid: Option<&str>, cwd: Option<&str>, reason: Option<&str>) -> ClientMsg {
        ev_mode(event, sid, cwd, reason, None)
    }

    fn ev_mode(event: &str, sid: Option<&str>, cwd: Option<&str>, reason: Option<&str>, mode: Option<&str>) -> ClientMsg {
        ClientMsg::ClaudeEvent {
            session: 1,
            event: event.into(),
            message: None,
            notification_type: None,
            agent_id: None,
            agent_type: None,
            background: None,
            claude_session: sid.map(Into::into),
            cwd: cwd.map(Into::into),
            reason: reason.map(Into::into),
            permission_mode: mode.map(Into::into),
            source: None,
        }
    }

    #[test]
    fn resume_tracking() {
        let r = next_resume(None, r"C:\s", &ev("SessionStart", Some("a"), Some(r"C:\p"), None)).unwrap();
        assert_eq!((r.id.as_str(), r.cwd.as_str()), ("a", r"C:\p"));
        // 同一对话后续事件的 cwd 变化不影响记录的启动目录
        assert_eq!(next_resume(Some(&r), r"C:\s", &ev("Stop", Some("a"), Some(r"C:\p\sub"), None)).as_ref(), Some(&r));
        // 不带 session_id 的事件不改变
        assert_eq!(next_resume(Some(&r), r"C:\s", &ev("Stop", None, None, None)).as_ref(), Some(&r));
        // 新对话；缺 cwd 时用会话目录
        let b = next_resume(Some(&r), r"C:\s", &ev("SessionStart", Some("b"), None, None)).unwrap();
        assert_eq!((b.id.as_str(), b.cwd.as_str()), ("b", r"C:\s"));
        // 正常退出清除；进程被结束（other）保留
        assert_eq!(next_resume(Some(&b), "", &ev("SessionEnd", Some("b"), None, Some("prompt_input_exit"))), None);
        assert_eq!(next_resume(Some(&b), "", &ev("SessionEnd", Some("b"), None, Some("other"))).as_ref(), Some(&b));
    }

    #[test]
    fn resume_permission_mode() {
        let next = |cur: Option<&saved::Resume>, sid: Option<&str>, m: Option<&str>| {
            next_resume(cur, r"C:\s", &ev_mode("Stop", sid, None, None, m)).unwrap()
        };
        // 以 --dangerously-skip-permissions 启动
        let r = next(None, Some("a"), Some("bypassPermissions"));
        assert_eq!((r.permission_mode.as_deref(), r.bypass), (Some("bypassPermissions"), true));
        // Shift+Tab 切到别的模式：记录最近模式，bypass 粘滞
        let r = next(Some(&r), Some("a"), Some("acceptEdits"));
        assert_eq!((r.permission_mode.as_deref(), r.bypass), (Some("acceptEdits"), true));
        // 不带 permission_mode 的事件不改变记录
        assert_eq!(next(Some(&r), Some("a"), None), r);
        assert_eq!(next(Some(&r), None, None), r);
        // 不带 session_id 但带模式的事件也更新
        let r = next(Some(&r), None, Some("plan"));
        assert_eq!((r.permission_mode.as_deref(), r.bypass), (Some("plan"), true));
        // 新对话重置
        let b = next(Some(&r), Some("b"), None);
        assert_eq!((b.permission_mode, b.bypass), (None, false));
        let b = next(Some(&r), Some("b"), Some("default"));
        assert_eq!((b.permission_mode.as_deref(), b.bypass), (Some("default"), false));
        // 尚无对话记录时，不带 session_id 的事件不建立记录
        assert_eq!(next_resume(None, "", &ev_mode("Stop", None, None, None, Some("plan"))), None);
    }

    #[test]
    fn resume_permission_across_conversations() {
        let start = |cur: &saved::Resume, sid: &str, source: Option<&str>| {
            let mut e = ev("SessionStart", Some(sid), None, None);
            if let ClientMsg::ClaudeEvent { source: s, .. } = &mut e {
                *s = source.map(Into::into);
            }
            next_resume(Some(cur), r"C:\s", &e).unwrap()
        };
        // 以 skip 启动后切到 acceptEdits
        let r = saved::Resume {
            id: "a".into(),
            cwd: r"C:\p".into(),
            permission_mode: Some("acceptEdits".into()),
            bypass: true,
        };
        // 同一进程内换对话：沿用权限记录
        for source in ["clear", "resume", "compact"] {
            let b = start(&r, "b", Some(source));
            assert_eq!((b.id.as_str(), b.permission_mode.as_deref(), b.bypass), ("b", Some("acceptEdits"), true), "{source}");
        }
        // 新进程或 source 缺失：重置
        for source in [Some("startup"), None] {
            let b = start(&r, "b", source);
            assert_eq!((b.permission_mode, b.bypass), (None, false), "{source:?}");
        }
        // 非 SessionStart 事件带来新 session_id：重置
        let b = next_resume(Some(&r), "", &ev("Stop", Some("b"), None, None)).unwrap();
        assert_eq!((b.permission_mode, b.bypass), (None, false));
    }

    #[test]
    fn legacy_counting_without_background_tasks() {
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        s.start("a");
        assert_eq!(s.feed("Stop", None, None), Some(Background));
        assert_eq!(s.feed("SubagentStop", Some(("a", "Explore")), None), Some(Thinking));
        assert_eq!(s.feed("Stop", None, None), Some(Done));
    }
}
