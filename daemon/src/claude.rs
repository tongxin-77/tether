//! Claude Code 集成：`tetherd hook` 命令行模式（Claude hook 调用，把事件发回 daemon），
//! 以及在 ~/.claude/settings.json 中写入 / 移除 Tether 的 hook 条目。

use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use proto::{ClientMsg, ServerMsg};
use serde_json::{Map, Value, json};

/// 写入的 hook 事件（SubagentStart / SubagentStop 用于跟踪后台子代理；UserPromptExpansion 用于自定义命令 / skill 进入思考中）
const EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "UserPromptExpansion",
    "Notification",
    "Stop",
    "SubagentStart",
    "SubagentStop",
    "SessionEnd",
];
/// hook 超时（秒）；`tetherd hook` 自身最多约 2 秒即退出
const HOOK_TIMEOUT: u64 = 5;
/// 读 stdin / 连接管道各自的上限
const IO_LIMIT: Duration = Duration::from_millis(1000);
const ERROR_PIPE_BUSY: i32 = 231;

// ---------------- `tetherd hook`：由 Claude Code 调用 ----------------

/// 读 stdin 的 hook JSON，经 TETHER_PIPE 发给 daemon。不在 Tether 会话中（无环境变量）时立即退出；
/// 任何失败都静默忽略：不输出、不阻塞 Claude（退出码恒为 0）
pub fn run_hook() {
    let (Some(session), Some(pipe)) = (
        std::env::var("TETHER_SESSION").ok().and_then(|s| s.trim().parse::<u32>().ok()),
        std::env::var("TETHER_PIPE").ok().filter(|p| !p.trim().is_empty()),
    ) else {
        return;
    };
    let Some(msg) = event_msg(session, &read_stdin().unwrap_or_default(), std::env::args().nth(2)) else { return };
    let Ok(line) = serde_json::to_string(&msg) else { return };
    // 在线程中发送并最多等 IO_LIMIT：daemon 无响应时直接退出进程（线程随之结束），不拖住 Claude
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        send_line(&pipe_path(&pipe), &(line + "\n"));
        let _ = tx.send(());
    });
    let _ = rx.recv_timeout(IO_LIMIT);
}

/// 由 hook JSON 构造发给 daemon 的消息；事件名优先取命令行参数，否则取 hook_event_name。
/// 容忍开头的 UTF-8 BOM（如 .NET 写 stdin 时会加）；JSON 非法时仍可凭参数上报事件
pub(crate) fn event_msg(session: u32, stdin: &str, arg: Option<String>) -> Option<ClientMsg> {
    let input: Value = serde_json::from_str(stdin.trim_start_matches('\u{feff}')).unwrap_or(Value::Null);
    let text = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_string);
    let event = arg.or_else(|| text("hook_event_name"))?;
    // background_tasks（Stop / SubagentStop 携带，父会话视角）中未结束的后台代理数：只算子代理与 teammate；
    // 后台 shell、monitor（如长期运行的开发服务）不算，session_crons（定时唤醒）也不算；
    // SubagentStop 时列表里还带着该子代理自己（id 即 agent_id，状态仍为 running），不计
    let agent_id = text("agent_id");
    let own = |t: &Value| agent_id.is_some() && t.get("id").and_then(Value::as_str) == agent_id.as_deref();
    let finished = |t: &Value| matches!(t.get("status").and_then(Value::as_str), Some("completed" | "failed" | "killed" | "stopped"));
    let agent = |t: &Value| matches!(t.get("type").and_then(Value::as_str), Some("subagent" | "teammate"));
    let background = input.get("background_tasks").and_then(Value::as_array).map(|tasks| {
        tasks.iter().filter(|t| agent(t) && !own(t) && !finished(t)).count() as u32
    });
    let tasks = input.get("background_tasks").and_then(Value::as_array).map(|tasks| {
        let field = |t: &Value, k: &str| t.get(k).and_then(Value::as_str).unwrap_or("?").to_string();
        tasks.iter().map(|t| format!("{}:{}:{}", field(t, "type"), field(t, "status"), field(t, "id"))).collect::<Vec<_>>().join(",")
    });
    let teammates = input.get("background_tasks").and_then(Value::as_array).map_or_else(Vec::new, |tasks| {
        let teammate = |t: &Value| t.get("type").and_then(Value::as_str) == Some("teammate");
        tasks
            .iter()
            .filter(|t| teammate(t) && !own(t) && !finished(t))
            .filter_map(|t| t.get("id").and_then(Value::as_str).map(str::to_string))
            .collect()
    });
    Some(ClientMsg::ClaudeEvent {
        session,
        event,
        message: text("message"),
        notification_type: text("notification_type"),
        agent_id,
        agent_type: text("agent_type"),
        background,
        tasks,
        teammates,
        prompt: text("prompt").map(|p| p.chars().take(20).collect()),
        claude_session: text("session_id"),
        cwd: text("cwd"),
        reason: text("reason"),
        permission_mode: text("permission_mode"),
        source: text("source"),
    })
}

