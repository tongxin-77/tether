//! hub 客户端连接层集成测试：本地起明文 ws 的 mock hub（不依赖外部网络），验证 hello → welcome → sessions 上报、
//! 会话变化防抖上报、401 分类为致命错误、断线后自动重连。

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
}

/// mock hub 的行为
#[derive(Clone, Copy)]
enum Mode {
    /// 回 welcome；第一条连接收到 2 条 sessions 后主动断开，之后的连接保持
    Normal,
    /// 首条消息后回 proto_unsupported 错误并以 4002 关闭
    ProtoUnsupported,
    /// 回 welcome，收到首条 sessions 后回 replaced 错误并以 4004 关闭
    Replaced,
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
                            let _ = tx.send(Event::Hello(n, v));
                            let reply = match mode {
                                Mode::Normal | Mode::Replaced => r#"{"type":"welcome","proto":1,"hubVersion":"9.9.9"}"#,
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
                            if let Mode::Replaced = mode {
                                let _ = ws.send(Message::Text(r#"{"type":"error","code":"replaced","message":"x"}"#.into())).await;
                                let frame = CloseFrame { code: CloseCode::from(4004), reason: "replaced".into() };
                                let _ = ws.close(Some(frame)).await;
                                return;
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
                        _ => {}
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
    }
}

struct Harness {
    list: Arc<Mutex<Vec<SessionInfo>>>,
    changed: Arc<Notify>,
    states: Arc<Mutex<Vec<(HubState, Option<String>)>>>,
}

fn make_link(port: u16, token: &str) -> (link::Link, Harness) {
    let list = Arc::new(Mutex::new(vec![session(1, "one")]));
    let changed = Arc::new(Notify::new());
    let states = Arc::new(Mutex::new(Vec::new()));
    let (l, s) = (list.clone(), states.clone());
    let link = link::Link {
        endpoint: link::parse_address(&format!("ws://127.0.0.1:{port}"), true).unwrap(),
        token: Secret(token.into()),
        hello: link::Up::Hello { proto: 1, machine_id: "mid-1".into(), machine_name: "TEST-PC".into(), version: "0.0.1".into() },
        sessions: Arc::new(move || l.lock().unwrap().clone()),
        changed: changed.clone(),
        status: Arc::new(move |state, error| s.lock().unwrap().push((state, error))),
    };
    (link, Harness { list, changed, states })
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
    assert_eq!(hello["proto"], 1);
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
    let hello = link::Up::Hello { proto: 1, machine_id: "m".into(), machine_name: "n".into(), version: "v".into() };
    let f = link::probe(&ep, &Secret("wrong-token".into()), &hello).await.unwrap_err();
    assert_eq!(f.kind, link::Kind::Auth);
}

#[tokio::test]
async fn probe_reports_hub_version_and_closes() {
    let (port, mut rx) = mock_hub(Mode::Normal).await;
    let ep = link::parse_address(&format!("ws://127.0.0.1:{port}/agent"), true).unwrap();
    let hello = link::Up::Hello { proto: 1, machine_id: "m".into(), machine_name: "n".into(), version: "v".into() };
    let version = link::probe(&ep, &Secret(TOKEN.into()), &hello).await.unwrap();
    assert_eq!(version, "9.9.9");
    assert!(matches!(next(&mut rx).await, Event::Hello(1, _)));
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
