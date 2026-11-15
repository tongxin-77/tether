//! daemon ↔ hub 协议 v2 的连接层（见 tether-hub 仓库 docs/protocol.md）：地址解析、分阶段建连（DNS → TCP → TLS → WebSocket 握手）、
//! hello / welcome、会话全量上报（500 ms 防抖）、心跳超时、指数退避重连与错误分类；远程接管的消息在这里收发，处理交给 daemon。
//!
//! 本文件只依赖外部 crate 与 `proto`，不引用 daemon 的其它模块（集成测试以 `#[path]` 直接引入本文件）；
//! 调用方需在引入前定义 `log!` 宏。

use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use proto::{HubState, Secret, SessionInfo};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::{Notify, mpsc},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{self, Message, client::IntoClientRequest, protocol::CloseFrame},
};

/// hub 协议版本
pub const HUB_PROTO: u32 = 2;
/// 连接路径固定为 /agent
const PATH: &str = "/agent";
/// 建连 + hello / welcome 的总超时（测试连接与常驻连接相同）
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 这么久没收到 hub 的任何帧（hub 每 25 s 发 ping）即判定断线
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// 会话列表变化后的防抖时间
const DEBOUNCE: Duration = Duration::from_millis(500);
/// 退避上限
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// 被同一 machineId 的新连接替换（replaced / 4004）时的原因；daemon 据此识别“测试连接”造成的临时替换
pub const REPLACED_MSG: &str = "另一台机器使用了相同的机器 ID（例如克隆的虚拟机），已停止重连";
/// hello 字段上限（超出 hub 按 bad_hello 处理）：machineId 1–64 字符，machineName / version 不超过 256 字符
pub const MACHINE_ID_MAX: usize = 64;
const NAME_MAX: usize = 256;
/// 本机支持的能力位（hello.caps）：双方都声明的才使用；proto 仍为 2，旧 hub 不回 caps 即视为空集
pub const CAPS: &[&str] = &["history", "image", "close"];

/// 截断到 max 个字符以内（按 UTF-16 码元计，与 hub 端 JavaScript 的字符串长度一致，不切断代理对）
pub fn truncate_chars(s: &str, max: usize) -> String {
    let mut units = 0;
    s.chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= max
        })
        .collect()
}

/// 构造常驻连接的 hello（probe 为 false）：machineName / version 截断到协议上限
pub fn hello(machine_id: String, machine_name: &str, version: &str) -> Up {
    Up::Hello {
        proto: HUB_PROTO,
        machine_id,
        machine_name: truncate_chars(machine_name, NAME_MAX),
        version: truncate_chars(version, NAME_MAX),
        probe: false,
        caps: CAPS.iter().map(|c| c.to_string()).collect(),
    }
}

// ---------------- 地址 ----------------

/// 规范化后的 hub 地址
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub tls: bool,
    /// 主机名（小写）或 IP；IPv6 不带方括号
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    /// 完整连接 URL，如 `wss://hub.example.com:443/agent`
    pub fn url(&self) -> String {
        let scheme = if self.tls { "wss" } else { "ws" };
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        format!("{scheme}://{host}:{}{PATH}", self.port)
    }
}

/// 解析设置中填写的服务器地址：`host`、`host:port`、`[v6]:port`，可带 `ws://` / `wss://` 前缀与结尾的 `/` 或 `/agent`。
/// 未写协议时默认 wss，勾选 allow_insecure 则默认 ws；未写端口 wss 用 443、ws 用 80；ws 需 allow_insecure。错误信息不带“地址无效”前缀
pub fn parse_address(input: &str, allow_insecure: bool) -> Result<Endpoint, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("未填写服务器地址".into());
    }
    let (tls, rest) = match s.find("://") {
        Some(i) => match s[..i].to_ascii_lowercase().as_str() {
            "wss" => (true, &s[i + 3..]),
            "ws" => (false, &s[i + 3..]),
            other => return Err(format!("不支持的协议 {other}://，只能是 wss:// 或 ws://")),
        },
        None => (!allow_insecure, s),
    };
    if !tls && !allow_insecure {
        return Err("ws:// 为明文连接，需勾选「允许明文连接（仅限局域网调试）」".into());
    }
    if rest.contains(['?', '#', '@']) || rest.chars().any(char::is_whitespace) {
        return Err("地址只能包含主机和端口".into());
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if !matches!(path.trim_end_matches('/'), "" | PATH) {
        return Err(format!("地址不能包含路径（连接路径固定为 {PATH}）"));
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, after) = v6.split_once(']').ok_or("IPv6 地址缺少右方括号")?;
        if h.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(format!("{h} 不是合法的 IPv6 地址"));
        }
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':').ok_or("IPv6 地址后只能跟 :端口")?),
        };
        (h.to_ascii_lowercase(), port)
    } else {
        match authority.split_once(':') {
            Some((_, p)) if p.contains(':') => return Err("IPv6 地址需用方括号括起，如 [fe80::1]:8443".into()),
            Some((h, p)) => (h.to_ascii_lowercase(), Some(p)),
            None => (authority.to_ascii_lowercase(), None),
        }
    };
    if host.is_empty() {
        return Err("缺少主机名".into());
    }
    if !host.contains(':') && !host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
        return Err(format!("主机名 {host} 含非法字符"));
    }
    let port = match port {
        None => if tls { 443 } else { 80 },
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 => n,
            _ => return Err(format!("端口 {p} 无效，应为 1–65535")),
        },
    };
    Ok(Endpoint { tls, host, port })
}

