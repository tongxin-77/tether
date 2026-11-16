//! GUI 只是查看器：会话全部由后台进程 tetherd 持有，本进程通过命名管道收发协议消息。

mod claude_history;

use std::{
    io::ErrorKind,
    os::windows::{io::AsRawHandle, process::CommandExt},
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

/// 当前连接的 daemon 进程 id（管道服务端），供“重启后台”结束不支持 Shutdown 的旧版 daemon
#[derive(Default)]
struct DaemonPid(Mutex<Option<u32>>);

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
/// 连接后首条消息为 Hello（GUI 版本与协议版本），daemon 回 Hello；旧版 daemon 不回复，由前端超时判定
#[tauri::command]
async fn connect(app: AppHandle, pipe: State<'_, Pipe>, daemon_pid: State<'_, DaemonPid>) -> Result<Option<u32>, String> {
    let client = open_pipe().await?;
    let mut pid = 0u32;
    let ok = unsafe { windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId(client.as_raw_handle(), &mut pid) };
    *daemon_pid.0.lock().unwrap() = (ok != 0).then_some(pid);
    let (rd, mut wr) = tokio::io::split(client);
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let hello = ClientMsg::Hello { version: app.package_info().version.to_string(), proto: proto::PROTO_VERSION };
    if let Ok(json) = serde_json::to_string(&hello) {
        let _ = tx.send(json + "\n");
    }
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

/// 结束当前连接的 daemon 进程（用于不支持 Shutdown 的旧版 daemon）；会话已持续写入 sessions.json，
/// GUI 随后重连时拉起新的 tetherd.exe 并恢复会话
#[tauri::command]
fn kill_daemon(daemon_pid: State<'_, DaemonPid>) -> Result<(), String> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess},
    };
    let pid = daemon_pid.0.lock().unwrap().ok_or("未连接后台进程")?;
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            return Err(format!("无法打开后台进程 {pid}"));
        }
        let ok = TerminateProcess(h, 0);
        CloseHandle(h);
        if ok == 0 { Err(format!("结束后台进程 {pid} 失败")) } else { Ok(()) }
    }
}

/// debug 构建（dev）不检查更新，避免开发版去安装发布版
#[tauri::command]
fn is_debug() -> bool {
    cfg!(debug_assertions)
}

