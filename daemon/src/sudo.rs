//! SSH 文件树“以 root 浏览”：在同一 SSH 连接上开 exec channel，经 sudo 启动 sftp-server，把 channel stream 交给 SftpSession，
//! 代替 sftp 子系统。提权会话缓存在 Conn 内（与普通会话分开），关闭开关即丢弃，连接断开随之失效。
//!
//! 流程：先 `sudo -n` 探测（免密 / 已缓存凭据，同时确认找得到 sftp-server）；需要密码时先试凭据管理器中保存的该主机登录密码，
//! 不对再弹窗询问 sudo 密码。密码用单独的 exec `sudo -k -S -p ''` 先验证，通过后再开 sftp channel；`-k` 忽略已缓存的凭据，
//! 保证 sudo 一定先读走 stdin 首行的密码，剩余数据才交给 sftp-server。密码只经 channel stdin 传递，不进命令行、日志与错误信息。

use std::{sync::Arc, time::Duration};

use proto::{PromptKind, ServerMsg};
use russh::ChannelMsg;
use russh_sftp::client::SftpSession;

use crate::{
    fs,
    session::Shared,
    ssh::{self, Conn, CredStore},
};

/// 单次 sudo 探测 / 验证、启动 SFTP 的最长等待（sudo 密码错误时自带约 2 秒延迟）
const RUN_TIMEOUT: Duration = Duration::from_secs(20);
/// 依次尝试的 sftp-server 路径：Debian/Ubuntu、RHEL/Fedora、Arch/Alpine/SUSE、BSD 等
const SFTP_SERVERS: [&str; 5] = [
    "/usr/lib/openssh/sftp-server",
    "/usr/libexec/openssh/sftp-server",
    "/usr/lib/ssh/sftp-server",
    "/usr/libexec/sftp-server",
    "/usr/local/libexec/sftp-server",
];
/// 找不到 sftp-server 时脚本写到 stderr 的标记
const NO_SFTP: &str = "tether: sftp-server not found";

/// sudo 失败的原因（按 LC_ALL=C 下 sudo 的英文 stderr 分类）
#[derive(Debug, PartialEq)]
enum Failure {
    NeedPassword,
    WrongPassword,
    NotAllowed,
    RequireTty,
    NoSudo,
    NoSftpServer,
    /// 其它：stderr 摘要
    Other(String),
}

impl Failure {
    fn message(&self) -> String {
        match self {
            Failure::NeedPassword => tr!("sudo.need_password"),
            Failure::WrongPassword => tr!("sudo.wrong_password"),
            Failure::NotAllowed => tr!("sudo.not_allowed"),
            Failure::RequireTty => tr!("sudo.requiretty"),
            Failure::NoSudo => tr!("sudo.no_sudo"),
            Failure::NoSftpServer => tr!("sudo.no_sftp_server", paths = SFTP_SERVERS.join(&tr!("common.list_sep"))),
            Failure::Other(s) => tr!("sudo.failed", error = s),
        }
    }
}

/// 处理 ClientMsg::FsRoot（异步任务）：开启时提权该会话所属连接的文件树，关闭时丢弃提权会话；
/// 先推送会话列表（fs_root 变化）再以 FsResult 回复
pub async fn toggle(state: Shared, req_id: u64, backend: fs::Backend, enabled: bool) {
    let result = match backend {
        fs::Backend::Local => Err(tr!("sudo.local")),
        fs::Backend::Remote { conn, .. } => match conn.upgrade().filter(|c| !c.handle.is_closed()) {
            None => Err(tr!("common.ssh_disconnected")),
            Some(conn) if !enabled => {
                conn.set_root(None);
                log!("文件树恢复普通身份：{}", conn.label);
                Ok(())
            }
            Some(conn) if conn.is_root() => Ok(()),
            Some(conn) => elevate(&state, &conn).await,
        },
    };
    if let Err(e) = &result {
        log!("文件树以 root 浏览失败（请求 {req_id}）：{e}");
    }
    let st = state.lock().unwrap();
    st.push_sessions();
    st.send(&ServerMsg::FsResult { req_id, error: result.err() });
}

async fn elevate(state: &Shared, conn: &Conn) -> Result<(), String> {
    let probe = run(conn, &command(false, false), None).await?;
    let password = match classify(probe.0, &probe.1) {
        Ok(()) => None,
        Err(Failure::NeedPassword) => Some(password(state, conn).await?),
        Err(f) => return Err(f.message()),
    };
    let sftp = open(conn, password.as_deref()).await?;
    conn.set_root(Some(Arc::new(sftp)));
    log!("文件树已以 root 浏览：{}（{}）", conn.label, if password.is_some() { "sudo 密码" } else { "免密 sudo" });
    Ok(())
}

