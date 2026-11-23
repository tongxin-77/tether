//! 管理服务器（hub）客户端：按 config.json 的 hub 设置与凭据管理器中的 token 维持一条常驻出站连接，上报会话列表；
//! 设置允许时处理 hub 的远程接管（协议第 6 节）。
//! 默认关闭；未启用 / 未配置 / 连接失败时不影响 daemon 其它功能（连接在独立异步任务中，失败只体现在 HubStatus）。

mod link;

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use proto::{HubConfig, HubState, Secret, ServerMsg};
use tokio::{
    sync::{Notify, mpsc::UnboundedSender},
    task::AbortHandle,
};

use crate::{
    config, image,
    session::{self, Shared, State},
    ssh,
};

/// 单个网页贴图上传从 imageBegin 起的最长组装时间，超时丢弃
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// token 在 Windows 凭据管理器中的条目：服务名同 SSH 密码（tether / dev 构建 tether-dev），账户 `hub-token`，
/// 凭据名（target）即 `hub-token.tether`；SSH 密码账户形如 `user@host:port`，不会与之冲突
const KEYRING_SERVICE: &str = if cfg!(debug_assertions) { "tether-dev" } else { "tether" };
const KEYRING_ACCOUNT: &str = "hub-token";

/// daemon 内的 hub 客户端状态
#[derive(Default)]
pub struct Hub {
    state: HubState,
    error: Option<String>,
    /// 常驻连接任务；设置变化时中止并按新设置重建
    task: Option<AbortHandle>,
    /// 每次重建加一，旧任务的迟到状态回调据此丢弃
    generation: u64,
    /// 会话列表变化通知（State::push_sessions 触发），常驻连接据此防抖上报
    pub changed: Arc<Notify>,
    /// 当前 hub 连接的上行发送端（已连接时 Some），接管消息经它推送；接管中的会话以 SessionInfo.taken_over 标记
    tx: Option<UnboundedSender<link::Up>>,
    /// 当前连接双方都声明的能力位（welcome 时写入，断开清空）
    caps: HashSet<String>,
    /// 组装中的网页贴图：uploadId → 上传
    uploads: HashMap<String, Upload>,
}

/// 组装中的网页贴图
struct Upload {
    session_id: u32,
    mime: String,
    /// imageBegin 声明的总字节数（不超过 10MB）
    size: usize,
    data: Vec<u8>,
    started: Instant,
}

impl Hub {
    pub fn status_msg(&self) -> ServerMsg {
        ServerMsg::HubStatus { state: self.state, error: self.error.clone() }
    }

    fn up(&self, msg: link::Up) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(msg);
        }
    }

    /// 被接管会话的输出推给 hub（PTY 读线程持锁调用：只入队，不等网络）
    pub fn output(&self, id: u32, data: &[u8]) {
        self.up(link::Up::Output { session_id: id, data: B64.encode(data) });
    }

    /// 本机结束接管：reclaimed / disabled / exited
    pub fn detached(&self, id: u32, reason: &str) {
        self.up(link::Up::Detached { session_id: id, reason: reason.into() });
    }
}

/// 更新状态并推给 GUI；generation 不匹配（已被新任务取代）或无变化时忽略
fn set_status(st: &mut State, generation: u64, state: HubState, error: Option<String>) {
    if st.hub.generation != generation || (st.hub.state == state && st.hub.error == error) {
        return;
    }
    st.hub.state = state;
    st.hub.error = error;
    st.send(&st.hub.status_msg());
}

/// 按当前配置（重新）建立常驻连接：先中止旧连接；未启用则为 disabled，地址无效 / 无 token 为 failed
pub fn restart(state: &Shared, st: &mut State) {
    if let Some(task) = st.hub.task.take() {
        task.abort();
    }
    // 旧连接随任务中止，其上的接管与上传全部结束（无需发消息）
    st.hub.tx = None;
    st.hub.caps.clear();
    st.hub.uploads.clear();
    release_all(st, None);
    st.hub.generation += 1;
    let generation = st.hub.generation;
    let HubConfig { enabled, address, allow_insecure, .. } = st.config.hub.clone();
    if !enabled {
        return set_status(st, generation, HubState::Disabled, None);
    }
    let endpoint = match link::parse_address(&address, allow_insecure) {
        Ok(ep) => ep,
        Err(e) => return set_status(st, generation, HubState::Failed, Some(tr!("common.invalid_address", error = e))),
    };
    set_status(st, generation, HubState::Connecting, None);
    let shared = state.clone();
    let changed = st.hub.changed.clone();
    let task = tokio::spawn(async move {
        // 读凭据管理器放在任务里，不拖慢启动 / 保存设置
        let Some(token) = saved_token() else {
            let mut st = shared.lock().unwrap();
            return set_status(&mut st, generation, HubState::Failed, Some(tr!("hub.no_token_saved")));
        };
        log!("hub 客户端启动：{}", endpoint.url());
        link::run(make_link(&shared, generation, endpoint, Secret(token), hello(), changed)).await;
    });
    st.hub.task = Some(task.abort_handle());
}

/// 上报给 hub 的会话列表
fn sessions(st: &State) -> Vec<proto::SessionInfo> {
    st.sessions.values().map(|s| s.info.clone()).collect()
}

/// 组装常驻连接：各回调持锁操作 State，generation 不匹配（已被新任务取代）的迟到回调丢弃
fn make_link(shared: &Shared, generation: u64, endpoint: link::Endpoint, token: Secret, hello: link::Up, changed: Arc<Notify>) -> link::Link {
    let (sessions_state, status_state, conn_state, down_state) = (shared.clone(), shared.clone(), shared.clone(), shared.clone());
    link::Link {
        endpoint,
        token,
        hello,
        sessions: Arc::new(move || sessions(&sessions_state.lock().unwrap())),
        changed,
        status: Arc::new(move |state, error| set_status(&mut status_state.lock().unwrap(), generation, state, error)),
        conn: Arc::new(move |conn| {
            let mut st = conn_state.lock().unwrap();
            if st.hub.generation == generation {
                let (tx, caps) = conn.unzip();
                st.hub.tx = tx;
                // 只保留双方都声明的能力（未知能力名忽略）
                st.hub.caps = caps.unwrap_or_default().into_iter().filter(|c| link::CAPS.contains(&c.as_str())).collect();
                st.hub.uploads.clear();
                release_all(&mut st, None);
            }
        }),
        down: Arc::new(move |msg| {
            let mut st = down_state.lock().unwrap();
            if st.hub.generation == generation {
                on_down(&down_state, &mut st, msg);
            }
        }),
    }
}

