//! 远程会话的 git 查询（本地会话由 Tauri 直接运行 git）：在会话所属的已有 SSH 连接上开一个 exec channel，
//! 一段 sh 脚本跑完全部命令，各段以 `@名称` 行分隔。
//!
//! 引号：脚本单行、不含单引号 / 反斜杠 / `!`，经 `sh -c '<脚本>' sh <参数…>` 运行，目录等参数以 `"$1"` 引用、
//! 由登录 shell 按单引号转义传入，兼容 fish / csh 等非 POSIX 登录 shell（同 stats.rs 的做法）。
//! 所有 git 命令带 --no-optional-locks，不抢 index.lock、不与终端里的 git 冲突。

use std::time::Duration;

use proto::{GitChanges, GitOp, GitOut, ServerMsg};

use crate::{
    fs,
    session::Shared,
    ssh::{self, Conn},
};

/// 远程命令的最长等待；GUI 的等待更长，超时未回复才判定为旧 daemon
const TIMEOUT: Duration = Duration::from_secs(8);
/// 单文件 diff 的字节上限（与 Tauri 的 DIFF_LIMIT 一致），超出截断
const DIFF_LIMIT: usize = 512 * 1024;
/// 改动列表输出的字节上限（防止巨量未跟踪文件撑爆内存）
const CHANGES_LIMIT: usize = 8 * 1024 * 1024;

/// 公共前缀：进入 $1、确认装了 git；G 为带固定选项的 git 命令
const PRELUDE: &str = "export LC_ALL=C; cd \"$1\" 2>/dev/null || { echo @nodir; exit 0; }; \
command -v git >/dev/null 2>&1 || { echo @nogit; exit 0; }; G=\"git --no-optional-locks -c core.quotepath=false\"; ";

/// 改动列表：$2 为提交数。@root 段失败时为 git 的错误输出（不是仓库、safe.directory 等）并结束；
/// status / numstat 为 -z 输出，段尾补一个换行使下一个 @ 标记独占一行；空仓库 numstat 比暂存区、log 为空
const CHANGES: &str = "echo @root; $G rev-parse --show-toplevel 2>&1 || exit 0; \
echo @status; $G status --porcelain=v2 -z --branch -unormal 2>/dev/null || { echo; echo @statusfail; exit 0; }; echo; \
echo @numstat; if $G rev-parse -q --verify HEAD >/dev/null; then $G diff --numstat -z HEAD --; else $G diff --numstat -z --cached --; fi; echo; \
echo @log; $G log --no-show-signature -n\"$2\" --format=%H%x1f%h%x1f%D%x1f%an%x1f%at%x1f%s -- 2>/dev/null; true";

/// 单文件 diff：$2 为路径，$3 为 1 时是未跟踪文件（与 /dev/null 比较）；已跟踪比 HEAD，空仓库比暂存区
const DIFF: &str = "echo @diff; if [ \"$3\" = 1 ]; then $G diff --no-color --no-ext-diff --no-index -- /dev/null \"$2\"; \
elif $G rev-parse -q --verify HEAD >/dev/null; then $G diff --no-color --no-ext-diff HEAD -- \"$2\"; \
else $G diff --no-color --no-ext-diff --cached -- \"$2\"; fi; true";

/// 处理 ClientMsg::Git（异步任务）：仅远程会话，结果以 ServerMsg::Git 回复
pub async fn run(state: Shared, req_id: u64, backend: fs::Backend, op: GitOp) {
    let result = match backend {
        fs::Backend::Local => Err(tr!("git.local_only")),
        fs::Backend::Remote { conn, .. } => match conn.upgrade() {
            Some(conn) => remote(&conn, &op).await,
            None => Err(tr!("common.ssh_disconnected")),
        },
    };
    let (out, error) = match result {
        Ok(o) => (Some(o), None),
        Err(e) => (None, Some(e)),
    };
    state.lock().unwrap().send(&ServerMsg::Git { req_id, out, error });
}

async fn remote(conn: &Conn, op: &GitOp) -> Result<GitOut, String> {
    let (cmd, limit) = command(op);
    let (out, truncated) = ssh::exec(conn, &cmd, Some(limit), TIMEOUT).await?;
    match op {
        GitOp::Changes { dir, .. } => parse_changes(&out, dir).map(GitOut::Changes),
        GitOp::Diff { dir, .. } => parse_diff(&out, dir).map(|text| GitOut::Diff { text, truncated }),
    }
}

/// 交给登录 shell 执行的命令行与输出上限
pub fn command(op: &GitOp) -> (String, usize) {
    let q = ssh::shell_quote;
    match op {
        GitOp::Changes { dir, log_n } => (format!("sh -c '{PRELUDE}{CHANGES}' sh {} {}", q(dir), (*log_n).clamp(1, 100)), CHANGES_LIMIT),
        GitOp::Diff { dir, path, untracked } => (format!("sh -c '{PRELUDE}{DIFF}' sh {} {} {}", q(dir), q(path), u8::from(*untracked)), DIFF_LIMIT),
    }
}

