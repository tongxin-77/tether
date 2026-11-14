//! SSH 远程会话：连接池（同一主机只建一条连接，终端会话为其上的 channel）、主机指纹校验、密钥 / agent / 密码认证。
//!
//! 连接池按 Weak 持有连接，每个 channel 任务持有一份 Arc<Conn>；最后一个持有者释放后 Handle 与全部 channel 发送端都被丢弃，
//! russh 会话循环随之结束并断开。文件树的 SFTP 子系统按连接懒创建并缓存在 Conn 内；会话只持有 Weak<Conn>，
//! 文件操作进行期间临时持有 Arc<Conn>，不延长连接寿命。

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

use proto::{AuthKind, Host, PromptKind, ServerMsg};
use russh::{
    ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Disconnect, MethodKind,
    client::{self, AuthResult, KeyboardInteractiveAuthResponse},
    keys::{self, HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate, agent::AgentIdentity, agent::client::AgentClient, known_hosts},
};
use russh_sftp::client::SftpSession;
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::{
    config, fs,
    session::{self, Backend, Shared},
};

/// 主机连接池：key 为 `<host id>|user@host:port`，值为该主机连接的弱引用（异步锁保证并发新建会话只连一次）
pub type Pool = HashMap<String, Arc<tokio::sync::Mutex<Weak<Conn>>>>;

const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);
/// 新建会话时用 SFTP 解析远程目录的最长等待，超时则保留原值（如 `~`）
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
/// 凭据管理器服务名；debug 构建用 `tether-dev`，与安装版保存的密码隔离（同 proto::pipe_name 的 dev 隔离）
const KEYRING_SERVICE: &str = if cfg!(debug_assertions) { "tether-dev" } else { "tether" };
/// Windows OpenSSH 的 ssh-agent 命名管道
const AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
const DEFAULT_KEYS: [&str; 3] = ["id_ed25519", "id_ecdsa", "id_rsa"];
/// 连接测试的最长耗时（等待用户确认指纹 / 输入私钥口令的时间不计）
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// 密码来源：正式连接先用凭据管理器中保存的密码，再弹窗询问；连接测试只用给定的密码（None 为没有），不弹窗、不改凭据管理器
#[derive(Clone, Copy)]
enum Pw<'a> {
    Interactive,
    Given(Option<&'a str>),
}

/// 会话发给其 channel 任务的命令
pub enum Cmd {
    Data(Vec<u8>),
    Resize(u16, u16),
    Close,
}

/// 一条已认证的 SSH 连接；在 handle 上可继续开 channel（终端）或 SFTP 子系统
pub struct Conn {
    pub handle: client::Handle<Client>,
    /// 懒创建的 SFTP 会话（异步锁，并发请求只开一次子系统）
    sftp: tokio::sync::Mutex<Option<Arc<SftpSession>>>,
}

impl Conn {
    /// 取该连接上的 SFTP 会话，首次调用时开 channel + sftp 子系统
    pub async fn sftp(&self) -> Result<Arc<SftpSession>, String> {
        let mut slot = self.sftp.lock().await;
        if let Some(s) = slot.as_ref() {
            return Ok(s.clone());
        }
        let ch = self.handle.channel_open_session().await.map_err(|e| format!("打开 SFTP channel 失败：{e}"))?;
        ch.request_subsystem(true, "sftp").await.map_err(|e| format!("服务器不支持 SFTP：{e}"))?;
        let sftp = Arc::new(SftpSession::new(ch.into_stream()).await.map_err(|e| format!("初始化 SFTP 失败：{e}"))?);
        *slot = Some(sftp.clone());
        Ok(sftp)
    }
}

pub struct Client {
    state: Shared,
    host: String,
    port: u16,
    label: String,
    /// check_server_key 拒绝的原因，connect 失败时用于报错
    reject: Arc<Mutex<Option<String>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        match verify_host_key(&self.state, &self.host, self.port, &self.label, key).await {
            Ok(()) => Ok(true),
            Err(e) => {
                *self.reject.lock().unwrap() = Some(e);
                Ok(false)
            }
        }
    }

    /// 记录连接断开原因（服务端主动断开 / 网络错误 / keepalive 超时），便于排查掉线
    async fn disconnected(&mut self, reason: client::DisconnectReason<Self::Error>) -> Result<(), Self::Error> {
        match reason {
            client::DisconnectReason::ReceivedDisconnect(info) => {
                log!("SSH 连接 {} 被服务端断开：{info:?}", self.label);
                Ok(())
            }
            client::DisconnectReason::Error(e) => {
                log!("SSH 连接 {} 异常断开：{e}", self.label);
                Err(e)
            }
        }
    }
}

