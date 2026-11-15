//! 管理服务器（hub）客户端：按 config.json 的 hub 设置与凭据管理器中的 token 维持一条常驻出站连接，上报会话列表。
//! 默认关闭；未启用 / 未配置 / 连接失败时不影响 daemon 其它功能（连接在独立异步任务中，失败只体现在 HubStatus）。

mod link;

use std::{path::Path, sync::Arc};

use proto::{HubConfig, HubState, Secret, ServerMsg};
use tokio::{sync::Notify, task::AbortHandle};

use crate::session::{Shared, State};

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
}

impl Hub {
    pub fn status_msg(&self) -> ServerMsg {
        ServerMsg::HubStatus { state: self.state, error: self.error.clone() }
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
    st.hub.generation += 1;
    let generation = st.hub.generation;
    let HubConfig { enabled, address, allow_insecure } = st.config.hub.clone();
    if !enabled {
        return set_status(st, generation, HubState::Disabled, None);
    }
    let endpoint = match link::parse_address(&address, allow_insecure) {
        Ok(ep) => ep,
        Err(e) => return set_status(st, generation, HubState::Failed, Some(format!("地址无效：{e}"))),
    };
    set_status(st, generation, HubState::Connecting, None);
    let shared = state.clone();
    let changed = st.hub.changed.clone();
    let task = tokio::spawn(async move {
        // 读凭据管理器放在任务里，不拖慢启动 / 保存设置
        let Some(token) = saved_token() else {
            let mut st = shared.lock().unwrap();
            return set_status(&mut st, generation, HubState::Failed, Some("未保存 token：请在设置中填写 hub 签发的 token".into()));
        };
        let sessions_state = shared.clone();
        let status_state = shared.clone();
        log!("hub 客户端启动：{}", endpoint.url());
        link::run(link::Link {
            endpoint,
            token: Secret(token),
            hello: hello(),
            sessions: Arc::new(move || sessions_state.lock().unwrap().sessions.values().map(|s| s.info.clone()).collect()),
            changed,
            status: Arc::new(move |state, error| set_status(&mut status_state.lock().unwrap(), generation, state, error)),
        })
        .await;
    });
    st.hub.task = Some(task.abort_handle());
}

/// 设置面板“测试连接”（异步任务）：独立连接，握手 → hello → welcome 后立即关闭；结果以 ServerMsg::TestHub 回复
pub async fn test(state: Shared, req_id: u64, address: String, allow_insecure: bool, token: Option<String>) {
    let start = std::time::Instant::now();
    let result = match (link::parse_address(&address, allow_insecure), token) {
        (Err(e), _) => Err(format!("地址无效：{e}")),
        (Ok(_), None) => Err("未填写 token，凭据管理器中也没有已保存的 token".into()),
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
    let entry = entry().ok_or("无法打开 Windows 凭据管理器")?;
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
    use super::*;

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
}