// ---------------- 退避 ----------------

/// 第 attempt 次（0 起）重连前的基础等待：1 s、2 s、4 s … 上限 60 s
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << attempt.min(6)).min(BACKOFF_MAX)
}

/// 加 ±20% 抖动；r 为 [0, 1) 的随机数
pub fn jitter(base: Duration, r: f64) -> Duration {
    base.mul_f64(0.8 + 0.4 * r.clamp(0.0, 1.0))
}

/// [0, 1) 的随机数：用标准库 RandomState 的随机种子，不额外引入随机数依赖
fn random() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    (h.finish() >> 11) as f64 / (1u64 << 53) as f64
}

// ---------------- 协议消息 ----------------

/// daemon → hub
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Up {
    Hello {
        proto: u32,
        machine_id: String,
        machine_name: String,
        version: String,
        /// 测试连接的探测连接：hub 照常回 welcome，但不登记、不替换同一 machineId 的常驻连接；常驻连接为 false（不写出）
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        probe: bool,
        /// 能力位（缺省为空集）
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        caps: Vec<String>,
    },
    Sessions { sessions: Vec<SessionInfo> },
    /// 接管成功：当前尺寸 + 屏幕快照（base64），之后该会话的输出以 Output 推送；
    /// 双方都有 history 能力时另带历史（base64，浏览器先写 history 再写 snapshot）
    Attached {
        session_id: u32,
        cols: u16,
        rows: u16,
        snapshot: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        history: Option<String>,
    },
    /// 拒绝接管：disabled / not_found / probe
    AttachDenied { session_id: u32, reason: String },
    /// PTY 原始输出（base64）
    Output { session_id: u32, data: String },
    /// 本机结束接管：reclaimed / disabled / exited
    Detached { session_id: u32, reason: String },
    /// 网页贴图已保存（能力 image），path 已作为输入写入会话
    ImageSaved { session_id: u32, upload_id: String, path: String },
    /// 网页贴图失败：disabled / not_taken_over / unsupported / too_large / bad_type / io
    ImageFailed { session_id: u32, upload_id: String, reason: String },
    /// 网页关闭会话的结果（能力 close）；失败时 reason 为 disabled / not_found
    CloseResult {
        session_id: u32,
        request_id: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// hub → daemon；未知 type 忽略
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Down {
    Welcome {
        proto: u32,
        hub_version: String,
        /// 能力位（旧 hub 不带，缺省为空集）
        #[serde(default)]
        caps: Vec<String>,
    },
    Error {
        code: String,
        #[serde(default)]
        message: String,
    },
    /// 远程接管（见协议第 6 节）；input.data 为 base64
    Attach { session_id: u32 },
    Input { session_id: u32, data: String },
    Resize { session_id: u32, cols: u16, rows: u16 },
    Detach { session_id: u32 },
    /// 网页贴图（能力 image）：begin 声明类型与总字节数，chunk 为 base64 分块，end 后 daemon 回 imageSaved / imageFailed
    ImageBegin { session_id: u32, upload_id: String, mime: String, size: u64 },
    ImageChunk { session_id: u32, upload_id: String, data: String },
    ImageEnd { session_id: u32, upload_id: String },
    /// 网页关闭会话（能力 close），不要求正被接管；daemon 回 closeResult
    Close { session_id: u32, request_id: String },
    #[serde(other)]
    Unknown,
}

// ---------------- 错误分类 ----------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 地址无效
    Address,
    /// 未保存 token
    NoToken,
    Dns,
    Tcp,
    Tls,
    /// HTTP 401
    Auth,
    /// 握手被拒（非 401 的 HTTP 状态）或对端不是 WebSocket 服务
    Handshake,
    /// proto 不兼容
    Proto,
    /// 同一 machineId 有新连接，本连接被替换
    Replaced,
    /// hub 报告的其它错误（revoked / bad_hello / 未知 code）
    Hub,
    /// 超时（建连 / 等 welcome / 心跳）
    Timeout,
    /// 连接中途断开
    Closed,
}

/// 一次连接失败：分类 + 给用户看的原因
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub kind: Kind,
    pub msg: String,
}

impl Failure {
    fn new(kind: Kind, msg: impl Into<String>) -> Self {
        Self { kind, msg: msg.into() }
    }

    /// 不自动重连、停在 failed 等用户改设置：地址无效、无 token、401、协议不兼容、被同一机器 ID 的连接替换
    pub fn fatal(&self) -> bool {
        matches!(self.kind, Kind::Address | Kind::NoToken | Kind::Auth | Kind::Proto | Kind::Replaced)
    }
}