/// 新建远程会话（异步任务）：结果以 Sessions + Activate 或 Error 告知 GUI
pub async fn create(state: Shared, host: Host, cwd: Option<String>, cols: u16, rows: u16) {
    let result = open_shell(&state, &host, cwd, cols, rows).await;
    let st = state.lock().unwrap();
    match result {
        Ok(id) => {
            st.push_sessions();
            st.send(&ServerMsg::Activate { id });
        }
        Err(e) => {
            log!("连接 {} 失败：{e}", host.name);
            st.send(&ServerMsg::Error { message: format!("连接 {} 失败：{e}", host.name) });
        }
    }
}

fn label(host: &Host) -> String {
    format!("{}@{}:{}", host.user, host.host, host.port)
}

/// 用 SFTP 把会话目录解析为绝对路径（未指定时为 home），作为文件树根；SFTP 不可用或超时则保留原值
async fn resolve_cwd(conn: &Conn, cwd: Option<&str>) -> String {
    let fallback = cwd.unwrap_or("~").to_string();
    let resolve = async { conn.sftp().await?.canonicalize(cwd.unwrap_or(".")).await.map_err(|e| e.to_string()) };
    match tokio::time::timeout(RESOLVE_TIMEOUT, resolve).await {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            log!("解析远程目录失败，保留 {fallback}：{e}");
            fallback
        }
        Err(_) => {
            log!("解析远程目录超时，保留 {fallback}");
            fallback
        }
    }
}

/// POSIX shell 单引号转义
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 在主机连接上开 channel：request_pty + shell（指定目录时 exec `cd '<dir>' && exec "$SHELL" -l`），登记会话并起 channel 任务
async fn open_shell(state: &Shared, host: &Host, cwd: Option<String>, cols: u16, rows: u16) -> Result<u32, String> {
    let conn = connection(state, host).await?;
    let ch = conn.handle.channel_open_session().await.map_err(|e| format!("打开 channel 失败：{e}"))?;
    let err = |e: russh::Error| format!("启动远程 shell 失败：{e}");
    ch.request_pty(false, "xterm-256color", cols.into(), rows.into(), 0, 0, &[]).await.map_err(err)?;
    // 多数 sshd 默认不接受（AcceptEnv），不要求回复，被拒也无影响
    let _ = ch.set_env(false, "COLORTERM", "truecolor").await;
    match &cwd {
        Some(dir) => ch.exec(false, format!("cd {} && exec \"$SHELL\" -l", shell_quote(dir))).await,
        None => ch.request_shell(false).await,
    }
    .map_err(err)?;

    let (rd, wr) = ch.split();
    let (tx, rx) = mpsc::unbounded_channel();
    let name = match &cwd {
        Some(d) => d.split('/').filter(|s| !s.is_empty()).last().unwrap_or(d).to_string(),
        None => "~".into(),
    };
    let root = resolve_cwd(&conn, cwd.as_deref()).await;
    let mut info = session::new_info(name, root, format!("ssh {}", label(host)), Some(host.name.clone()));
    info.host_id = Some(host.id.clone());
    // 远程文件打开时的临时目录名，`:` 在 Windows 路径中非法
    let fs = fs::Backend::Remote { conn: Arc::downgrade(&conn), label: format!("{}@{}_{}", host.user, host.host, host.port) };
    let id = session::register(state, info, Backend::Remote(tx), fs, cols, rows);
    tokio::spawn(pump(state.clone(), id, conn, rd, wr, rx));
    Ok(id)
}