/// 取得可用的 sudo 密码：先试凭据管理器中保存的该主机登录密码，不对再弹窗询问（最多 3 次）；
/// sudo 因密码以外的原因失败（不在 sudoers 等）立即报错
async fn password(state: &Shared, conn: &Conn) -> Result<String, String> {
    let mut message = tr!("sudo.prompt", host = conn.label);
    if let Some(saved) = ssh::Keyring.get(&conn.label) {
        match verify(conn, &saved).await? {
            Ok(()) => {
                log!("已保存的登录密码通过 sudo 验证：{}", conn.label);
                return Ok(saved);
            }
            Err(Failure::WrongPassword) => message = tr!("sudo.saved_rejected", host = conn.label),
            Err(f) => return Err(f.message()),
        }
    }
    for _ in 0..3 {
        let (password, _) = ssh::ask(state, PromptKind::Sudo, &conn.label, message, None).await?;
        match verify(conn, &password).await? {
            Ok(()) => return Ok(password),
            Err(Failure::WrongPassword) => message = tr!("sudo.retry", host = conn.label),
            Err(f) => return Err(f.message()),
        }
    }
    Err(tr!("sudo.too_many"))
}

/// 用密码执行一次 sudo（同时确认 sftp-server 存在）；外层 Err 为 channel 层面的失败
async fn verify(conn: &Conn, password: &str) -> Result<Result<(), Failure>, String> {
    let (status, stderr) = run(conn, &command(true, false), Some(password)).await?;
    Ok(classify(status, &stderr))
}

