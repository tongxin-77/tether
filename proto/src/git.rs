//! 本地 git 查询（GUI 的变更面板 / 提交历史与 daemon 的 hub 变更面板共用）：直接运行 git 进程，不经 shell；
//! 输出的结构化解析（与 GUI 的 git-utils.ts 同规则）供 daemon 上报 hub。
//! 错误以 [`Error`] 返回，由各进程按自己的词条表翻译。

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};

use crate::GitChanges;

/// 单文件 diff / 提交详情 diff 的字节上限，超出截断
pub const DIFF_LIMIT: usize = 512 * 1024;

/// 最近提交的格式（与 daemon 远程脚本一致）：完整哈希、短哈希、refs 装饰、作者、作者时间（秒）、标题，以 \x1f 分隔
const LOG_FORMAT: &str = "--format=%H%x1f%h%x1f%D%x1f%an%x1f%at%x1f%s";

/// git 调用失败
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// 目录不存在
    DirNotFound(String),
    /// 找不到 git 程序
    NotInstalled,
    /// 启动 git 失败
    Run(String),
    /// git 非零退出且没有错误输出（附退出状态）
    Exit(String),
    /// git 的错误输出
    Git(String),
    /// 无效的提交哈希
    BadHash(String),
}

/// 不为控制台子进程创建窗口（避免调用 git 时闪黑窗）
#[cfg(windows)]
fn no_window(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000)
}

#[cfg(not(windows))]
fn no_window(cmd: &mut Command) -> &mut Command {
    cmd
}

/// 在 dir 下运行 git：参数逐个作为 argv 传入，不经 shell。成功返回 (stdout, 是否截断)，失败返回 git 的错误输出。
/// limit 为 Some 时 stdout 最多读这么多字节（截到最后一个换行），超出即结束 git；ok 为额外视作成功的退出码（如 diff --no-index 有差异时的 1）。
/// 一律带 --no-optional-locks：后台查询不抢 index.lock，不与终端里的 git 冲突
pub fn run(dir: &str, args: &[&str], limit: Option<usize>, ok: &[i32]) -> Result<(String, bool), Error> {
    if !Path::new(dir).is_dir() {
        return Err(Error::DirNotFound(dir.to_string()));
    }
    let mut cmd = Command::new("git");
    // 路径中的非 ASCII 字符不转义
    cmd.args(["--no-optional-locks", "-c", "core.quotepath=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = no_window(&mut cmd)
        .spawn()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { Error::NotInstalled } else { Error::Run(e.to_string()) })?;
    // stderr 另起线程读，避免两个管道互相写满阻塞
    let mut stderr = child.stderr.take().unwrap();
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let mut out = Vec::new();
    let max = limit.map_or(u64::MAX, |n| n as u64 + 1);
    child.stdout.take().unwrap().take(max).read_to_end(&mut out).map_err(|e| Error::Run(e.to_string()))?;
    let truncated = limit.is_some_and(|n| out.len() > n);
    if truncated {
        let _ = child.kill();
        out.truncate(out.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1).min(limit.unwrap()));
    }
    let status = child.wait().map_err(|e| Error::Run(e.to_string()))?;
    let err = String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).trim().to_string();
    // 允许的退出码也可能是出错（如 diff --no-index 找不到文件同样退出 1）：没有输出且有错误输出时按失败处理
    let allowed = status.code().is_some_and(|c| ok.contains(&c)) && !(out.is_empty() && !err.is_empty());
    if !truncated && !status.success() && !allowed {
        return Err(if err.is_empty() { Error::Exit(status.to_string()) } else { Error::Git(err) });
    }
    Ok((String::from_utf8_lossy(&out).into_owned(), truncated))
}

// ---------------- 仓库根与分支（只读 .git 下的文件，不调用 git 进程） ----------------

/// 从 dir 向上查找 `.git`（目录或 worktree / submodule 的 gitdir 文件），读其 HEAD 解析分支；返回 (`.git` 所在目录即仓库根, 分支)
pub fn find_head(dir: &Path) -> Option<(PathBuf, String)> {
    let root = dir.ancestors().find(|d| d.join(".git").exists())?;
    let dot = root.join(".git");
    let git_dir = if dot.is_dir() { dot } else { parse_gitdir(&std::fs::read_to_string(&dot).ok()?, root)? };
    Some((root.to_path_buf(), parse_head(&std::fs::read_to_string(git_dir.join("HEAD")).ok()?)?))
}