/// channel 任务：远程输出走与本地相同的处理；EOF / 退出状态标记 exited，Close 后结束；会话关闭时关 channel
async fn pump(
    state: Shared,
    id: u32,
    conn: Arc<Conn>,
    mut rd: ChannelReadHalf,
    wr: ChannelWriteHalf<client::Msg>,
    mut rx: UnboundedReceiver<Cmd>,
) {
    // 结束原因写入日志：远端 shell 退出（含 TMOUT 自动登出）与连接断开需区分
    let mut reason = String::from("连接已断开");
    loop {
        tokio::select! {
            msg = rd.wait() => match msg {
                Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                    if !session::on_output(&state, id, &data) {
                        reason = "会话被关闭".into();
                        let _ = wr.close().await;
                        break;
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    reason = format!("远端进程退出，状态码 {exit_status}");
                    session::on_exit(&state, id);
                }
                Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                    reason = format!("远端进程被信号 {signal_name:?} 结束");
                    session::on_exit(&state, id);
                }
                Some(ChannelMsg::Eof) => session::on_exit(&state, id),
                Some(ChannelMsg::Close) => {
                    if reason == "连接已断开" {
                        reason = "服务端关闭 channel".into();
                    }
                    break;
                }
                None => break,
                Some(_) => {}
            },
            cmd = rx.recv() => match cmd {
                Some(Cmd::Data(d)) => {
                    let _ = wr.data_bytes(d).await;
                }
                Some(Cmd::Resize(cols, rows)) => {
                    let _ = wr.window_change(cols.into(), rows.into(), 0, 0).await;
                }
                Some(Cmd::Close) | None => {
                    reason = "会话被关闭".into();
                    let _ = wr.close().await;
                    break;
                }
            },
        }
    }
    session::on_exit(&state, id);
    log!("远程会话 {id} 的 channel 已结束：{reason}");
    drop(conn);
}

/// 取主机连接：池中有存活连接则复用，否则新建（持该主机的异步锁，不持全局锁）
pub async fn connection(state: &Shared, host: &Host) -> Result<Arc<Conn>, String> {
    let label = label(host);
    let slot = state.lock().unwrap().pool.entry(format!("{}|{label}", host.id)).or_default().clone();
    let mut slot = slot.lock().await;
    if let Some(conn) = slot.upgrade().filter(|c| !c.handle.is_closed()) {
        log!("复用 SSH 连接 {label}");
        return Ok(conn);
    }
    log!("新建 SSH 连接 {label}");
    let conn = Arc::new(connect(state, host, &label).await?);
    *slot = Arc::downgrade(&conn);
    Ok(conn)
}

/// 客户端配置与 handler（含主机密钥校验）；reject 记录 check_server_key 拒绝的原因
fn new_client(state: &Shared, host: &Host, label: &str) -> (Arc<client::Config>, Client, Arc<Mutex<Option<String>>>) {
    let config = client::Config { keepalive_interval: Some(Duration::from_secs(30)), keepalive_max: 3, ..Default::default() };
    let reject = Arc::new(Mutex::new(None));
    let handler =
        Client { state: state.clone(), host: host.host.clone(), port: host.port, label: label.into(), reject: reject.clone() };
    (Arc::new(config), handler, reject)
}

async fn connect(state: &Shared, host: &Host, label: &str) -> Result<Conn, String> {
    let (config, handler, reject) = new_client(state, host, label);
    let mut handle = match client::connect(config, (host.host.as_str(), host.port), handler).await {
        Ok(h) => h,
        Err(e) => return Err(reject.lock().unwrap().take().unwrap_or_else(|| format!("连接失败：{e}"))),
    };
    authenticate(state, &mut handle, host, label, Pw::Interactive).await?;
    log!("SSH 认证成功 {label}");
    Ok(Conn { handle, sftp: Default::default() })
}

async fn authenticate(state: &Shared, h: &mut client::Handle<Client>, host: &Host, label: &str, pw: Pw<'_>) -> Result<(), String> {
    match host.auth {
        // 与 OpenSSH 一致：密钥全部失败后回退到密码（~/.ssh/config 导入的主机只读，否则无法用密码登录）
        AuthKind::Key => {
            if let Err(key_err) = auth_key(state, h, host, label).await {
                log!("{label} {key_err}，改用密码认证");
                auth_password(state, h, host, label, pw).await.map_err(|e| format!("{key_err}；{e}"))?;
            }
        }
        AuthKind::Password => auth_password(state, h, host, label, pw).await?,
    }
    Ok(())
}

