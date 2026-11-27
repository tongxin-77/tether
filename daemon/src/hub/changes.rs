//! hub 能力 git（协议 8.6）：工作区摘要的常驻上报与变更面板的按需请求。
//! 工作区与桌面端侧栏一致：本地为启动目录所在仓库根（proto::git::find_head，不在仓库里为启动目录），SSH 为远端仓库根。
//! 本地直接运行 git（proto::git，与 GUI 的本地变更面板同一份实现），SSH 经会话已有连接运行远程脚本（crate::git，同 GUI 的远程变更面板）。

use std::{
    collections::HashMap,
    path::Path,
    sync::Arc,
    time::Duration,
};

use proto::{
    GitChanges, GitOp, GitOut,
    git::{self as pgit, ChangeSet},
};
use tokio::sync::Notify;

use super::link::{self, ChangeSummary, GitChangesOut, GitDiffOut, GitLogOut, GitShowOut, Up, Workspace};
use crate::{
    fs, git,
    session::{self, Shared, State},
};

/// 本地工作区的改动摘要每隔这么久重新计算（同桌面端「其他本地工作区」的轮询）；SSH 工作区只在会话列表变化时查询
const LOCAL_POLL: Duration = Duration::from_secs(30);
/// 会话列表变化后的防抖
const DEBOUNCE: Duration = Duration::from_secs(1);
/// gitChangesResult 的文件数上限、gitLogResult 的提交数
const MAX_FILES: usize = 1000;
const LOG_LIMIT: u32 = 100;

/// 一个会话的查询目标
#[derive(Clone)]
struct Target {
    id: u32,
    cwd: String,
    host: Option<String>,
    host_id: Option<String>,
    fs: fs::Backend,
}

fn targets(st: &State) -> Vec<Target> {
    st.sessions.values().filter_map(|s| target(st, s.info.id)).collect()
}

fn target(st: &State, id: u32) -> Option<Target> {
    let s = st.sessions.get(&id)?;
    Some(Target { id, cwd: s.info.cwd.clone(), host: s.info.host.clone(), host_id: s.info.host_id.clone(), fs: session::fs_backend(st, id)? })
}

/// 本地：启动目录所在仓库的根与分支；不在仓库里为 None
fn local_head(cwd: &str) -> Option<(String, String)> {
    pgit::find_head(Path::new(cwd)).map(|(root, branch)| (root.to_string_lossy().into_owned(), branch))
}

/// 比较用的本地路径：Windows 忽略大小写与分隔符差异、去掉末尾分隔符
fn norm(p: &str) -> String {
    let s = p.replace('/', "\\");
    let s = s.trim_end_matches('\\');
    (if s.ends_with(':') { format!("{s}\\") } else { s.to_string() }).to_lowercase()
}

/// 未提交改动与最近 log_n 个提交的原始输出：本地在仓库根运行 git，SSH 在启动目录运行远程脚本（结果带仓库根）
async fn query(t: &Target, log_n: u32) -> Result<GitChanges, String> {
    match &t.fs {
        fs::Backend::Local => {
            let Some((root, _)) = local_head(&t.cwd) else { return Err(tr!("git.not_repo", dir = t.cwd)) };
            tokio::task::spawn_blocking(move || pgit::changes(&root, log_n).map_err(|e| git::local_error(e, &root)))
                .await
                .map_err(|e| e.to_string())?
        }
        fs::Backend::Remote { conn, .. } => {
            let conn = conn.upgrade().ok_or_else(|| tr!("common.ssh_disconnected"))?;
            match git::remote(&conn, &GitOp::Changes { dir: t.cwd.clone(), log_n }).await? {
                GitOut::Changes(c) => Ok(c),
                GitOut::Diff { .. } => Err(tr!("git.failed", error = "unexpected reply")),
            }
        }
    }
}

fn summary(cs: &ChangeSet) -> ChangeSummary {
    ChangeSummary { files: cs.files.len(), added: cs.added, deleted: cs.deleted }
}

// ---------------- 工作区摘要 ----------------

/// SSH 会话启动目录的查询结果：(仓库根, 分支, 摘要)；None 为不是仓库 / 查询失败
type RemoteHead = Option<(String, String, ChangeSummary)>;