/// 在线程中读完 stdin，最多等 IO_LIMIT（stdin 未关闭时不挂住）
fn read_stdin() -> Option<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        let _ = tx.send(s);
    });
    rx.recv_timeout(IO_LIMIT).ok()
}

/// TETHER_PIPE 为不含 `\\.\pipe\` 前缀的管道名；兼容带前缀的写法，且只取最后一段：
/// Claude Code 经 Git Bash 运行 hook 时，MSYS 转换环境变量会把开头的 `\\` 改成 `\`
fn pipe_path(pipe: &str) -> String {
    let name = pipe.trim().rsplit(['\\', '/']).next().unwrap_or_default();
    format!(r"\\.\pipe\{name}")
}

/// 写入会话环境变量 TETHER_PIPE 的值：本 daemon 的管道名（不含前缀、不含反斜杠，避免 shell 转换）
pub fn pipe_env() -> String {
    let full = proto::pipe_name();
    full.rsplit('\\').next().unwrap_or(&full).to_string()
}

/// 以文件方式打开命名管道写一行；管道忙时短暂重试，总时长不超过 IO_LIMIT；daemon 未运行直接放弃
fn send_line(pipe: &str, line: &str) {
    let deadline = Instant::now() + IO_LIMIT;
    loop {
        match OpenOptions::new().write(true).open(pipe) {
            Ok(mut f) => {
                let _ = f.write_all(line.as_bytes());
                // FlushFileBuffers：等 daemon 读走数据再关闭，否则立即关闭句柄时数据可能被丢弃
                let _ = f.sync_all();
                return;
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return,
        }
    }
}

// ---------------- settings.json 读写 ----------------

/// Claude Code 用户设置文件：CLAUDE_CONFIG_DIR 优先，否则 %USERPROFILE%\.claude\settings.json
pub fn settings_path() -> Option<PathBuf> {
    let dir = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("USERPROFILE")?).join(".claude"),
    };
    Some(dir.join("settings.json"))
}

/// 写入 settings.json 的命令：当前运行的 tetherd.exe 绝对路径（正斜杠，Git Bash 与 PowerShell 都能直接执行）+ ` hook`。
/// 路径无空格时不加引号（两种 shell 通用）；含空格时用双引号（适用于 Claude Code 默认的 Git Bash）
pub fn hook_command() -> Result<String, String> {
    let exe = crate::stable_exe().map_err(|e| format!("无法获取 tetherd 路径：{e}"))?;
    Ok(command_for(&exe))
}

fn command_for(exe: &Path) -> String {
    let p = exe.display().to_string().replace('\\', "/");
    if p.contains(' ') { format!("\"{p}\" hook") } else { format!("{p} hook") }
}

/// 识别 Tether 写入的 hook：command 含 `tetherd` 与 ` hook`
fn is_tether(handler: &Value) -> bool {
    handler.get("command").and_then(Value::as_str).is_some_and(|c| c.to_lowercase().contains("tetherd") && c.contains(" hook"))
}

/// 读取设置：文件不存在或为空视为 `{}`；非法 JSON / 顶层不是对象时报错（不覆盖）
fn read_settings(path: &Path) -> Result<(Map<String, Value>, bool), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Map::new(), false)),
        Err(e) => return Err(format!("读取 {} 失败：{e}", path.display())),
    };
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Ok((Map::new(), true));
    }
    match serde_json::from_str(text) {
        Ok(Value::Object(m)) => Ok((m, true)),
        Ok(_) => Err(format!("{} 顶层不是 JSON 对象，未修改", path.display())),
        Err(e) => Err(format!("{} 不是合法 JSON（{e}），未修改", path.display())),
    }
}