/// 主机对话框“测试连接”（异步任务）：用对话框当前填写的配置依次 DNS 解析 → TCP 连接 → 握手与主机密钥校验（与正式连接相同，
/// 未知主机请用户确认指纹）→ 认证，成功后立即断开；不进连接池、不建会话、不改凭据管理器。结果以 ServerMsg::TestHost 回复
pub async fn test(state: Shared, req_id: u64, host: Host, password: Option<String>) {
    let label = label(&host);
    let start = Instant::now();
    let stage = Mutex::new("DNS 解析");
    let result = with_timeout(&state, &stage, test_connect(&state, &host, &label, password.as_deref(), &stage)).await;
    let (elapsed_ms, error) = match result {
        Ok(()) => (Some(start.elapsed().as_millis() as u64), None),
        Err(e) => (None, Some(e)),
    };
    log!("连接测试 {label}：{}", error.as_deref().unwrap_or("成功"));
    state.lock().unwrap().send(&ServerMsg::TestHost { req_id, elapsed_ms, error });
}

/// 连接测试限时 TEST_TIMEOUT；有交互提示在等 GUI 回复时暂停计时
async fn with_timeout(state: &Shared, stage: &Mutex<&str>, fut: impl Future<Output = Result<(), String>>) -> Result<(), String> {
    const TICK: Duration = Duration::from_millis(200);
    tokio::pin!(fut);
    let mut left = TEST_TIMEOUT;
    loop {
        tokio::select! {
            r = &mut fut => return r,
            _ = tokio::time::sleep(TICK) => {
                if !state.lock().unwrap().prompting() {
                    left = left.saturating_sub(TICK);
                    if left.is_zero() {
                        return Err(format!("连接超时：{}阶段 {} 秒内未完成", stage.lock().unwrap(), TEST_TIMEOUT.as_secs()));
                    }
                }
            }
        }
    }
}

async fn test_connect(state: &Shared, host: &Host, label: &str, password: Option<&str>, stage: &Mutex<&str>) -> Result<(), String> {
    let addrs: Vec<_> = tokio::net::lookup_host((host.host.as_str(), host.port))
        .await
        .map_err(|e| format!("DNS 解析失败：无法解析主机名 {}（{e}）", host.host))?
        .collect();
    *stage.lock().unwrap() = "TCP 连接";
    let stream = tokio::net::TcpStream::connect(&addrs[..]).await.map_err(|e| tcp_error(&e, &host.host, host.port))?;
    *stage.lock().unwrap() = "SSH 握手";
    let (config, handler, reject) = new_client(state, host, label);
    let mut handle = client::connect_stream(config, stream, handler).await.map_err(|e| match reject.lock().unwrap().take() {
        Some(r) => format!("主机密钥校验未通过：{r}"),
        None => format!("SSH 握手失败：{e}"),
    })?;
    *stage.lock().unwrap() = "认证";
    let result = authenticate(state, &mut handle, host, label, Pw::Given(password)).await.map_err(|e| format!("认证失败：{e}"));
    let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
    result
}

/// TCP 连接错误按原因分类
fn tcp_error(e: &std::io::Error, host: &str, port: u16) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        ConnectionRefused => format!("连接被拒绝：{host}:{port} 没有在监听 SSH 服务（{e}）"),
        TimedOut => format!("连接超时：{host}:{port} 无响应（{e}）"),
        HostUnreachable | NetworkUnreachable => format!("网络不可达：无法到达 {host}:{port}（{e}）"),
        _ => format!("TCP 连接失败：{host}:{port}（{e}）"),
    }
}

/// 向 GUI 发交互提示并等待回复；没有 GUI、取消、GUI 断开或超时都返回 Err
async fn ask(state: &Shared, kind: PromptKind, host: &str, message: String, fingerprint: Option<String>) -> Result<(String, bool), String> {
    let msg = ServerMsg::Prompt { req_id: 0, kind, host: host.into(), message, fingerprint };
    let rx = state.lock().unwrap().prompt(msg).ok_or("需要输入，但没有打开的 Tether 窗口")?;
    match tokio::time::timeout(PROMPT_TIMEOUT, rx).await {
        Ok(Ok(Some(answer))) => Ok(answer),
        Ok(_) => Err("已取消".into()),
        Err(_) => Err("等待输入超时，已取消".into()),
    }
}

