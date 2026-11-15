// release 下不弹控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::mpsc,
};
use winreg::HKCU;

/// 追加一行到 %LOCALAPPDATA%\tether\daemon.log
macro_rules! log {
    ($($t:tt)*) => { $crate::write_log(&format!($($t)*)) };
}

mod claude;
mod config;
mod crash;
mod fs;
mod history;
mod hub;
mod image;
mod notify;
mod recent;
mod saved;
mod session;
mod ssh;
mod stats;

use session::{Gui, Shared, State};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "tetherd";
/// 为未打包应用注册的 AppUserModelID，使 toast 显示为 "Tether" 而非 PowerShell
pub const AUMID: &str = "com.tether.app";
pub const AUMID_KEY: &str = r"Software\Classes\AppUserModelId\com.tether.app";

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--install") => return log!("--install: {:?}", install()),
        Some("--uninstall") => return log!("--uninstall: {:?}", uninstall()),
        // Claude Code hook 调用：发一条事件给 daemon 后退出，不输出任何内容
        Some("hook") => return claude::run_hook(),
        #[cfg(debug_assertions)]
        Some("--crash-test") => return crash::crash_test(std::env::args().nth(2).as_deref().unwrap_or("av")),
        _ => {}
    }
    crash::install();
    notify::init();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("创建 tokio 运行时失败");
    let mut state = State::default();
    state.config = config::load();
    state.recent = recent::load();
    rt.block_on(serve(Arc::new(Mutex::new(state))));
}

/// 更新时安装程序把运行中的 tetherd.exe 改名为 tetherd.old*.exe 让出文件名（见 hooks.nsh），旧 daemon 继续服务。
/// 写入注册表 / Claude hooks 的路径一律指向同目录的 tetherd.exe，避免记下即将被删除的旧文件名
pub fn stable_exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let old = exe.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().starts_with("tetherd.old"));
    Ok(if old { exe.with_file_name("tetherd.exe") } else { exe })
}

/// 删除更新遗留的 tetherd.old*.exe（旧 daemon 已退出才删得掉，仍在运行时忽略）
fn remove_old_exes() {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from)) else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name.starts_with("tetherd.old") && name.ends_with(".exe") && std::fs::remove_file(entry.path()).is_ok() {
            log!("已删除更新遗留的 {name}");
        }
    }
}

/// 写入登录自启项，并注册 AUMID（DisplayName）供系统通知使用
fn install() -> std::io::Result<()> {
    let exe = stable_exe()?;
    let (run, _) = HKCU.create_subkey(RUN_KEY)?;
    run.set_value(RUN_VALUE, &format!("\"{}\"", exe.display()))?;
    let (aumid, _) = HKCU.create_subkey(AUMID_KEY)?;
    aumid.set_value("DisplayName", &"Tether")
}

fn uninstall() -> std::io::Result<()> {
    let (run, _) = HKCU.create_subkey(RUN_KEY)?;
    let r1 = run.delete_value(RUN_VALUE);
    let r2 = HKCU.delete_subkey_all(AUMID_KEY);
    r1.and(r2)
}

/// 管道服务：首个实例创建失败（已有 daemon 占用）即退出；之后每接入一个客户端就再建一个实例等待
async fn serve(state: Shared) {
    let name = proto::pipe_name();
    let mut server = match ServerOptions::new().first_pipe_instance(true).create(&name) {
        Ok(s) => s,
        Err(e) => return log!("管道 {name} 创建失败（可能已有 daemon 在运行），退出：{e}"),
    };
    log!("daemon {} 启动，监听 {name}", env!("CARGO_PKG_VERSION"));
    remove_old_exes();
    // 确认是唯一的 daemon 后再恢复上次未关闭的会话
    session::restore(&state);
    // hub 默认关闭；启用时在后台任务中连接，不阻塞管道服务
    hub::restart(&state, &mut state.lock().unwrap());
    // 网页贴图保存的临时文件：启动时及每小时清理超过 24 小时的
    image::start_cleanup();
    let mut conn = 0u64;
    loop {
        let connected = server.connect().await;
        let next = match ServerOptions::new().create(&name) {
            Ok(s) => s,
            Err(e) => return log!("创建管道实例失败，退出：{e}"),
        };
        let pipe = std::mem::replace(&mut server, next);
        if let Err(e) = connected {
            log!("等待客户端连接失败：{e}");
            continue;
        }
        conn += 1;
        tokio::spawn(handle(state.clone(), pipe, conn));
    }
}

/// 单个连接：首条消息为 ClaudeEvent 时是 `tetherd hook` 的一次性连接，处理后断开，不影响 GUI；
/// 否则视为 GUI，新连接顶替旧连接（旧连接的发送端被丢弃，其后续消息被忽略并断开）
async fn handle(state: Shared, pipe: NamedPipeServer, conn: u64) {
    let (rd, mut wr) = tokio::io::split(pipe);
    let mut lines = BufReader::new(rd).lines();
    let first = match lines.next_line().await {
        Ok(Some(line)) => line,
        _ => return,
    };
    let first: Option<proto::ClientMsg> = match serde_json::from_str(&first) {
        Ok(msg) => Some(msg),
        Err(e) => {
            log!("无法解析客户端消息：{e}");
            None
        }
    };
    if let Some(ev @ proto::ClientMsg::ClaudeEvent { .. }) = first {
        return session::claude_event(&state, ev);
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    {
        let mut st = state.lock().unwrap();
        st.set_gui(Some(Gui::new(conn, tx)));
        st.push_recent();
        session::restore_remote(&state, &mut st);
    }
    log!("GUI 已连接 #{conn}");

    tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if wr.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });

    let alive = first.is_none_or(|msg| session::dispatch(&state, conn, msg));
    while alive && let Ok(Some(line)) = lines.next_line().await {
        match serde_json::from_str(&line) {
            Ok(msg) => {
                if !session::dispatch(&state, conn, msg) {
                    break;
                }
            }
            Err(e) => log!("无法解析 GUI 消息：{e}"),
        }
    }

    let mut st = state.lock().unwrap();
    if st.gui.as_ref().is_some_and(|g| g.conn == conn) {
        st.set_gui(None);
    }
    log!("GUI 已断开 #{conn}");
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

pub fn write_log(msg: &str) {
    if let Some(mut f) = open_log() {
        let _ = writeln!(f, "[{}] {msg}", now_ms() / 1000);
    }
}

/// 以追加模式打开 daemon.log
pub fn open_log() -> Option<std::fs::File> {
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join(proto::data_dir_name());
    let _ = std::fs::create_dir_all(&dir);
    OpenOptions::new().create(true).append(true).open(dir.join("daemon.log")).ok()
}