/// hub error 消息的 code → 失败
pub fn classify_code(code: &str, message: &str) -> Failure {
    let detail = if message.is_empty() { String::new() } else { format!("（{message}）") };
    match code {
        "proto_unsupported" => Failure::new(Kind::Proto, format!("协议不兼容：hub 不支持协议 {HUB_PROTO}{detail}")),
        "revoked" => Failure::new(Kind::Hub, format!("token 已被 hub 吊销{detail}")),
        "replaced" => Failure::new(Kind::Replaced, REPLACED_MSG),
        "bad_hello" => Failure::new(Kind::Hub, format!("hub 拒绝了 hello{detail}")),
        other => Failure::new(Kind::Hub, format!("hub 返回错误 {other}{detail}")),
    }
}

/// 未先收到 error 消息时按 WebSocket 关闭码分类
pub fn classify_close(frame: Option<&CloseFrame>) -> Failure {
    let Some(f) = frame else { return Failure::new(Kind::Closed, "hub 关闭了连接") };
    let code = u16::from(f.code);
    match code {
        4001 => classify_code("revoked", &f.reason),
        4002 => classify_code("proto_unsupported", &f.reason),
        4003 => classify_code("bad_hello", &f.reason),
        4004 => classify_code("replaced", &f.reason),
        _ if f.reason.is_empty() => Failure::new(Kind::Closed, format!("hub 关闭了连接（关闭码 {code}）")),
        _ => Failure::new(Kind::Closed, format!("hub 关闭了连接（关闭码 {code}：{}）", f.reason)),
    }
}

/// WebSocket 握手错误分类：401 为认证失败
pub fn classify_handshake(e: &tungstenite::Error) -> Failure {
    match e {
        tungstenite::Error::Http(resp) if resp.status().as_u16() == 401 => {
            // hub 对错误与已吊销的 token 都回 401，无法区分
            Failure::new(Kind::Auth, "认证失败：token 无效或已吊销（HTTP 401）")
        }
        tungstenite::Error::Http(resp) => Failure::new(
            Kind::Handshake,
            format!("WebSocket 握手被拒绝：HTTP {}（地址或端口可能指向了其它服务）", resp.status()),
        ),
        tungstenite::Error::Io(e) => Failure::new(Kind::Closed, format!("WebSocket 握手时连接断开（{e}）")),
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
            Failure::new(Kind::Closed, "WebSocket 握手时连接断开")
        }
        e => Failure::new(Kind::Handshake, format!("WebSocket 握手失败，对端可能不是 Tether hub（{e}）")),
    }
}

/// TCP 连接错误分类
fn tcp_failure(e: &std::io::Error, ep: &Endpoint) -> Failure {
    use std::io::ErrorKind::*;
    let at = format!("{}:{}", ep.host, ep.port);
    let msg = match e.kind() {
        ConnectionRefused => format!("TCP 连接被拒绝：{at} 没有在监听（{e}）"),
        TimedOut => format!("TCP 连接超时：{at} 无响应（{e}）"),
        HostUnreachable | NetworkUnreachable => format!("TCP 连接失败：网络不可达 {at}（{e}）"),
        _ => format!("TCP 连接失败：{at}（{e}）"),
    };
    Failure::new(Kind::Tcp, msg)
}

/// TLS 握手错误分类：证书问题单独说明
fn tls_failure(e: &std::io::Error) -> Failure {
    use rustls::{CertificateError, Error as TlsError};
    let detail = e.get_ref().and_then(|inner| inner.downcast_ref::<TlsError>());
    let msg = match detail {
        Some(TlsError::InvalidCertificate(CertificateError::UnknownIssuer)) => {
            "TLS 失败：证书不受信任（签发者不在 Windows 受信任根证书中；自签证书需先导入证书库）".to_string()
        }
        Some(TlsError::InvalidCertificate(CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. })) => {
            "TLS 失败：证书与服务器地址不匹配".to_string()
        }
        Some(TlsError::InvalidCertificate(CertificateError::Expired | CertificateError::ExpiredContext { .. })) => {
            "TLS 失败：证书已过期".to_string()
        }
        Some(TlsError::InvalidCertificate(c)) => format!("TLS 失败：证书无效（{c:?}）"),
        Some(other) => format!("TLS 失败：{other}"),
        None => format!("TLS 失败：{e}（对端可能未启用 TLS，明文 hub 需写 ws:// 并勾选允许明文）"),
    };
    Failure::new(Kind::Tls, msg)
}

// ---------------- 建连 ----------------

/// 明文 TCP 或 TLS 上的字节流
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub type Ws = WebSocketStream<Box<dyn Io>>;

/// 当前所处阶段，超时时据此说明卡在哪一步
pub type Stage = Mutex<&'static str>;