// ---------------- 远程接管 ----------------

/// 处理 hub 的接管消息：未允许接管时拒绝；input / resize 走与本地 GUI 相同的写入 / resize 代码；未接管的会话忽略
fn on_down(shared: &Shared, st: &mut State, msg: link::Down) {
    let taken = |st: &State, id: u32| st.sessions.get(&id).is_some_and(|s| s.info.taken_over);
    // 超时未完成的上传丢弃（hub 侧同样 60 s 超时，不再回复）
    st.hub.uploads.retain(|_, u| u.started.elapsed() < UPLOAD_TIMEOUT);
    match msg {
        link::Down::Attach { session_id: id } => {
            let reply = if st.config.hub.allow_takeover { session::snapshot(st, id).ok_or("not_found") } else { Err("disabled") };
            match reply {
                // 重复 attach 按成功处理，重发快照；attached 先入队，之后的输出（同在锁内判断 taken_over）排在其后
                Ok((cols, rows, snapshot)) => {
                    let history = if st.hub.caps.contains("history") { session::history(st, id) } else { None };
                    st.hub.up(link::Up::Attached { session_id: id, cols, rows, snapshot, history });
                    let s = st.sessions.get_mut(&id).unwrap();
                    if !std::mem::replace(&mut s.info.taken_over, true) {
                        log!("hub 开始接管会话 #{id}");
                        st.push_sessions();
                    }
                }
                Err(reason) => {
                    log!("拒绝 hub 接管会话 #{id}：{reason}");
                    st.hub.up(link::Up::AttachDenied { session_id: id, reason: reason.into() });
                }
            }
        }
        link::Down::Input { session_id: id, data } if taken(st, id) => {
            if let Ok(bytes) = B64.decode(data) {
                session::input(st, id, &bytes);
            }
        }
        link::Down::Resize { session_id: id, cols, rows } if taken(st, id) && (1..=1000).contains(&cols) && (1..=1000).contains(&rows) => {
            session::resize(st, id, cols, rows);
        }
        // hub 主动结束的不回 detached
        link::Down::Detach { session_id: id } if taken(st, id) => {
            st.sessions.get_mut(&id).unwrap().info.taken_over = false;
            st.hub.uploads.retain(|_, u| u.session_id != id);
            log!("hub 结束接管会话 #{id}");
            st.push_sessions();
        }
        // 网页贴图：begin 先做一次校验（尽早拒绝，免得传完 10MB），end 时再完整校验
        link::Down::ImageBegin { session_id: id, upload_id, mime, size } => {
            let check = image_check(st, id).and_then(|()| match (image::ext(&mime), size > image::MAX_BYTES as u64) {
                (None, _) => Err("bad_type"),
                (_, true) => Err("too_large"),
                _ => Ok(()),
            });
            match check {
                Ok(()) => {
                    let size = size as usize;
                    let upload = Upload { session_id: id, mime, size, data: Vec::with_capacity(size), started: Instant::now() };
                    st.hub.uploads.insert(upload_id, upload);
                }
                Err(reason) => image_failed(st, id, upload_id, reason),
            }
        }
        link::Down::ImageChunk { session_id: id, upload_id, data } => {
            let Some(u) = st.hub.uploads.get_mut(&upload_id).filter(|u| u.session_id == id) else { return };
            let reason = match B64.decode(data) {
                Ok(bytes) if u.data.len() + bytes.len() <= u.size => return u.data.extend_from_slice(&bytes),
                Ok(_) => "too_large",
                Err(_) => "io",
            };
            st.hub.uploads.remove(&upload_id);
            image_failed(st, id, upload_id, reason);
        }
        link::Down::ImageEnd { session_id: id, upload_id } => {
            let Some(u) = st.hub.uploads.remove(&upload_id).filter(|u| u.session_id == id) else { return };
            let check = image_check(st, id).and_then(|()| match (u.data.len() == u.size, image::magic_ok(&u.mime, &u.data)) {
                (false, _) => Err("too_large"),
                (_, false) => Err("bad_type"),
                _ => Ok(()),
            });
            if let Err(reason) = check {
                return image_failed(st, id, upload_id, reason);
            }
            // 写文件不持锁；完成后再加锁写入路径
            let shared = shared.clone();
            tokio::task::spawn_blocking(move || {
                let saved = image::save(image::ext(&u.mime).unwrap_or("png"), &u.data);
                let mut st = shared.lock().unwrap();
                let path = match saved {
                    Ok(path) => path,
                    Err(e) => {
                        log!("保存 hub 贴图失败：{e}");
                        return image_failed(&st, id, upload_id, "io");
                    }
                };
                // 保存期间接管可能已结束
                if let Err(reason) = image_check(&st, id) {
                    let _ = std::fs::remove_file(&path);
                    return image_failed(&st, id, upload_id, reason);
                }
                let path = path.display().to_string();
                session::paste_path(&mut st, id, &path);
                log!("hub 贴图已保存并写入会话 #{id}（{}，{} 字节）", u.mime, u.data.len());
                st.hub.up(link::Up::ImageSaved { session_id: id, upload_id, path });
            });
        }
        // 网页关闭会话：复用允许接管开关，不要求正被接管；与本地 GUI 关闭相同的路径（正被接管时随之发 detached exited）
        link::Down::Close { session_id: id, request_id } => {
            let alive = st.sessions.get(&id).is_some_and(|s| s.info.status != proto::Status::Exited);
            let reason = match (st.config.hub.allow_takeover, alive) {
                (false, _) => Some("disabled"),
                (_, false) => Some("not_found"),
                _ => None,
            };
            let removed = if reason.is_none() { session::remove(st, id) } else { None };
            log!("hub 关闭会话 #{id}：{}", reason.unwrap_or("成功"));
            st.hub.up(link::Up::CloseResult { session_id: id, request_id, ok: reason.is_none(), reason: reason.map(Into::into) });
            // 调用方持有 State 锁：结束后端放到独立线程（drop ConPTY 可能要等读线程抢锁排空输出）
            if let Some(s) = removed {
                std::thread::spawn(move || s.close());
            }
        }
        // 网页新建（能力 create）：均复用允许接管开关
        link::Down::CreateOptions { request_id } => {
            let msg = if st.config.hub.allow_takeover {
                let hosts = config::all_hosts(&st.config);
                link::Up::CreateOptionsResult {
                    request_id,
                    ok: true,
                    reason: None,
                    home: Some(config::home().map(|h| h.display().to_string()).unwrap_or_default()),
                    recent: Some(recent_dirs(&st.recent, &hosts)),
                    hosts: Some(hosts.into_iter().map(|h| link::HostItem { id: h.id, name: h.name }).collect()),
                }
            } else {
                link::Up::CreateOptionsResult { request_id, ok: false, reason: Some("disabled".into()), home: None, recent: None, hosts: None }
            };
            st.hub.up(msg);
        }
        link::Down::ClaudeHistory { request_id, cwd } => {
            if !st.config.hub.allow_takeover {
                let msg = link::Up::ClaudeHistoryResult { request_id, ok: false, reason: Some("disabled".into()), message: None, items: None };
                return st.hub.up(msg);
            }
            // 读对话文件不持锁
            let (shared, open) = (shared.clone(), session::claude_sessions(st));
            tokio::task::spawn_blocking(move || {
                let cwd = if cwd.trim().is_empty() { config::home().map(|h| h.display().to_string()).unwrap_or(cwd) } else { cwd };
                let msg = match proto::claude_history::projects_dir() {
                    Some(root) => link::Up::ClaudeHistoryResult {
                        request_id,
                        ok: true,
                        reason: None,
                        message: None,
                        items: Some(history_items(&root, &cwd, &open)),
                    },
                    None => link::Up::ClaudeHistoryResult {
                        request_id,
                        ok: false,
                        reason: Some("error".into()),
                        message: Some(tr!("common.no_home")),
                        items: None,
                    },
                };
                shared.lock().unwrap().hub.up(msg);
            });
        }
        // 网页新建会话：与桌面端新建相同的路径，但不发 Activate（桌面端不切换）、失败不向 GUI 报错；SSH 不弹任何提示
        link::Down::Create { request_id, target, resume, cols, rows } => {
            let size = |v: Option<u16>, default| v.filter(|v| (1..=1000).contains(v)).unwrap_or(default);
            let (cols, rows) = (size(cols, 120), size(rows, 32));
            if !st.config.hub.allow_takeover {
                return create_result(st, request_id, Err(("disabled", tr!("hub.takeover_disabled"))));
            }
            let shared = shared.clone();
            match target {
                link::CreateTarget::Local { cwd } => {
                    // 启动 PTY 较慢且 spawn 要加锁：放到阻塞线程
                    tokio::task::spawn_blocking(move || {
                        let cwd = cwd.filter(|c| !c.trim().is_empty());
                        // 桌面端从目录选择框 / 最近目录选，ConPTY 遇到不存在的目录会悄悄改用主目录；网页手填的目录需先校验
                        let result = match &cwd {
                            Some(c) if !Path::new(c).is_dir() => Err(tr!("common.dir_not_found", dir = c)),
                            _ => session::create_local(&shared, None, cwd.unwrap_or_default(), cols, rows, resume),
                        };
                        let st = shared.lock().unwrap();
                        if result.is_ok() {
                            st.push_sessions();
                        }
                        create_result(&st, request_id, result.map_err(|e| ("error", e)));
                    });
                }
                link::CreateTarget::Remote { host_id, cwd } => {
                    let Some(host) = config::all_hosts(&st.config).into_iter().find(|h| h.id == host_id) else {
                        return create_result(st, request_id, Err(("not_found", tr!("common.host_not_found", host = host_id))));
                    };
                    let cwd = cwd.filter(|c| !c.trim().is_empty());
                    tokio::spawn(async move {
                        let result = ssh::create(shared.clone(), host, cwd, cols, rows, true).await;
                        let result = result.map_err(|e| (if ssh::needs_input(&e) { "auth_required" } else { "error" }, e));
                        create_result(&shared.lock().unwrap(), request_id, result);
                    });
                }
            }
        }
        _ => {}
    }
}

