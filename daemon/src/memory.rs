//! 会话的内存占用：本地会话 shell 及其全部子孙进程（如 pwsh → claude → MCP 服务器 node）的专用工作集之和，
//! 与任务管理器“内存”列一致。
//!
//! 有 GUI 连接时每 5 秒采样一次，结果与上次推送的不同（或换了 GUI 连接）才以 ServerMsg::Memory 全量推送。
//! 采样时顺带检查进程树中是否还有 Claude 进程，交给 session::check_claude 发现异常退出（GUI 未连接时不检测，下次采样再纠正）。

use std::{collections::HashSet, time::Duration};

use proto::{ServerMsg, SessionMemory};

use crate::session::{self, Shared};

const INTERVAL: Duration = Duration::from_secs(5);

/// 启动采样任务（daemon 生命周期内常驻；无 GUI 时只空转计时）
pub fn start(state: Shared) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(INTERVAL);
        // 上次推送时的 GUI 连接与内容
        let mut last: Option<(u64, Vec<SessionMemory>)> = None;
        loop {
            tick.tick().await;
            let roots = {
                let st = state.lock().unwrap();
                if st.gui.is_none() {
                    last = None;
                    continue;
                }
                session::local_pids(&st)
            };
            let Ok(sampled) = tokio::task::spawn_blocking(move || sample(&roots)).await else { continue };
            let mut st = state.lock().unwrap();
            session::check_claude(&mut st, &sampled.iter().map(|(m, claude)| (m.id, *claude)).collect::<Vec<_>>());
            let items: Vec<_> = sampled.into_iter().map(|(m, _)| m).collect();
            let Some(conn) = st.gui.as_ref().map(|g| g.conn) else { continue };
            if last.as_ref().is_some_and(|(c, v)| *c == conn && *v == items) {
                continue;
            }
            st.send(&ServerMsg::Memory { items: items.clone() });
            last = Some((conn, items));
        }
    });
}

/// 采样各会话（会话 id, shell pid）的进程树内存，及树中是否有 Claude 进程；shell 已不存在（或无权访问）的会话不出现在结果中。
/// Claude 进程：claude.exe（原生安装），或 Claude Code 写有 `<配置目录>\sessions\<pid>.json` 的进程（npm 安装时为 node）
fn sample(roots: &[(u32, u32)]) -> Vec<(SessionMemory, bool)> {
    if roots.is_empty() {
        return Vec::new();
    }
    let (procs, claude) = snapshot();
    let dir = crate::claude::settings_path().and_then(|p| Some(p.parent()?.join("sessions")));
    let is_claude = |pid: u32| claude.contains(&pid) || dir.as_ref().is_some_and(|d| d.join(format!("{pid}.json")).is_file());
    roots
        .iter()
        .filter_map(|&(id, pid)| {
            let mut found = false;
            let bytes = tree_sum(&procs, pid, |p| {
                found |= is_claude(p);
                query(p)
            })?;
            Some((SessionMemory { id, bytes }, found))
        })
        .collect()
}

/// 进程树求和：从 root 起沿 (pid, ppid) 向下遍历，info 给出进程的 (创建时间, 字节)，取不到的进程连同其子树跳过。
/// Windows 会复用 pid：父进程退出后其 pid 可能被新进程占用，旧子进程的 ppid 便指向无关进程，
/// 故只认创建时间不早于父进程的子进程。root 本身取不到时为 None
pub fn tree_sum(procs: &[(u32, u32)], root: u32, mut info: impl FnMut(u32) -> Option<(u64, u64)>) -> Option<u64> {
    let (created, bytes) = info(root)?;
    let mut total = bytes;
    let mut stack = vec![(root, created)];
    let mut seen = HashSet::from([root]);
    while let Some((pid, created)) = stack.pop() {
        for &(child, _) in procs.iter().filter(|&&(c, ppid)| ppid == pid && c != pid) {
            if !seen.insert(child) {
                continue;
            }
            let Some((child_created, bytes)) = info(child) else { continue };
            if child_created < created {
                continue;
            }
            total += bytes;
            stack.push((child, child_created));
        }
    }
    Some(total)
}

/// 进程映像名是否为 claude.exe（不区分大小写；szExeFile 以 0 结尾）
fn is_claude_exe(name: &[u16]) -> bool {
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    String::from_utf16_lossy(&name[..len]).eq_ignore_ascii_case("claude.exe")
}