fn set_stage(stage: &Stage, s: &'static str) {
    if let Ok(mut g) = stage.lock() {
        *g = s;
    }
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, Failure> {
    let mut roots = rustls::RootCertStore::empty();
    let found = rustls_native_certs::load_native_certs();
    let (added, _) = roots.add_parsable_certificates(found.certs);
    if added == 0 {
        return Err(Failure::new(Kind::Tls, "TLS 失败：无法读取 Windows 证书库中的受信任根证书"));
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Failure::new(Kind::Tls, format!("TLS 初始化失败：{e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// DNS → TCP → （TLS）→ WebSocket 握手（带 Authorization: Bearer）
pub async fn connect(ep: &Endpoint, token: &Secret, stage: &Stage) -> Result<Ws, Failure> {
    set_stage(stage, "DNS 解析");
    let addrs: Vec<_> = tokio::net::lookup_host((ep.host.as_str(), ep.port))
        .await
        .map_err(|e| Failure::new(Kind::Dns, format!("DNS 解析失败：无法解析 {}（{e}）", ep.host)))?
        .collect();
    set_stage(stage, "TCP 连接");
    let tcp = TcpStream::connect(&addrs[..]).await.map_err(|e| tcp_failure(&e, ep))?;
    let _ = tcp.set_nodelay(true);
    let io: Box<dyn Io> = if ep.tls {
        set_stage(stage, "TLS 握手");
        let name = rustls::pki_types::ServerName::try_from(ep.host.clone())
            .map_err(|_| Failure::new(Kind::Address, format!("地址无效：{} 不能用作 TLS 服务器名", ep.host)))?;
        let tls = tokio_rustls::TlsConnector::from(tls_config()?).connect(name, tcp).await.map_err(|e| tls_failure(&e))?;
        Box::new(tls)
    } else {
        Box::new(tcp)
    };
    set_stage(stage, "WebSocket 握手");
    let mut req = ep.url().into_client_request().map_err(|e| Failure::new(Kind::Address, format!("地址无效：{e}")))?;
    let auth = format!("Bearer {}", token.0)
        .parse()
        .map_err(|_| Failure::new(Kind::NoToken, "token 含非法字符（只能是可见 ASCII 字符）"))?;
    req.headers_mut().insert("Authorization", auth);
    let (ws, _) = tokio_tungstenite::client_async(req, io).await.map_err(|e| classify_handshake(&e))?;
    Ok(ws)
}

async fn send_up(ws: &mut Ws, msg: &Up) -> Result<(), Failure> {
    let json = serde_json::to_string(msg).map_err(|e| Failure::new(Kind::Closed, format!("序列化失败：{e}")))?;
    ws.send(Message::Text(json.into())).await.map_err(|e| Failure::new(Kind::Closed, format!("发送失败，连接已断开（{e}）")))
}

/// 读下一条 hub 消息：文本帧解析为 Down（无法解析的忽略），ping / pong 等其它帧返回 None
fn read_frame(item: Option<Result<Message, tungstenite::Error>>) -> Result<Option<Down>, Failure> {
    match item {
        None => Err(Failure::new(Kind::Closed, "hub 关闭了连接")),
        Some(Err(e)) => Err(Failure::new(Kind::Closed, format!("连接断开（{e}）"))),
        Some(Ok(Message::Close(frame))) => Err(classify_close(frame.as_ref())),
        Some(Ok(Message::Text(text))) => Ok(serde_json::from_str::<Down>(&text).ok()),
        Some(Ok(_)) => Ok(None),
    }
}

/// 发 hello，等 welcome；返回 hubVersion 与 hub 声明的能力位
pub async fn handshake(ws: &mut Ws, hello: &Up, stage: &Stage) -> Result<(String, Vec<String>), Failure> {
    set_stage(stage, "等待 welcome");
    send_up(ws, hello).await?;
    loop {
        match read_frame(ws.next().await)? {
            Some(Down::Welcome { proto, hub_version, caps }) if proto == HUB_PROTO => return Ok((hub_version, caps)),
            Some(Down::Welcome { proto, .. }) => {
                return Err(Failure::new(Kind::Proto, format!("协议不兼容：hub 使用协议 {proto}，本机为 {HUB_PROTO}")));
            }
            Some(Down::Error { code, message }) => return Err(classify_code(&code, &message)),
            // welcome 前的接管消息不合协议，忽略；测试连接收到 welcome 即关闭，不会处理 attach
            _ => {}
        }
    }
}

/// 给 fut 加总超时，超时说明卡在哪个阶段
pub async fn with_timeout<T>(limit: Duration, stage: &Stage, fut: impl Future<Output = Result<T, Failure>>) -> Result<T, Failure> {
    match tokio::time::timeout(limit, fut).await {
        Ok(r) => r,
        Err(_) => {
            let s = stage.lock().map(|g| *g).unwrap_or("连接");
            let msg = if s == "等待 welcome" {
                format!("超时：{} 秒内未收到 hub 的 welcome", limit.as_secs())
            } else {
                format!("连接超时：{s}阶段 {} 秒内未完成", limit.as_secs())
            };
            Err(Failure::new(Kind::Timeout, msg))
        }
    }
}

/// 测试连接：新建独立连接，握手 → hello（probe: true）→ welcome 后立即关闭（不发 sessions）；返回 hubVersion
pub async fn probe(ep: &Endpoint, token: &Secret, hello: &Up) -> Result<String, Failure> {
    let mut hello = hello.clone();
    if let Up::Hello { probe, .. } = &mut hello {
        *probe = true;
    }
    let hello = &hello;
    let stage = Mutex::new("DNS 解析");
    with_timeout(CONNECT_TIMEOUT, &stage, async {
        let mut ws = connect(ep, token, &stage).await?;
        let version = handshake(&mut ws, hello, &stage).await;
        let _ = ws.close(None).await;
        version.map(|(version, _)| version)
    })
    .await
}

// ---------------- 常驻连接 ----------------

/// 可用连接交给 daemon 的部分：上行发送端 + hub 声明的能力位
pub type Conn = (mpsc::UnboundedSender<Up>, Vec<String>);

/// 常驻连接所需的一切（由 daemon 从配置 / 凭据管理器组装）
pub struct Link {
    pub endpoint: Endpoint,
    pub token: Secret,
    pub hello: Up,
    /// 当前会话列表快照
    pub sessions: Arc<dyn Fn() -> Vec<SessionInfo> + Send + Sync>,
    /// 会话列表或字段可能变化时被通知
    pub changed: Arc<Notify>,
    /// 状态变化回调
    pub status: Arc<dyn Fn(HubState, Option<String>) + Send + Sync>,
    /// 连接可用（收到 welcome，附上行发送端与 hub 声明的能力位，daemon 经发送端推送接管消息）/ 断开（None，daemon 据此结束全部接管）
    pub conn: Arc<dyn Fn(Option<Conn>) + Send + Sync>,
    /// 接管下行消息（attach / input / resize / detach / image*）交给 daemon 处理
    pub down: Arc<dyn Fn(Down) + Send + Sync>,
}

/// 常驻连接主循环：连接 → hello / welcome → 上报；断线按退避重连，致命错误停在 failed 后返回。
/// 被 abort 即断开（WebSocket 随任务一起丢弃）
pub async fn run(link: Link) {
    let mut attempt = 0u32;
    let mut last: Option<String> = None;
    loop {
        (link.status)(HubState::Connecting, last.clone());
        let stage = Mutex::new("DNS 解析");
        let connected = with_timeout(CONNECT_TIMEOUT, &stage, async {
            let mut ws = connect(&link.endpoint, &link.token, &stage).await?;
            let (version, caps) = handshake(&mut ws, &link.hello, &stage).await?;
            Ok((ws, version, caps))
        })
        .await;
        let failure = match connected {
            Ok((ws, version, caps)) => {
                log!("hub 已连接 {}（hub 版本 {version}）", link.endpoint.url());
                attempt = 0;
                last = None;
                (link.status)(HubState::Connected, None);
                let failure = serve(ws, &link, caps).await;
                (link.conn)(None);
                failure
            }
            Err(f) => f,
        };
        if failure.fatal() {
            log!("hub 连接失败，不再自动重连：{}", failure.msg);
            (link.status)(HubState::Failed, Some(failure.msg));
            return;
        }
        let wait = jitter(backoff(attempt), random());
        attempt = attempt.saturating_add(1);
        // hub 长时间不可用时每次重连都失败，原因不变则不重复记日志
        if last.as_deref() != Some(failure.msg.as_str()) {
            log!("hub 连接断开 / 失败：{}；{} ms 后重连", failure.msg, wait.as_millis());
        }
        last = Some(failure.msg);
        (link.status)(HubState::Connecting, last.clone());
        tokio::time::sleep(wait).await;
    }
}

/// 已收到 welcome：立即上报一次全量，之后会话变化防抖上报；转发接管消息；60 s 无任何帧判定断线。返回断开原因。
/// 接管上行（快照 / 输出）走无界通道：daemon 在 PTY 读线程中持锁推送，不能等待网络
async fn serve(mut ws: Ws, link: &Link, caps: Vec<String>) -> Failure {
    let mut sent = String::new();
    if let Err(f) = report(&mut ws, link, &mut sent).await {
        return f;
    }
    let (tx, mut rx) = mpsc::unbounded_channel();
    (link.conn)(Some((tx, caps)));
    // hub 的 ping 由 tungstenite 自动回 pong：收到 ping 后排队，下一次 read（本循环始终在等 ws.next()）先写出并 flush
    let idle = tokio::time::sleep(IDLE_TIMEOUT);
    tokio::pin!(idle);
    // 防抖：首次变化后 DEBOUNCE 到期再发，期间的变化合并
    let mut due: Option<tokio::time::Instant> = None;
    loop {
        tokio::select! {
            item = ws.next() => {
                idle.as_mut().reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
                match read_frame(item) {
                    Ok(Some(Down::Error { code, message })) => {
                        let f = classify_code(&code, &message);
                        let _ = ws.close(None).await;
                        return f;
                    }
                    Ok(Some(
                        msg @ (Down::Attach { .. }
                        | Down::Input { .. }
                        | Down::Resize { .. }
                        | Down::Detach { .. }
                        | Down::ImageBegin { .. }
                        | Down::ImageChunk { .. }
                        | Down::ImageEnd { .. }
                        | Down::Close { .. }),
                    )) => (link.down)(msg),
                    Ok(_) => {}
                    Err(f) => return f,
                }
            }
            Some(msg) = rx.recv() => {
                if let Err(f) = send_up(&mut ws, &msg).await {
                    return f;
                }
            }
            _ = &mut idle => {
                return Failure::new(Kind::Timeout, format!("{} 秒内未收到 hub 的任何数据，判定断线", IDLE_TIMEOUT.as_secs()));
            }
            _ = link.changed.notified(), if due.is_none() => {
                due = Some(tokio::time::Instant::now() + DEBOUNCE);
            }
            _ = tokio::time::sleep_until(due.unwrap_or_else(tokio::time::Instant::now)), if due.is_some() => {
                due = None;
                if let Err(f) = report(&mut ws, link, &mut sent).await {
                    return f;
                }
            }
        }
    }
}

/// 发送会话全量；与上次发送的内容相同则跳过
async fn report(ws: &mut Ws, link: &Link, sent: &mut String) -> Result<(), Failure> {
    let msg = Up::Sessions { sessions: (link.sessions)() };
    let json = serde_json::to_string(&msg).unwrap_or_default();
    if json == *sent {
        return Ok(());
    }
    send_up(ws, &msg).await?;
    *sent = json;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(tls: bool, host: &str, port: u16) -> Endpoint {
        Endpoint { tls, host: host.into(), port }
    }

    #[test]
    fn address_defaults() {
        assert_eq!(parse_address("192.168.1.10:8443", false), Ok(ep(true, "192.168.1.10", 8443)));
        assert_eq!(parse_address("  Hub.Example.com ", false), Ok(ep(true, "hub.example.com", 443)));
        assert_eq!(parse_address("wss://hub.example.com:8443", false), Ok(ep(true, "hub.example.com", 8443)));
        assert_eq!(parse_address("WSS://hub.example.com/", false), Ok(ep(true, "hub.example.com", 443)));
        assert_eq!(parse_address("wss://hub.example.com/agent", false), Ok(ep(true, "hub.example.com", 443)));
        assert_eq!(parse_address("ws://10.0.0.2", true), Ok(ep(false, "10.0.0.2", 80)));
        assert_eq!(parse_address("ws://10.0.0.2:9000/agent/", true), Ok(ep(false, "10.0.0.2", 9000)));
        assert_eq!(parse_address("[::1]:8443", false), Ok(ep(true, "::1", 8443)));
        assert_eq!(parse_address("wss://[FE80::1]", false), Ok(ep(true, "fe80::1", 443)));
        // 勾选允许明文时，未写协议默认 ws；显式 wss 仍走 TLS
        assert_eq!(parse_address("127.0.0.1:8443", true), Ok(ep(false, "127.0.0.1", 8443)));
        assert_eq!(parse_address("hub.example.com", true), Ok(ep(false, "hub.example.com", 80)));
        assert_eq!(parse_address("wss://hub.example.com", true), Ok(ep(true, "hub.example.com", 443)));
    }

    #[test]
    fn address_url() {
        assert_eq!(ep(true, "hub.example.com", 443).url(), "wss://hub.example.com:443/agent");
        assert_eq!(ep(false, "10.0.0.2", 80).url(), "ws://10.0.0.2:80/agent");
        assert_eq!(ep(true, "::1", 8443).url(), "wss://[::1]:8443/agent");
    }

    #[test]
    fn address_errors() {
        let err = |s: &str, insecure: bool| parse_address(s, insecure).unwrap_err();
        assert!(err("", false).contains("未填写"));
        assert!(err("ws://10.0.0.2", false).contains("允许明文"));
        assert!(err("https://h", false).contains("不支持的协议"));
        assert!(err("h:0", false).contains("端口"));
        assert!(err("h:70000", false).contains("端口"));
        assert!(err("h:abc", false).contains("端口"));
        assert!(err("h/other", false).contains("路径"));
        assert!(err("user@h", false).contains("只能包含"));
        assert!(err("h?x=1", false).contains("只能包含"));
        assert!(err("fe80::1", false).contains("方括号"));
        assert!(err("[fe80::1", false).contains("右方括号"));
        assert!(err("[zz]:1", false).contains("IPv6"));
        assert!(err("wss://:8443", false).contains("主机名"));
        assert!(err("hub example", false).contains("只能包含"));
        assert!(err("hub!.com", false).contains("非法字符"));
    }

    #[test]
    fn backoff_sequence() {
        let secs: Vec<u64> = (0..9).map(|i| backoff(i).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(backoff(u32::MAX), BACKOFF_MAX);
        let base = Duration::from_secs(10);
        assert_eq!(jitter(base, 0.0), Duration::from_secs(8));
        assert_eq!(jitter(base, 0.5), Duration::from_secs(10));
        assert!(jitter(base, 0.999_999) < Duration::from_secs(12));
        for _ in 0..100 {
            let r = random();
            assert!((0.0..1.0).contains(&r));
        }
    }

    #[test]
    fn up_wire_format() {
        let probe = Up::Hello { proto: 2, machine_id: "m".into(), machine_name: "n".into(), version: "v".into(), probe: true, caps: vec![] };
        let v = serde_json::to_value(&probe).unwrap();
        assert_eq!(v["probe"], true);
        // 空能力集不写出（与旧版 hello 相同）
        assert!(v.get("caps").is_none());
        let hello = hello("9f1c".into(), "DEV-PC", "0.4.0");
        assert_eq!(
            serde_json::to_string(&hello).unwrap(),
            r#"{"type":"hello","proto":2,"machineId":"9f1c","machineName":"DEV-PC","version":"0.4.0","caps":["history","image","close"]}"#
        );
        // 旧版 hello（无 caps）解析为空集
        let old: Up = serde_json::from_str(r#"{"type":"hello","proto":2,"machineId":"m","machineName":"n","version":"v"}"#).unwrap();
        assert!(matches!(old, Up::Hello { caps, .. } if caps.is_empty()));
        let info = SessionInfo {
            id: 1,
            name: "n".into(),
            cwd: "C:\\x".into(),
            command: "pwsh".into(),
            status: proto::Status::Running,
            started_at: 1,
            last_activity_at: 2,
            host: None,
            host_id: None,
            claude: None,
            taken_over: false,
        };
        let v = serde_json::to_value(Up::Sessions { sessions: vec![info] }).unwrap();
        assert_eq!(v["type"], "sessions");
        assert_eq!(v["sessions"][0]["lastActivityAt"], 2);
        assert_eq!(v["sessions"][0]["status"], "running");
    }

    #[test]
    fn hello_fields_truncated() {
        let long = "名".repeat(300);
        let Up::Hello { machine_name, version, proto, .. } = hello("id".into(), &long, "0.4.0") else { panic!() };
        assert_eq!((machine_name.chars().count(), version.as_str(), proto), (256, "0.4.0", 2));
        // 非 BMP 字符占 2 个 UTF-16 码元，不切断
        assert_eq!(truncate_chars(&"😀".repeat(200), 256).chars().count(), 128);
        assert_eq!(truncate_chars("DEV-PC", 256), "DEV-PC");
        let v = serde_json::to_value(hello("id".into(), "PC", "1")).unwrap();
        assert!(v["proto"].is_u64());
    }

    #[test]
    fn down_wire_format() {
        // 旧 hub 的 welcome 无 caps：空集；未知能力名照常解析（使用时按交集忽略）
        let w: Down = serde_json::from_str(r#"{"type":"welcome","proto":2,"hubVersion":"0.1.0"}"#).unwrap();
        assert_eq!(w, Down::Welcome { proto: 2, hub_version: "0.1.0".into(), caps: vec![] });
        let w: Down = serde_json::from_str(r#"{"type":"welcome","proto":2,"hubVersion":"0.4.0","caps":["history","image","future"]}"#).unwrap();
        assert_eq!(w, Down::Welcome { proto: 2, hub_version: "0.4.0".into(), caps: vec!["history".into(), "image".into(), "future".into()] });
        let e: Down = serde_json::from_str(r#"{"type":"error","code":"proto_unsupported","message":"hub 仅支持协议 1"}"#).unwrap();
        assert_eq!(e, Down::Error { code: "proto_unsupported".into(), message: "hub 仅支持协议 1".into() });
        let u: Down = serde_json::from_str(r#"{"type":"takeover","session":1}"#).unwrap();
        assert_eq!(u, Down::Unknown);
        let a: Down = serde_json::from_str(r#"{"type":"attach","sessionId":3}"#).unwrap();
        assert_eq!(a, Down::Attach { session_id: 3 });
        let i: Down = serde_json::from_str(r#"{"type":"input","sessionId":3,"data":"bHMK"}"#).unwrap();
        assert_eq!(i, Down::Input { session_id: 3, data: "bHMK".into() });
        let r: Down = serde_json::from_str(r#"{"type":"resize","sessionId":3,"cols":120,"rows":30}"#).unwrap();
        assert_eq!(r, Down::Resize { session_id: 3, cols: 120, rows: 30 });
        let d: Down = serde_json::from_str(r#"{"type":"detach","sessionId":3}"#).unwrap();
        assert_eq!(d, Down::Detach { session_id: 3 });
        let up = Up::Attached { session_id: 3, cols: 120, rows: 30, snapshot: "eA==".into(), history: None };
        assert_eq!(serde_json::to_string(&up).unwrap(), r#"{"type":"attached","sessionId":3,"cols":120,"rows":30,"snapshot":"eA=="}"#);
        let up = Up::Attached { session_id: 3, cols: 120, rows: 30, snapshot: "eA==".into(), history: Some("eQ==".into()) };
        assert_eq!(
            serde_json::to_string(&up).unwrap(),
            r#"{"type":"attached","sessionId":3,"cols":120,"rows":30,"snapshot":"eA==","history":"eQ=="}"#
        );
        let up = Up::AttachDenied { session_id: 3, reason: "not_found".into() };
        assert_eq!(serde_json::to_string(&up).unwrap(), r#"{"type":"attachDenied","sessionId":3,"reason":"not_found"}"#);
    }

    #[test]
    fn image_wire_format() {
        let b: Down = serde_json::from_str(r#"{"type":"imageBegin","sessionId":3,"uploadId":"u1","mime":"image/png","size":834211}"#).unwrap();
        assert_eq!(b, Down::ImageBegin { session_id: 3, upload_id: "u1".into(), mime: "image/png".into(), size: 834211 });
        let c: Down = serde_json::from_str(r#"{"type":"imageChunk","sessionId":3,"uploadId":"u1","data":"iVBO"}"#).unwrap();
        assert_eq!(c, Down::ImageChunk { session_id: 3, upload_id: "u1".into(), data: "iVBO".into() });
        let e: Down = serde_json::from_str(r#"{"type":"imageEnd","sessionId":3,"uploadId":"u1"}"#).unwrap();
        assert_eq!(e, Down::ImageEnd { session_id: 3, upload_id: "u1".into() });
        let up = Up::ImageSaved { session_id: 3, upload_id: "u1".into(), path: r"C:\t\a.png".into() };
        assert_eq!(serde_json::to_string(&up).unwrap(), r#"{"type":"imageSaved","sessionId":3,"uploadId":"u1","path":"C:\\t\\a.png"}"#);
        let up = Up::ImageFailed { session_id: 3, upload_id: "u1".into(), reason: "bad_type".into() };
        assert_eq!(serde_json::to_string(&up).unwrap(), r#"{"type":"imageFailed","sessionId":3,"uploadId":"u1","reason":"bad_type"}"#);
    }

    #[test]
    fn close_wire_format() {
        let c: Down = serde_json::from_str(r#"{"type":"close","sessionId":3,"requestId":"r1"}"#).unwrap();
        assert_eq!(c, Down::Close { session_id: 3, request_id: "r1".into() });
        let up = Up::CloseResult { session_id: 3, request_id: "r1".into(), ok: true, reason: None };
        assert_eq!(serde_json::to_string(&up).unwrap(), r#"{"type":"closeResult","sessionId":3,"requestId":"r1","ok":true}"#);
        let up = Up::CloseResult { session_id: 3, request_id: "r1".into(), ok: false, reason: Some("disabled".into()) };
        assert_eq!(
            serde_json::to_string(&up).unwrap(),
            r#"{"type":"closeResult","sessionId":3,"requestId":"r1","ok":false,"reason":"disabled"}"#
        );
    }

    #[test]
    fn error_classification() {
        assert_eq!(classify_code("proto_unsupported", "").kind, Kind::Proto);
        assert_eq!(classify_code("revoked", "").kind, Kind::Hub);
        assert_eq!(classify_code("replaced", "").kind, Kind::Replaced);
        assert_eq!(classify_code("bad_hello", "x").kind, Kind::Hub);
        assert_eq!(classify_code("whatever", "").kind, Kind::Hub);
        assert!(classify_code("proto_unsupported", "").fatal());
        assert!(classify_code("replaced", "").fatal());
        assert_eq!(classify_code("replaced", "x").msg, REPLACED_MSG);
        assert!(!classify_code("revoked", "").fatal());
        assert!(!classify_code("bad_hello", "").fatal());

        let close = |code: u16| classify_close(Some(&CloseFrame { code: code.into(), reason: "".into() }));
        assert_eq!(close(4001).kind, Kind::Hub);
        assert_eq!(close(4002).kind, Kind::Proto);
        assert_eq!(close(4003).kind, Kind::Hub);
        assert_eq!(close(4004).kind, Kind::Replaced);
        assert_eq!(close(1001).kind, Kind::Closed);
        assert_eq!(classify_close(None).kind, Kind::Closed);

        let http = |status: u16| {
            let resp = tungstenite::http::Response::builder().status(status).body(None).unwrap();
            classify_handshake(&tungstenite::Error::Http(Box::new(resp)))
        };
        assert_eq!(http(401).kind, Kind::Auth);
        assert!(http(401).fatal());
        assert_eq!(http(404).kind, Kind::Handshake);
        assert!(!http(502).fatal());

        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        let f = tcp_failure(&refused, &ep(true, "h", 1));
        assert_eq!(f.kind, Kind::Tcp);
        assert!(f.msg.contains("拒绝") && f.msg.contains("h:1"));
        let bad_cert = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer),
        );
        let f = tls_failure(&bad_cert);
        assert_eq!(f.kind, Kind::Tls);
        assert!(f.msg.contains("不受信任"), "{}", f.msg);
        assert!(!f.fatal());
    }
}