/// HEAD 内容：`ref: refs/heads/<name>` 为分支名；十六进制提交哈希为分离 HEAD，取前 7 位；其它为 None
pub fn parse_head(content: &str) -> Option<String> {
    let s = content.trim();
    if let Some(name) = s.strip_prefix("ref: refs/heads/") {
        return (!name.is_empty()).then(|| name.to_string());
    }
    (s.len() >= 40 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s[..7].to_string())
}

/// `.git` 文件内容 `gitdir: <path>`；相对路径相对于该文件所在目录 base 解析
pub fn parse_gitdir(content: &str, base: &Path) -> Option<PathBuf> {
    let path = content.trim().strip_prefix("gitdir:")?.trim();
    // join 遇到绝对路径时直接取该路径
    (!path.is_empty()).then(|| base.join(path))
}

// ---------------- git 操作（调用 git 进程） ----------------

/// 提交哈希：4 ~ 64 位十六进制
pub fn valid_hash(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 提交改动：(numstat, diff, diff 是否截断)。合并提交按第一父提交比较；不调用外部 diff / textconv 程序
pub fn show_commit(dir: &str, hash: &str) -> Result<(String, String, bool), Error> {
    if !valid_hash(hash) {
        return Err(Error::BadHash(hash.to_string()));
    }
    let base = ["show", "--format=", "--no-color", "--no-ext-diff", "--no-textconv", "--diff-merges=first-parent"];
    let numstat = run(dir, &[&base[..], &["--numstat", hash, "--"]].concat(), None, &[])?.0;
    let (diff, truncated) = run(dir, &[&base[..], &["--patch", hash, "--"]].concat(), Some(DIFF_LIMIT), &[])?;
    Ok((numstat, diff, truncated))
}

/// 当前分支最近 limit 个提交（无图形列），格式同 [`LOG_FORMAT`]；空仓库为空串
pub fn log(dir: &str, limit: u32) -> Result<String, Error> {
    // rev-parse --verify 找不到 HEAD 时退出码为 1、输出为空
    if run(dir, &["rev-parse", "-q", "--verify", "HEAD"], None, &[1])?.0.trim().is_empty() {
        return Ok(String::new());
    }
    let n = format!("-n{limit}");
    run(dir, &["log", "--no-show-signature", &n, LOG_FORMAT, "--"], None, &[]).map(|r| r.0)
}

/// 未提交改动（status + numstat）与最近 log_n 个提交的原始输出。
/// 空仓库（还没有提交）numstat 改比暂存区、log 为空
pub fn changes(dir: &str, log_n: u32) -> Result<GitChanges, Error> {
    let status = run(dir, &["status", "--porcelain=v2", "-z", "--branch", "-unormal"], None, &[])?.0;
    let initial = status.starts_with("# branch.oid (initial)\0");
    let base = if initial { "--cached" } else { "HEAD" };
    let numstat = run(dir, &["diff", "--numstat", "-z", base, "--"], None, &[])?.0;
    let n = format!("-n{log_n}");
    let log = if initial { String::new() } else { run(dir, &["log", "--no-show-signature", &n, LOG_FORMAT, "--"], None, &[])?.0 };
    let root = find_head(Path::new(dir)).map_or_else(|| dir.to_string(), |(r, _)| r.to_string_lossy().into_owned());
    Ok(GitChanges { root, status, numstat, log })
}

/// 单个文件的 diff：(内容, 是否截断)。已跟踪文件比 HEAD（空仓库比暂存区）；untracked 与 /dev/null 比较（有差异时 git 退出码为 1）
pub fn file_diff(dir: &str, path: &str, untracked: bool) -> Result<(String, bool), Error> {
    let base = ["diff", "--no-color", "--no-ext-diff"];
    if untracked {
        return run(dir, &[&base[..], &["--no-index", "--", "/dev/null", path]].concat(), Some(DIFF_LIMIT), &[1]);
    }
    // rev-parse --verify 找不到 HEAD 时退出码为 1、输出为空
    let head = !run(dir, &["rev-parse", "-q", "--verify", "HEAD"], None, &[1])?.0.trim().is_empty();
    run(dir, &[&base[..], &[if head { "HEAD" } else { "--cached" }, "--", path]].concat(), Some(DIFF_LIMIT), &[])
}

// ---------------- 输出解析（规则同 GUI 的 git-utils.ts） ----------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub hash: String,
    pub short: String,
    /// %D 的各项，如 `HEAD -> main`、`origin/main`、`tag: v1`
    pub refs: Vec<String>,
    pub author: String,
    /// 作者时间，Unix 秒
    pub time: u64,
    pub subject: String,
}

