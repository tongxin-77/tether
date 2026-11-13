//! GUI 只是查看器：会话全部由后台进程 tetherd 持有，本进程通过命名管道收发协议消息。

use std::{
    io::ErrorKind,
    os::windows::process::CommandExt,
    process::Command,
    sync::Mutex,
    time::Duration,
};

use proto::{ClientMsg, ServerMsg};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient},
    sync::mpsc::{self, UnboundedSender},
};

const ERROR_PIPE_BUSY: i32 = 231;
/// 不分配控制台窗口（debug 构建的 daemon 是控制台子系统程序）
const DETACHED_PROCESS: u32 = 0x0000_0008;
const DAEMON_EXE: &str = "tetherd.exe";

/// 发往 daemon 的消息通道（连接建立后才有）
#[derive(Default)]
struct Pipe(Mutex<Option<UnboundedSender<String>>>);

/// daemon 与 GUI 同目录（workspace 下 debug/release 产物同在 target/<profile>/）
fn spawn_daemon() {
    let Ok(exe) = std::env::current_exe() else { return };
    let path = exe.with_file_name(DAEMON_EXE);
    if let Err(e) = Command::new(&path).creation_flags(DETACHED_PROCESS).spawn() {
        eprintln!("拉起 {} 失败：{e}", path.display());
    }
}

/// 连接 daemon；管道不存在则拉起一次 daemon，随后重试约 5 秒
async fn open_pipe() -> Result<NamedPipeClient, String> {
    let name = proto::pipe_name();
    let mut spawned = false;
    for _ in 0..50 {
        match ClientOptions::new().open(&name) {
            Ok(client) => return Ok(client),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                if !spawned {
                    spawned = true;
                    spawn_daemon();
                }
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {}
            Err(e) => return Err(e.to_string()),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("无法连接后台进程 tetherd".into())
}

/// 启动参数 `--session <id>`：启动后切到该会话
fn startup_session() -> Option<u32> {
    session_arg(&std::env::args().collect::<Vec<_>>())
}

fn session_arg(args: &[String]) -> Option<u32> {
    let i = args.iter().position(|a| a == "--session")?;
    args.get(i + 1)?.parse().ok()
}

/// 前端注册好事件监听后调用：建立连接，daemon 消息以 `daemon` 事件转给前端，断开时发 `daemon-closed`
#[tauri::command]
async fn connect(app: AppHandle, pipe: State<'_, Pipe>) -> Result<Option<u32>, String> {
    let client = open_pipe().await?;
    let (rd, mut wr) = tokio::io::split(client);
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    *pipe.0.lock().unwrap() = Some(tx);

    tauri::async_runtime::spawn(async move {
        while let Some(line) = rx.recv().await {
            if wr.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    tauri::async_runtime::spawn(async move {
        let mut lines = BufReader::new(rd).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(msg) = serde_json::from_str::<ServerMsg>(&line) {
                let _ = app.emit("daemon", msg);
            }
        }
        let _ = app.emit("daemon-closed", ());
    });
    Ok(startup_session())
}

#[tauri::command]
fn send(pipe: State<'_, Pipe>, msg: ClientMsg) {
    let guard = pipe.0.lock().unwrap();
    if let (Some(tx), Ok(json)) = (guard.as_ref(), serde_json::to_string(&msg)) {
        let _ = tx.send(json + "\n");
    }
}

/// 系统剪贴板序号：每次剪贴板内容变化递增；文件树复制时记下，粘贴时据此判断系统剪贴板是否更新过
#[tauri::command]
fn clipboard_seq() -> u32 {
    unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

/// 系统剪贴板中资源管理器复制的文件路径（CF_HDROP）及当前序号；没有文件时为空列表
#[tauri::command]
fn clipboard_files() -> (u32, Vec<String>) {
    use windows_sys::Win32::{
        System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard},
        UI::Shell::DragQueryFileW,
    };
    const CF_HDROP: u32 = 15;
    let seq = clipboard_seq();
    let mut files = Vec::new();
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP) == 0 {
            return (seq, files);
        }
        // 剪贴板可能正被其它程序占用，短暂重试
        let mut opened = false;
        for _ in 0..5 {
            if OpenClipboard(std::ptr::null_mut()) != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !opened {
            return (seq, files);
        }
        let hdrop = GetClipboardData(CF_HDROP);
        if !hdrop.is_null() {
            let count = DragQueryFileW(hdrop, u32::MAX, std::ptr::null_mut(), 0);
            for i in 0..count {
                let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0);
                let mut buf = vec![0u16; len as usize + 1];
                let n = DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32);
                files.push(String::from_utf16_lossy(&buf[..n as usize]));
            }
        }
        CloseClipboard();
    }
    (seq, files)
}

/// 删除旧版背景图功能留在应用数据目录下的 `background-*` 文件（该功能已移除），只删这一前缀的文件
fn remove_legacy_backgrounds(app: &AppHandle) {
    let Ok(dir) = app.path().app_data_dir() else { return };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let is_file = entry.file_type().is_ok_and(|t| t.is_file());
        if is_file && entry.file_name().to_string_lossy().starts_with("background-") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 关闭窗口即退出 GUI 进程，会话由 daemon 继续持有
    // debug 构建改用独立 identifier：应用数据目录、WebView2 数据（localStorage）、单实例键均与安装版隔离
    let mut context = tauri::generate_context!();
    if cfg!(debug_assertions) {
        let config = context.config_mut();
        config.identifier = "com.tether.app.dev".into();
        if let Some(w) = config.app.windows.first_mut() {
            w.title = "Tether (dev)".into();
        }
    }
    tauri::Builder::default()
        // 单实例须最先注册：第二个 tether.exe 直接退出，已有窗口前置；带 `--session` 时转交切换会话
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // 复用 daemon 的 Activate 处理：由会话所在窗口（主窗口或拖出的独立窗口）自行前置
            if let Some(id) = session_arg(&args) {
                let _ = app.emit("daemon", ServerMsg::Activate { id });
            } else if let Some(win) = app.get_webview_window("main") {
                let _ = win.unminimize();
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(Pipe::default())
        .setup(|app| {
            remove_legacy_backgrounds(app.handle());
            Ok(())
        })
        // 关闭主窗口即退出 GUI（连同拖出的独立窗口），下次启动所有会话都回到主窗口
        .on_window_event(|win, event| {
            if win.label() == "main" && matches!(event, WindowEvent::Destroyed) {
                win.app_handle().exit(0);
            }
        })
        .invoke_handler(tauri::generate_handler![connect, send, clipboard_seq, clipboard_files])
        .run(context)
        .expect("error while running tauri application");
}