/// 先备份为 settings.json.tether-bak-<毫秒时间戳>（原文件存在时），再以 2 空格缩进写回
fn write_settings(path: &Path, root: &Map<String, Value>, existed: bool) -> Result<(), String> {
    if existed {
        let name = format!("{}.tether-bak-{}", path.file_name().and_then(|n| n.to_str()).unwrap_or("settings.json"), crate::now_ms());
        std::fs::copy(path, path.with_file_name(name)).map_err(|e| format!("备份失败：{e}"))?;
    } else if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(root).map_err(|e| e.to_string())?;
    std::fs::write(path, json + "\n").map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}

/// 从 hooks 对象中删除 Tether 的条目；因此变空的匹配组 / 事件数组随之删除。返回是否有改动
fn strip(hooks: &mut Map<String, Value>) -> bool {
    let mut changed = false;
    hooks.retain(|_, groups| {
        let Some(arr) = groups.as_array_mut() else { return true };
        let before = arr.len();
        arr.retain_mut(|group| {
            let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else { return true };
            let n = handlers.len();
            handlers.retain(|h| !is_tether(h));
            if handlers.len() == n {
                return true;
            }
            changed = true;
            !handlers.is_empty()
        });
        // 只删因移除 Tether 条目而变空的数组，用户原本的空数组保留
        !(arr.is_empty() && before > 0)
    });
    changed
}

/// 启用：为每个事件追加 Tether 的 hook（先移除旧的 Tether 条目，不动用户自己的 hook）；已是目标状态时不写文件
pub fn enable(path: &Path, command: &str) -> Result<(), String> {
    let (mut root, existed) = read_settings(path)?;
    let original = root.clone();
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or("settings.json 中的 hooks 不是对象，未修改")?;
    strip(hooks);
    for event in EVENTS {
        let groups = hooks.entry(event).or_insert_with(|| json!([]));
        let groups = groups.as_array_mut().ok_or_else(|| format!("settings.json 中的 hooks.{event} 不是数组，未修改"))?;
        groups.push(json!({ "hooks": [{ "type": "command", "command": command, "timeout": HOOK_TIMEOUT }] }));
    }
    if root == original {
        return Ok(());
    }
    write_settings(path, &root, existed)
}