/// 枚举全部进程的 (pid, ppid)，及其中 claude.exe 的 pid
fn snapshot() -> (Vec<(u32, u32)>, HashSet<u32>) {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS},
    };
    let (mut out, mut claude) = (Vec::new(), HashSet::new());
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return (out, claude);
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            out.push((e.th32ProcessID, e.th32ParentProcessID));
            if is_claude_exe(&e.szExeFile) {
                claude.insert(e.th32ProcessID);
            }
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
    }
    (out, claude)
}

/// 单个进程的 (创建时间, 专用工作集字节)；专用工作集取不到（PROCESS_MEMORY_COUNTERS_EX2 需较新的 Windows）时退回 PrivateUsage
fn query(pid: u32) -> Option<(u64, u64)> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME},
        System::{
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX, PROCESS_MEMORY_COUNTERS_EX2},
            Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    };
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let (mut created, mut exited, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) = std::mem::zeroed();
        let timed = GetProcessTimes(h, &mut created, &mut exited, &mut kernel, &mut user) != 0;
        let mut ex2: PROCESS_MEMORY_COUNTERS_EX2 = std::mem::zeroed();
        ex2.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
        let private_ws = K32GetProcessMemoryInfo(h, &mut ex2 as *mut _ as *mut PROCESS_MEMORY_COUNTERS, ex2.cb) != 0;
        let bytes = match (private_ws, ex2.PrivateWorkingSetSize) {
            (true, n) if n > 0 => Some(n as u64),
            _ => {
                let mut ex: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
                ex.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
                (K32GetProcessMemoryInfo(h, &mut ex as *mut _ as *mut PROCESS_MEMORY_COUNTERS, ex.cb) != 0).then_some(ex.PrivateUsage as u64)
            }
        };
        CloseHandle(h);
        let created = (created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64;
        if timed { bytes.map(|b| (created, b)) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn sum(procs: &[(u32, u32)], root: u32, info: &HashMap<u32, (u64, u64)>) -> Option<u64> {
        tree_sum(procs, root, |p| info.get(&p).copied())
    }

    #[test]
    fn sums_whole_tree() {
        // 10 → 11 → 12、10 → 13；20 是无关进程
        let procs = [(10, 1), (11, 10), (12, 11), (13, 10), (20, 1)];
        let info = HashMap::from([(10, (100, 1)), (11, (101, 2)), (12, (102, 4)), (13, (103, 8)), (20, (50, 16))]);
        assert_eq!(sum(&procs, 10, &info), Some(15));
        assert_eq!(sum(&procs, 11, &info), Some(6));
        assert_eq!(sum(&procs, 99, &info), None);
    }

    #[test]
    fn pid_reuse_and_inaccessible_skipped() {
        // 12 早于父进程 11 创建：其原父进程已退出、pid 11 被复用，不计入；13 无权访问，连同子进程 14 跳过
        let procs = [(10, 1), (11, 10), (12, 11), (13, 10), (14, 13)];
        let info = HashMap::from([(10, (100, 1)), (11, (101, 2)), (12, (90, 4)), (14, (105, 8))]);
        assert_eq!(sum(&procs, 10, &info), Some(3));
    }

    #[test]
    fn cycle_terminates() {
        let procs = [(10, 11), (11, 10), (0, 0)];
        let info = HashMap::from([(10, (100, 1)), (11, (100, 2)), (0, (0, 4))]);
        assert_eq!(sum(&procs, 10, &info), Some(3));
        assert_eq!(sum(&procs, 0, &info), Some(4));
    }

    #[test]
    fn current_process_has_memory() {
        let pid = std::process::id();
        let (_, bytes) = query(pid).expect("取不到当前进程内存");
        assert!(bytes > 0);
        let (procs, _) = snapshot();
        assert!(procs.iter().any(|&(p, _)| p == pid));
        let items = sample(&[(1, pid)]);
        eprintln!("当前进程树内存：{items:?}");
        assert!(items[0].0.bytes > 0);
    }

    #[test]
    fn claude_exe_name() {
        let w = |s: &str| s.encode_utf16().chain([0, 'x' as u16]).collect::<Vec<_>>();
        assert!(is_claude_exe(&w("claude.exe")));
        assert!(is_claude_exe(&w("Claude.EXE")));
        assert!(!is_claude_exe(&w("claude.cmd")));
        assert!(!is_claude_exe(&w("node.exe")));
        assert!(!is_claude_exe(&w("myclaude.exe")));
    }
}