/// 按 ~/.ssh/known_hosts 校验主机公钥：一致放行；不一致直接拒绝；未记录则请用户确认指纹后追加记录
async fn verify_host_key(state: &Shared, host: &str, port: u16, label: &str, key: &PublicKeyOrCertificate) -> Result<(), String> {
    let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
        return Err("不支持主机证书".into());
    };
    match known_hosts::check_known_hosts(host, port, key) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(keys::Error::KeyChanged { line }) => {
            return Err(format!("主机指纹与 ~/.ssh/known_hosts 第 {line} 行的记录不一致，可能遭到中间人攻击，已拒绝连接"));
        }
        Err(e) => return Err(format!("读取 known_hosts 失败：{e}")),
    }
    let message = format!("首次连接该主机，无法确认其真实性。请核对 {} 主机密钥指纹，信任后将记录到 ~/.ssh/known_hosts。", key.algorithm());
    let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
    let (answer, _) = ask(state, PromptKind::HostKey, label, message, Some(fingerprint)).await?;
    if answer != "yes" {
        return Err("未信任主机指纹".into());
    }
    known_hosts::learn_known_hosts(host, port, key).map_err(|e| format!("写入 known_hosts 失败：{e}"))
}

/// 密钥认证：指定的私钥 → ssh-agent → （未指定私钥时）~/.ssh 下的默认私钥
async fn auth_key(state: &Shared, h: &mut client::Handle<Client>, host: &Host, label: &str) -> Result<(), String> {
    let mut notes = Vec::new();
    let explicit = host.key_path.as_deref().map(config::expand_tilde);
    if let Some(p) = &explicit {
        if try_key_file(state, h, &host.user, Path::new(p), label, &mut notes).await {
            return Ok(());
        }
    }
    if try_agent(h, &host.user, &mut notes).await {
        return Ok(());
    }
    if explicit.is_none() {
        let dir = config::home().map(|d| d.join(".ssh")).unwrap_or_default();
        let found: Vec<_> = DEFAULT_KEYS.iter().map(|name| dir.join(name)).filter(|p| p.is_file()).collect();
        if found.is_empty() {
            notes.push(format!("~/.ssh 下没有默认私钥（{}）", DEFAULT_KEYS.join(" / ")));
        }
        for p in found {
            if try_key_file(state, h, &host.user, &p, label, &mut notes).await {
                return Ok(());
            }
        }
    }
    notes.insert(0, "密钥认证失败".into());
    Err(notes.join("；"))
}

/// 用私钥文件认证；有口令时弹窗询问（最多 3 次，不保存），取消则跳过该私钥
async fn try_key_file(
    state: &Shared,
    h: &mut client::Handle<Client>,
    user: &str,
    path: &Path,
    label: &str,
    notes: &mut Vec<String>,
) -> bool {
    if !path.is_file() {
        notes.push(format!("私钥文件 {} 不存在", path.display()));
        return false;
    }
    let key = match keys::load_secret_key(path, None) {
        Ok(k) => k,
        Err(keys::Error::KeyIsEncrypted) => {
            let mut unlocked = None;
            for i in 0..3 {
                let hint = if i == 0 { "请输入私钥口令" } else { "口令错误，请重新输入私钥口令" };
                let message = format!("{hint}：{}", path.display());
                let Ok((pass, _)) = ask(state, PromptKind::Passphrase, label, message, None).await else { break };
                if let Ok(k) = keys::load_secret_key(path, Some(&pass)) {
                    unlocked = Some(k);
                    break;
                }
            }
            match unlocked {
                Some(k) => k,
                None => {
                    notes.push(format!("私钥 {} 未解锁", path.display()));
                    return false;
                }
            }
        }
        Err(e) => {
            notes.push(format!("无法解析私钥 {}：{e}", path.display()));
            return false;
        }
    };
    let hash = h.best_supported_rsa_hash().await.ok().flatten().flatten();
    match h.authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await {
        Ok(r) if r.success() => true,
        Ok(_) => {
            notes.push(format!("服务器未接受私钥 {}", path.display()));
            false
        }
        Err(e) => {
            notes.push(format!("私钥 {} 认证出错：{e}", path.display()));
            false
        }
    }
}