/// 回复 createResult；成功时先立即上报一次会话列表（不等防抖），hub 收到结果时列表中已有新会话
fn create_result(st: &State, request_id: String, result: Result<u32, (&str, String)>) {
    let msg = match result {
        Ok(id) => {
            log!("hub 新建会话 #{id}");
            st.hub.up(link::Up::Sessions { sessions: sessions(st) });
            link::Up::CreateResult { request_id, ok: true, session_id: Some(id), reason: None, message: None }
        }
        Err((reason, message)) => {
            log!("hub 新建会话失败（{reason}）：{message}");
            link::Up::CreateResult { request_id, ok: false, session_id: None, reason: Some(reason.into()), message: Some(message) }
        }
    };
    st.hub.up(msg);
}

/// 新建选项的最近目录：remote 项带主机当前显示名，主机已不存在的剔除
fn recent_dirs(recent: &[proto::RecentItem], hosts: &[proto::Host]) -> Vec<link::RecentDir> {
    recent
        .iter()
        .filter_map(|r| match &r.target {
            proto::Target::Local { cwd } => Some(link::RecentDir::Local { cwd: cwd.clone() }),
            proto::Target::Remote { host_id, cwd } => {
                let host = hosts.iter().find(|h| &h.id == host_id)?;
                Some(link::RecentDir::Remote { host_id: host_id.clone(), host: host.name.clone(), cwd: cwd.clone().unwrap_or_default() })
            }
        })
        .collect()
}

/// 目录 cwd 的 Claude 历史对话（同桌面端「恢复 Claude 对话…」），最多 50 条、新的在前；open 中的对话已有会话在运行
fn history_items(projects: &Path, cwd: &str, open: &HashSet<String>) -> Vec<link::HistoryItem> {
    proto::claude_history::list(projects, cwd)
        .into_iter()
        .take(50)
        .map(|c| link::HistoryItem {
            open: open.contains(&c.id),
            id: c.id,
            title: c.summary.title,
            mtime: c.mtime,
            messages: c.summary.messages,
            permission_mode: c.summary.permission_mode,
            bypass: c.summary.bypass,
        })
        .collect()
}

/// 网页贴图的前提：本机允许接管、该会话正被 hub 接管、本地会话（SSH 会话不支持）
fn image_check(st: &State, id: u32) -> Result<(), &'static str> {
    if !st.config.hub.allow_takeover {
        Err("disabled")
    } else if !st.sessions.get(&id).is_some_and(|s| s.info.taken_over) {
        Err("not_taken_over")
    } else if !session::is_local(st, id) {
        Err("unsupported")
    } else {
        Ok(())
    }
}

fn image_failed(st: &State, id: u32, upload_id: String, reason: &str) {
    log!("hub 贴图失败（会话 #{id}）：{reason}");
    st.hub.up(link::Up::ImageFailed { session_id: id, upload_id, reason: reason.into() });
}

