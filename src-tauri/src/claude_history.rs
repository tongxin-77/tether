//! Claude Code 历史对话列表：只读 `<配置目录>\projects\<编码后的目录名>\<session-id>.jsonl`，不修改这些文件。

use std::{
    io::BufRead,
    path::{Path, PathBuf},
};

/// 一个对话文件的摘要
#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub title: String,
    /// 主对话（非 sidechain）的 user / assistant 行数
    pub messages: u32,
    /// 最近一次记录的权限模式
    pub permission_mode: Option<String>,
    /// 出现过 bypassPermissions（恢复时需带 --dangerously-skip-permissions）
    pub bypass: bool,
}

#[derive(Debug, PartialEq)]
pub struct Conversation {
    pub id: String,
    /// 文件修改时间，Unix 毫秒
    pub mtime: u64,
    pub summary: Summary,
}

/// 标题最多显示的字符数
const TITLE_MAX: usize = 60;

/// Claude Code 存放对话的目录：`%CLAUDE_CONFIG_DIR%\projects`，未设置时 `%USERPROFILE%\.claude\projects`
pub fn projects_dir() -> Option<PathBuf> {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join(".claude")))?;
    Some(base.join("projects"))
}

/// Claude Code 的项目目录名：路径中 ASCII 字母数字以外的字符（按 UTF-16 码元）都换成 '-'；末尾分隔符不计（盘符根除外）
pub fn project_dir_name(cwd: &str) -> String {
    let t = cwd.trim_end_matches(['\\', '/']);
    let t = if t.is_empty() || t.ends_with(':') { cwd } else { t };
    t.encode_utf16().map(|u| if u < 128 && (u as u8).is_ascii_alphanumeric() { u as u8 as char } else { '-' }).collect()
}

/// 列出 cwd 的对话（没有消息的除外），按修改时间倒序；目录不存在时为空
pub fn list(projects: &Path, cwd: &str) -> Vec<Conversation> {
    let Some(entries) = find_dir(projects, &project_dir_name(cwd)).and_then(|d| std::fs::read_dir(d).ok()) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension()? != "jsonl" || !e.file_type().ok()?.is_file() {
                return None;
            }
            let id = path.file_stem()?.to_str()?.to_string();
            let modified = e.metadata().ok()?.modified().ok()?;
            let mtime = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as u64;
            let summary = parse(std::io::BufReader::new(std::fs::File::open(&path).ok()?));
            (summary.messages > 0).then_some(Conversation { id, mtime, summary })
        })
        .collect();
    out.sort_by_key(|c| std::cmp::Reverse(c.mtime));
    out
}

/// 按名称找项目目录（不区分大小写：盘符大小写可能不同）；超过 200 字符的名称 Claude Code 截断后追加哈希，按前 200 字符匹配
fn find_dir(projects: &Path, name: &str) -> Option<PathBuf> {
    let same = |a: Option<&str>, b: Option<&str>| a.zip(b).is_some_and(|(a, b)| a.eq_ignore_ascii_case(b));
    std::fs::read_dir(projects)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.eq_ignore_ascii_case(name) || (name.len() > 200 && same(n.get(..200), name.get(..200))))
        .map(|n| projects.join(n))
}

/// 逐行扫描对话文件：每行只走一遍顶层键（不解析嵌套内容），仅对标题行与首条用户消息做 JSON 解析
pub fn parse(mut reader: impl BufRead) -> Summary {
    let mut s = Summary::default();
    let (mut custom, mut ai, mut summary, mut prompt) = (None, None, None, None);
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
        let (mut ty, mut side, mut mode) = (None, false, None);
        top_level(&line, |k, v| match k {
            b"type" => ty = unquote(v),
            b"isSidechain" => side = v == b"true",
            b"permissionMode" => mode = unquote(v),
            _ => {}
        });
        if let Some(m) = mode {
            s.bypass |= m == "bypassPermissions";
            s.permission_mode = Some(m.to_string());
        }
        let field = |key: &str| {
            let v: serde_json::Value = serde_json::from_slice(&line).ok()?;
            v.get(key)?.as_str().map(String::from)
        };
        match ty {
            Some("user" | "assistant") if !side => {
                s.messages += 1;
                if ty == Some("user") && prompt.is_none() {
                    prompt = serde_json::from_slice(&line).ok().and_then(|v| user_prompt(&v));
                }
            }
            Some("custom-title") => custom = field("customTitle").or(custom),
            Some("ai-title") => ai = field("aiTitle").or(ai),
            Some("summary") => summary = field("summary").or(summary),
            _ => {}
        }
        line.clear();
    }
    s.title = custom.or(ai).or(summary).or(prompt).map(|t| short(&t)).unwrap_or_default();
    s
}