/// 依次尝试 Windows OpenSSH ssh-agent 中的公钥；agent 未运行时直接跳过
async fn try_agent(h: &mut client::Handle<Client>, user: &str, notes: &mut Vec<String>) -> bool {
    let Ok(mut agent) = AgentClient::connect_named_pipe(AGENT_PIPE).await else {
        notes.push("ssh-agent 未运行".into());
        return false;
    };
    let Ok(identities) = agent.request_identities().await else {
        notes.push("读取 ssh-agent 中的密钥失败".into());
        return false;
    };
    if identities.is_empty() {
        notes.push("ssh-agent 中没有密钥".into());
        return false;
    }
    notes.push(format!("服务器未接受 ssh-agent 中的 {} 个密钥", identities.len()));
    let hash = h.best_supported_rsa_hash().await.ok().flatten().flatten();
    for id in identities {
        let AgentIdentity::PublicKey { key, .. } = id else { continue };
        if let Ok(r) = h.authenticate_publickey_with(user, key, hash, &mut agent).await {
            if r.success() {
                return true;
            }
        }
    }
    false
}

/// 密码认证：先用凭据管理器中保存的密码，失败则删除该保存项并弹窗（最多 3 次）；勾选保存且成功后写入凭据管理器。
/// 连接测试（Pw::Given）只试给定的密码一次
async fn auth_password(state: &Shared, h: &mut client::Handle<Client>, host: &Host, label: &str, pw: Pw<'_>) -> Result<(), String> {
    if let Pw::Given(given) = pw {
        let Some(password) = given else { return Err("未填写密码，凭据管理器中也没有已保存的密码".into()) };
        return if try_password(h, &host.user, password).await? { Ok(()) } else { Err("密码错误".into()) };
    }
    let entry = keyring::Entry::new(KEYRING_SERVICE, label).map_err(|e| log!("打开凭据管理器失败：{e}")).ok();
    if let Some(saved) = entry.as_ref().and_then(|e| e.get_password().ok()) {
        if try_password(h, &host.user, &saved).await? {
            log!("使用已保存的密码登录 {label}");
            return Ok(());
        }
        log!("已保存的密码无效，删除：{label}");
        if let Some(e) = &entry {
            let _ = e.delete_credential();
        }
    }
    let mut message = format!("请输入 {label} 的密码");
    for _ in 0..3 {
        let (password, save) = ask(state, PromptKind::Password, label, message, None).await?;
        if try_password(h, &host.user, &password).await? {
            if save {
                match entry.as_ref().map(|e| e.set_password(&password)) {
                    Some(Ok(())) => log!("密码已保存到凭据管理器：{label}"),
                    Some(Err(e)) => log!("保存密码失败：{e}"),
                    None => {}
                }
            }
            return Ok(());
        }
        message = format!("密码错误，请重新输入 {label} 的密码");
    }
    Err("密码错误次数过多".into())
}

/// 已保存密码的存取（按账户 `user@host:port`）；生产实现为 Windows 凭据管理器，测试用内存实现
pub trait CredStore {
    fn get(&self, account: &str) -> Option<String>;
    fn set(&self, account: &str, password: &str) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;
}

/// Windows 凭据管理器（服务名 KEYRING_SERVICE）
pub struct Keyring;

