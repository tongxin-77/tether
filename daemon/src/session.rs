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

use crate::{claude, config, fs, git, history, hub, notify, now_ms, recent, saved, ssh, stats, sudo};

pub type Shared = Arc<Mutex<State>>;

/// 交互提示的回复：(输入值, 是否保存)，None 表示取消
pub type PromptAnswer = Option<(String, bool)>;

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
    /// 有 Claude 状态但进程树中连续不见 Claude 进程的采样次数（检测异常退出）
    claude_missing: u8,
}

/// Claude 子代理 / 后台任务跟踪：running 为 SubagentStart 登记、SubagentStop 移除的子代理（id → 登记时间）；
/// bg 为主代理 Stop 时仍在运行的后台任务数，此后由 SubagentStop 刷新（旧版不带 background_tasks 时每个减一）；
/// session 为最近一次 SessionStart 的 Claude 对话 session_id，据此丢弃旧对话迟到的事件
#[derive(Default)]
struct Agents {
    running: HashMap<String, Instant>,
    /// 在 background_tasks 中以子代理出现过的 id：running 里其余的登记即忙碌的 teammate
    /// （teammate 空闲等待时 background_tasks 仍报 running，且其 id 与 agent_id 不同，无法按 id 扣除）
    subagents: HashSet<String>,
    bg: u32,
    session: Option<String>,
}