/// 用户亲自输入的文字：跳过元信息、压缩摘要、工具结果与 `<command-name>` 等包装
fn user_prompt(v: &serde_json::Value) -> Option<String> {
    if v["isMeta"] == true || v["isCompactSummary"] == true {
        return None;
    }
    let c = &v["message"]["content"];
    let text = c.as_str().or_else(|| c.as_array()?.iter().find(|b| b["type"] == "text")?["text"].as_str())?.trim();
    (!text.is_empty() && !text.starts_with('<')).then(|| text.to_string())
}

/// 空白压成单个空格，过长截断
fn short(t: &str) -> String {
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= TITLE_MAX { t } else { t.chars().take(TITLE_MAX).chain(['…']).collect() }
}

/// 不含转义的 JSON 字符串值
fn unquote(v: &[u8]) -> Option<&str> {
    let inner = v.strip_prefix(b"\"")?.strip_suffix(b"\"")?;
    if inner.contains(&b'\\') { None } else { std::str::from_utf8(inner).ok() }
}

/// 依次回调 JSON 对象的顶层键与值（值为原始 JSON 片段）；不是对象或格式错误时提前结束
fn top_level<'a>(b: &'a [u8], mut f: impl FnMut(&'a [u8], &'a [u8])) {
    let ws = |mut i: usize| {
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        i
    };
    let mut i = ws(0);
    if b.get(i) != Some(&b'{') {
        return;
    }
    i += 1;
    loop {
        i = ws(i);
        if b.get(i) != Some(&b'"') {
            return;
        }
        let Some(k_end) = string_end(b, i) else { return };
        let key = &b[i + 1..k_end - 1];
        i = ws(k_end);
        if b.get(i) != Some(&b':') {
            return;
        }
        i = ws(i + 1);
        let Some(v_end) = value_end(b, i) else { return };
        f(key, &b[i..v_end]);
        i = ws(v_end);
        if b.get(i) != Some(&b',') {
            return;
        }
        i += 1;
    }
}

