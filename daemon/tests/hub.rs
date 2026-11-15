//! hub 客户端连接层集成测试：本地起明文 ws 的 mock hub（不依赖外部网络），验证 hello → welcome → sessions 上报、
//! 会话变化防抖上报、401 分类为致命错误、断线后自动重连、接管消息的收发。接管的 daemon 侧逻辑见 src/hub.rs 的单元测试。

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use proto::{HubState, Secret, SessionInfo, Status};
use serde_json::Value;
use tokio::{
    net::TcpListener,
    sync::{Notify, mpsc},
};
use tokio_tungstenite::tungstenite::{
    Message,
    handshake::server::{ErrorResponse, Request, Response},
    protocol::{CloseFrame, frame::coding::CloseCode},
    http::StatusCode,
};

macro_rules! log {
    ($($t:tt)*) => {{ let _ = format!($($t)*); }};
}

#[allow(dead_code)]
#[path = "../src/hub/link.rs"]
mod link;

const TOKEN: &str = "test-token";

/// mock hub 每条连接收到的上行消息
#[derive(Debug)]
enum Event {
    Hello(usize, Value),
    Sessions(usize, Value),
    Pong(usize),
    /// 其它上行消息（接管相关）
    Up(usize, Value),
}

/// mock hub 的行为
#[derive(Clone, Copy)]
enum Mode {
    /// 回 welcome；第一条连接收到 2 条 sessions 后主动断开，之后的连接保持
    Normal,
    /// 首条消息后回 proto_unsupported 错误并以 4002 关闭
    ProtoUnsupported,
    /// 回 welcome；常驻连接（非 probe）收到首条 sessions 后回 replaced 错误并以 4004 关闭，probe 连接不受影响
    Replaced,
    /// 回 welcome；收到首条 sessions 后发 attach / input，收到 output 后断开
    Takeover,
}