impl CredStore for Keyring {
    fn get(&self, account: &str) -> Option<String> {
        keyring::Entry::new(KEYRING_SERVICE, account).ok()?.get_password().ok()
    }
    fn set(&self, account: &str, password: &str) -> Result<(), String> {
        keyring::Entry::new(KEYRING_SERVICE, account).and_then(|e| e.set_password(password)).map_err(|e| e.to_string())
    }
    fn delete(&self, account: &str) -> Result<(), String> {
        match keyring::Entry::new(KEYRING_SERVICE, account).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// 有已保存密码的主机 id
pub fn password_saved(store: &impl CredStore, hosts: &[Host]) -> Vec<String> {
    hosts.iter().filter(|h| store.get(&label(h)).is_some()).map(|h| h.id.clone()).collect()
}

/// 连接测试未填写密码时用的已保存密码，与保存配置时的迁移规则一致：新账户已有密码则用它，否则用该主机原账户（old）的密码
pub fn saved_password(store: &impl CredStore, old: Option<&Host>, new: &Host) -> Option<String> {
    store.get(&label(new)).or_else(|| old.and_then(|o| store.get(&label(o))))
}

/// 设置（Some）/ 清除（None）主机的已保存密码
pub fn set_password(store: &impl CredStore, host: &Host, password: Option<&str>) -> Result<(), String> {
    let account = label(host);
    match password {
        Some(p) => store.set(&account, p).inspect(|_| log!("密码已保存到凭据管理器：{account}")),
        None => store.delete(&account).inspect(|_| log!("已清除保存的密码：{account}")),
    }
}

/// 保存配置后同步已保存密码（old / new 为保存前后的合并主机列表，按 id 对应）：
/// 主机账户（user@host:port）变化时把密码迁到新账户（新账户已有密码则不覆盖），主机删除时删除其密码；
/// 旧账户仍被其它主机使用时保留，不删
pub fn migrate_credentials(store: &impl CredStore, old: &[Host], new: &[Host]) {
    for o in old {
        let from = label(o);
        let to = new.iter().find(|n| n.id == o.id).map(label);
        if to.as_deref() == Some(from.as_str()) {
            continue;
        }
        let Some(password) = store.get(&from) else { continue };
        if let Some(to) = &to {
            if store.get(to).is_none() {
                if let Err(e) = store.set(to, &password) {
                    log!("迁移保存的密码失败（{from} → {to}）：{e}");
                    continue;
                }
                log!("保存的密码已迁移：{from} → {to}");
            }
        }
        if new.iter().any(|n| label(n) == from) {
            continue;
        }
        match store.delete(&from) {
            Ok(()) => log!("已删除保存的密码：{from}"),
            Err(e) => log!("删除保存的密码失败（{from}）：{e}"),
        }
    }
}

/// 尝试一次密码；服务器不接受 password 而只开了 keyboard-interactive 时，用同一密码回应其单个密码提示
async fn try_password(h: &mut client::Handle<Client>, user: &str, password: &str) -> Result<bool, String> {
    let remaining = match h.authenticate_password(user, password).await.map_err(|e| e.to_string())? {
        AuthResult::Success => return Ok(true),
        AuthResult::Failure { remaining_methods, .. } => remaining_methods,
    };
    if remaining.contains(&MethodKind::Password) || !remaining.contains(&MethodKind::KeyboardInteractive) {
        return Ok(false);
    }
    let kbd_err = |e: russh::Error| e.to_string();
    let mut resp = h.authenticate_keyboard_interactive_start(user, None::<String>).await.map_err(kbd_err)?;
    // 服务器可能先发不含提示的空请求；轮数设上限，避免对异常服务器无限应答
    for _ in 0..4 {
        let prompts = match resp {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => prompts,
        };
        let answers = match prompts.len() {
            0 => Vec::new(),
            1 => vec![password.to_string()],
            _ => return Ok(false),
        };
        resp = h.authenticate_keyboard_interactive_respond(answers).await.map_err(kbd_err)?;
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::HashMap};

    use proto::HostSource;

    use super::*;

    #[derive(Default)]
    struct Mem(RefCell<HashMap<String, String>>);

    impl CredStore for Mem {
        fn get(&self, account: &str) -> Option<String> {
            self.0.borrow().get(account).cloned()
        }
        fn set(&self, account: &str, password: &str) -> Result<(), String> {
            self.0.borrow_mut().insert(account.into(), password.into());
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<(), String> {
            self.0.borrow_mut().remove(account);
            Ok(())
        }
    }

    fn host(id: &str, user: &str, port: u16) -> Host {
        Host {
            id: id.into(),
            name: id.into(),
            host: "127.0.0.1".into(),
            port,
            user: user.into(),
            auth: AuthKind::Password,
            key_path: None,
            source: HostSource::Tether,
        }
    }

    fn store(pairs: &[(&str, &str)]) -> Mem {
        let m = Mem::default();
        for (a, p) in pairs {
            m.set(a, p).unwrap();
        }
        m
    }

    #[test]
    fn account_change_migrates() {
        let s = store(&[("test@127.0.0.1:2222", "pw")]);
        migrate_credentials(&s, &[host("h1", "test", 2222)], &[host("h1", "test", 2223)]);
        assert_eq!(s.get("test@127.0.0.1:2222"), None);
        assert_eq!(s.get("test@127.0.0.1:2223").as_deref(), Some("pw"));
    }

    #[test]
    fn unchanged_account_untouched() {
        let s = store(&[("test@127.0.0.1:2222", "pw")]);
        let mut renamed = host("h1", "test", 2222);
        renamed.name = "新名称".into();
        renamed.auth = AuthKind::Key;
        migrate_credentials(&s, &[host("h1", "test", 2222)], &[renamed]);
        assert_eq!(s.get("test@127.0.0.1:2222").as_deref(), Some("pw"));
    }

    #[test]
    fn deleted_host_deletes_password() {
        let s = store(&[("test@127.0.0.1:2222", "pw"), ("other@127.0.0.1:22", "x")]);
        migrate_credentials(&s, &[host("h1", "test", 2222), host("h2", "other", 22)], &[host("h2", "other", 22)]);
        assert_eq!(s.get("test@127.0.0.1:2222"), None);
        assert_eq!(s.get("other@127.0.0.1:22").as_deref(), Some("x"));
    }

    #[test]
    fn shared_account_kept() {
        // 两台主机同账户：删除 / 改走其中一台，另一台仍在用，旧账户的密码保留
        let s = store(&[("test@127.0.0.1:2222", "pw")]);
        let old = [host("h1", "test", 2222), host("h2", "test", 2222)];
        migrate_credentials(&s, &old, &[host("h1", "test", 2223), host("h2", "test", 2222)]);
        assert_eq!(s.get("test@127.0.0.1:2222").as_deref(), Some("pw"));
        assert_eq!(s.get("test@127.0.0.1:2223").as_deref(), Some("pw"));
        migrate_credentials(&s, &[host("h2", "test", 2222)], &[]);
        assert_eq!(s.get("test@127.0.0.1:2222"), None);
    }

    #[test]
    fn existing_target_not_overwritten() {
        let s = store(&[("a@127.0.0.1:22", "old"), ("b@127.0.0.1:22", "keep")]);
        migrate_credentials(&s, &[host("h1", "a", 22)], &[host("h1", "b", 22)]);
        assert_eq!(s.get("a@127.0.0.1:22"), None);
        assert_eq!(s.get("b@127.0.0.1:22").as_deref(), Some("keep"));
    }

    #[test]
    fn set_clear_and_status() {
        let s = Mem::default();
        let h = host("h1", "test", 2222);
        set_password(&s, &h, Some("pw")).unwrap();
        assert_eq!(password_saved(&s, &[h.clone(), host("h2", "x", 22)]), vec!["h1".to_string()]);
        set_password(&s, &h, None).unwrap();
        assert!(password_saved(&s, &[h]).is_empty());
    }

    #[test]
    fn test_uses_saved_password_like_migration() {
        // 账户未变：用其已保存密码；账户改了且新账户无密码：用原账户的（保存时会迁过去）；新账户已有密码：用新账户的
        let s = store(&[("test@127.0.0.1:2222", "old")]);
        let old = host("h1", "test", 2222);
        assert_eq!(saved_password(&s, Some(&old), &old).as_deref(), Some("old"));
        assert_eq!(saved_password(&s, Some(&old), &host("h1", "test", 2223)).as_deref(), Some("old"));
        s.set("test@127.0.0.1:2223", "new").unwrap();
        assert_eq!(saved_password(&s, Some(&old), &host("h1", "test", 2223)).as_deref(), Some("new"));
        // 新主机（无原配置）且账户无密码
        assert_eq!(saved_password(&s, None, &host("", "x", 22)), None);
    }

    #[test]
    fn tcp_errors_classified() {
        use std::io::{Error, ErrorKind};
        let msg = |k| tcp_error(&Error::from(k), "h", 22);
        assert!(msg(ErrorKind::ConnectionRefused).starts_with("连接被拒绝"));
        assert!(msg(ErrorKind::TimedOut).starts_with("连接超时"));
        assert!(msg(ErrorKind::HostUnreachable).starts_with("网络不可达"));
        assert!(msg(ErrorKind::PermissionDenied).starts_with("TCP 连接失败"));
    }

    fn shared() -> Shared {
        Arc::new(Mutex::new(session::State::default()))
    }

    async fn run_test(port: u16) -> Result<(), String> {
        let state = shared();
        let stage = Mutex::new("DNS 解析");
        let h = host("h1", "test", port);
        with_timeout(&state, &stage, test_connect(&state, &h, &label(&h), Some("pw"), &stage)).await
    }

    #[tokio::test]
    async fn test_refused() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let err = run_test(port).await.unwrap_err();
        assert!(err.starts_with("连接被拒绝"), "{err}");
    }

    #[tokio::test]
    async fn test_not_ssh_server() {
        // 对端不是 SSH 服务：回一行 HTTP 后关闭
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        });
        let err = run_test(port).await.unwrap_err();
        assert!(err.starts_with("SSH 握手失败"), "{err}");
    }
}