/// 输出中 `@名称` 标记行（在开头或紧跟换行）之后的位置；rc 文件可能在前面输出别的内容，所以不要求在开头
fn after_marker(out: &str, name: &str) -> Option<usize> {
    let m = format!("@{name}\n");
    if out.starts_with(&m) {
        return Some(m.len());
    }
    out.find(&format!("\n{m}")).map(|i| i + m.len() + 1)
}

/// PRELUDE 的失败标记
fn prelude_error(out: &str, dir: &str) -> Option<String> {
    if after_marker(out, "nodir").is_some() {
        return Some(tr!("common.dir_not_found", dir = dir));
    }
    after_marker(out, "nogit").is_some().then(|| tr!("git.not_installed"))
}

/// 解析改动列表脚本的输出
pub fn parse_changes(out: &str, dir: &str) -> Result<GitChanges, String> {
    if let Some(e) = prelude_error(out, dir) {
        return Err(e);
    }
    let start = after_marker(out, "root").ok_or_else(|| tr!("git.failed", error = out.trim()))?;
    let rest = &out[start..];
    let Some(i) = rest.find("\n@status\n") else {
        // rev-parse 失败：@root 段为 git 的错误输出
        let err = rest.trim();
        return Err(if err.contains("dubious ownership") {
            tr!("git.unsafe_dir", dir = dir)
        } else if err.contains("not a git repository") {
            tr!("git.not_repo", dir = dir)
        } else {
            tr!("git.failed", error = err)
        });
    };
    let root = rest[..i].trim().to_string();
    let rest = &rest[i + "\n@status\n".len()..];
    if rest.contains("\n@statusfail\n") {
        return Err(tr!("git.status_failed"));
    }
    // 依次切出各段；输出被截断时缺失的段为空
    let (status, rest) = rest.split_once("\n@numstat\n").unwrap_or((rest, ""));
    let (numstat, log) = rest.split_once("\n@log\n").unwrap_or((rest, ""));
    Ok(GitChanges { root, status: status.into(), numstat: numstat.into(), log: log.into() })
}