/// 文件增删行数；二进制文件为 None
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStat {
    pub path: String,
    pub add: Option<u64>,
    pub del: Option<u64>,
}

/// 未提交的一个文件
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    /// 重命名前的路径
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orig: Option<String>,
    /// modified / added / deleted / renamed / conflict / untracked
    pub kind: String,
    /// 相对 HEAD 的增删行数；二进制与未跟踪文件为 None
    pub add: Option<u64>,
    pub del: Option<u64>,
}

/// 一个仓库的未提交改动（同 git-utils.ts 的 ChangeSet，提交另由 [`parse_log`] 解析）
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSet {
    pub root: String,
    /// 分支名；分离 HEAD 时为提交前 7 位
    pub branch: String,
    pub ahead: u64,
    pub behind: u64,
    pub files: Vec<ChangedFile>,
    /// 已跟踪文件的增删行数合计（未跟踪文件不计）
    pub added: u64,
    pub deleted: u64,
}

fn num(s: &str) -> Option<u64> {
    s.parse().ok()
}

/// `git log --format=%H%x1f%h%x1f%D%x1f%an%x1f%at%x1f%s` 的输出，每行一个提交
pub fn parse_log(out: &str) -> Vec<Commit> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.splitn(6, '\x1f');
            let (hash, short, decor, author, time, subject) = (f.next()?, f.next()?, f.next()?, f.next()?, f.next()?, f.next()?);
            let refs = if decor.is_empty() { vec![] } else { decor.split(", ").map(String::from).collect() };
            Some(Commit { hash: hash.into(), short: short.into(), refs, author: author.into(), time: num(time).unwrap_or(0), subject: subject.into() })
        })
        .collect()
}

/// `git show --numstat --format=` 的输出：`增\t删\t路径`，二进制文件为 `-\t-\t路径`
pub fn parse_numstat(out: &str) -> Vec<FileStat> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.splitn(3, '\t');
            let (a, d, path) = (f.next()?, f.next()?, f.next()?);
            let ok = |s: &str| s == "-" || num(s).is_some();
            (ok(a) && ok(d)).then(|| FileStat { path: path.into(), add: num(a), del: num(d) })
        })
        .collect()
}