/// 会话 I/O 后端
pub enum Backend {
    Local {
        master: Box<dyn MasterPty + Send>,
        writer: Box<dyn Write + Send>,
        killer: Box<dyn ChildKiller + Send + Sync>,
        /// shell 进程 pid（内存占用据此统计进程树），取不到为 None
        pid: Option<u32>,
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
        let sessions = self
            .sessions
            .values()
            .map(|s| SessionInfo {
                claude_session: s.resume.as_ref().map(|r| r.id.clone()),
                fs_root: s.info.status != Status::Exited && s.fs.is_root(),
                ..s.info.clone()
            })
            .collect();
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
            name_custom: s.info.name_custom,
            claude_title: s.info.claude_title.clone(),
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
                Some(host) => drop(tokio::spawn(ssh::create(state.clone(), host, cwd, cols, rows, false))),
                None => st.send(&ServerMsg::Error { message: tr!("common.host_not_found", host = host_id) }),
            }
        }
        ClientMsg::Create { target: Target::Local { cwd }, cols, rows, command, resume } => {
            // 启动 PTY 较慢，不持锁
            drop(guard);
            let result = create_local(state, command, cwd, cols, rows, resume);
            let st = state.lock().unwrap();
            match result {
                Ok(id) => {
                    st.push_sessions();
                    st.send(&ServerMsg::Activate { id });
                }
                Err(message) => st.send(&ServerMsg::Error { message }),
            }
        }
        ClientMsg::Attach { id, history } => {
            let Some(s) = st.sessions.get(&id) else { return true };
            let mut data = if history { s.parser.callbacks().history.render(s.parser.screen()) } else { Vec::new() };
            data.extend_from_slice(s.parser.screen().state_formatted().as_bytes());
            let data = B64.encode(data);
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
            // 手动改过名后不再被 Claude 对话标题覆盖
            s.info.name_custom = true;
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
            // 语言只由 SetLocale 修改
            config.locale = st.config.locale.clone();
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
                    st.send(&ServerMsg::Error { message: tr!("session.save_config_failed", error = e) });
                }
            }
        }
        ClientMsg::SetHostPassword { host_id, password } => {
            let Some(host) = config::all_hosts(&st.config).into_iter().find(|h| h.id == host_id) else {
                st.send(&ServerMsg::Error { message: tr!("common.host_not_found", host = host_id) });
                return true;
            };
            if let Err(e) = ssh::set_password(&ssh::Keyring, &host, password.as_ref().map(|p| p.0.as_str())) {
                log!("保存密码失败：{e}");
                st.send(&ServerMsg::Error { message: tr!("session.save_password_failed", error = e) });
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
        ClientMsg::FsWrite { req_id, session_id, path, text, encoding, crlf, mtime, size, force } => {
            let w = fs::WriteReq { path, text, encoding, crlf, mtime, size, force };
            fs_spawn(state, st, req_id, session_id, fs::Op::Write(Box::new(w)))
        }
        ClientMsg::Upload { req_id, session_id, local_paths, dest_dir } => {
            fs_spawn(state, st, req_id, session_id, fs::Op::Upload(local_paths, dest_dir))
        }
        ClientMsg::Download { req_id, session_id, paths, local_dir } => {
            fs_spawn(state, st, req_id, session_id, fs::Op::Download(paths, local_dir))
        }
        ClientMsg::FsCopy { req_id, session_id, source, items, dest_dir } => {
            let Some(dest) = st.sessions.get(&session_id).map(|s| s.fs.clone()) else {
                st.send(&ServerMsg::FsResult { req_id, error: Some(tr!("common.session_gone")) });
                return true;
            };
            // 源按主机定位：源会话关闭后只要主机仍可连接（池中连接复用，或重新连接）即可粘贴
            let host = match source {
                FsSource::Local => None,
                FsSource::Remote { host_id } => match config::all_hosts(&st.config).into_iter().find(|h| h.id == host_id) {
                    Some(h) => Some(h),
                    None => {
                        st.send(&ServerMsg::FsResult { req_id, error: Some(tr!("session.source_host_not_found", host = host_id)) });
                        return true;
                    }
                },
            };
            drop(tokio::spawn(fs::copy(state.clone(), req_id, dest, host, items, dest_dir)));
        }
        ClientMsg::FsRoot { req_id, session_id, enabled } => match st.sessions.get(&session_id) {
            // 可能要等用户输入 sudo 密码，放到异步任务中
            Some(s) => drop(tokio::spawn(sudo::toggle(state.clone(), req_id, s.fs.clone(), enabled))),
            None => st.send(&ServerMsg::FsResult { req_id, error: Some(tr!("common.session_gone")) }),
        },
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
            let result = claude::settings_path().ok_or_else(|| tr!("common.no_home")).and_then(|p| {
                let r = if enabled { claude::hook_command().and_then(|c| claude::enable(&p, &c)) } else { claude::disable(&p) };
                log!("Claude 集成{}（{}）：{r:?}", if enabled { "启用" } else { "移除" }, p.display());
                r
            });
            st.send(&claude::status_msg(result.err()));
        }
        ClientMsg::PlaySound => notify::play(),
        ClientMsg::SetLocale { locale } => {
            let code = crate::i18n::set(&locale).code();
            if st.config.locale.as_deref() != Some(code) {
                log!("界面语言：{code}");
                st.config.locale = Some(code.into());
                if let Err(e) = config::save(&st.config) {
                    log!("保存配置失败：{e}");
                }
            }
        }
        ClientMsg::Hello { version, proto } => {
            log!("GUI 版本 {version}（协议 {proto}），daemon 版本 {}", env!("CARGO_PKG_VERSION"));
            st.send(&ServerMsg::Hello { version: env!("CARGO_PKG_VERSION").into(), proto: proto::PROTO_VERSION });
        }
        ClientMsg::HostStats { req_id, session_id } => match st.sessions.get(&session_id) {
            Some(s) => drop(tokio::spawn(stats::run(state.clone(), req_id, s.fs.clone()))),
            None => st.send(&ServerMsg::HostStats { req_id, stats: None, error: Some(tr!("common.session_gone")) }),
        },
        ClientMsg::Git { req_id, session_id, op } => match st.sessions.get(&session_id) {
            Some(s) => drop(tokio::spawn(git::run(state.clone(), req_id, s.fs.clone(), op))),
            None => st.send(&ServerMsg::Git { req_id, out: None, error: Some(tr!("common.session_gone")) }),
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
                    st.send(&ServerMsg::Error { message: tr!("session.save_hub_token_failed", error = e) });
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
    // 等待确认时用户作答（按键）后 Claude 继续处理；主代理已结束、只剩后台任务时回到后台运行中
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

/// 未退出的本地会话及其 shell pid（内存占用采样用）
pub fn local_pids(st: &State) -> Vec<(u32, u32)> {
    st.sessions
        .iter()
        .filter(|(_, s)| s.info.status != Status::Exited)
        .filter_map(|(&id, s)| match s.backend {
            Backend::Local { pid: Some(pid), .. } => Some((id, pid)),
            _ => None,
        })
        .collect()
}

/// 关闭鼠标跟踪（各模式与编码）、显示光标、关闭焦点上报、恢复 DECCKM；不动 bracketed paste（PSReadLine 自己会开）
const MODE_RESET: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?25h\x1b[?1004l\x1b[?1l";

/// Claude Code 会话状态文件处于 idle 时的 (sessionId, statusUpdatedAt 毫秒)
pub type ClaudeIdle = (String, u64);

/// 内存采样时顺带检查 Claude 进程：alive 为（会话 id, 进程树中是否有 Claude 进程）。有 Claude 状态却连续两次不见 Claude 进程
/// 视为异常退出（未触发 SessionEnd）：按 SessionEnd（reason 为 other，保留 resume 以便恢复）清除状态，复位 vt100 的终端模式
/// （之后的快照不再带鼠标跟踪 / 备用缓冲），并让 GUI 复位 xterm。
/// idle 为 Claude Code 会话状态文件处于 idle 时的 (sessionId, statusUpdatedAt)：Esc 中断不触发 Stop（也不发 idle_prompt），
/// 思考中之后同一对话变为 idle 即视为已中断，回到空闲
pub fn check_claude(st: &mut State, alive: &[(u32, bool, Option<ClaudeIdle>)]) {
    let mut changed = false;
    for (id, present, idle) in alive {
        let (id, present) = (*id, *present);
        let Some(s) = st.sessions.get_mut(&id).filter(|s| s.info.status != Status::Exited) else { continue };
        if let (Some(c), Some((sid, at))) = (s.info.claude.as_mut(), idle)
            && c.state == ClaudeStatus::Thinking
            && *at > c.at
            && s.agents.session.as_ref().is_none_or(|cur| cur == sid)
        {
            log!("会话 #{id}：思考中后 Claude 已空闲（未收到 Stop，多为 Esc 中断），回到空闲");
            *c = claude_state(ClaudeStatus::Idle, None, None);
            changed = true;
        }
        if !claude_gone(&mut s.claude_missing, s.info.claude.is_some(), present) {
            continue;
        }
        log!("会话 #{id}：Claude 进程已不在（未收到 SessionEnd），清除 Claude 状态并复位终端模式");
        // 同 step 的 SessionEnd：保留 session，该对话迟到的事件仍按过期丢弃
        s.agents = Agents { session: s.agents.session.take(), ..Agents::default() };
        s.info.claude = None;
        let mut reset = Vec::new();
        if s.parser.screen().alternate_screen() {
            reset.extend_from_slice(b"\x1b[?1049l");
        }
        reset.extend_from_slice(MODE_RESET);
        history::process(&mut s.parser, &reset);
        if let Some(g) = &st.gui {
            g.send(&ServerMsg::ResetModes { id });
        }
        // hub 网页终端只镜像 pty 输出，缓冲区状态与 daemon 的 parser 一致，直接下发同一段复位序列
        if s.info.taken_over {
            st.hub.output(id, &reset);
        }
        changed = true;
    }
    if changed {
        st.push_sessions();
    }
}

/// 连续缺失计数：Claude 运行中且进程树中不见 Claude 进程时加一，满两次（防竞态：如 hook 先于进程出现在快照中）返回 true；其它情况清零
fn claude_gone(missing: &mut u8, running: bool, present: bool) -> bool {
    if !running || present {
        *missing = 0;
        return false;
    }
    *missing += 1;
    if *missing < 2 {
        return false;
    }
    *missing = 0;
    true
}

/// 未退出的会话正在运行的 Claude 对话 id（hub 网页列 Claude 历史时标记 open，同桌面端）
pub fn claude_sessions(st: &State) -> HashSet<String> {
    st.sessions.values().filter(|s| s.info.status != Status::Exited).filter_map(|s| s.resume.as_ref().map(|r| r.id.clone())).collect()
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
        None => st.send(&ServerMsg::FsResult { req_id, error: Some(tr!("common.session_gone")) }),
    }
}

/// 新建本地会话（GUI 与 hub 网页新建共用，不持锁调用）：command 缺省用配置的 shell，cwd 为空进用户主目录，
/// 有 resume 时在 shell 就绪后恢复该 Claude 对话；失败已记日志，返回给用户看的原因。推送与激活由调用方负责
pub fn create_local(state: &Shared, command: Option<String>, cwd: String, cols: u16, rows: u16, resume: Option<proto::ClaudeResume>) -> Result<u32, String> {
    // 自动模式每次新建时现场探测，安装 PowerShell 7 后无需重启 daemon
    let shell = command.unwrap_or_else(|| config::resolve_shell(&state.lock().unwrap().config.shell));
    // 未指定目录（“+ 新建 → 本地”）时进用户主目录
    let cwd = match cwd.trim() {
        "" => config::home().map(|h| h.display().to_string()).unwrap_or(cwd),
        _ => cwd,
    };
    let result = spawn(state, &shell, cwd, cols, rows);
    if let (Ok(id), Some(r)) = (&result, resume) {
        resume_claude(state, *id, r);
    }
    result.map_err(|e| {
        log!("启动 shell 失败（{shell}）：{e}");
        tr!("session.shell_start_failed", shell = shell, error = e)
    })
}

/// 启动配置中的 shell，登记会话并起读线程、等待线程
fn spawn(state: &Shared, shell: &str, cwd: String, cols: u16, rows: u16) -> Result<u32, String> {
    let argv = config::split_command(shell);
    let (program, args) = argv.split_first().ok_or_else(|| tr!("session.no_default_shell"))?;
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
    let pid = child.process_id();

    let name = cwd.split(['\\', '/']).rfind(|s| !s.is_empty()).unwrap_or(cwd.as_str()).to_string();
    let info = new_info(name, cwd, shell.into(), None);
    register_with_id(state, id, info, Backend::Local { master: pair.master, writer, killer, pid }, fs::Backend::Local, cols, rows);

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
    SessionInfo {
        id: 0,
        name,
        cwd,
        command,
        status: Status::Running,
        started_at: now,
        last_activity_at: now,
        host,
        host_id: None,
        claude: None,
        taken_over: false,
        claude_session: None,
        fs_root: false,
        name_custom: false,
        claude_title: None,
    }
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
        claude_missing: 0,
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
                    s.info.name_custom = item.name_custom;
                    s.info.claude_title = item.claude_title;
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
            Some(host) => drop(tokio::spawn(ssh::create(state.clone(), host, Some(item.cwd), 120, 30, false))),
            None => log!("恢复远程会话失败：未找到主机 {host_id}"),
        }
    }
}

/// 在新建的会话中恢复 Claude 对话（GUI「恢复 Claude 对话」）：与 daemon 重启后的恢复相同，记下对话并在 shell 就绪后输入
/// 带权限模式的 `claude --resume`；id 含意外字符时什么都不做
fn resume_claude(state: &Shared, id: u32, r: proto::ClaudeResume) {
    let mut st = state.lock().unwrap();
    let Some(s) = st.sessions.get_mut(&id) else { return };
    let resume = saved::Resume { id: r.id, cwd: s.info.cwd.clone(), permission_mode: r.permission_mode, bypass: r.bypass };
    let Some(cmd) = saved::resume_command(&resume) else { return };
    s.resume = Some(resume);
    st.save_sessions();
    drop(st);
    type_when_ready(state, id, cmd);
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
    let message = s.pending_msg.take().filter(|m| !m.is_empty()).unwrap_or_else(|| tr!("notify.attention"));
    // 去抖期间收到了 hook 事件：交给 hook 通知
    if hooked(s) {
        return;
    }
    let title = s.info.display_name().to_string();
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
    let ClientMsg::ClaudeEvent {
        session: id,
        event,
        notification_type,
        agent_id,
        agent_type,
        background,
        tasks,
        prompt,
        claude_session,
        reason,
        source,
        ..
    } = &ev
    else {
        return;
    };
    let id = *id;
    let sid: String = claude_session.as_deref().unwrap_or_default().chars().take(8).collect();
    log!(
        "Claude hook：会话 #{id} {event} source={source:?} reason={reason:?} type={notification_type:?} bg={background:?}[{}] prompt={prompt:?} agent={agent_id:?}/{agent_type:?} sid={sid}",
        tasks.as_deref().unwrap_or_default()
    );
    let mut guard = state.lock().unwrap();
    let st = &mut *guard;
    let Some(s) = st.sessions.get_mut(&id) else { return };
    if s.info.status == Status::Exited {
        return;
    }
    s.hook_at = Some(Instant::now());
    if stale(&s.agents, &ev) {
        return;
    }
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
    let title = format!("{} · Claude", s.info.display_name());
    let source = title_source(s.resume.as_ref(), &ev, proto::claude_history::projects_dir().as_deref());
    st.push_sessions();
    // 对话文件可能较大：在独立线程读（daemon 是单线程运行时，不阻塞管道 I/O），读完再加锁写回
    if let Some(src) = source {
        let state = state.clone();
        thread::spawn(move || {
            let title = src.path.as_deref().and_then(proto::claude_history::title);
            update_title(&mut state.lock().unwrap(), id, &src.id, title, src.reset);
        });
    }
    let Some(body) = body else { return };
    let toast = attention(st, id, &body);
    log!("Claude 事件：会话 #{id} {event}，{}", if toast { "弹出系统通知" } else { "会话在前台，不弹通知" });
    drop(guard);
    if toast {
        notify::toast(state, id, &title, &body);
    }
}

/// 读 Claude 对话标题的来源
#[derive(Debug, PartialEq)]
struct TitleSource {
    /// 对话 session_id：读完写回时据此确认会话仍是这个对话
    id: String,
    /// 对话文件；找不到时为 None
    path: Option<std::path::PathBuf>,
    /// 换了对话（SessionStart）：读不到标题时清空旧标题，显示原名
    reset: bool,
}

/// 需要刷新对话标题的事件：Stop（一轮结束，ai-title / 首条消息此时已落盘）与 SessionStart（compact 除外：同一对话）。
/// UserPromptSubmit 时对话文件是异步写入、往往还没有本轮内容，且紧接着必有 Stop，故不读。
/// 对话文件优先用 hook 的 transcript_path；旧版 hook 不带时按 Claude 启动目录（resume.cwd，而非 hook 当时的 cwd：
/// Claude 中途 cd 不改变项目目录）与 session_id 推导
fn title_source(resume: Option<&saved::Resume>, ev: &ClientMsg, projects: Option<&std::path::Path>) -> Option<TitleSource> {
    let ClientMsg::ClaudeEvent { event, transcript_path, source, .. } = ev else { return None };
    let reset = match event.as_str() {
        "Stop" => false,
        "SessionStart" if source.as_deref() != Some("compact") => true,
        _ => return None,
    };
    let r = resume?;
    let path = match transcript_path {
        Some(p) => Some(p.into()),
        None => projects.and_then(|p| proto::claude_history::transcript(p, &r.cwd, &r.id)),
    };
    Some(TitleSource { id: r.id.clone(), path, reset })
}

/// 写回读到的对话标题：会话已换对话（或已退出 Claude 且清除了对话）时丢弃；Stop 读不到时保留上次的标题。
/// 标题变化才推送并保存；手动改过名（name_custom）时照样记下标题，只是显示名不用它
fn update_title(st: &mut State, id: u32, sid: &str, title: Option<String>, reset: bool) {
    let Some(s) = st.sessions.get_mut(&id) else { return };
    if s.resume.as_ref().is_none_or(|r| r.id != sid) {
        return;
    }
    let next = if reset { title } else { title.or_else(|| s.info.claude_title.clone()) };
    if next == s.info.claude_title {
        return;
    }
    log!("Claude 对话标题：会话 #{id} {next:?}");
    s.info.claude_title = next;
    st.push_sessions();
    st.save_sessions();
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

/// 每个 hook 是独立的 `tetherd hook` 进程，到达顺序无保证：带 session_id 且不是最近一次 SessionStart 的对话的事件
/// （如 /clear 后旧对话迟到的事件）视为过期；没有 session_id 的事件（旧版）或尚未见过 SessionStart 时不过滤
fn stale(a: &Agents, ev: &ClientMsg) -> bool {
    let ClientMsg::ClaudeEvent { event, claude_session: Some(sid), .. } = ev else { return false };
    event != "SessionStart" && a.session.as_ref().is_some_and(|cur| cur != sid)
}

/// UserPromptSubmit 的 prompt 是否为斜杠命令（首个词以 / 开头且不含路径分隔符；以 / 开头的路径不算）
fn slash_command(prompt: &str) -> bool {
    prompt.strip_prefix('/').and_then(|rest| rest.split(char::is_whitespace).next()).is_some_and(|name| !name.is_empty() && !name.contains(['/', '\\']))
}

/// Claude 状态机（不含推送 / 通知，便于测试）。返回 None 表示忽略该事件（状态不变、不推送）。
/// 主代理 Stop 时仍有后台任务（子代理、后台 shell 等）在运行 → Background（不通知）；新版 Claude Code 在后台任务结束后
/// 经 UserPromptSubmit 唤起 Claude 处理结果（转为 Thinking），等到下一次没有后台任务的 Stop 才算 Done 并通知
/// 后台代理数：background_tasks 中的子代理照计；teammate 只计忙碌的（已 SubagentStart、未 SubagentStop、不是子代理的登记），
/// 至多为列表中的 teammate 数。n 为列表中未结束的代理总数
fn agents_bg(a: &mut Agents, n: u32, teammates: &[String], subagents: &[String]) -> u32 {
    a.subagents.extend(subagents.iter().cloned());
    let busy = a.running.keys().filter(|id| !a.subagents.contains(*id)).count() as u32;
    n.saturating_sub(teammates.len() as u32) + busy.min(teammates.len() as u32)
}

fn step(prev: Option<ClaudeStatus>, a: &mut Agents, ev: &ClientMsg) -> Option<Step> {
    let ClientMsg::ClaudeEvent { event, message, notification_type, agent_id, agent_type, background, teammates, subagents, prompt, claude_session, source, .. } = ev
    else {
        return None;
    };
    if stale(a, ev) {
        return None;
    }
    let set = |next| Some(Step { next, message: None, body: None });
    // Claude Code 内部代理（提示建议、/btw 等）的 agent_type 为空串，不算子代理
    let subagent = agent_type.as_deref().is_some_and(|t| !t.is_empty());
    match event.as_str() {
        // 压缩（可能是回复中途的自动压缩）：同一对话继续，状态与计数不变；尚无状态时视为空闲
        "SessionStart" if source.as_deref() == Some("compact") => {
            if claude_session.is_some() {
                a.session = claude_session.clone();
            }
            prev.is_none().then(|| set(Some(ClaudeStatus::Idle))).flatten()
        }
        "SessionStart" => {
            *a = Agents { session: claude_session.clone(), ..Agents::default() };
            set(Some(ClaudeStatus::Idle))
        }
        "SessionEnd" => {
            // 保留 session：该对话迟到的事件仍按过期丢弃
            *a = Agents { session: a.session.take(), ..Agents::default() };
            set(None)
        }
        // 斜杠命令：旧版 Claude Code 的内置命令（/clear、/help 等）也触发 UserPromptSubmit，不进入思考中；
        // 自定义命令 / skill 随后有 UserPromptExpansion
        "UserPromptSubmit" if prompt.as_deref().is_some_and(slash_command) => None,
        "UserPromptSubmit" | "UserPromptExpansion" => {
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
            // 新版带 background_tasks（父会话视角，已除去该子代理自身）：只刷新计数、不自行退出 Background——
            // 子代理留下的后台任务可能仍在运行；Claude 被唤起时有 UserPromptSubmit，最终由 Stop 判断是否完成
            if let Some(n) = background {
                a.bg = agents_bg(a, *n, teammates, subagents);
                return (prev == Some(ClaudeStatus::Background)).then(|| set(Some(ClaudeStatus::Background))).flatten();
            }
            // 旧版：每个 SubagentStop 减一，最后一个结束时 Claude 将被唤起处理结果（不经 UserPromptSubmit）
            if a.bg == 0 {
                return None;
            }
            a.bg -= 1;
            match prev {
                Some(ClaudeStatus::Background) if a.bg == 0 => set(Some(ClaudeStatus::Thinking)),
                Some(ClaudeStatus::Background) => set(Some(ClaudeStatus::Background)),
                // 等待确认等其它状态只更新计数
                _ => None,
            }
        }
        "Stop" => {
            // background_tasks 为权威数据：没有后台任务时清空子代理登记，防止漏掉的 SubagentStop 让会话一直卡住；
            // 旧版 Claude Code 不带该字段时按登记数计算，并丢弃过久未结束的登记
            a.bg = match background {
                Some(n) => {
                    if *n == 0 {
                        a.running.clear();
                    }
                    agents_bg(a, *n, teammates, subagents)
                }
                None => {
                    a.running.retain(|_, t| t.elapsed() < AGENT_EXPIRE);
                    a.running.len() as u32
                }
            };
            if a.bg > 0 {
                return set(Some(ClaudeStatus::Background));
            }
            Some(Step { next: Some(ClaudeStatus::Done), message: None, body: Some(tr!("notify.task_done")) })
        }
        "Notification" => {
            let message = message.clone().filter(|m| !m.trim().is_empty());
            match notification_type.as_deref() {
                // 空闲提醒（回复完成后一段时间）：已完成时 Stop 已通知过，后台运行中也不是在等用户，均保持不变
                Some("idle_prompt") if matches!(prev, Some(ClaudeStatus::Done | ClaudeStatus::Background)) => None,
                // 其余情况（Esc 中断后没有 Stop、刚启动未提问等）只是空闲，不算等待确认，也不通知
                Some("idle_prompt") => set(Some(ClaudeStatus::Idle)),
                // 需要用户操作的确认 / 输入（含后台子代理的权限请求，显示在主会话中）；旧版不带 notification_type 时同样处理
                None | Some("permission_prompt" | "elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input") => {
                    let body = message.clone().unwrap_or_else(|| tr!("notify.claude_attention"));
                    Some(Step { next: Some(ClaudeStatus::Waiting), message, body: Some(body) })
                }
                // 其余只是告知（agent_completed、quota_auto_resume_*、plugin_notification、auth_success 等）：不改状态、不通知
                _ => None,
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
                tasks: None,
                teammates: vec![],
                subagents: vec![],
                prompt: None,
                claude_session: None,
                cwd: None,
                transcript_path: None,
                reason: None,
                permission_mode: None,
                source: None,
            };
            self.apply(&ev)
        }
        /// 喂一条 hook JSON（与 `tetherd hook` 相同的解析）
        fn hook(&mut self, json: &str) -> Option<ClaudeStatus> {
            self.apply(&claude::event_msg(1, json, None).unwrap())
        }
        fn apply(&mut self, ev: &ClientMsg) -> Option<ClaudeStatus> {
            if let Some(step) = step(self.state, &mut self.agents, ev) {
                self.state = step.next;
                self.toasts.extend(step.body);
            }
            self.state
        }
        fn start(&mut self, id: &str) {
            self.feed("SubagentStart", Some((id, "general-purpose")), None);
        }
        /// 子代理结束；remaining 为 SubagentStop 的 background_tasks 中其余未结束的后台任务数
        fn stop_agent(&mut self, id: &str, remaining: u32) -> Option<ClaudeStatus> {
            self.feed("SubagentStop", Some((id, "general-purpose")), Some(remaining))
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
        assert_eq!(s.stop_agent("a", 0), Some(Thinking));
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
        assert_eq!(s.stop_agent("a", 1), Some(Background));
        assert_eq!(s.agents.bg, 1);
        // 最后一个结束：仍为 Background，直到 Claude 被唤起处理结果（经 UserPromptSubmit）
        assert_eq!(s.stop_agent("b", 0), Some(Background));
        assert_eq!(s.agents.bg, 0);
        assert_eq!(s.feed("UserPromptSubmit", None, None), Some(Thinking));
        assert!(s.toasts.is_empty());
        assert_eq!(s.feed("Stop", None, Some(0)), Some(Done));
        assert_eq!(s.toasts, ["任务已完成"]);
    }

    #[test]
    fn resumed_turn_with_remaining_subagent_stays_background() {
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        assert_eq!(s.feed("Stop", None, Some(2)), Some(Background));
        s.stop_agent("a", 1);
        // Claude 处理第一个结果后再次 Stop，仍有 1 个在运行
        s.feed("UserPromptSubmit", None, None);
        assert_eq!(s.feed("Stop", None, Some(1)), Some(Background));
        assert_eq!(s.stop_agent("b", 0), Some(Background));
        assert_eq!(s.feed("UserPromptSubmit", None, None), Some(Thinking));
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
            tasks: None,
            teammates: vec![],
            subagents: vec![],
            prompt: None,
            claude_session: None,
            cwd: None,
            transcript_path: None,
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
            tasks: None,
            teammates: vec![],
            subagents: vec![],
            prompt: None,
            claude_session: None,
            cwd: None,
            transcript_path: None,
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
        assert_eq!(s.stop_agent("zz", 0), None);
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
            tasks: None,
            teammates: vec![],
            subagents: vec![],
            prompt: None,
            claude_session: sid.map(Into::into),
            cwd: cwd.map(Into::into),
            transcript_path: None,
            reason: reason.map(Into::into),
            permission_mode: mode.map(Into::into),
            source: None,
        }
    }

    #[test]
    fn claude_gone_needs_two_misses() {
        let mut n = 0;
        // 没有 Claude 状态：不判定
        assert!(!claude_gone(&mut n, false, false));
        assert!(!claude_gone(&mut n, false, false));
        // 缺失一次后又出现（如 hook 先于进程进快照）：清零重计
        assert!(!claude_gone(&mut n, true, false));
        assert!(!claude_gone(&mut n, true, true));
        assert!(!claude_gone(&mut n, true, false));
        // 连续第二次缺失才判定，判定后清零
        assert!(claude_gone(&mut n, true, false));
        assert_eq!(n, 0);
    }

    /// Esc 中断不触发 Stop：思考中之后 Claude Code 的会话状态文件变为 idle（同一对话）即回到空闲；
    /// 早于思考开始的 idle、别的对话的 idle、非思考中的状态均不动
    #[test]
    fn check_claude_interrupt_becomes_idle() {
        let mut st = State::default();
        let (id, _rx) = test_session(&mut st, 80, 24);
        let s = st.sessions.get_mut(&id).unwrap();
        s.agents.session = Some("a".into());
        s.info.claude = Some(claude_state(Thinking, None, None));
        let at = s.info.claude.as_ref().unwrap().at;
        let state = |st: &State| st.sessions[&id].info.claude.as_ref().map(|c| c.state);
        check_claude(&mut st, &[(id, true, Some(("a".into(), at - 1)))]);
        assert_eq!(state(&st), Some(Thinking));
        check_claude(&mut st, &[(id, true, Some(("b".into(), at + 1)))]);
        assert_eq!(state(&st), Some(Thinking));
        check_claude(&mut st, &[(id, true, None)]);
        assert_eq!(state(&st), Some(Thinking));
        check_claude(&mut st, &[(id, true, Some(("a".into(), at + 1)))]);
        assert_eq!(state(&st), Some(Idle));
        st.sessions.get_mut(&id).unwrap().info.claude = Some(claude_state(Background, None, Some(1)));
        check_claude(&mut st, &[(id, true, Some(("a".into(), u64::MAX)))]);
        assert_eq!(state(&st), Some(Background));
    }

    #[test]
    fn check_claude_clears_state_and_resets_modes() {
        let mut st = State::default();
        let (id, _rx) = test_session(&mut st, 80, 24);
        let s = st.sessions.get_mut(&id).unwrap();
        s.info.claude = Some(claude_state(Background, None, Some(1)));
        s.agents.session = Some("a".into());
        s.resume = Some(saved::Resume { id: "a".into(), ..Default::default() });
        s.parser.process(b"\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[?25l\x1b[?1h\x1b[?2004h");
        check_claude(&mut st, &[(id, false, None)]);
        assert!(st.sessions[&id].info.claude.is_some());
        check_claude(&mut st, &[(id, false, None)]);
        let s = &st.sessions[&id];
        assert!(s.info.claude.is_none());
        // 同 SessionEnd（reason other）：resume 与对话 id 保留
        assert_eq!(s.resume.as_ref().map(|r| r.id.as_str()), Some("a"));
        assert_eq!(s.agents.session.as_deref(), Some("a"));
        let screen = s.parser.screen();
        assert!(!screen.alternate_screen());
        assert_eq!(screen.mouse_protocol_mode(), vt100::MouseProtocolMode::None);
        assert_eq!(screen.mouse_protocol_encoding(), vt100::MouseProtocolEncoding::Default);
        assert!(!screen.hide_cursor());
        assert!(!screen.application_cursor());
        assert!(screen.bracketed_paste());
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

    // 以下 hook JSON 取自 Claude Code 2.1.325 的真实事件（去掉无关字段）

    #[test]
    fn stop_with_only_shell_task_is_done() {
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        // 主代理用 run_in_background 启动后台 shell（如开发服务）后结束本轮：shell 不算后台代理，直接完成
        let stop = r#"{"hook_event_name":"Stop","background_tasks":[{"id":"b79id3ldq","type":"shell","status":"running"}],"session_crons":[]}"#;
        assert_eq!(s.hook(stop), Some(Done));
        assert_eq!(s.agents.bg, 0);
        assert_eq!(s.toasts.len(), 1);
        // shell 结束后 Claude 被任务通知唤起，再次结束
        assert_eq!(s.hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"<task-notification>\n<task-id>b79id3ldq"}"#), Some(Thinking));
        assert_eq!(s.hook(r#"{"hook_event_name":"Stop","background_tasks":[],"session_crons":[]}"#), Some(Done));
    }

    #[test]
    fn stop_with_teammate_task_is_background() {
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        // teammate 只计忙碌的：已 SubagentStart（其 agent_id 与 background_tasks 的 id 不同）
        s.feed("SubagentStart", Some(("aworker-0123", "worker")), None);
        let stop = r#"{"hook_event_name":"Stop","background_tasks":[{"id":"t1","type":"teammate","status":"running"},{"id":"m1","type":"monitor","status":"running"},{"id":"x","type":"shell","status":"completed"}]}"#;
        assert_eq!(s.hook(stop), Some(Background));
        // 只有 teammate 计入；monitor、shell 不算
        assert_eq!(s.agents.bg, 1);
        // session_crons 只是定时唤醒，不算后台任务
        s.feed("UserPromptSubmit", None, None);
        let cron = r#"{"hook_event_name":"Stop","background_tasks":[],"session_crons":[{"id":"c1","schedule":"0 9 * * 1-5","recurring":true}]}"#;
        assert_eq!(s.hook(cron), Some(Done));
    }

    #[test]
    fn idle_teammates_not_counted() {
        // agent teams：teammate 完成工作后进入空闲等待（已 SubagentStop），background_tasks 中仍标 running，不应算后台运行中
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        s.feed("SubagentStart", Some(("t1", "backend")), None);
        s.feed("SubagentStart", Some(("t2", "frontend")), None);
        let both = r#"{"hook_event_name":"Stop","background_tasks":[{"id":"t1","type":"teammate","status":"running"},{"id":"t2","type":"teammate","status":"running"}]}"#;
        assert_eq!(s.hook(both), Some(Background));
        assert_eq!(s.agents.bg, 2);
        s.hook(r#"{"hook_event_name":"SubagentStop","agent_id":"t1","agent_type":"backend","background_tasks":[{"id":"t1","type":"teammate","status":"running"},{"id":"t2","type":"teammate","status":"running"}]}"#);
        assert_eq!(s.agents.bg, 1);
        s.hook(r#"{"hook_event_name":"SubagentStop","agent_id":"t2","agent_type":"frontend","background_tasks":[{"id":"t1","type":"teammate","status":"running"},{"id":"t2","type":"teammate","status":"running"}]}"#);
        s.feed("UserPromptSubmit", None, None);
        assert_eq!(s.hook(both), Some(Done));
        // 空闲的 teammate 被再次派活：重新计入
        s.feed("UserPromptSubmit", None, None);
        s.feed("SubagentStart", Some(("t1", "backend")), None);
        assert_eq!(s.hook(both), Some(Background));
        assert_eq!(s.agents.bg, 1);
    }

    /// 现场（smartlic-nova-report 显示后台 ×27）：teammate 的 SubagentStart/Stop 的 agent_id（如 alc-v-k7-60b6…）
    /// 与 background_tasks 中 teammate 的 id（如 ts53dh0tq）不同，按 id 扣除空闲 teammate 永远对不上；
    /// 子代理的 id 两边一致。忙碌的 teammate 数应按“已 SubagentStart、未 SubagentStop、且不是子代理”的登记计
    #[test]
    fn idle_teammates_with_distinct_task_ids_not_counted() {
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        s.feed("SubagentStart", Some(("alc-v-k7-60b6a71de51a50ff", "lc-v-k7")), None);
        s.feed("SubagentStart", Some(("alc-v-k8-41a161ca396fcabf", "lc-v-k8")), None);
        s.start("ad9377af9741f60af");
        let list = r#""background_tasks":[{"id":"ts53dh0tq","type":"teammate","status":"running"},{"id":"t5l4moowz","type":"teammate","status":"running"},{"id":"ad9377af9741f60af","type":"subagent","status":"running"}]"#;
        let stop = format!(r#"{{"hook_event_name":"Stop",{list}}}"#);
        assert_eq!(s.hook(&stop), Some(Background));
        assert_eq!(s.agents.bg, 3);
        // 两个 teammate 完成工作进入空闲（background_tasks 仍报 running）
        for (id, name) in [("alc-v-k7-60b6a71de51a50ff", "lc-v-k7"), ("alc-v-k8-41a161ca396fcabf", "lc-v-k8")] {
            s.hook(&format!(r#"{{"hook_event_name":"SubagentStop","agent_id":"{id}","agent_type":"{name}",{list}}}"#));
        }
        assert_eq!(s.agents.bg, 1);
        s.feed("UserPromptSubmit", None, None);
        assert_eq!(s.hook(&stop), Some(Background));
        assert_eq!(s.agents.bg, 1);
        // 子代理没发 SubagentStop 就从列表里消失：不能被当成忙碌的 teammate
        let only_teammates = r#"{"hook_event_name":"Stop","background_tasks":[{"id":"ts53dh0tq","type":"teammate","status":"running"},{"id":"t5l4moowz","type":"teammate","status":"running"}]}"#;
        s.feed("UserPromptSubmit", None, None);
        assert_eq!(s.hook(only_teammates), Some(Done));
        // 空闲的 teammate 被再次派活：重新计入
        s.feed("UserPromptSubmit", None, None);
        s.feed("SubagentStart", Some(("alc-v-k7-60b6a71de51a50ff", "lc-v-k7")), None);
        assert_eq!(s.hook(only_teammates), Some(Background));
        assert_eq!(s.agents.bg, 1);
    }

    #[test]
    fn subagent_stop_with_own_bg_task_keeps_background() {
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        s.start("a49");
        // 主代理结束，后台子代理仍在运行
        let stop = r#"{"hook_event_name":"Stop","background_tasks":[{"id":"a49","type":"subagent","status":"running","agent_type":"general-purpose"}]}"#;
        assert_eq!(s.hook(stop), Some(Background));
        // 内部代理（agent_type 为空）的 SubagentStop 忽略
        let internal = r#"{"hook_event_name":"SubagentStop","agent_id":"x1","agent_type":"","background_tasks":[{"id":"a49","type":"subagent","status":"running"}]}"#;
        assert_eq!(s.hook(internal), Some(Background));
        // 子代理结束，但它启动的后台 shell 仍在运行；列表里还带着子代理自己（id 即 agent_id，不计）
        let sub = r#"{"hook_event_name":"SubagentStop","agent_id":"a49","agent_type":"general-purpose","background_tasks":[{"id":"a49","type":"subagent","status":"running","agent_type":"general-purpose"},{"id":"bla","type":"shell","status":"running"}]}"#;
        assert_eq!(s.hook(sub), Some(Background));
        // 剩下的只有后台 shell，不计数；是否完成仍以主代理的 Stop 为准
        assert_eq!(s.agents.bg, 0);
        // Claude 被子代理结果唤起处理，结束时只剩 shell 在运行：完成
        assert_eq!(s.hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"<agent-message from=\"a49"}"#), Some(Thinking));
        assert_eq!(s.hook(r#"{"hook_event_name":"Stop","background_tasks":[{"id":"bla","type":"shell","status":"running"}]}"#), Some(Done));
        assert_eq!(s.toasts, ["任务已完成"]);
    }

    #[test]
    fn slash_clear_stays_idle() {
        let mut s = Sim::default();
        assert_eq!(s.hook(r#"{"hook_event_name":"SessionStart","session_id":"a","source":"startup"}"#), Some(Idle));
        // 旧版 Claude Code 的内置命令也会触发 UserPromptSubmit（2.1.325 已不触发）：不进入思考中
        assert_eq!(s.hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"a","prompt":"/clear"}"#), Some(Idle));
        assert_eq!(s.hook(r#"{"hook_event_name":"SessionStart","session_id":"b","source":"clear"}"#), Some(Idle));
    }

    #[test]
    fn slash_help_no_thinking() {
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        assert_eq!(s.feed("Stop", None, Some(0)), Some(Done));
        for p in ["/help", "/model sonnet", "/context"] {
            let ups = format!(r#"{{"hook_event_name":"UserPromptSubmit","prompt":"{p}"}}"#);
            assert_eq!(s.hook(&ups), Some(Done), "{p}");
        }
        // 以 / 开头的路径不是命令
        assert_eq!(s.hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"/c/work/a.txt 看看"}"#), Some(Thinking));
    }

    #[test]
    fn custom_command_expansion_thinks() {
        let mut s = Sim::default();
        s.feed("UserPromptSubmit", None, None);
        s.feed("Stop", None, Some(0));
        // 自定义命令 / skill：先有 UserPromptSubmit（prompt 为命令本身），随后 UserPromptExpansion
        assert_eq!(s.hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"/hello"}"#), Some(Done));
        let expansion = r#"{"hook_event_name":"UserPromptExpansion","expansion_type":"slash_command","command_name":"hello","prompt":"/hello"}"#;
        assert_eq!(s.hook(expansion), Some(Thinking));
        assert_eq!(s.hook(r#"{"hook_event_name":"Stop","background_tasks":[]}"#), Some(Done));
        assert_eq!(s.toasts.len(), 2);
    }

    #[test]
    fn late_ups_from_old_session_ignored() {
        let mut s = Sim::default();
        s.hook(r#"{"hook_event_name":"SessionStart","session_id":"a","source":"startup"}"#);
        assert_eq!(s.hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"a","prompt":"hi"}"#), Some(Thinking));
        assert_eq!(s.hook(r#"{"hook_event_name":"Stop","session_id":"a","background_tasks":[]}"#), Some(Done));
        // /clear 后新对话已开始：旧对话迟到的事件（各 hook 是独立进程，到达顺序无保证）丢弃
        assert_eq!(s.hook(r#"{"hook_event_name":"SessionStart","session_id":"b","source":"clear"}"#), Some(Idle));
        assert_eq!(s.hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"a","prompt":"hi"}"#), Some(Idle));
        assert_eq!(s.hook(r#"{"hook_event_name":"SessionEnd","session_id":"a","reason":"clear"}"#), Some(Idle));
        // 新对话的事件、不带 session_id 的事件（旧版）照常处理
        assert_eq!(s.hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"b","prompt":"hi"}"#), Some(Thinking));
        assert_eq!(s.feed("Stop", None, Some(0)), Some(Done));
    }

    #[test]
    fn auto_compact_mid_turn_keeps_thinking() {
        let compact = r#"{"hook_event_name":"SessionStart","session_id":"a","source":"compact"}"#;
        let mut s = Sim::default();
        s.hook(r#"{"hook_event_name":"SessionStart","session_id":"a","source":"startup"}"#);
        s.feed("UserPromptSubmit", None, None);
        assert_eq!(s.hook(compact), Some(Thinking));
        assert_eq!(s.feed("Stop", None, Some(0)), Some(Done));
        // 后台任务计数不被清空
        s.feed("UserPromptSubmit", None, None);
        assert_eq!(s.feed("Stop", None, Some(2)), Some(Background));
        assert_eq!(s.hook(compact), Some(Background));
        assert_eq!(s.agents.bg, 2);
        // 尚无状态（如 daemon 重启后）时视为空闲
        let mut s = Sim::default();
        assert_eq!(s.hook(compact), Some(Idle));
    }

    #[test]
    fn non_attention_notifications_ignored() {
        let note = |t: &str| format!(r#"{{"hook_event_name":"Notification","notification_type":"{t}","message":"m"}}"#);
        for t in [
            "agent_completed",
            "quota_auto_resume_fired",
            "quota_auto_resume_stale",
            "quota_auto_resume_disabled",
            "plugin_notification",
            "auth_success",
            "elicitation_complete",
            "elicitation_response",
            "some_future_type",
        ] {
            for prev in [Thinking, Done, Background] {
                let mut s = Sim { state: Some(prev), ..Default::default() };
                assert_eq!(s.hook(&note(t)), Some(prev), "{t}");
                assert!(s.toasts.is_empty(), "{t}");
            }
        }
        // 需要用户操作的确认 / 输入，以及不带 notification_type 的旧版事件：等待确认并通知
        for t in ["permission_prompt", "elicitation_dialog", "elicitation_url_dialog", "agent_needs_input"] {
            let mut s = Sim { state: Some(Thinking), ..Default::default() };
            assert_eq!(s.hook(&note(t)), Some(Waiting), "{t}");
            assert_eq!(s.toasts, ["m"], "{t}");
        }
        let mut s = Sim { state: Some(Thinking), ..Default::default() };
        assert_eq!(s.hook(r#"{"hook_event_name":"Notification","message":"m"}"#), Some(Waiting));
    }

    /// 恢复 Claude 对话：会话列表带上 claudeSession，shell 有输出并静默后输入带权限模式的 resume 命令
    #[test]
    fn resume_claude_reports_session_and_types_command() {
        let shared: Shared = Arc::new(Mutex::new(State::default()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (id, mut cmds) = {
            let mut st = shared.lock().unwrap();
            st.gui = Some(Gui::new(1, tx));
            test_session(&mut st, 80, 24)
        };
        let r = |id: &str| proto::ClaudeResume { id: id.into(), permission_mode: Some("plan".into()), bypass: false };
        resume_claude(&shared, id, r("a1"));
        shared.lock().unwrap().push_sessions();
        let v: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(v["sessions"][0]["claudeSession"], "a1");
        thread::sleep(Duration::from_millis(5));
        assert!(on_output(&shared, id, b"PS> "));
        thread::sleep(Duration::from_millis(900));
        let Ok(ssh::Cmd::Data(data)) = cmds.try_recv() else { panic!("未输入命令") };
        assert_eq!(data, b"claude --resume a1 --permission-mode plan\r");
        // id 含意外字符：不记录、不输入
        let (id2, mut cmds2) = test_session(&mut shared.lock().unwrap(), 80, 24);
        resume_claude(&shared, id2, r("x; rm"));
        assert!(shared.lock().unwrap().sessions[&id2].resume.is_none());
        assert!(cmds2.try_recv().is_err());
    }

    /// 重复 attach 只重发快照、不重复推 output；history 为 true 时快照前带推出屏幕的历史（接管结束后 GUI 刷新）
    #[test]
    fn reattach_resends_snapshot_with_history() {
        let shared: Shared = Arc::new(Mutex::new(State::default()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let id = {
            let mut st = shared.lock().unwrap();
            st.gui = Some(Gui::new(1, tx));
            test_session(&mut st, 20, 3).0
        };
        let mut next = || serde_json::from_str::<serde_json::Value>(&rx.try_recv().unwrap()).unwrap();
        let decode = |v: &serde_json::Value| String::from_utf8(B64.decode(v["data"].as_str().unwrap()).unwrap()).unwrap();
        assert!(dispatch(&shared, 1, ClientMsg::Attach { id, history: false }));
        assert_eq!(next()["type"], "snapshot");
        // 3 行屏幕上输出 5 行：old 0、old 1 被推出屏幕进入历史
        assert!(on_output(&shared, id, b"old 0\r\nold 1\r\nold 2\r\nold 3\r\nold 4"));
        assert_eq!(next()["type"], "output");
        assert!(dispatch(&shared, 1, ClientMsg::Attach { id, history: false }));
        let plain = decode(&next());
        assert!(!plain.contains("old 0") && plain.contains("old 4"), "{plain:?}");
        assert!(dispatch(&shared, 1, ClientMsg::Attach { id, history: true }));
        let full = decode(&next());
        assert!(full.starts_with("\x1b[mold 0\r\n\x1b[mold 1"), "{full:?}");
        assert!(full.ends_with(&plain), "{full:?}");
        // 已 attach 多次，之后的输出仍只推一次
        assert!(on_output(&shared, id, b"x"));
        assert_eq!(next()["type"], "output");
        assert!(rx.try_recv().is_err());
    }

    fn title_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tether-title-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const AI_TITLE: &str = concat!(
        r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"首条"}}"#,
        "\n",
        r#"{"type":"ai-title","aiTitle":"自动标题","sessionId":"a"}"#
    );

    /// 标题来源：优先 hook 的 transcript_path，否则按 Claude 启动目录 + session_id 推导；只在 Stop / SessionStart（非 compact）读
    #[test]
    fn title_source_transcript_path_or_cwd() {
        let projects = title_dir("src");
        std::fs::create_dir_all(projects.join("C--work-p")).unwrap();
        std::fs::write(projects.join("C--work-p").join("a.jsonl"), AI_TITLE).unwrap();
        let r = saved::Resume { id: "a".into(), cwd: r"C:\work\p".into(), ..Default::default() };
        let hook = |json: &str| claude::event_msg(1, json, None).unwrap();
        let src = |json: &str| title_source(Some(&r), &hook(json), Some(&projects));

        let s = src(r#"{"hook_event_name":"Stop","session_id":"a","transcript_path":"D:\\t\\a.jsonl","cwd":"C:\\elsewhere"}"#).unwrap();
        assert_eq!(s, TitleSource { id: "a".into(), path: Some(r"D:\t\a.jsonl".into()), reset: false });
        // 不带 transcript_path：按 resume.cwd 推导（hook 当时的 cwd 已 cd 到别处也不影响）
        let s = src(r#"{"hook_event_name":"Stop","session_id":"a","cwd":"C:\\elsewhere"}"#).unwrap();
        assert_eq!(s.path.as_deref().and_then(proto::claude_history::title).as_deref(), Some("自动标题"));
        assert!(src(r#"{"hook_event_name":"SessionStart","session_id":"a","source":"resume"}"#).unwrap().reset);
        assert_eq!(src(r#"{"hook_event_name":"SessionStart","session_id":"a","source":"compact"}"#), None);
        assert_eq!(src(r#"{"hook_event_name":"UserPromptSubmit","session_id":"a","prompt":"hi"}"#), None);
        assert_eq!(title_source(None, &hook(r#"{"hook_event_name":"Stop"}"#), Some(&projects)), None);
    }

    /// Stop 后读到的标题推送给 GUI 并保存；手动改名后显示名不被标题覆盖；换对话 / 过期 / 读不到时的处理
    #[test]
    fn title_update_pushed_and_custom_name_wins() {
        let dir = title_dir("push");
        let path = dir.join("a.jsonl");
        std::fs::write(&path, AI_TITLE).unwrap();
        let shared: Shared = Arc::new(Mutex::new(State::default()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let id = {
            let mut st = shared.lock().unwrap();
            let mut gui = Gui::new(1, tx);
            // 会话在前台：Stop 不弹系统通知
            gui.focused = true;
            st.gui = Some(gui);
            let id = test_session(&mut st, 80, 24).0;
            st.gui.as_mut().unwrap().active = Some(id);
            st.sessions.get_mut(&id).unwrap().resume = Some(saved::Resume { id: "a".into(), cwd: r"C:\p".into(), ..Default::default() });
            id
        };
        let info = |sh: &Shared| sh.lock().unwrap().sessions[&id].info.clone();
        let stop = format!(r#"{{"hook_event_name":"Stop","session_id":"a","transcript_path":{},"background_tasks":[]}}"#, serde_json::to_string(&path).unwrap());
        claude_event(&shared, claude::event_msg(id, &stop, None).unwrap());
        // 标题在独立线程读取
        let deadline = Instant::now() + Duration::from_secs(5);
        while info(&shared).claude_title.is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!((info(&shared).claude_title.as_deref(), info(&shared).display_name()), (Some("自动标题"), "自动标题"));
        let pushed: Vec<serde_json::Value> = std::iter::from_fn(|| rx.try_recv().ok()).map(|m| serde_json::from_str(&m).unwrap()).collect();
        let last = pushed.iter().rfind(|v| v["type"] == "sessions").unwrap();
        assert_eq!(last["sessions"][0]["claudeTitle"], "自动标题");

        // 手动改名：带上标记并推送，之后的标题只记下、不改显示名
        assert!(dispatch(&shared, 1, ClientMsg::Rename { id, name: "我的".into() }));
        let v: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!((&v["sessions"][0]["name"], &v["sessions"][0]["nameCustom"]), (&"我的".into(), &true.into()));
        update_title(&mut shared.lock().unwrap(), id, "a", Some("新标题".into()), false);
        assert_eq!((info(&shared).claude_title.as_deref(), info(&shared).display_name()), (Some("新标题"), "我的"));

        let mut st = shared.lock().unwrap();
        // Stop 读不到：保留上次；别的对话的结果：丢弃；换对话（SessionStart）读不到：清空，回到原名
        update_title(&mut st, id, "a", None, false);
        update_title(&mut st, id, "b", Some("别的".into()), true);
        assert_eq!(st.sessions[&id].info.claude_title.as_deref(), Some("新标题"));
        update_title(&mut st, id, "a", None, true);
        assert_eq!(st.sessions[&id].info.claude_title, None);
    }
}