/// 结束全部接管：reason 为 Some 时逐个发 detached（关闭了允许接管），None 用于连接断开 / 重建（不发消息）
pub fn release_all(st: &mut State, reason: Option<&str>) {
    let ids: Vec<u32> = st.sessions.values_mut().filter_map(|s| std::mem::take(&mut s.info.taken_over).then_some(s.info.id)).collect();
    if ids.is_empty() {
        return;
    }
    log!("结束 hub 对 {} 个会话的接管（{}）", ids.len(), reason.unwrap_or("连接断开"));
    if let Some(reason) = reason {
        for id in ids {
            st.hub.detached(id, reason);
        }
    }
    st.push_sessions();
}

/// 本地 GUI 点「收回」：结束该会话的接管，通知 hub（reclaimed）
pub fn reclaim(st: &mut State, id: u32) {
    let Some(s) = st.sessions.get_mut(&id).filter(|s| s.info.taken_over) else { return };
    s.info.taken_over = false;
    log!("本地收回会话 #{id} 的 hub 接管");
    st.hub.detached(id, "reclaimed");
    st.push_sessions();
}

/// 设置面板“测试连接”（异步任务）：独立连接，握手 → hello → welcome 后立即关闭；结果以 ServerMsg::TestHub 回复
pub async fn test(state: Shared, req_id: u64, address: String, allow_insecure: bool, token: Option<String>) {
    let start = std::time::Instant::now();
    let result = match (link::parse_address(&address, allow_insecure), token) {
        (Err(e), _) => Err(tr!("common.invalid_address", error = e)),
        (Ok(_), None) => Err(tr!("hub.no_token")),
        // hello 带 probe: true，hub 不会替换常驻连接
        (Ok(ep), Some(token)) => link::probe(&ep, &Secret(token), &hello()).await.map_err(|f| f.msg),
    };
    let msg = match result {
        Ok(version) => {
            let ms = start.elapsed().as_millis() as u64;
            log!("hub 连接测试 {address}：成功（{ms} ms，hub 版本 {version}）");
            ServerMsg::TestHub { req_id, elapsed_ms: Some(ms), hub_version: Some(version), error: None }
        }
        Err(e) => {
            log!("hub 连接测试 {address}：{e}");
            ServerMsg::TestHub { req_id, elapsed_ms: None, hub_version: None, error: Some(e) }
        }
    };
    state.lock().unwrap().send(&msg);
}

fn hello() -> link::Up {
    link::hello(machine_id(), &std::env::var("COMPUTERNAME").unwrap_or_default(), env!("CARGO_PKG_VERSION"))
}

// ---------------- token ----------------

fn entry() -> Option<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).map_err(|e| log!("打开凭据管理器失败：{e}")).ok()
}

pub fn saved_token() -> Option<String> {
    entry()?.get_password().ok().filter(|t| !t.is_empty())
}

pub fn token_saved() -> bool {
    saved_token().is_some()
}

/// 保存（Some）/ 清除（None）token
pub fn set_token(token: Option<&str>) -> Result<(), String> {
    let entry = entry().ok_or_else(|| tr!("hub.keyring_unavailable"))?;
    match token.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => entry.set_password(t).map_err(|e| e.to_string()),
        None => match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        },
    }
}

// ---------------- machine-id ----------------

/// 稳定机器 ID：%APPDATA%\tether\machine-id（dev 构建为 tether-dev），首次需要时生成 UUID v4，之后不变。
/// 读写失败时本次进程用临时生成的 ID（记日志）
fn machine_id() -> String {
    let Some(dir) = std::env::var_os("APPDATA") else {
        log!("未找到 APPDATA，machine-id 无法持久化");
        return uuid::Uuid::new_v4().to_string();
    };
    let path = Path::new(&dir).join(proto::data_dir_name()).join("machine-id");
    load_or_create(&path).unwrap_or_else(|e| {
        log!("machine-id 读写失败（{}）：{e}", path.display());
        uuid::Uuid::new_v4().to_string()
    })
}