/// 合并 status（`--porcelain=v2 -z --branch`）与 numstat（`diff --numstat -z`）：文件列表以 status 为准，行数按（新）路径取 numstat
pub fn build_changes(raw: &GitChanges) -> ChangeSet {
    let mut cs = ChangeSet { root: raw.root.clone(), ..Default::default() };
    let (mut head, mut oid) = (None, None);
    let items: Vec<&str> = raw.status.split('\0').collect();
    let mut i = 0;
    while i < items.len() {
        let it = items[i];
        i += 1;
        if let Some(h) = it.strip_prefix("# ") {
            let (key, v) = h.split_once(' ').unwrap_or((h, ""));
            match key {
                "branch.oid" => oid = (v != "(initial)").then(|| v.to_string()),
                "branch.head" => head = (v != "(detached)").then(|| v.to_string()),
                "branch.ab" => {
                    if let Some((a, b)) = v.strip_prefix('+').and_then(|v| v.split_once(" -")) {
                        cs.ahead = num(a).unwrap_or(0);
                        cs.behind = num(b).unwrap_or(0);
                    }
                }
                _ => {}
            }
            continue;
        }
        // 各类条目的字段数固定，路径为最后一个字段（可含空格）
        let field = |n: usize| {
            let parts: Vec<&str> = it.splitn(n + 1, ' ').collect();
            (parts.get(1).copied().unwrap_or(""), parts.get(n).copied().unwrap_or("").to_string())
        };
        let (path, orig, kind) = match it.get(..2) {
            Some("1 ") => {
                let (xy, path) = field(8);
                (path, None, if xy.contains('D') { "deleted" } else if xy.contains('A') { "added" } else { "modified" })
            }
            Some("2 ") => {
                let (_, path) = field(9);
                let orig = items.get(i).map(|s| s.to_string());
                i += 1;
                (path, orig, "renamed")
            }
            Some("u ") => (field(10).1, None, "conflict"),
            Some("? ") => (it[2..].to_string(), None, "untracked"),
            _ => continue,
        };
        cs.files.push(ChangedFile { path, orig, kind: kind.into(), add: None, del: None });
    }
    // numstat -z：`增\t删\t路径`，重命名为 `增\t删\t` 后跟原路径、新路径两项
    let items: Vec<&str> = raw.numstat.split('\0').collect();
    let mut nums = std::collections::HashMap::new();
    let mut i = 0;
    while i < items.len() {
        let mut f = items[i].splitn(3, '\t');
        i += 1;
        let (Some(a), Some(d), Some(p)) = (f.next(), f.next(), f.next()) else { continue };
        let path = if p.is_empty() {
            i += 2;
            items.get(i - 1).copied().unwrap_or("")
        } else {
            p
        };
        nums.insert(path, (num(a), num(d)));
    }
    for f in &mut cs.files {
        if f.kind != "untracked"
            && let Some(&(a, d)) = nums.get(f.path.as_str())
        {
            (f.add, f.del) = (a, d);
        }
    }
    cs.added = cs.files.iter().filter_map(|f| f.add).sum();
    cs.deleted = cs.files.iter().filter_map(|f| f.del).sum();
    cs.branch = head.or_else(|| oid.map(|o| o.chars().take(7).collect())).unwrap_or_default();
    cs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_change_set() {
        let raw = GitChanges {
            root: "/r".into(),
            status: "# branch.oid abcdef0123\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 h1 h2 src/a b.ts\0\
1 D. N... 100644 000000 000000 h1 h2 gone.txt\0\
1 A. N... 000000 100644 100644 h1 h2 added.txt\0\
2 R. N... 100644 100644 100644 h1 h2 R100 new.txt\0old.txt\0\
u UU N... 100644 100644 100644 100644 h1 h2 h3 c.txt\0\
? 新/\0! ignored\0"
                .into(),
            numstat: "2\t1\tsrc/a b.ts\x000\t3\tgone.txt\x00-\t-\tadded.txt\x000\t0\t\0old.txt\0new.txt\0".into(),
            log: String::new(),
        };
        let cs = build_changes(&raw);
        assert_eq!((cs.branch.as_str(), cs.ahead, cs.behind), ("main", 2, 1));
        let kinds: Vec<_> = cs.files.iter().map(|f| (f.path.as_str(), f.kind.as_str(), f.add, f.del)).collect();
        assert_eq!(
            kinds,
            [
                ("src/a b.ts", "modified", Some(2), Some(1)),
                ("gone.txt", "deleted", Some(0), Some(3)),
                ("added.txt", "added", None, None),
                ("new.txt", "renamed", Some(0), Some(0)),
                ("c.txt", "conflict", None, None),
                ("新/", "untracked", None, None),
            ]
        );
        assert_eq!(cs.files[3].orig.as_deref(), Some("old.txt"));
        assert_eq!((cs.added, cs.deleted), (2, 4));
        // 分离 HEAD 取 oid 前 7 位；空仓库没有 oid 时为 branch.head
        let cs = build_changes(&GitChanges { status: "# branch.oid 0123456789abcdef\0# branch.head (detached)\0".into(), ..Default::default() });
        assert_eq!(cs.branch, "0123456");
        let cs = build_changes(&GitChanges { status: "# branch.oid (initial)\0# branch.head main\0".into(), ..Default::default() });
        assert_eq!((cs.branch.as_str(), cs.files.len()), ("main", 0));
    }

    #[test]
    fn parses_log_and_numstat() {
        let log = parse_log("h1\x1fs1\x1fHEAD -> main, tag: v1\x1fme\x1f100\x1fa\x1fb\nh2\x1fs2\x1f\x1fyou\x1f50\x1fsecond\n\nbad line\n");
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].refs, ["HEAD -> main", "tag: v1"]);
        assert_eq!((log[0].subject.as_str(), log[0].time), ("a\x1fb", 100));
        assert!(log[1].refs.is_empty());
        let ns = parse_numstat("1\t2\ta.txt\n-\t-\tbin.png\n\nx\ty\tz\n");
        assert_eq!(ns, [FileStat { path: "a.txt".into(), add: Some(1), del: Some(2) }, FileStat { path: "bin.png".into(), add: None, del: None }]);
    }

    #[test]
    fn json_shape() {
        let f = ChangedFile { path: "a".into(), orig: None, kind: "untracked".into(), add: None, del: None };
        assert_eq!(serde_json::to_value(&f).unwrap(), serde_json::json!({ "path": "a", "kind": "untracked", "add": null, "del": null }));
    }
}