/// 起 mock hub，返回端口与事件接收端；token 不对时以 401 拒绝握手
async fn mock_hub(mode: Mode) -> (u16, mpsc::UnboundedReceiver<Event>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut conn = 0usize;
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            conn += 1;
            let (tx, n) = (tx.clone(), conn);
            tokio::spawn(async move {
                // 返回类型由 tungstenite 的握手回调签名决定，无法缩小 Err
                #[allow(clippy::result_large_err)]
                let check = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
                    let ok = req.uri().path() == "/agent"
                        && req.headers().get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"));
                    if ok {
                        Ok(resp)
                    } else {
                        let mut err = ErrorResponse::new(None);
                        *err.status_mut() = StatusCode::UNAUTHORIZED;
                        Err(err)
                    }
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, check).await else { return };
                let mut sessions = 0;
                let mut probe = false;
                while let Some(Ok(msg)) = ws.next().await {
                    if let Message::Pong(data) = &msg {
                        if data.as_ref() == b"hb" {
                            let _ = tx.send(Event::Pong(n));
                        }
                        continue;
                    }
                    let Message::Text(text) = msg else { continue };
                    let v: Value = serde_json::from_str(&text).unwrap();
                    match v["type"].as_str() {
                        Some("hello") => {
                            probe = v["probe"] == true;
                            let _ = tx.send(Event::Hello(n, v));
                            let reply = match mode {
                                Mode::Normal | Mode::Replaced | Mode::Takeover => r#"{"type":"welcome","proto":2,"hubVersion":"9.9.9"}"#,
                                Mode::ProtoUnsupported => r#"{"type":"error","code":"proto_unsupported","message":"hub 仅支持协议 2"}"#,
                            };
                            // 未知类型的消息应被忽略
                            let _ = ws.send(Message::Text(r#"{"type":"futureThing"}"#.into())).await;
                            let _ = ws.send(Message::Text(reply.into())).await;
                            if let Mode::ProtoUnsupported = mode {
                                let _ = ws.close(None).await;
                                return;
                            }
                        }
                        Some("sessions") => {
                            sessions += 1;
                            let _ = tx.send(Event::Sessions(n, v));
                            if let (Mode::Replaced, false) = (mode, probe) {
                                let _ = ws.send(Message::Text(r#"{"type":"error","code":"replaced","message":"x"}"#.into())).await;
                                let frame = CloseFrame { code: CloseCode::from(4004), reason: "replaced".into() };
                                let _ = ws.close(Some(frame)).await;
                                return;
                            }
                            if let (Mode::Takeover, 1) = (mode, sessions) {
                                let _ = ws.send(Message::Text(r#"{"type":"attach","sessionId":1}"#.into())).await;
                                let _ = ws.send(Message::Text(r#"{"type":"input","sessionId":1,"data":"bHMK"}"#.into())).await;
                                continue;
                            }
                            if sessions == 1 {
                                // 心跳：客户端应自动回 pong
                                let _ = ws.send(Message::Ping(b"hb".to_vec().into())).await;
                            }
                            if n == 1 && sessions == 2 {
                                // 模拟 hub 重启 / 断网：直接丢弃连接
                                return;
                            }
                        }
                        _ => {
                            let output = v["type"] == "output";
                            let _ = tx.send(Event::Up(n, v));
                            if let (Mode::Takeover, true) = (mode, output) {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
    (port, rx)
}

fn session(id: u32, name: &str) -> SessionInfo {
    SessionInfo {
        id,
        name: name.into(),
        cwd: "C:\\work".into(),
        command: "pwsh.exe".into(),
        status: Status::Running,
        started_at: 1,
        last_activity_at: 2,
        host: None,
        host_id: None,
        claude: None,
        taken_over: false,
    }
}

type HubStates = Arc<Mutex<Vec<(HubState, Option<String>)>>>;

struct Harness {
    list: Arc<Mutex<Vec<SessionInfo>>>,
    changed: Arc<Notify>,
    states: HubStates,
    /// conn 回调：Some(是否带发送端)
    conns: mpsc::UnboundedReceiver<Option<mpsc::UnboundedSender<link::Up>>>,
    downs: mpsc::UnboundedReceiver<link::Down>,
}

fn make_link(port: u16, token: &str) -> (link::Link, Harness) {
    let list = Arc::new(Mutex::new(vec![session(1, "one")]));
    let changed = Arc::new(Notify::new());
    let states = Arc::new(Mutex::new(Vec::new()));
    let (l, s) = (list.clone(), states.clone());
    let (conn_tx, conns) = mpsc::unbounded_channel();
    let (down_tx, downs) = mpsc::unbounded_channel();
    let link = link::Link {
        endpoint: link::parse_address(&format!("ws://127.0.0.1:{port}"), true).unwrap(),
        token: Secret(token.into()),
        hello: link::hello("mid-1".into(), "TEST-PC", "0.0.1"),
        sessions: Arc::new(move || l.lock().unwrap().clone()),
        changed: changed.clone(),
        status: Arc::new(move |state, error| s.lock().unwrap().push((state, error))),
        conn: Arc::new(move |tx| drop(conn_tx.send(tx))),
        down: Arc::new(move |msg| drop(down_tx.send(msg))),
    };
    (link, Harness { list, changed, states, conns, downs })
}

async fn next(rx: &mut mpsc::UnboundedReceiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("等待 mock hub 事件超时").unwrap()
}

#[tokio::test]
async fn hello_welcome_sessions_then_reconnect() {
    let (port, mut rx) = mock_hub(Mode::Normal).await;
    let (link, h) = make_link(port, TOKEN);
    let task = tokio::spawn(link::run(link));

    let Event::Hello(1, hello) = next(&mut rx).await else { panic!("应先收到 hello") };
    assert_eq!(hello["proto"], 2);
    assert!(hello.get("probe").is_none(), "常驻连接不带 probe");
    assert_eq!(hello["machineId"], "mid-1");
    assert_eq!(hello["machineName"], "TEST-PC");
    assert_eq!(hello["version"], "0.0.1");

    // welcome 后立即上报一次全量
    let Event::Sessions(1, first) = next(&mut rx).await else { panic!("welcome 后应上报 sessions") };
    assert_eq!(first["sessions"][0]["name"], "one");
    assert_eq!(first["sessions"][0]["lastActivityAt"], 2);
    assert!(h.states.lock().unwrap().iter().any(|(s, _)| *s == HubState::Connected));
    // hub 发 ping，客户端自动回 pong
    assert!(matches!(next(&mut rx).await, Event::Pong(1)), "应自动回 pong");

    // 会话变化：多次通知合并，约 500 ms 后上报一次
    h.list.lock().unwrap().push(session(2, "two"));
    let changed_at = Instant::now();
    for _ in 0..5 {
        h.changed.notify_one();
    }
    let Event::Sessions(1, second) = next(&mut rx).await else { panic!("会话变化后应再次上报") };
    let waited = changed_at.elapsed();
    assert!(waited >= Duration::from_millis(400), "防抖时间过短：{waited:?}");
    assert_eq!(second["sessions"].as_array().unwrap().len(), 2);

    // mock hub 在第二条 sessions 后断开：客户端退避（首次约 1 s）后重连，重新 hello 并上报
    let dropped_at = Instant::now();
    let Event::Hello(2, _) = next(&mut rx).await else { panic!("断线后应重连") };
    let waited = dropped_at.elapsed();
    assert!(waited >= Duration::from_millis(700) && waited < Duration::from_secs(3), "重连间隔异常：{waited:?}");
    let Event::Sessions(2, again) = next(&mut rx).await else { panic!("重连后应上报 sessions") };
    assert_eq!(again["sessions"].as_array().unwrap().len(), 2);

    let states = h.states.lock().unwrap().clone();
    assert_eq!(states.iter().filter(|(s, _)| *s == HubState::Connected).count(), 2, "{states:?}");
    // 退避等待期间为 connecting，并带上一次断开的原因
    assert!(states.iter().any(|(s, e)| *s == HubState::Connecting && e.is_some()), "{states:?}");
    assert!(!states.iter().any(|(s, _)| *s == HubState::Failed));
    assert!(!task.is_finished());
    task.abort();
}

#[tokio::test]
async fn unauthorized_is_fatal() {
    let (port, _rx) = mock_hub(Mode::Normal).await;
    let (link, h) = make_link(port, "wrong-token");
    // 401 不自动重连：run 应很快返回，停在 failed
    tokio::time::timeout(Duration::from_secs(5), link::run(link)).await.expect("401 后不应继续重连");
    let states = h.states.lock().unwrap().clone();
    let (last, error) = states.last().unwrap();
    assert_eq!(*last, HubState::Failed);
    assert!(error.as_deref().unwrap().contains("401"), "{error:?}");

    // 测试连接同样分类为认证失败
    let ep = link::parse_address(&format!("ws://127.0.0.1:{port}"), true).unwrap();
    let hello = link::hello("m".into(), "n", "v");
    let f = link::probe(&ep, &Secret("wrong-token".into()), &hello).await.unwrap_err();
    assert_eq!(f.kind, link::Kind::Auth);
}

#[tokio::test]
async fn probe_reports_hub_version_and_closes() {
    let (port, mut rx) = mock_hub(Mode::Normal).await;
    let ep = link::parse_address(&format!("ws://127.0.0.1:{port}/agent"), true).unwrap();
    let hello = link::hello("m".into(), "n", "v");
    let version = link::probe(&ep, &Secret(TOKEN.into()), &hello).await.unwrap();
    assert_eq!(version, "9.9.9");
    let Event::Hello(1, sent) = next(&mut rx).await else { panic!("应收到 hello") };
    assert_eq!(sent["probe"], true, "测试连接的 hello 应带 probe: true");
    // 测试连接不发 sessions
    assert!(tokio::time::timeout(Duration::from_millis(300), rx.recv()).await.is_err());
}

#[tokio::test]
async fn proto_unsupported_is_fatal() {
    let (port, _rx) = mock_hub(Mode::ProtoUnsupported).await;
    let (link, h) = make_link(port, TOKEN);
    tokio::time::timeout(Duration::from_secs(5), link::run(link)).await.expect("协议不兼容后不应继续重连");
    let states = h.states.lock().unwrap().clone();
    let (last, error) = states.last().unwrap();
    assert_eq!(*last, HubState::Failed);
    assert!(error.as_deref().unwrap().contains("协议不兼容"), "{error:?}");
}

#[tokio::test]
async fn connection_refused_keeps_retrying() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let (link, h) = make_link(port, TOKEN);
    let task = tokio::spawn(link::run(link));
    // Windows 上连接本机未监听端口约 2 s 才报拒绝
    let retrying = || h.states.lock().unwrap().iter().any(|(s, e)| *s == HubState::Connecting && e.as_deref().is_some_and(|e| e.contains("TCP")));
    let start = Instant::now();
    while !retrying() && start.elapsed() < Duration::from_secs(8) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let states = h.states.lock().unwrap().clone();
    assert!(retrying(), "{states:?}");
    assert!(!task.is_finished(), "TCP 失败应按退避重连而不是停止");
    assert!(!states.iter().any(|(s, _)| *s == HubState::Failed));
    task.abort();
}

#[tokio::test]
async fn replaced_is_fatal() {
    let (port, mut rx) = mock_hub(Mode::Replaced).await;
    let (link, h) = make_link(port, TOKEN);
    tokio::time::timeout(Duration::from_secs(5), link::run(link)).await.expect("被替换后不应继续重连");
    assert!(matches!(next(&mut rx).await, Event::Hello(1, _)));
    assert!(matches!(next(&mut rx).await, Event::Sessions(1, _)));
    let states = h.states.lock().unwrap().clone();
    let (last, error) = states.last().unwrap();
    assert_eq!(*last, HubState::Failed);
    assert_eq!(error.as_deref(), Some(link::REPLACED_MSG));
    // 没有第二次连接
    assert!(tokio::time::timeout(Duration::from_millis(1500), rx.recv()).await.is_err());
}

#[tokio::test]
async fn probe_not_replaced() {
    // replaced 模式下 probe 连接照常成功（hub 不替换 / 不登记探测连接）
    let (port, mut rx) = mock_hub(Mode::Replaced).await;
    let ep = link::parse_address(&format!("ws://127.0.0.1:{port}"), true).unwrap();
    let hello = link::hello("m".into(), "n", "v");
    assert_eq!(link::probe(&ep, &Secret(TOKEN.into()), &hello).await.unwrap(), "9.9.9");
    let Event::Hello(1, sent) = next(&mut rx).await else { panic!() };
    assert_eq!(sent["probe"], true);
}

#[tokio::test]
async fn takeover_messages_relayed() {
    let (port, mut rx) = mock_hub(Mode::Takeover).await;
    let (link, mut h) = make_link(port, TOKEN);
    let task = tokio::spawn(link::run(link));
    assert!(matches!(next(&mut rx).await, Event::Hello(1, _)));
    assert!(matches!(next(&mut rx).await, Event::Sessions(1, _)));
    // 收到 welcome 后交出上行发送端
    let up = tokio::time::timeout(Duration::from_secs(5), h.conns.recv()).await.unwrap().unwrap().expect("连接可用时应带发送端");
    // 下行接管消息原样交给 daemon
    let first = tokio::time::timeout(Duration::from_secs(5), h.downs.recv()).await.unwrap().unwrap();
    assert_eq!(first, link::Down::Attach { session_id: 1 });
    assert_eq!(h.downs.recv().await.unwrap(), link::Down::Input { session_id: 1, data: "bHMK".into() });
    // daemon 经发送端推送的消息按序写到 hub
    up.send(link::Up::Attached { session_id: 1, cols: 80, rows: 24, snapshot: "eA==".into() }).unwrap();
    up.send(link::Up::Output { session_id: 1, data: "eQ==".into() }).unwrap();
    let Event::Up(1, attached) = next(&mut rx).await else { panic!() };
    assert_eq!(attached, serde_json::json!({ "type": "attached", "sessionId": 1, "cols": 80, "rows": 24, "snapshot": "eA==" }));
    let Event::Up(1, output) = next(&mut rx).await else { panic!() };
    assert_eq!(output["type"], "output");
    // mock hub 收到 output 后断开：daemon 收到 None（据此结束全部接管）
    let gone = tokio::time::timeout(Duration::from_secs(5), h.conns.recv()).await.unwrap().unwrap();
    assert!(gone.is_none());
    task.abort();
}