/// sudo 命令行：password 为 false 用 `sudo -n`（不询问密码，需要时失败）；为 true 用 `sudo -k -S -p ''`（忽略已缓存凭据，
/// 从 stdin 首行读密码，不输出提示）。exec 为 true 时 exec 找到的 sftp-server，否则只检查其存在。
/// 前缀 `env LC_ALL=C` 使 sudo 输出英文错误信息以便分类（不依赖登录 shell 的语法）
fn command(password: bool, exec: bool) -> String {
    let action = if exec { r#"exec "$p""# } else { "exit 0" };
    let script = format!(r#"for p in {}; do [ -x "$p" ] && {action}; done; echo "{NO_SFTP}" >&2; exit 127"#, SFTP_SERVERS.join(" "));
    let sudo = if password { "sudo -k -S -p ''" } else { "sudo -n" };
    format!("env LC_ALL=C {sudo} sh -c {}", ssh::shell_quote(&script))
}

/// 在连接上开 exec channel 运行命令；password 有值时先向 stdin 写入 `密码\n`。返回 (退出码, stderr)，stderr 中的密码替换为 ***
async fn run(conn: &Conn, cmd: &str, password: Option<&str>) -> Result<(Option<u32>, String), String> {
    let mut ch = conn.handle.channel_open_session().await.map_err(|e| tr!("common.open_channel_failed", error = e))?;
    ch.exec(true, cmd).await.map_err(|e| tr!("sudo.exec_failed", error = e))?;
    if let Some(p) = password {
        ch.data_bytes(format!("{p}\n").into_bytes()).await.map_err(|e| tr!("sudo.write_password_failed", error = e))?;
    }
    let _ = ch.eof().await;
    let read = async {
        let (mut status, mut err) = (None, Vec::new());
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::ExtendedData { data, ext: 1 } => err.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        (status, err)
    };
    let result = tokio::time::timeout(RUN_TIMEOUT, read).await;
    let _ = ch.close().await;
    let (status, err) = result.map_err(|_| tr!("sudo.timeout"))?;
    Ok((status, scrub(&String::from_utf8_lossy(&err), password)))
}

/// 开 sftp channel：sudo 启动 sftp-server（需要密码时先写入 stdin 首行，由 sudo 读走），channel stream 交给 SftpSession
async fn open(conn: &Conn, password: Option<&str>) -> Result<SftpSession, String> {
    let ch = conn.handle.channel_open_session().await.map_err(|e| tr!("common.open_sftp_channel_failed", error = e))?;
    ch.exec(true, command(password.is_some(), true)).await.map_err(|e| tr!("sudo.exec_failed", error = e))?;
    if let Some(p) = password {
        ch.data_bytes(format!("{p}\n").into_bytes()).await.map_err(|e| tr!("sudo.write_password_failed", error = e))?;
    }
    match tokio::time::timeout(RUN_TIMEOUT, SftpSession::new(ch.into_stream())).await {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(tr!("sudo.root_sftp_failed", error = e)),
        Err(_) => Err(tr!("sudo.root_sftp_timeout")),
    }
}

/// 按退出码与 stderr 分类 sudo 结果
fn classify(status: Option<u32>, stderr: &str) -> Result<(), Failure> {
    if status == Some(0) {
        return Ok(());
    }
    let s = stderr.to_ascii_lowercase();
    Err(if s.contains(NO_SFTP) {
        Failure::NoSftpServer
    } else if s.contains("incorrect password") || s.contains("try again") || s.contains("no password was provided") {
        Failure::WrongPassword
    } else if s.contains("password is required") {
        Failure::NeedPassword
    } else if s.contains("not in the sudoers") || s.contains("not allowed to") || s.contains("may not run sudo") {
        Failure::NotAllowed
    } else if s.contains("must have a tty") || s.contains("no tty present") {
        Failure::RequireTty
    } else if status == Some(127) && s.contains("sudo") && (s.contains("not found") || s.contains("no such file")) {
        Failure::NoSudo
    } else {
        Failure::Other(summary(status, stderr))
    })
}

/// stderr 前 3 个非空行（最多 200 字符）；为空时给出退出码
fn summary(status: Option<u32>, stderr: &str) -> String {
    let text = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).take(3).collect::<Vec<_>>().join(&tr!("common.sep"));
    match (text.is_empty(), status) {
        (false, _) => text.chars().take(200).collect(),
        (true, Some(code)) => tr!("sudo.exit_code", code = code),
        (true, None) => tr!("sudo.no_exit_status"),
    }
}

/// 防御：把 stderr 中可能回显的密码替换为 ***
fn scrub(stderr: &str, password: Option<&str>) -> String {
    match password {
        Some(p) if !p.is_empty() => stderr.replace(p, "***"),
        _ => stderr.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines() {
        let probe = command(false, false);
        assert!(probe.starts_with("env LC_ALL=C sudo -n sh -c '"), "{probe}");
        assert!(probe.contains(r#"[ -x "$p" ] && exit 0"#) && probe.contains("/usr/lib/openssh/sftp-server"));
        let open = command(true, true);
        assert!(open.starts_with("env LC_ALL=C sudo -k -S -p '' sh -c '"), "{open}");
        assert!(open.contains(r#"exec "$p""#) && open.contains(NO_SFTP));
        // 脚本整体单引号包裹，内部没有需要转义的单引号
        assert_eq!(open.matches('\'').count(), 4);
    }

    #[test]
    fn classify_results() {
        assert_eq!(classify(Some(0), "anything"), Ok(()));
        assert_eq!(classify(Some(1), "sudo: a password is required\n"), Err(Failure::NeedPassword));
        assert_eq!(classify(Some(1), "Sorry, try again.\nsudo: no password was provided\nsudo: 1 incorrect password attempt\n"), Err(Failure::WrongPassword));
        assert_eq!(classify(Some(1), "bob is not in the sudoers file.  This incident will be reported.\n"), Err(Failure::NotAllowed));
        assert_eq!(classify(Some(1), "Sorry, user bob is not allowed to execute '/bin/sh' as root on h.\n"), Err(Failure::NotAllowed));
        assert_eq!(classify(Some(1), "sudo: sorry, you must have a tty to run sudo\n"), Err(Failure::RequireTty));
        assert_eq!(classify(Some(127), "env: 'sudo': No such file or directory\n"), Err(Failure::NoSudo));
        assert_eq!(classify(Some(127), &format!("{NO_SFTP}\n")), Err(Failure::NoSftpServer));
        assert_eq!(classify(Some(1), "  weird\n\nerror  \n"), Err(Failure::Other("weird；error".into())));
        assert_eq!(classify(Some(2), ""), Err(Failure::Other("退出码 2".into())));
        assert_eq!(classify(None, ""), Err(Failure::Other("未返回退出状态".into())));
    }

    #[test]
    fn password_scrubbed() {
        assert_eq!(scrub("bad s3cret here", Some("s3cret")), "bad *** here");
        assert_eq!(scrub("x", Some("")), "x");
        assert_eq!(scrub("x", None), "x");
        assert!(!Failure::Other(summary(Some(1), &scrub("echo s3cret", Some("s3cret")))).message().contains("s3cret"));
    }
}