/// 移除：只删除 Tether 的条目（及因此变空的容器）；没有 Tether 条目时不写文件
pub fn disable(path: &Path) -> Result<(), String> {
    let (mut root, existed) = read_settings(path)?;
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else { return Ok(()) };
    if !strip(hooks) {
        return Ok(());
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
    write_settings(path, &root, existed)
}

/// 集成状态 (已启用, 需要更新)：任一事件有 Tether 的 hook 即视为已启用；已启用但缺少部分事件（旧版只写了 5 个）时需要更新
pub fn installed(path: &Path) -> (bool, bool) {
    let Ok((root, _)) = read_settings(path) else { return (false, false) };
    let has = |e: &str| {
        root.get("hooks").and_then(|h| h.get(e)).and_then(Value::as_array).is_some_and(|groups| {
            groups.iter().any(|g| g.get("hooks").and_then(Value::as_array).is_some_and(|hs| hs.iter().any(is_tether)))
        })
    };
    let n = EVENTS.iter().filter(|e| has(e)).count();
    (n > 0, n > 0 && n < EVENTS.len())
}

/// 是否已完整启用：每个事件都有 Tether 的 hook
#[cfg(test)]
fn enabled(path: &Path) -> bool {
    installed(path) == (true, false)
}

/// 当前集成状态，附带本次操作的错误
pub fn status_msg(error: Option<String>) -> ServerMsg {
    let path = settings_path();
    let (enabled, outdated) = path.as_deref().map(installed).unwrap_or_default();
    ServerMsg::ClaudeHooks { path: path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(), enabled, outdated, error }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "C:/Tools/tether/tetherd.exe hook";

    /// 每个测试一个独立临时目录
    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tether-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.json")
    }

    fn read(p: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    fn backups(p: &Path) -> usize {
        std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".tether-bak-"))
            .count()
    }

    #[test]
    fn missing_file_is_created() {
        let p = temp("missing");
        enable(&p, CMD).unwrap();
        assert!(enabled(&p));
        let v = read(&p);
        for e in EVENTS {
            assert_eq!(v["hooks"][e][0]["hooks"][0]["command"], CMD);
            assert_eq!(v["hooks"][e][0]["hooks"][0]["type"], "command");
        }
        assert_eq!(backups(&p), 0);
        disable(&p).unwrap();
        assert_eq!(read(&p), json!({}));
        assert!(!enabled(&p));
    }

    #[test]
    fn empty_file_is_ok() {
        let p = temp("empty");
        std::fs::write(&p, "").unwrap();
        enable(&p, CMD).unwrap();
        assert!(enabled(&p));
        assert_eq!(backups(&p), 1);
    }

    #[test]
    fn user_hooks_and_keys_preserved() {
        let p = temp("preserve");
        let user = json!({
            "model": "opus",
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": "say done" }] }],
                "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "check.sh" }] }]
            },
            "zz": 1
        });
        std::fs::write(&p, serde_json::to_string_pretty(&user).unwrap()).unwrap();
        enable(&p, CMD).unwrap();
        let v = read(&p);
        assert_eq!(v["model"], "opus");
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "say done");
        assert_eq!(v["hooks"]["Stop"][1]["hooks"][0]["command"], CMD);
        assert_eq!(v["hooks"]["PreToolUse"], user["hooks"]["PreToolUse"]);
        // 键顺序保持
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["model", "hooks", "zz"]);
        assert_eq!(backups(&p), 1);
        // 移除后与原内容一致
        disable(&p).unwrap();
        assert_eq!(read(&p), user);
    }

    #[test]
    fn enable_is_idempotent_and_updates_path() {
        let p = temp("idem");
        enable(&p, CMD).unwrap();
        let first = std::fs::read_to_string(&p).unwrap();
        enable(&p, CMD).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), first);
        // 未改动时不备份、不写
        assert_eq!(backups(&p), 0);
        // 换路径：旧条目被替换而非重复
        enable(&p, "\"C:/Program Files/Tether/tetherd.exe\" hook").unwrap();
        let v = read(&p);
        for e in EVENTS {
            assert_eq!(v["hooks"][e].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn old_install_is_outdated_and_upgraded() {
        let p = temp("outdated");
        // 旧版只写了 5 个事件
        let old: Map<String, Value> = ["SessionStart", "UserPromptSubmit", "Notification", "Stop", "SessionEnd"]
            .iter()
            .map(|e| (e.to_string(), json!([{ "hooks": [{ "type": "command", "command": CMD, "timeout": 5 }] }])))
            .collect();
        std::fs::write(&p, json!({ "hooks": old }).to_string()).unwrap();
        assert_eq!(installed(&p), (true, true));
        enable(&p, CMD).unwrap();
        assert_eq!(installed(&p), (true, false));
        let v = read(&p);
        for e in EVENTS {
            assert_eq!(v["hooks"][e].as_array().unwrap().len(), 1, "{e}");
        }
        assert_eq!(v["hooks"]["SubagentStop"][0]["hooks"][0]["command"], CMD);
        // 再次启用不改动
        let text = std::fs::read_to_string(&p).unwrap();
        enable(&p, CMD).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), text);
        // 移除时新事件一并删除
        disable(&p).unwrap();
        assert_eq!(read(&p), json!({}));
        assert_eq!(installed(&p), (false, false));
    }

    #[test]
    fn hook_input_parsing() {
        let stop = "\u{feff}{\"hook_event_name\":\"Stop\",\"background_tasks\":[{\"id\":\"a\",\"type\":\"subagent\"},{\"id\":\"b\",\"type\":\"shell\"},{\"id\":\"c\",\"type\":\"subagent\"}]}";
        let ClientMsg::ClaudeEvent { session, event, background, .. } = event_msg(3, stop, None).unwrap() else { panic!() };
        assert_eq!((session, event.as_str(), background), (3, "Stop", Some(2)));
        // SubagentStop：不计该子代理自身，后台 shell 也不计；日志摘要列出各项 type:status:id
        let sub = r#"{"hook_event_name":"SubagentStop","agent_id":"a","background_tasks":[{"id":"a","type":"subagent","status":"running"},{"id":"b","type":"shell","status":"running"}]}"#;
        let ClientMsg::ClaudeEvent { background, tasks, .. } = event_msg(1, sub, None).unwrap() else { panic!() };
        assert_eq!((background, tasks.as_deref()), (Some(0), Some("subagent:running:a,shell:running:b")));
        // prompt 只转发前 20 个字符
        let ups = r#"{"hook_event_name":"UserPromptSubmit","prompt":"/hello 一二三四五六七八九十一二三四五"}"#;
        let ClientMsg::ClaudeEvent { prompt, .. } = event_msg(1, ups, None).unwrap() else { panic!() };
        assert_eq!(prompt.as_deref(), Some("/hello 一二三四五六七八九十一二三"));
        let sub = r#"{"hook_event_name":"SubagentStart","agent_id":"x1","agent_type":"Explore"}"#;
        let ClientMsg::ClaudeEvent { event, agent_id, agent_type, background, .. } = event_msg(1, sub, None).unwrap() else { panic!() };
        assert_eq!((event.as_str(), agent_id.as_deref(), agent_type.as_deref(), background), ("SubagentStart", Some("x1"), Some("Explore"), None));
        // 参数优先；JSON 非法时凭参数上报
        let ClientMsg::ClaudeEvent { event, .. } = event_msg(1, "garbage", Some("Stop".into())).unwrap() else { panic!() };
        assert_eq!(event, "Stop");
        assert!(event_msg(1, "garbage", None).is_none());
    }

    #[test]
    fn hook_session_id_parsing() {
        let start = r#"{"hook_event_name":"SessionStart","session_id":"abc-123","cwd":"C:\\work\\p","source":"startup","permission_mode":"bypassPermissions"}"#;
        let ClientMsg::ClaudeEvent { claude_session, cwd, reason, permission_mode, source, .. } = event_msg(1, start, None).unwrap() else { panic!() };
        assert_eq!((claude_session.as_deref(), cwd.as_deref(), reason), (Some("abc-123"), Some(r"C:\work\p"), None));
        assert_eq!((permission_mode.as_deref(), source.as_deref()), (Some("bypassPermissions"), Some("startup")));
        let end = r#"{"hook_event_name":"SessionEnd","session_id":"abc-123","reason":"prompt_input_exit"}"#;
        let ClientMsg::ClaudeEvent { reason, permission_mode, .. } = event_msg(1, end, None).unwrap() else { panic!() };
        assert_eq!((reason.as_deref(), permission_mode), (Some("prompt_input_exit"), None));
        // 旧版 daemon / hook 不带这些字段时反序列化为 None
        let old: ClientMsg = serde_json::from_str(r#"{"type":"claudeEvent","session":1,"event":"Stop"}"#).unwrap();
        let ClientMsg::ClaudeEvent { claude_session, permission_mode, source, .. } = old else { panic!() };
        assert_eq!((claude_session, permission_mode, source), (None, None, None));
    }

    #[test]
    fn remove_keeps_user_hooks_in_same_group() {
        let p = temp("group");
        let user = json!({ "hooks": { "Notification": [{ "matcher": "", "hooks": [
            { "type": "command", "command": "notify.exe" },
            { "type": "command", "command": "D:/x/tetherd.exe hook" }
        ] }], "Stop": [] } });
        std::fs::write(&p, user.to_string()).unwrap();
        disable(&p).unwrap();
        let v = read(&p);
        assert_eq!(v["hooks"]["Notification"][0]["hooks"], json!([{ "type": "command", "command": "notify.exe" }]));
        // 用户原有的空数组保留
        assert_eq!(v["hooks"]["Stop"], json!([]));
    }

    #[test]
    fn invalid_json_refused() {
        let p = temp("invalid");
        std::fs::write(&p, "{ not json").unwrap();
        assert!(enable(&p, CMD).is_err());
        assert!(disable(&p).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ not json");
        assert_eq!(backups(&p), 0);
        std::fs::write(&p, r#"{"hooks": []}"#).unwrap();
        assert!(enable(&p, CMD).is_err());
    }

    #[test]
    fn disable_without_file_is_noop() {
        let p = temp("nofile");
        disable(&p).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn command_quoting() {
        assert_eq!(command_for(Path::new(r"C:\a\tetherd.exe")), "C:/a/tetherd.exe hook");
        assert_eq!(command_for(Path::new(r"C:\Program Files\t\tetherd.exe")), "\"C:/Program Files/t/tetherd.exe\" hook");
        assert!(is_tether(&json!({ "command": command_for(Path::new(r"C:\Program Files\t\tetherd.exe")) })));
        assert_eq!(pipe_path("tetherd-x"), r"\\.\pipe\tetherd-x");
        assert_eq!(pipe_path(r"\\.\pipe\tetherd-x"), r"\\.\pipe\tetherd-x");
        assert_eq!(pipe_path(r"\.\pipe\tetherd-x"), r"\\.\pipe\tetherd-x");
        assert_eq!(pipe_path(&pipe_env()), proto::pipe_name());
    }
}