/// 常驻上报任务（双方都有 git 时随连接启动、断开时中止）：连接后立即算一次；会话列表变化防抖后全部重算，
/// 本地工作区另每 30 s 重算；与上次发送的相同则不发
pub async fn report(shared: Shared, changed: Arc<Notify>) {
    let mut remote: HashMap<(String, String), RemoteHead> = HashMap::new();
    let mut sent: Option<Vec<Workspace>> = None;
    let mut tick = tokio::time::interval(LOCAL_POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // 第一次（interval 立即触发）也查 SSH 工作区
    let mut all = true;
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = changed.notified() => {
                tokio::time::sleep(DEBOUNCE).await;
                all = true;
            }
        }
        let list = collect(&shared, &mut remote, all).await;
        all = false;
        if sent.as_ref() != Some(&list) {
            shared.lock().unwrap().hub.up(Up::Workspaces { workspaces: list.clone() });
            sent = Some(list);
        }
    }
}

/// 计算全部工作区；all 为 false 时 SSH 会话沿用上次的查询结果（还没查过的仍会查）
async fn collect(shared: &Shared, remote: &mut HashMap<(String, String), RemoteHead>, all: bool) -> Vec<Workspace> {
    let targets = targets(&shared.lock().unwrap());
    // SSH：按（主机, 启动目录）查询；连接已断开（如已退出的会话）时保留上次的结果
    for t in targets.iter().filter(|t| t.host.is_some()) {
        let key = (t.host_id.clone().or_else(|| t.host.clone()).unwrap_or_default(), t.cwd.clone());
        if !all && remote.contains_key(&key) {
            continue;
        }
        if matches!(&t.fs, fs::Backend::Remote { conn, .. } if conn.upgrade().is_none()) && remote.contains_key(&key) {
            continue;
        }
        let head = query(t, 1).await.ok().map(|raw| {
            let cs = pgit::build_changes(&raw);
            (cs.root.clone(), cs.branch.clone(), summary(&cs))
        });
        remote.insert(key, head);
    }
    remote.retain(|(h, cwd), _| targets.iter().any(|t| t.host.is_some() && t.host_id.as_ref().or(t.host.as_ref()) == Some(h) && &t.cwd == cwd));
    let remote = remote.clone();
    // 本地：find_head 只读文件；仓库的改动摘要运行 git，都放到阻塞线程
    tokio::task::spawn_blocking(move || group(&targets, &remote)).await.unwrap_or_default()
}

/// 按工作区分组（组按其第一个会话的先后），并计算本地仓库的改动摘要
fn group(targets: &[Target], remote: &HashMap<(String, String), RemoteHead>) -> Vec<Workspace> {
    let mut list: Vec<Workspace> = Vec::new();
    let mut keys: HashMap<String, usize> = HashMap::new();
    for t in targets {
        let (key, ws) = match &t.host {
            None => {
                let head = local_head(&t.cwd);
                let root = head.as_ref().map_or(t.cwd.clone(), |h| h.0.clone());
                let ws = Workspace { root: root.clone(), host: None, host_id: None, branch: head.map(|h| h.1), sessions: vec![], changes: None };
                (norm(&root), ws)
            }
            Some(host) => {
                let h = t.host_id.clone().unwrap_or_else(|| host.clone());
                let head = remote.get(&(h.clone(), t.cwd.clone())).cloned().flatten();
                let root = head.as_ref().map_or(t.cwd.clone(), |h| h.0.clone());
                let key = format!("ssh:{h}:{root}");
                let ws = Workspace { root, host: Some(host.clone()), host_id: t.host_id.clone(), branch: head.as_ref().map(|h| h.1.clone()), sessions: vec![], changes: head.map(|h| h.2) };
                (key, ws)
            }
        };
        let i = *keys.entry(key).or_insert_with(|| {
            list.push(ws);
            list.len() - 1
        });
        list[i].sessions.push(t.id);
    }
    for w in list.iter_mut().filter(|w| w.host.is_none() && w.branch.is_some()) {
        w.changes = pgit::changes(&w.root, 1).ok().map(|raw| summary(&pgit::build_changes(&raw)));
    }
    list
}

// ---------------- 按需请求 ----------------