/// 系统剪贴板序号：每次剪贴板内容变化递增；文件树复制时记下，粘贴时据此判断系统剪贴板是否更新过
#[tauri::command]
fn clipboard_seq() -> u32 {
    unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

/// Windows build 号（注册表 CurrentBuildNumber），供本地会话的 xterm 设置 windowsPty；读取失败为 None
#[tauri::command]
fn windows_build() -> Option<u32> {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (key, name) = (wide(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"), wide("CurrentBuildNumber"));
    let mut buf = [0u16; 32];
    // 传入缓冲区字节数，返回写入的字节数（含结尾的 0）
    let mut len = size_of_val(&buf) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len)
    };
    if r != 0 {
        return None;
    }
    String::from_utf16_lossy(&buf[..len as usize / 2]).trim_end_matches('\0').parse().ok()
}

/// 目录所在 git 仓库的当前分支（分离 HEAD 时为提交哈希前 7 位）；不在仓库里或读取失败为 None。
/// 只读 .git 下的文件，不调用 git 进程
#[tauri::command]
async fn git_branch(dir: String) -> Option<String> {
    find_branch(std::path::Path::new(&dir))
}

/// 从 dir 向上查找 `.git`（目录或 worktree / submodule 的 gitdir 文件），读其 HEAD 解析分支
fn find_branch(dir: &std::path::Path) -> Option<String> {
    let dot = dir.ancestors().map(|d| d.join(".git")).find(|p| p.exists())?;
    let git_dir = if dot.is_dir() { dot } else { parse_gitdir(&std::fs::read_to_string(&dot).ok()?, dot.parent()?)? };
    parse_head(&std::fs::read_to_string(git_dir.join("HEAD")).ok()?)
}

/// HEAD 内容：`ref: refs/heads/<name>` 为分支名；十六进制提交哈希为分离 HEAD，取前 7 位；其它为 None
fn parse_head(content: &str) -> Option<String> {
    let s = content.trim();
    if let Some(name) = s.strip_prefix("ref: refs/heads/") {
        return (!name.is_empty()).then(|| name.to_string());
    }
    (s.len() >= 40 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s[..7].to_string())
}

/// `.git` 文件内容 `gitdir: <path>`；相对路径相对于该文件所在目录 base 解析
fn parse_gitdir(content: &str, base: &std::path::Path) -> Option<std::path::PathBuf> {
    let path = content.trim().strip_prefix("gitdir:")?.trim();
    // join 遇到绝对路径时直接取该路径
    (!path.is_empty()).then(|| base.join(path))
}

// ---------------- git 操作（调用 git 进程） ----------------

/// 不为控制台子进程创建窗口（避免调用 git 时闪黑窗）
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 提交详情 diff 的字节上限，超出截断
const DIFF_LIMIT: usize = 512 * 1024;

/// 在 dir 下运行 git：参数逐个作为 argv 传入，不经 shell。成功返回 (stdout, 是否截断)，失败返回 git 的错误输出。
/// limit 为 Some 时 stdout 最多读这么多字节（截到最后一个换行），超出即结束 git
fn run_git(dir: &str, args: &[&str], limit: Option<usize>) -> Result<(String, bool), String> {
    use std::{io::Read, process::Stdio};
    if !std::path::Path::new(dir).is_dir() {
        return Err(format!("目录不存在：{dir}"));
    }
    let mut child = Command::new("git")
        // 路径中的非 ASCII 字符不转义
        .args(["-c", "core.quotepath=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| if e.kind() == ErrorKind::NotFound { "未找到 git，请安装 Git 并加入 PATH".into() } else { format!("运行 git 失败：{e}") })?;
    // stderr 另起线程读，避免两个管道互相写满阻塞
    let mut stderr = child.stderr.take().unwrap();
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let mut out = Vec::new();
    let max = limit.map_or(u64::MAX, |n| n as u64 + 1);
    child.stdout.take().unwrap().take(max).read_to_end(&mut out).map_err(|e| e.to_string())?;
    let truncated = limit.is_some_and(|n| out.len() > n);
    if truncated {
        let _ = child.kill();
        out.truncate(out.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1).min(limit.unwrap()));
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).trim().to_string();
    if !truncated && !status.success() {
        return Err(if err.is_empty() { format!("git 执行失败（{status}）") } else { err });
    }
    Ok((String::from_utf8_lossy(&out).into_owned(), truncated))
}

/// 分支名参数：非空、不以 - 开头（防止被当成选项）、不含空白与控制字符
fn valid_ref_arg(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// 提交哈希：4 ~ 64 位十六进制
fn valid_hash(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 本地与远程分支，每行 `<*| > refs/...`（* 为当前分支）
fn list_refs(dir: &str) -> Result<String, String> {
    run_git(dir, &["for-each-ref", "--format=%(HEAD) %(refname)", "refs/heads", "refs/remotes"], None).map(|r| r.0)
}

/// `git switch <name>`；track 时 name 为远程分支（如 origin/x），`git switch --track` 创建同名本地分支并跟踪
fn switch_branch(dir: &str, name: &str, track: bool) -> Result<(), String> {
    if !valid_ref_arg(name) {
        return Err(format!("无效的分支名：{name}"));
    }
    let args: &[&str] = if track { &["switch", "--track", "--", name] } else { &["switch", "--", name] };
    run_git(dir, args, None).map(|_| ())
}

/// 当前分支最近 limit 个提交的 `git log --graph`；字段以 \x1f 分隔：图形列、完整哈希、短哈希、refs 装饰、作者、作者时间（秒）、标题
fn log_graph(dir: &str, limit: u32) -> Result<String, String> {
    let n = format!("-n{limit}");
    let args = ["log", "--graph", "--no-color", "--no-show-signature", &n, "--format=%x1f%H%x1f%h%x1f%D%x1f%an%x1f%at%x1f%s", "--"];
    run_git(dir, &args, None).map(|r| r.0)
}

/// 提交改动：(numstat, diff, diff 是否截断)。合并提交按第一父提交比较；不调用外部 diff / textconv 程序
fn show_commit(dir: &str, hash: &str) -> Result<(String, String, bool), String> {
    if !valid_hash(hash) {
        return Err(format!("无效的提交哈希：{hash}"));
    }
    let base = ["show", "--format=", "--no-color", "--no-ext-diff", "--no-textconv", "--diff-merges=first-parent"];
    let numstat = run_git(dir, &[&base[..], &["--numstat", hash, "--"]].concat(), None)?.0;
    let (diff, truncated) = run_git(dir, &[&base[..], &["--patch", hash, "--"]].concat(), Some(DIFF_LIMIT))?;
    Ok((numstat, diff, truncated))
}

/// 在后台线程执行阻塞的 git 调用，不占用异步运行时
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn git_refs(dir: String) -> Result<String, String> {
    blocking(move || list_refs(&dir)).await
}

#[tauri::command]
async fn git_switch(dir: String, name: String, track: bool) -> Result<(), String> {
    blocking(move || switch_branch(&dir, &name, track)).await
}

#[tauri::command]
async fn git_log(dir: String, limit: u32) -> Result<String, String> {
    blocking(move || log_graph(&dir, limit.clamp(1, 1000))).await
}

#[tauri::command]
async fn git_show(dir: String, hash: String) -> Result<(String, String, bool), String> {
    blocking(move || show_commit(&dir, &hash)).await
}

/// 目录 dir 的 Claude Code 历史对话（只读其 jsonl），按修改时间倒序
#[tauri::command]
async fn claude_history(dir: String) -> Result<Vec<serde_json::Value>, String> {
    blocking(move || {
        let root = claude_history::projects_dir().ok_or("未找到用户目录")?;
        let items = claude_history::list(&root, &dir).into_iter().map(|c| {
            let s = c.summary;
            serde_json::json!({ "id": c.id, "title": s.title, "mtime": c.mtime, "messages": s.messages, "permissionMode": s.permission_mode, "bypass": s.bypass })
        });
        Ok(items.collect())
    })
    .await
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(Pipe::default())
        .manage(DaemonPid::default())
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
        .invoke_handler(tauri::generate_handler![connect, send, kill_daemon, is_debug, clipboard_seq, clipboard_files, windows_build, git_branch, git_refs, git_switch, git_log, git_show, claude_history])
        .run(context)
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_windows_build() {
        assert!(super::windows_build().is_some_and(|b| b > 0));
    }

    #[test]
    fn parses_head_branch() {
        assert_eq!(super::parse_head("ref: refs/heads/main\n").as_deref(), Some("main"));
        assert_eq!(super::parse_head("ref: refs/heads/feature/x\r\n").as_deref(), Some("feature/x"));
    }

    #[test]
    fn parses_detached_head() {
        assert_eq!(super::parse_head("9897eda1234567890abcdef1234567890abcdef0\n").as_deref(), Some("9897eda"));
    }

    #[test]
    fn rejects_invalid_head() {
        for s in ["", "\n", "ref: refs/heads/", "ref: refs/tags/v1", "not a hash", "abc12"] {
            assert_eq!(super::parse_head(s), None, "{s:?}");
        }
    }

    #[test]
    fn parses_gitdir() {
        use std::path::Path;
        let base = Path::new(r"C:\repo\sub");
        assert_eq!(super::parse_gitdir("gitdir: ../.git/modules/sub\n", base), Some(base.join("../.git/modules/sub")));
        assert_eq!(super::parse_gitdir("gitdir: C:/repo/.git/worktrees/wt\r\n", base), Some(Path::new("C:/repo/.git/worktrees/wt").into()));
        assert_eq!(super::parse_gitdir("nonsense", base), None);
        assert_eq!(super::parse_gitdir("gitdir: ", base), None);
    }

    /// 在临时目录下构造假仓库；每个用例独立子目录
    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tether-git-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_branch_in_git_dir_and_parents() {
        let d = temp("dir");
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join(".git/HEAD"), "ref: refs/heads/dev\n").unwrap();
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        assert_eq!(super::find_branch(&d).as_deref(), Some("dev"));
        assert_eq!(super::find_branch(&d.join("a/b")).as_deref(), Some("dev"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn finds_branch_via_gitdir_file() {
        let d = temp("file");
        std::fs::create_dir_all(d.join("real/worktrees/wt")).unwrap();
        std::fs::write(d.join("real/worktrees/wt/HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        std::fs::create_dir_all(d.join("wt/src")).unwrap();
        // 相对路径
        std::fs::write(d.join("wt/.git"), "gitdir: ../real/worktrees/wt\n").unwrap();
        assert_eq!(super::find_branch(&d.join("wt/src")).as_deref(), Some("feature/x"));
        // 绝对路径
        std::fs::write(d.join("wt/.git"), format!("gitdir: {}\n", d.join("real/worktrees/wt").display())).unwrap();
        assert_eq!(super::find_branch(&d.join("wt")).as_deref(), Some("feature/x"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn none_outside_repo() {
        // 临时目录本身不在仓库里；.git 存在但没有 HEAD 也是 None
        let d = temp("none");
        assert_eq!(super::find_branch(&d), None);
        std::fs::create_dir_all(d.join("x/.git")).unwrap();
        assert_eq!(super::find_branch(&d.join("x")), None);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn finds_this_repo_branch() {
        // 仓库根（src-tauri 的上一级）；分支随检出变化，只要求能读出
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        assert!(super::find_branch(root).is_some());
    }

    #[test]
    fn validates_git_args() {
        for s in ["main", "feature/x", "origin/feat-1", "v1.0_rc"] {
            assert!(super::valid_ref_arg(s), "{s:?}");
        }
        for s in ["", "-x", "--force", "a b", "a\nb", "a\0b"] {
            assert!(!super::valid_ref_arg(s), "{s:?}");
        }
        assert!(super::valid_hash("9897eda"));
        assert!(super::valid_hash(&"a".repeat(40)));
        for s in ["", "abc", "-abcdef", "9897edz", "HEAD~1", &"a".repeat(65)] {
            assert!(!super::valid_hash(s), "{s:?}");
        }
    }

    /// 测试仓库里执行 git（固定作者、关闭签名），失败即 panic
    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// 环境里没有 git 时跳过真实仓库用例
    fn has_git() -> bool {
        let ok = std::process::Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success());
        if !ok {
            eprintln!("跳过：未找到 git");
        }
        ok
    }

    /// 上游仓库 up（main 两个提交、标签 v1、分支 feat 停在第一个提交）与其克隆 cl；返回 (根目录, cl)
    fn repos(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let d = temp(name);
        let up = d.join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "-q"]);
        std::fs::write(up.join("a.txt"), "1\n").unwrap();
        git(&up, &["add", "."]);
        git(&up, &["commit", "-q", "-m", "first"]);
        git(&up, &["branch", "feat"]);
        std::fs::write(up.join("a.txt"), "1\n2\n").unwrap();
        git(&up, &["commit", "-q", "-am", "second"]);
        git(&up, &["tag", "v1"]);
        git(&d, &["clone", "-q", "up", "cl"]);
        let cl = d.join("cl");
        (d, cl)
    }

    #[test]
    fn git_refs_switch_and_track() {
        if !has_git() {
            return;
        }
        let (d, cl) = repos("switch");
        let cl_s = cl.to_str().unwrap();
        let refs = super::list_refs(cl_s).unwrap();
        assert!(refs.contains("* refs/heads/main"), "{refs}");
        assert!(refs.contains("refs/remotes/origin/feat"), "{refs}");
        // 远程分支无同名本地分支：--track 创建并跟踪
        super::switch_branch(cl_s, "origin/feat", true).unwrap();
        assert_eq!(super::find_branch(&cl).as_deref(), Some("feat"));
        assert_eq!(git(&cl, &["rev-parse", "--abbrev-ref", "feat@{upstream}"]).trim(), "origin/feat");
        // 本地分支直接切换
        super::switch_branch(cl_s, "main", false).unwrap();
        assert_eq!(super::find_branch(&cl).as_deref(), Some("main"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn git_switch_errors() {
        if !has_git() {
            return;
        }
        let (d, cl) = repos("switch-err");
        let cl_s = cl.to_str().unwrap();
        // 未提交改动与目标分支冲突：返回 git 的错误输出，分支不变
        std::fs::write(cl.join("a.txt"), "dirty\n").unwrap();
        let err = super::switch_branch(cl_s, "origin/feat", true).unwrap_err();
        assert!(err.contains("a.txt"), "{err}");
        assert_eq!(super::find_branch(&cl).as_deref(), Some("main"));
        // 不存在的分支、以 - 开头的参数、不存在的目录
        assert!(!super::switch_branch(cl_s, "nope", false).unwrap_err().is_empty());
        assert!(super::switch_branch(cl_s, "--detach", false).is_err());
        assert!(super::list_refs(d.join("missing").to_str().unwrap()).unwrap_err().contains("目录不存在"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn git_log_and_show() {
        if !has_git() {
            return;
        }
        let (d, cl) = repos("log");
        let cl_s = cl.to_str().unwrap();
        let log = super::log_graph(cl_s, 200).unwrap();
        let lines: Vec<_> = log.lines().collect();
        assert_eq!(lines.len(), 2, "{log}");
        let f: Vec<_> = lines[0].split('\x1f').collect();
        assert_eq!(f[0], "* ");
        assert!(f[3].contains("HEAD -> main") && f[3].contains("tag: v1"), "{}", f[3]);
        assert_eq!(f[6], "second");
        assert_eq!(super::log_graph(cl_s, 1).unwrap().lines().count(), 1);

        let (numstat, diff, truncated) = super::show_commit(cl_s, f[1]).unwrap();
        assert_eq!(numstat.trim(), "1\t0\ta.txt");
        assert!(diff.contains("@@") && diff.contains("+2"), "{diff}");
        assert!(!truncated);
        assert!(super::show_commit(cl_s, "--help").is_err());
        assert!(super::show_commit(cl_s, &"0".repeat(40)).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn git_diff_truncated() {
        if !has_git() {
            return;
        }
        let d = temp("big");
        git(&d, &["init", "-q"]);
        // 约 1.2 MB 的新增文件，diff 超过上限
        std::fs::write(d.join("big.txt"), "0123456789abcdef0123456789abcdef\n".repeat(36_000)).unwrap();
        git(&d, &["add", "."]);
        git(&d, &["commit", "-q", "-m", "big"]);
        let head = git(&d, &["rev-parse", "--short=12", "HEAD"]);
        let (_, diff, truncated) = super::show_commit(d.to_str().unwrap(), head.trim()).unwrap();
        assert!(truncated);
        assert!(diff.len() <= super::DIFF_LIMIT && diff.ends_with('\n'), "{}", diff.len());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