/// b[start] 为 '"'：返回闭合引号之后的位置
fn string_end(b: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// 从 start 起的一个 JSON 值的结束位置
fn value_end(b: &[u8], start: usize) -> Option<usize> {
    match b.get(start)? {
        b'"' => string_end(b, start),
        b'{' | b'[' => {
            let (mut depth, mut i) = (0u32, start);
            while i < b.len() {
                match b[i] {
                    b'"' => {
                        i = string_end(b, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        _ => {
            let n = b[start..].iter().position(|c| matches!(c, b',' | b'}' | b']') || c.is_ascii_whitespace());
            Some(start + n.unwrap_or(b.len() - start))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_project_dir_name() {
        assert_eq!(project_dir_name(r"C:\workspace\tether"), "C--workspace-tether");
        assert_eq!(project_dir_name(r"C:\"), "C--");
        // 末尾分隔符不计（Claude 的 cwd 不带）
        assert_eq!(project_dir_name(r"C:\work\my.proj\"), "C--work-my-proj");
        // 非 ASCII 按 UTF-16 码元逐个替换（与 Claude Code 的 JS 正则一致）
        assert_eq!(project_dir_name(r"D:\工作\p_1"), "D-----p-1");
        assert_eq!(project_dir_name("/home/u/a b"), "-home-u-a-b");
    }

    fn lines(ls: &[&str]) -> Vec<u8> {
        ls.join("\n").into_bytes()
    }

    #[test]
    fn parses_title_count_and_permission() {
        let data = lines(&[
            r#"{"type":"permission-mode","permissionMode":"default","sessionId":"s"}"#,
            r#"{"parentUuid":null,"isSidechain":false,"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"},"isMeta":true}"#,
            r#"{"parentUuid":null,"isSidechain":false,"type":"user","message":{"role":"user","content":"  修复\n登录  bug  "},"permissionMode":"acceptEdits"}"#,
            r#"{"parentUuid":"a","isSidechain":false,"message":{"role":"assistant","content":[{"type":"text","text":"好"}]},"type":"assistant"}"#,
            r#"{"parentUuid":"b","isSidechain":false,"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"x"}]}}"#,
            r#"{"parentUuid":"c","isSidechain":true,"type":"user","message":{"role":"user","content":"子代理"}}"#,
            r#"{"type":"attachment","attachment":{"type":"user"}}"#,
            r#"{"type":"ai-title","aiTitle":"旧标题","sessionId":"s"}"#,
            r#"{"type":"ai-title","aiTitle":"登录 \"bug\" 修复","sessionId":"s"}"#,
            r#"{"type":"permission-mode","permissionMode":"plan","sessionId":"s"}"#,
            "not json",
            "",
        ]);
        let s = parse(&data[..]);
        assert_eq!(s, Summary { title: "登录 \"bug\" 修复".into(), messages: 4, permission_mode: Some("plan".into()), bypass: false });
    }

    #[test]
    fn title_falls_back_to_first_prompt() {
        let data = lines(&[
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"<local-command-stdout>x</local-command-stdout>"}}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":[{"type":"image"},{"type":"text","text":"  看看\r\n这张图  "}]}}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"第二条"}}"#,
        ]);
        assert_eq!(parse(&data[..]).title, "看看 这张图");
        // 过长截断
        let long = format!(r#"{{"isSidechain":false,"type":"user","message":{{"role":"user","content":"{}"}}}}"#, "字".repeat(100));
        let t = parse(long.as_bytes()).title;
        assert_eq!(t.chars().count(), 61);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn custom_title_wins_and_bypass_is_sticky() {
        let data = lines(&[
            r#"{"type":"permission-mode","permissionMode":"bypassPermissions","sessionId":"s"}"#,
            r#"{"type":"custom-title","customTitle":"我的名字","sessionId":"s"}"#,
            r#"{"type":"ai-title","aiTitle":"自动","sessionId":"s"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"hi"},"permissionMode":"acceptEdits"}"#,
        ]);
        let s = parse(&data[..]);
        assert_eq!((s.title.as_str(), s.permission_mode.as_deref(), s.bypass), ("我的名字", Some("acceptEdits"), true));
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tether-cc-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(path: &Path, body: &str, mtime_secs: u64) {
        std::fs::write(path, body).unwrap();
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime_secs);
        std::fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
    }

    #[test]
    fn lists_conversations_newest_first() {
        let root = temp("list");
        let dir = root.join("C--work-p");
        std::fs::create_dir_all(dir.join("aaaa/subagents")).unwrap();
        let msg = |t: &str| format!(r#"{{"isSidechain":false,"type":"user","message":{{"role":"user","content":"{t}"}}}}"#);
        write(&dir.join("aaaa.jsonl"), &msg("旧"), 1_000);
        write(&dir.join("bbbb.jsonl"), &msg("新"), 2_000);
        // 没有消息的（如只有元数据）不列出；非 jsonl 忽略；其它目录不混入
        write(&dir.join("cccc.jsonl"), r#"{"type":"permission-mode","permissionMode":"plan"}"#, 3_000);
        write(&dir.join("notes.txt"), &msg("x"), 4_000);
        std::fs::create_dir_all(root.join("C--work-p2")).unwrap();
        write(&root.join("C--work-p2").join("dddd.jsonl"), &msg("别的"), 5_000);

        // 盘符大小写、末尾分隔符不影响
        let got = list(&root, r"c:\work\p\");
        let ids: Vec<_> = got.iter().map(|c| (c.id.as_str(), c.summary.title.as_str(), c.mtime)).collect();
        assert_eq!(ids, [("bbbb", "新", 2_000_000), ("aaaa", "旧", 1_000_000)]);
        assert!(list(&root, r"C:\nowhere").is_empty());
        assert!(list(&root.join("missing"), r"C:\work\p").is_empty());
    }

    #[test]
    fn long_dir_names_match_by_prefix() {
        let root = temp("long");
        let cwd = format!(r"C:\{}", "a".repeat(250));
        let name = project_dir_name(&cwd);
        let dir = root.join(format!("{}-1a2b3c", &name[..200]));
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir.join("e.jsonl"), r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"x"}}"#, 1);
        assert_eq!(list(&root, &cwd).len(), 1);
    }
}