/// 文件路径参数：仓库根相对路径，非空、不是绝对路径、不含 `..` 段（不能读仓库之外的文件）
fn valid_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 4096
        && !p.starts_with(['/', '\\'])
        && !p.contains(':')
        && !p.split(['/', '\\']).any(|seg| seg == "..")
        && !p.contains('\0')
}

type Failure = (&'static str, Option<String>);

/// 处理 gitChanges / gitDiff / gitLog / gitShow：复用允许接管开关；查询在异步任务中进行，结果经上行通道回复
pub fn request(shared: &Shared, st: &State, msg: link::Down) {
    let (request_id, session_id) = match &msg {
        link::Down::GitChanges { request_id, session_id }
        | link::Down::GitDiff { request_id, session_id, .. }
        | link::Down::GitLog { request_id, session_id }
        | link::Down::GitShow { request_id, session_id, .. } => (request_id.clone(), *session_id),
        _ => return,
    };
    let pre: Result<Target, Failure> = if !st.config.hub.allow_takeover {
        Err(("disabled", None))
    } else {
        target(st, session_id).ok_or(("not_found", None))
    };
    let shared = shared.clone();
    tokio::spawn(async move {
        let reply = match pre {
            Ok(t) => run(&t, &msg).await,
            Err(e) => Err(e),
        };
        if let Err((reason, message)) = &reply {
            log!("hub 变更面板请求失败（会话 #{session_id}，{reason}）{}", message.as_deref().unwrap_or(""));
        }
        shared.lock().unwrap().hub.up(result(&msg, request_id, reply));
    });
}

/// 成功时的回复内容
enum Out {
    Changes(GitChangesOut),
    Diff(GitDiffOut),
    Log(GitLogOut),
    Show(GitShowOut),
}

async fn run(t: &Target, msg: &link::Down) -> Result<Out, Failure> {
    let err = |e: String| ("error", Some(e));
    match msg {
        link::Down::GitChanges { .. } => {
            let mut set = pgit::build_changes(&query(t, 1).await.map_err(err)?);
            let truncated = set.files.len() > MAX_FILES;
            set.files.truncate(MAX_FILES);
            Ok(Out::Changes(GitChangesOut { set, truncated }))
        }
        link::Down::GitDiff { path, untracked, .. } => {
            if !valid_path(path) {
                return Err(("bad_path", Some(tr!("git.bad_path", path = path))));
            }
            let (text, truncated) = match &t.fs {
                fs::Backend::Local => {
                    let root = local_head(&t.cwd).ok_or_else(|| err(tr!("git.not_repo", dir = t.cwd)))?.0;
                    let (path, untracked) = (path.clone(), *untracked);
                    tokio::task::spawn_blocking(move || pgit::file_diff(&root, &path, untracked).map_err(|e| git::local_error(e, &root)))
                        .await
                        .map_err(|e| err(e.to_string()))?
                        .map_err(err)?
                }
                fs::Backend::Remote { conn, .. } => {
                    // 路径相对仓库根：先取远端仓库根（同桌面端用改动结果的 root）
                    let root = query(t, 1).await.map_err(err)?.root;
                    let conn = conn.upgrade().ok_or_else(|| err(tr!("common.ssh_disconnected")))?;
                    match git::remote(&conn, &GitOp::Diff { dir: root, path: path.clone(), untracked: *untracked }).await.map_err(err)? {
                        GitOut::Diff { text, truncated } => (text, truncated),
                        GitOut::Changes(_) => return Err(err(tr!("git.failed", error = "unexpected reply"))),
                    }
                }
            };
            Ok(Out::Diff(GitDiffOut { text, truncated }))
        }
        link::Down::GitLog { .. } => {
            let log = match &t.fs {
                fs::Backend::Local => {
                    let root = local_head(&t.cwd).ok_or_else(|| err(tr!("git.not_repo", dir = t.cwd)))?.0;
                    tokio::task::spawn_blocking(move || pgit::log(&root, LOG_LIMIT).map_err(|e| git::local_error(e, &root)))
                        .await
                        .map_err(|e| err(e.to_string()))?
                        .map_err(err)?
                }
                // 远程没有单独的 log 通道：改动脚本带回最近的提交（同桌面端远程会话的「提交」页签）
                fs::Backend::Remote { .. } => query(t, LOG_LIMIT).await.map_err(err)?.log,
            };
            Ok(Out::Log(GitLogOut { commits: pgit::parse_log(&log) }))
        }
        link::Down::GitShow { hash, .. } => {
            // 远程会话暂不支持查看提交详情（同桌面端）
            if t.host.is_some() {
                return Err(("unsupported", None));
            }
            let root = local_head(&t.cwd).ok_or_else(|| err(tr!("git.not_repo", dir = t.cwd)))?.0;
            let hash = hash.clone();
            let (numstat, diff, truncated) = tokio::task::spawn_blocking(move || pgit::show_commit(&root, &hash).map_err(|e| git::local_error(e, &root)))
                .await
                .map_err(|e| err(e.to_string()))?
                .map_err(err)?;
            Ok(Out::Show(GitShowOut { files: pgit::parse_numstat(&numstat), diff, truncated }))
        }
        _ => Err(("error", None)),
    }
}

/// 按请求类型组装回复
fn result(msg: &link::Down, request_id: String, reply: Result<Out, Failure>) -> Up {
    let ok = reply.is_ok();
    let (reason, message) = match &reply {
        Ok(_) => (None, None),
        Err((r, m)) => (Some(r.to_string()), m.clone()),
    };
    let out = reply.ok();
    match msg {
        link::Down::GitChanges { .. } => {
            let out = out.and_then(|o| if let Out::Changes(c) = o { Some(c) } else { None });
            Up::GitChangesResult { request_id, ok, reason, message, out }
        }
        link::Down::GitDiff { .. } => {
            let out = out.and_then(|o| if let Out::Diff(c) = o { Some(c) } else { None });
            Up::GitDiffResult { request_id, ok, reason, message, out }
        }
        link::Down::GitLog { .. } => {
            let out = out.and_then(|o| if let Out::Log(c) = o { Some(c) } else { None });
            Up::GitLogResult { request_id, ok, reason, message, out }
        }
        _ => {
            let out = out.and_then(|o| if let Out::Show(c) = o { Some(c) } else { None });
            Up::GitShowResult { request_id, ok, reason, message, out }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_in_repo() {
        for p in ["a.txt", "src/a b.ts", "新/", ".github/x.yml", "a..b"] {
            assert!(valid_path(p), "{p:?}");
        }
        for p in ["", "/etc/passwd", "\\x", "C:\\Windows\\win.ini", "c:x", "../x", "a/../../x", "a\\..\\x", "a\0b"] {
            assert!(!valid_path(p), "{p:?}");
        }
    }

    #[test]
    fn norm_ignores_case_and_separators() {
        assert_eq!(norm("C:/Work/App/"), norm("c:\\work\\app"));
        assert_eq!(norm("C:\\"), "c:\\");
        assert_eq!(norm("C:"), "c:\\");
    }

    #[test]
    fn result_json() {
        let msg = link::Down::GitShow { request_id: "r".into(), session_id: 1, hash: "abc".into() };
        let v = serde_json::to_value(result(&msg, "r".into(), Err(("unsupported", None)))).unwrap();
        assert_eq!(v, serde_json::json!({ "type": "gitShowResult", "requestId": "r", "ok": false, "reason": "unsupported" }));
        let msg = link::Down::GitDiff { request_id: "r".into(), session_id: 1, path: "a".into(), untracked: false };
        let out = Out::Diff(GitDiffOut { text: "+a\n".into(), truncated: false });
        let v = serde_json::to_value(result(&msg, "r".into(), Ok(out))).unwrap();
        assert_eq!(v, serde_json::json!({ "type": "gitDiffResult", "requestId": "r", "ok": true, "text": "+a\n", "truncated": false }));
        let msg = link::Down::GitChanges { request_id: "r".into(), session_id: 1 };
        let set = ChangeSet { root: "/r".into(), branch: "main".into(), ..Default::default() };
        let v = serde_json::to_value(result(&msg, "r".into(), Ok(Out::Changes(GitChangesOut { set, truncated: false })))).unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "type": "gitChangesResult", "requestId": "r", "ok": true, "root": "/r", "branch": "main", "ahead": 0, "behind": 0, "files": [], "added": 0, "deleted": 0, "truncated": false })
        );
    }
}