/// 读已有的 machine-id（去掉首尾空白）；文件不存在、为空或超过协议上限（64 字符）时生成 UUID v4 写入
fn load_or_create(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() && s.trim().chars().count() <= link::MACHINE_ID_MAX => return Ok(s.trim().to_string()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, &id).map_err(|e| e.to_string())?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio::{net::TcpListener, sync::mpsc::UnboundedReceiver};
    use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

    use crate::ssh::Cmd;

    #[test]
    fn machine_id_created_once() {
        let dir = std::env::temp_dir().join(format!("tether-hub-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("sub").join("machine-id");
        let id = load_or_create(&path).unwrap();
        let parsed = uuid::Uuid::parse_str(&id).unwrap();
        assert_eq!(parsed.get_version_num(), 4);
        assert_eq!(id, id.to_lowercase());
        assert_eq!(load_or_create(&path).unwrap(), id);
        // 手写的值（带换行）原样沿用
        std::fs::write(&path, "custom-id\r\n").unwrap();
        assert_eq!(load_or_create(&path).unwrap(), "custom-id");
        // 空文件重新生成
        std::fs::write(&path, "  ").unwrap();
        let regenerated = load_or_create(&path).unwrap();
        assert!(uuid::Uuid::parse_str(&regenerated).is_ok());
        // 超过 64 字符（hub 会按 bad_hello 拒绝）重新生成
        std::fs::write(&path, "x".repeat(65)).unwrap();
        let regenerated = load_or_create(&path).unwrap();
        assert!(uuid::Uuid::parse_str(&regenerated).is_ok());
        assert_eq!(load_or_create(&path).unwrap(), regenerated);
        let _ = std::fs::remove_dir_all(&dir);
    }

    type Ws = WebSocketStream<tokio::net::TcpStream>;
    /// 下一条非 sessions 的上行消息
    async fn recv(ws: &mut Ws) -> Value {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.expect("等待上行消息超时").unwrap().unwrap();
            let Message::Text(text) = msg else { continue };
            let v: Value = serde_json::from_str(&text).unwrap();
            if v["type"] != "sessions" {
                return v;
            }
        }
    }
    /// 一段时间内没有非 sessions 的上行消息
    async fn silent(ws: &mut Ws) {
        assert!(tokio::time::timeout(Duration::from_millis(300), recv(ws)).await.is_err(), "不应再收到接管消息");
    }
    async fn send(ws: &mut Ws, v: Value) {
        ws.send(Message::Text(v.to_string().into())).await.unwrap();
    }
    async fn cmd(rx: &mut UnboundedReceiver<Cmd>) -> Cmd {
        tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("等待写入会话超时").unwrap()
    }
    async fn wait_until(f: impl Fn() -> bool) {
        for _ in 0..250 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("等待条件超时");
    }

    /// 远程接管端到端（daemon 侧逻辑在 bin crate 内，tests/ 无法引用，故放在这里）：真实 State + 远程后端的测试会话
    /// + 常驻连接，对接本地明文 ws 的模拟 hub
    #[tokio::test]
    async fn takeover_end_to_end() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared: Shared = Arc::new(Mutex::new(State::default()));
        let (id, mut cmds) = session::test_session(&mut shared.lock().unwrap(), 100, 20);
        let taken = || shared.lock().unwrap().sessions[&id].info.taken_over;
        let endpoint = link::parse_address(&format!("ws://127.0.0.1:{port}"), true).unwrap();
        let hello = link::hello("mid".into(), "PC", "0.0.1");
        let task = tokio::spawn(link::run(make_link(&shared, 0, endpoint, Secret("t".into()), hello, Arc::new(Notify::new()))));
        let mut ws = tokio_tungstenite::accept_async(listener.accept().await.unwrap().0).await.unwrap();
        assert_eq!(recv(&mut ws).await["proto"], 2);
        send(&mut ws, json!({ "type": "welcome", "proto": 2, "hubVersion": "t" })).await;

        // 未允许接管：拒绝
        send(&mut ws, json!({ "type": "attach", "sessionId": id })).await;
        assert_eq!(recv(&mut ws).await, json!({ "type": "attachDenied", "sessionId": id, "reason": "disabled" }));
        shared.lock().unwrap().config.hub.allow_takeover = true;
        send(&mut ws, json!({ "type": "attach", "sessionId": 999 })).await;
        assert_eq!(recv(&mut ws).await, json!({ "type": "attachDenied", "sessionId": 999, "reason": "not_found" }));

        // 接管前的输出不上链路；attach 后先收到含该输出的快照，再收到实时输出
        assert!(session::on_output(&shared, id, b"before"));
        silent(&mut ws).await;
        send(&mut ws, json!({ "type": "attach", "sessionId": id })).await;
        let attached = recv(&mut ws).await;
        assert_eq!((attached["type"].as_str(), attached["cols"].as_u64(), attached["rows"].as_u64()), (Some("attached"), Some(100), Some(20)));
        let snapshot = B64.decode(attached["snapshot"].as_str().unwrap()).unwrap();
        assert!(String::from_utf8_lossy(&snapshot).contains("before"));
        assert!(taken());
        session::on_output(&shared, id, b"hello");
        assert_eq!(recv(&mut ws).await, json!({ "type": "output", "sessionId": id, "data": B64.encode("hello") }));

        // input 写入会话，resize 生效；越界尺寸忽略
        send(&mut ws, json!({ "type": "input", "sessionId": id, "data": "bHMK" })).await;
        assert!(matches!(cmd(&mut cmds).await, Cmd::Data(d) if d == b"ls\n"));
        send(&mut ws, json!({ "type": "resize", "sessionId": id, "cols": 0, "rows": 30 })).await;
        send(&mut ws, json!({ "type": "resize", "sessionId": id, "cols": 120, "rows": 30 })).await;
        assert!(matches!(cmd(&mut cmds).await, Cmd::Resize(120, 30)));

        // hub detach：不回 detached，之后不再推送输出，input 忽略
        send(&mut ws, json!({ "type": "detach", "sessionId": id })).await;
        wait_until(|| !taken()).await;
        session::on_output(&shared, id, b"after");
        send(&mut ws, json!({ "type": "input", "sessionId": id, "data": "bHMK" })).await;
        silent(&mut ws).await;
        assert!(cmds.try_recv().is_err());

        // 重复 attach 按成功处理；本地收回 → reclaimed
        for _ in 0..2 {
            send(&mut ws, json!({ "type": "attach", "sessionId": id })).await;
            let again = recv(&mut ws).await;
            assert_eq!((again["type"].as_str(), again["cols"].as_u64()), (Some("attached"), Some(120)));
        }
        reclaim(&mut shared.lock().unwrap(), id);
        assert_eq!(recv(&mut ws).await, json!({ "type": "detached", "sessionId": id, "reason": "reclaimed" }));
        assert!(!taken());

        // 关闭允许接管 → disabled（与 SetConfig 中的处理相同）
        send(&mut ws, json!({ "type": "attach", "sessionId": id })).await;
        assert_eq!(recv(&mut ws).await["type"], "attached");
        {
            let mut st = shared.lock().unwrap();
            st.config.hub.allow_takeover = false;
            release_all(&mut st, Some("disabled"));
        }
        assert_eq!(recv(&mut ws).await, json!({ "type": "detached", "sessionId": id, "reason": "disabled" }));

        // 会话退出 → exited，之后 attach 为 not_found
        shared.lock().unwrap().config.hub.allow_takeover = true;
        send(&mut ws, json!({ "type": "attach", "sessionId": id })).await;
        assert_eq!(recv(&mut ws).await["type"], "attached");
        session::on_exit(&shared, id);
        assert_eq!(recv(&mut ws).await, json!({ "type": "detached", "sessionId": id, "reason": "exited" }));
        send(&mut ws, json!({ "type": "attach", "sessionId": id })).await;
        assert_eq!(recv(&mut ws).await["reason"], "not_found");

        // hub 断线：清空全部接管
        let (id2, _cmds2) = session::test_session(&mut shared.lock().unwrap(), 80, 24);
        send(&mut ws, json!({ "type": "attach", "sessionId": id2 })).await;
        assert_eq!(recv(&mut ws).await["type"], "attached");
        drop(ws);
        wait_until(|| !shared.lock().unwrap().sessions[&id2].info.taken_over).await;
        assert!(shared.lock().unwrap().hub.tx.is_none());
        task.abort();
    }

    /// 网页关闭会话端到端：disabled / not_found / 成功（结束接管、关闭后端、会话列表上报中消失）
    #[tokio::test]
    async fn close_end_to_end() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared: Shared = Arc::new(Mutex::new(State::default()));
        let (id, mut cmds) = session::test_session(&mut shared.lock().unwrap(), 80, 24);
        let (id2, _cmds2) = session::test_session(&mut shared.lock().unwrap(), 80, 24);
        // 用 State 的变化通知：关闭后的会话列表经防抖上报
        let changed = shared.lock().unwrap().hub.changed.clone();
        let endpoint = link::parse_address(&format!("ws://127.0.0.1:{port}"), true).unwrap();
        let hello = link::hello("mid".into(), "PC", "0.0.1");
        let task = tokio::spawn(link::run(make_link(&shared, 0, endpoint, Secret("t".into()), hello, changed)));
        let mut ws = tokio_tungstenite::accept_async(listener.accept().await.unwrap().0).await.unwrap();
        assert!(recv(&mut ws).await["caps"].as_array().unwrap().contains(&json!("close")));
        send(&mut ws, json!({ "type": "welcome", "proto": 2, "hubVersion": "0.4.0", "caps": ["history", "image", "close"] })).await;

        // 未允许接管 → disabled，会话不受影响
        send(&mut ws, json!({ "type": "close", "sessionId": id, "requestId": "r1" })).await;
        assert_eq!(recv(&mut ws).await, json!({ "type": "closeResult", "sessionId": id, "requestId": "r1", "ok": false, "reason": "disabled" }));
        assert!(shared.lock().unwrap().sessions.contains_key(&id));

        // 会话不存在 / 已退出 → not_found
        shared.lock().unwrap().config.hub.allow_takeover = true;
        send(&mut ws, json!({ "type": "close", "sessionId": 999, "requestId": "r2" })).await;
        assert_eq!(recv(&mut ws).await, json!({ "type": "closeResult", "sessionId": 999, "requestId": "r2", "ok": false, "reason": "not_found" }));
        session::on_exit(&shared, id2);
        send(&mut ws, json!({ "type": "close", "sessionId": id2, "requestId": "r3" })).await;
        assert_eq!(recv(&mut ws).await["reason"], "not_found");

        // 正被接管的会话：先结束接管（detached exited），再回成功；后端被关闭，之后的会话列表上报中不再有它
        send(&mut ws, json!({ "type": "attach", "sessionId": id })).await;
        assert_eq!(recv(&mut ws).await["type"], "attached");
        send(&mut ws, json!({ "type": "close", "sessionId": id, "requestId": "r4" })).await;
        assert_eq!(recv(&mut ws).await, json!({ "type": "detached", "sessionId": id, "reason": "exited" }));
        assert_eq!(recv(&mut ws).await, json!({ "type": "closeResult", "sessionId": id, "requestId": "r4", "ok": true }));
        assert!(matches!(cmd(&mut cmds).await, Cmd::Close));
        assert!(!shared.lock().unwrap().sessions.contains_key(&id));
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.expect("等待会话列表上报超时").unwrap().unwrap();
            let Message::Text(text) = msg else { continue };
            let v: Value = serde_json::from_str(&text).unwrap();
            if v["type"] == "sessions" && v["sessions"].as_array().unwrap().iter().all(|s| s["id"] != id) {
                break;
            }
        }
        task.abort();
    }

    /// 网页新建端到端：disabled / createOptions / claudeHistory / not_found / 目录不存在 / 本地新建成功（不向 GUI 发 Activate）
    #[tokio::test]
    async fn create_end_to_end() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared: Shared = Arc::new(Mutex::new(State::default()));
        // 模拟已连接的桌面端 GUI：网页新建不应向它发 Activate / Error
        let (gui_tx, mut gui_rx) = tokio::sync::mpsc::unbounded_channel();
        {
            let mut st = shared.lock().unwrap();
            st.gui = Some(session::Gui::new(1, gui_tx));
            st.config.shell = "cmd.exe".into();
            st.config.hosts = vec![proto::Host {
                id: "h-test".into(),
                name: "dev".into(),
                host: "127.0.0.1".into(),
                port: 1,
                user: "u".into(),
                auth: proto::AuthKind::Password,
                key_path: None,
                source: proto::HostSource::Tether,
            }];
            st.recent = vec![
                proto::RecentItem { target: proto::Target::Local { cwd: r"C:\a".into() }, host: None, closed_at: 3 },
                proto::RecentItem { target: proto::Target::Remote { host_id: "h-test".into(), cwd: Some("/srv".into()) }, host: Some("旧名".into()), closed_at: 2 },
                proto::RecentItem { target: proto::Target::Remote { host_id: "gone".into(), cwd: Some("/x".into()) }, host: None, closed_at: 1 },
            ];
        }
        let changed = shared.lock().unwrap().hub.changed.clone();
        let endpoint = link::parse_address(&format!("ws://127.0.0.1:{port}"), true).unwrap();
        let hello = link::hello("mid".into(), "PC", "0.0.1");
        let task = tokio::spawn(link::run(make_link(&shared, 0, endpoint, Secret("t".into()), hello, changed)));
        let mut ws = tokio_tungstenite::accept_async(listener.accept().await.unwrap().0).await.unwrap();
        assert!(recv(&mut ws).await["caps"].as_array().unwrap().contains(&json!("create")));
        send(&mut ws, json!({ "type": "welcome", "proto": 2, "hubVersion": "0.5.0", "caps": ["close", "create"] })).await;

        // 未允许接管：三种请求都是 disabled
        send(&mut ws, json!({ "type": "createOptions", "requestId": "o1" })).await;
        assert_eq!(recv(&mut ws).await, json!({ "type": "createOptionsResult", "requestId": "o1", "ok": false, "reason": "disabled" }));
        send(&mut ws, json!({ "type": "claudeHistory", "requestId": "c1", "cwd": r"C:\a" })).await;
        assert_eq!(recv(&mut ws).await, json!({ "type": "claudeHistoryResult", "requestId": "c1", "ok": false, "reason": "disabled" }));
        send(&mut ws, json!({ "type": "create", "requestId": "n1", "target": { "kind": "local" } })).await;
        let r = recv(&mut ws).await;
        assert_eq!((r["type"].as_str(), r["ok"].as_bool(), r["reason"].as_str()), (Some("createResult"), Some(false), Some("disabled")));
        assert!(shared.lock().unwrap().sessions.is_empty());

        // 新建选项：remote 最近目录带主机当前名称，主机已不存在的剔除
        shared.lock().unwrap().config.hub.allow_takeover = true;
        send(&mut ws, json!({ "type": "createOptions", "requestId": "o2" })).await;
        let r = recv(&mut ws).await;
        assert_eq!((r["type"].as_str(), r["ok"].as_bool()), (Some("createOptionsResult"), Some(true)));
        assert_eq!(r["home"].as_str(), config::home().map(|h| h.display().to_string()).as_deref());
        assert_eq!(r["recent"], json!([{ "kind": "local", "cwd": r"C:\a" }, { "kind": "remote", "hostId": "h-test", "host": "dev", "cwd": "/srv" }]));
        assert_eq!(r["hosts"][0], json!({ "id": "h-test", "name": "dev" }));

        // Claude 历史：没有对话的目录为空列表
        send(&mut ws, json!({ "type": "claudeHistory", "requestId": "c2", "cwd": r"C:\tether-no-such-dir-4f2a" })).await;
        assert_eq!(recv(&mut ws).await, json!({ "type": "claudeHistoryResult", "requestId": "c2", "ok": true, "items": [] }));

        // 主机不存在 → not_found；本地目录不存在 → error
        send(&mut ws, json!({ "type": "create", "requestId": "n2", "target": { "kind": "remote", "hostId": "nope" } })).await;
        let r = recv(&mut ws).await;
        assert_eq!((r["requestId"].as_str(), r["ok"].as_bool(), r["reason"].as_str()), (Some("n2"), Some(false), Some("not_found")));
        send(&mut ws, json!({ "type": "create", "requestId": "n3", "target": { "kind": "local", "cwd": r"C:\tether-no-such-dir-4f2a" } })).await;
        let r = recv(&mut ws).await;
        assert_eq!((r["requestId"].as_str(), r["reason"].as_str()), (Some("n3"), Some("error")));
        assert!(r["message"].as_str().unwrap().contains("目录不存在"));
        assert!(shared.lock().unwrap().sessions.is_empty());

        // 本地新建成功：createResult 之前已上报含新会话的列表
        let dir = std::env::temp_dir();
        let cwd = dir.display().to_string();
        send(&mut ws, json!({ "type": "create", "requestId": "n4", "target": { "kind": "local", "cwd": cwd }, "cols": 100, "rows": 30 })).await;
        let mut listed = false;
        let r = loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), ws.next()).await.expect("等待 createResult 超时").unwrap().unwrap();
            let Message::Text(text) = msg else { continue };
            let v: Value = serde_json::from_str(&text).unwrap();
            if v["type"] == "sessions" {
                listed |= v["sessions"].as_array().unwrap().iter().any(|s| s["cwd"] == cwd.as_str());
                continue;
            }
            break v;
        };
        assert_eq!((r["type"].as_str(), r["requestId"].as_str(), r["ok"].as_bool()), (Some("createResult"), Some("n4"), Some(true)));
        assert!(listed, "createResult 前应已上报新会话");
        let id = r["sessionId"].as_u64().unwrap() as u32;
        {
            let st = shared.lock().unwrap();
            let s = &st.sessions[&id].info;
            assert_eq!((s.cwd.as_str(), s.command.as_str(), s.host.as_deref()), (cwd.as_str(), "cmd.exe", None));
            assert_eq!(session::snapshot(&st, id).map(|(c, r, _)| (c, r)), Some((100, 30)));
        }
        // GUI 收到会话列表，但没有 Activate / Error
        let mut gui = Vec::new();
        while let Ok(line) = gui_rx.try_recv() {
            gui.push(serde_json::from_str::<Value>(&line).unwrap()["type"].as_str().unwrap().to_string());
        }
        assert!(gui.iter().any(|t| t == "sessions"), "{gui:?}");
        assert!(!gui.iter().any(|t| t == "activate" || t == "error"), "{gui:?}");

        let removed = session::remove(&mut shared.lock().unwrap(), id);
        std::thread::spawn(move || removed.unwrap().close()).join().unwrap();
        task.abort();
    }

    #[test]
    fn recent_dirs_and_history_items() {
        let host = |id: &str, name: &str| proto::Host {
            id: id.into(),
            name: name.into(),
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: proto::AuthKind::Key,
            key_path: None,
            source: proto::HostSource::Tether,
        };
        let recent = vec![
            proto::RecentItem { target: proto::Target::Remote { host_id: "gone".into(), cwd: Some("/x".into()) }, host: None, closed_at: 3 },
            proto::RecentItem { target: proto::Target::Remote { host_id: "h1".into(), cwd: None }, host: Some("旧".into()), closed_at: 2 },
            proto::RecentItem { target: proto::Target::Local { cwd: r"C:\a".into() }, host: None, closed_at: 1 },
        ];
        assert_eq!(
            recent_dirs(&recent, &[host("h1", "dev")]),
            [
                link::RecentDir::Remote { host_id: "h1".into(), host: "dev".into(), cwd: String::new() },
                link::RecentDir::Local { cwd: r"C:\a".into() }
            ]
        );

        // Claude 历史：临时 projects 目录；open 标记已有会话在运行的对话，最多 50 条
        let root = std::env::temp_dir().join(format!("tether-hub-cc-{}", uuid::Uuid::new_v4()));
        let dir = root.join("C--work-p");
        std::fs::create_dir_all(&dir).unwrap();
        let line = r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"你好"},"permissionMode":"acceptEdits"}"#;
        for i in 0..55 {
            let path = dir.join(format!("id{i:02}.jsonl"));
            std::fs::write(&path, line).unwrap();
            let t = std::time::UNIX_EPOCH + Duration::from_secs(1_000 + i);
            std::fs::File::options().write(true).open(&path).unwrap().set_modified(t).unwrap();
        }
        let items = history_items(&root, r"C:\work\p", &HashSet::from(["id54".to_string()]));
        assert_eq!(items.len(), 50);
        let expect = link::HistoryItem {
            id: "id54".into(),
            title: "你好".into(),
            mtime: 1_054_000,
            messages: 1,
            permission_mode: Some("acceptEdits".into()),
            bypass: false,
            open: true,
        };
        assert_eq!(items[0], expect);
        assert_eq!((items[1].id.as_str(), items[1].open), ("id53", false));
        assert!(history_items(&root, r"C:\other", &HashSet::new()).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 网页贴图：begin → 每块原始 ≤256KB 的 chunk → end
    async fn upload(ws: &mut Ws, id: u32, upload_id: &str, mime: &str, size: usize, data: &[u8]) {
        send(ws, json!({ "type": "imageBegin", "sessionId": id, "uploadId": upload_id, "mime": mime, "size": size })).await;
        for chunk in data.chunks(256 << 10) {
            send(ws, json!({ "type": "imageChunk", "sessionId": id, "uploadId": upload_id, "data": B64.encode(chunk) })).await;
        }
        send(ws, json!({ "type": "imageEnd", "sessionId": id, "uploadId": upload_id })).await;
    }

    /// 能力位协商、attached 的 history、网页贴图端到端
    #[tokio::test]
    async fn history_and_image_end_to_end() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared: Shared = Arc::new(Mutex::new(State::default()));
        shared.lock().unwrap().config.hub.allow_takeover = true;
        let (id, mut cmds) = session::test_session(&mut shared.lock().unwrap(), 40, 5);
        let (id2, _cmds2) = session::test_session(&mut shared.lock().unwrap(), 40, 5);
        for i in 0..20 {
            session::on_output(&shared, id, format!("\x1b[32mline {i}\x1b[m\r\n").as_bytes());
        }
        let endpoint = link::parse_address(&format!("ws://127.0.0.1:{port}"), true).unwrap();
        let hello = link::hello("mid".into(), "PC", "0.0.1");
        let task = tokio::spawn(link::run(make_link(&shared, 0, endpoint, Secret("t".into()), hello, Arc::new(Notify::new()))));

        // 旧 hub：welcome 无 caps → attached 不带 history
        let mut ws = tokio_tungstenite::accept_async(listener.accept().await.unwrap().0).await.unwrap();
        assert_eq!(recv(&mut ws).await["caps"], json!(["history", "image", "close", "create"]));
        send(&mut ws, json!({ "type": "welcome", "proto": 2, "hubVersion": "0.3.0" })).await;
        send(&mut ws, json!({ "type": "attach", "sessionId": id })).await;
        let attached = recv(&mut ws).await;
        assert_eq!(attached["type"], "attached");
        assert!(attached.get("history").is_none(), "{attached}");
        assert!(shared.lock().unwrap().hub.caps.is_empty());

        // 断线重连到新 hub：welcome 带 caps（未知能力忽略）→ attached 带 history
        drop(ws);
        let mut ws = tokio_tungstenite::accept_async(listener.accept().await.unwrap().0).await.unwrap();
        assert_eq!(recv(&mut ws).await["type"], "hello");
        send(&mut ws, json!({ "type": "welcome", "proto": 2, "hubVersion": "0.4.0", "caps": ["history", "image", "future"] })).await;
        send(&mut ws, json!({ "type": "attach", "sessionId": id })).await;
        let attached = recv(&mut ws).await;
        let history = String::from_utf8(B64.decode(attached["history"].as_str().unwrap()).unwrap()).unwrap();
        // 5 行屏、20 行输出 + 末尾空行：推出 line 0–15
        assert!(history.starts_with("\x1b[m\x1b[32mline 0\r\n"), "{history:?}");
        assert!(history.ends_with(&format!("line 15\x1b[m{}", "\r\n".repeat(5))), "{history:?}");
        assert_eq!(shared.lock().unwrap().hub.caps, HashSet::from(["history".to_string(), "image".to_string()]));

        // 未接管的会话 → not_taken_over（begin 时即拒绝）
        upload(&mut ws, id2, "u0", "image/png", 8, b"\x89PNG\r\n\x1a\n").await;
        assert_eq!(recv(&mut ws).await, json!({ "type": "imageFailed", "sessionId": id2, "uploadId": "u0", "reason": "not_taken_over" }));
        silent(&mut ws).await;

        // 正常上传（多块）：文件存在、内容一致，路径作为输入写入会话（不加回车）
        let png: Vec<u8> = b"\x89PNG\r\n\x1a\n".iter().copied().chain((0..600_000u32).map(|i| i as u8)).collect();
        upload(&mut ws, id, "u1", "image/png", png.len(), &png).await;
        let saved = recv(&mut ws).await;
        assert_eq!((saved["type"].as_str(), saved["uploadId"].as_str()), (Some("imageSaved"), Some("u1")));
        let path = saved["path"].as_str().unwrap().to_string();
        assert!(path.ends_with(".png") && std::path::Path::new(&path).starts_with(image::dir()));
        assert_eq!(std::fs::read(&path).unwrap(), png);
        let typed = if path.contains(' ') { format!("\"{path}\"") } else { path.clone() };
        assert!(matches!(cmd(&mut cmds).await, Cmd::Data(d) if d == typed.as_bytes()));
        let _ = std::fs::remove_file(&path);

        // 会话开启 bracketed paste：路径按粘贴包裹
        session::on_output(&shared, id, b"\x1b[?2004h");
        assert_eq!(recv(&mut ws).await["type"], "output");
        let gif = b"GIF89a\x01\x00\x01\x00";
        upload(&mut ws, id, "u2", "image/gif", gif.len(), gif).await;
        let saved = recv(&mut ws).await;
        let path = saved["path"].as_str().unwrap().to_string();
        assert!(path.ends_with(".gif"));
        let typed = if path.contains(' ') { format!("\"{path}\"") } else { path.clone() };
        assert!(matches!(cmd(&mut cmds).await, Cmd::Data(d) if d == format!("\x1b[200~{typed}\x1b[201~").into_bytes()));
        let _ = std::fs::remove_file(&path);

        // 超过 10MB（begin 声明）/ 实际数据超出声明大小 → too_large
        upload(&mut ws, id, "u3", "image/png", image::MAX_BYTES + 1, &[]).await;
        assert_eq!(recv(&mut ws).await["reason"], "too_large");
        upload(&mut ws, id, "u4", "image/png", 4, b"\x89PNG\r\n\x1a\n").await;
        assert_eq!(recv(&mut ws).await, json!({ "type": "imageFailed", "sessionId": id, "uploadId": "u4", "reason": "too_large" }));
        silent(&mut ws).await;

        // 文件头与声明类型不符 / 不支持的类型 → bad_type
        upload(&mut ws, id, "u5", "image/png", gif.len(), gif).await;
        assert_eq!(recv(&mut ws).await, json!({ "type": "imageFailed", "sessionId": id, "uploadId": "u5", "reason": "bad_type" }));
        upload(&mut ws, id, "u6", "image/svg+xml", 4, b"<svg").await;
        assert_eq!(recv(&mut ws).await["reason"], "bad_type");

        // SSH 会话 → unsupported
        send(&mut ws, json!({ "type": "attach", "sessionId": id2 })).await;
        assert_eq!(recv(&mut ws).await["type"], "attached");
        shared.lock().unwrap().sessions.get_mut(&id2).unwrap().info.host = Some("srv".into());
        upload(&mut ws, id2, "u7", "image/png", 8, b"\x89PNG\r\n\x1a\n").await;
        assert_eq!(recv(&mut ws).await["reason"], "unsupported");

        // 本机未允许接管 → disabled
        shared.lock().unwrap().config.hub.allow_takeover = false;
        upload(&mut ws, id, "u8", "image/png", 8, b"\x89PNG\r\n\x1a\n").await;
        assert_eq!(recv(&mut ws).await["reason"], "disabled");
        silent(&mut ws).await;
        assert!(cmds.try_recv().is_err());
        task.abort();
    }
}
