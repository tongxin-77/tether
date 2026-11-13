//! `tetherd hook`：不在 Tether 会话中（无 TETHER_SESSION / TETHER_PIPE）时立即静默退出

use std::{
    io::Write,
    process::{Command, Stdio},
    time::Instant,
};

fn run_hook(envs: &[(&str, &str)]) -> (std::process::Output, u128) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tetherd"));
    cmd.arg("hook").env_remove("TETHER_SESSION").env_remove("TETHER_PIPE");
    cmd.envs(envs.iter().copied()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = cmd.spawn().unwrap();
    // 无环境变量时子进程可能已退出，写入失败（管道已断开）不算错误
    let _ = child.stdin.take().unwrap().write_all(br#"{"hook_event_name":"Stop","session_id":"x"}"#);
    let out = child.wait_with_output().unwrap();
    (out, start.elapsed().as_millis())
}

#[test]
fn exits_silently_without_env() {
    // 首次启动新编译的 exe 可能被杀毒软件扫描而变慢，先预热一次再计时
    run_hook(&[]);
    let (out, ms) = run_hook(&[]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    assert!(ms < 1000, "耗时 {ms} ms");
}

#[test]
fn exits_silently_when_daemon_missing() {
    let (out, ms) = run_hook(&[("TETHER_SESSION", "1"), ("TETHER_PIPE", r"\\.\pipe\tether-test-no-such-pipe")]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    assert!(ms < 2000, "耗时 {ms} ms");
}