/// 解析单文件 diff 脚本的输出
pub fn parse_diff(out: &str, dir: &str) -> Result<String, String> {
    if let Some(e) = prelude_error(out, dir) {
        return Err(e);
    }
    let start = after_marker(out, "diff").ok_or_else(|| tr!("git.failed", error = out.trim()))?;
    Ok(out[start..].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_safe_for_any_login_shell() {
        // 单引号会提前结束 sh -c '…'；fish 在单引号内解释 \\ 与 \'；csh 的 ! 为历史替换且不接受跨行引号
        for s in [PRELUDE, CHANGES, DIFF] {
            assert!(!s.contains(['\'', '\\', '!', '\n']), "{s}");
        }
        // 每条 git 命令都经 $G（带 --no-optional-locks）
        for s in [CHANGES, DIFF] {
            assert!(!s.contains("git "), "{s}");
        }
        assert!(PRELUDE.contains("git --no-optional-locks"));
    }

    #[test]
    fn command_quotes_arguments() {
        let (cmd, limit) = command(&GitOp::Changes { dir: "/srv/it's here".into(), log_n: 500 });
        assert!(cmd.starts_with("sh -c 'export LC_ALL=C;"));
        assert!(cmd.ends_with(r#"' sh '/srv/it'\''s here' 100"#), "{cmd}");
        assert_eq!(limit, CHANGES_LIMIT);
        let (cmd, limit) = command(&GitOp::Diff { dir: "/r".into(), path: "a b.txt".into(), untracked: true });
        assert!(cmd.ends_with("' sh '/r' 'a b.txt' 1"), "{cmd}");
        assert_eq!(limit, DIFF_LIMIT);
    }

    #[test]
    fn parses_changes_sections() {
        let out = "motd from bashrc\n@root\n/srv/app\n@status\n# branch.oid abc\0# branch.head main\0? new.txt\0\n@numstat\n1\t2\ta.txt\0\n@log\nh\x1fs\x1f\x1fme\x1f1\x1fmsg\n";
        let c = parse_changes(out, "/srv/app/sub").unwrap();
        assert_eq!(c.root, "/srv/app");
        assert_eq!(c.status, "# branch.oid abc\0# branch.head main\0? new.txt\0");
        assert_eq!(c.numstat, "1\t2\ta.txt\0");
        assert_eq!(c.log, "h\x1fs\x1f\x1fme\x1f1\x1fmsg\n");
        // 干净的仓库：status 只有分支头，numstat / log 为空
        let c = parse_changes("@root\n/r\n@status\n# branch.oid (initial)\0\n@numstat\n\n@log\n", "/r").unwrap();
        assert_eq!((c.numstat.as_str(), c.log.as_str()), ("", ""));
        // 截断：缺失的段为空
        let c = parse_changes("@root\n/r\n@status\n? a\0? b", "/r").unwrap();
        assert_eq!((c.status.as_str(), c.numstat.as_str()), ("? a\0? b", ""));
    }

    #[test]
    fn changes_errors_are_clear() {
        assert_eq!(parse_changes("@nodir\n", "/x").unwrap_err(), tr!("common.dir_not_found", dir = "/x"));
        assert_eq!(parse_changes("@nogit\n", "/x").unwrap_err(), tr!("git.not_installed"));
        let unsafe_out = "@root\nfatal: detected dubious ownership in repository at '/x'\nTo add an exception ...\n";
        assert!(parse_changes(unsafe_out, "/x").unwrap_err().contains("safe.directory /x"));
        let not_repo = "@root\nfatal: not a git repository (or any of the parent directories): .git\n";
        assert_eq!(parse_changes(not_repo, "/x").unwrap_err(), tr!("git.not_repo", dir = "/x"));
        assert!(parse_changes("@root\n/x\n@status\n\n@statusfail\n", "/x").unwrap_err().contains("2.11"));
        assert!(parse_changes("garbage", "/x").unwrap_err().contains("garbage"));
        assert_eq!(parse_diff("@diff\n+a\n", "/x").unwrap(), "+a\n");
        assert_eq!(parse_diff("@nogit\n", "/x").unwrap_err(), tr!("git.not_installed"));
    }

    /// 找 POSIX sh：PATH 中的 sh，或 Git for Windows 自带的 sh.exe；都没有时跳过真实执行用例
    fn find_sh() -> Option<std::path::PathBuf> {
        let ok = |p: &std::path::Path| std::process::Command::new(p).args(["-c", "true"]).output().is_ok_and(|o| o.status.success());
        if ok("sh".as_ref()) {
            return Some("sh".into());
        }
        let out = std::process::Command::new("git").arg("--exec-path").output().ok()?;
        let exec = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        // <Git>/mingw64/libexec/git-core → <Git>/bin/sh.exe
        let sh = exec.ancestors().nth(3)?.join("bin").join("sh.exe");
        ok(&sh).then_some(sh)
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// 像登录 shell 那样解释整条命令行：`sh -c "<command>"`
    fn run_like_login_shell(sh: &std::path::Path, op: &GitOp) -> String {
        let (cmd, _) = command(op);
        let out = std::process::Command::new(sh).args(["-c", &cmd]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn script_runs_in_real_repo() {
        let Some(sh) = find_sh() else {
            eprintln!("跳过：未找到 sh");
            return;
        };
        // 目录名带空格与单引号，检验引号转义
        let d = std::env::temp_dir().join(format!("tether-daemon-git-{}", std::process::id())).join("it's repo");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let dir = d.to_str().unwrap().replace('\\', "/");
        // 空仓库：numstat 比暂存区、log 为空
        git(&d, &["init", "-q"]);
        std::fs::write(d.join("a.txt"), "1\n").unwrap();
        git(&d, &["add", "."]);
        let c = parse_changes(&run_like_login_shell(&sh, &GitOp::Changes { dir: format!("{dir}/sub"), log_n: 5 }), &dir).unwrap();
        assert!(c.status.starts_with("# branch.oid (initial)\0"), "{c:?}");
        assert_eq!((c.numstat.as_str(), c.log.as_str()), ("1\t0\ta.txt\0", ""));
        git(&d, &["commit", "-q", "-m", "first"]);
        std::fs::write(d.join("a.txt"), "2\n").unwrap();
        std::fs::write(d.join("new file.txt"), "n\n").unwrap();
        // 子目录也取到仓库根
        let c = parse_changes(&run_like_login_shell(&sh, &GitOp::Changes { dir: format!("{dir}/sub"), log_n: 5 }), &dir).unwrap();
        assert!(c.root.ends_with("it's repo"), "{}", c.root);
        assert!(c.status.contains("? new file.txt\0") && c.status.contains("# branch.head main\0"), "{:?}", c.status);
        assert_eq!(c.numstat, "1\t1\ta.txt\0");
        assert_eq!(c.log.lines().count(), 1);
        let diff = parse_diff(&run_like_login_shell(&sh, &GitOp::Diff { dir: dir.clone(), path: "a.txt".into(), untracked: false }), &dir).unwrap();
        assert!(diff.contains("-1") && diff.contains("+2"), "{diff}");
        let diff = parse_diff(&run_like_login_shell(&sh, &GitOp::Diff { dir: dir.clone(), path: "new file.txt".into(), untracked: true }), &dir).unwrap();
        assert!(diff.contains("+n"), "{diff}");
        // 不存在的目录、不是仓库
        let missing = run_like_login_shell(&sh, &GitOp::Changes { dir: format!("{dir}/nope"), log_n: 5 });
        assert_eq!(parse_changes(&missing, "nope").unwrap_err(), tr!("common.dir_not_found", dir = "nope"));
        let outside = d.parent().unwrap().join("plain");
        std::fs::create_dir_all(&outside).unwrap();
        let out = run_like_login_shell(&sh, &GitOp::Changes { dir: outside.to_str().unwrap().replace('\\', "/"), log_n: 5 });
        assert_eq!(parse_changes(&out, "plain").unwrap_err(), tr!("git.not_repo", dir = "plain"), "{out}");
        std::fs::remove_dir_all(d.parent().unwrap()).unwrap();
    }
}
