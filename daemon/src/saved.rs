//! 未关闭的会话：%APPDATA%\tether\sessions.json。daemon 重启（如重启电脑）后据此恢复会话，
//! Claude Code 会话自动 `claude --resume`。用户关闭的会话从文件中删除；读写失败只写日志，不影响主流程。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    pub name: String,
    pub cwd: String,
    /// 本地会话的启动命令（shell）
    pub command: String,
    /// 远程会话所用主机的 id，本地会话为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    /// 会话中运行的 Claude Code 对话
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<Resume>,
    /// 用户手动改过名（不被 Claude 对话标题覆盖）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub name_custom: bool,
    /// 最后一次读到的 Claude 对话标题
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_title: Option<String>,
}

/// Claude Code 的 session_id 与其启动目录（`--resume` 按项目目录查找对话），以及恢复时要带上的权限模式
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resume {
    pub id: String,
    pub cwd: String,
    /// 最近一次 hook 上报的 permission_mode
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// 本对话中出现过 bypassPermissions（以 --dangerously-skip-permissions 启动）：切到别的模式后恢复时仍需带上，才能再切回去
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bypass: bool,
}

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join(proto::data_dir_name()).join("sessions.json"))
}

/// 文件不存在或解析失败时为空列表
pub fn load() -> Vec<Saved> {
    let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else { return Vec::new() };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        log!("解析 sessions.json 失败：{e}");
        Vec::new()
    })
}

pub fn save(items: &[Saved]) {
    // 单元测试（如 hub 端到端关闭会话）不写用户数据目录
    if cfg!(test) {
        return;
    }
    let result = (|| -> Result<(), String> {
        let p = path().ok_or("未找到 APPDATA 环境变量")?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(items).map_err(|e| e.to_string())?;
        std::fs::write(p, json).map_err(|e| e.to_string())
    })();
    if let Err(e) = result {
        log!("保存 sessions.json 失败：{e}");
    }
}

/// 恢复时输入 shell 的命令，带上原来的权限模式；session_id 含意外字符（文件被改动）时不输入，权限模式只认白名单
pub fn resume_command(r: &Resume) -> Option<String> {
    let id = &r.id;
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let mut cmd = format!("claude --resume {id}");
    if r.bypass || r.permission_mode.as_deref() == Some("bypassPermissions") {
        cmd += " --dangerously-skip-permissions";
    }
    if let Some(m @ ("plan" | "acceptEdits" | "auto" | "dontAsk")) = r.permission_mode.as_deref() {
        cmd += &format!(" --permission-mode {m}");
    }
    Some(cmd + "\r")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let items = vec![
            Saved {
                name: "proj".into(),
                cwd: r"C:\work\proj".into(),
                command: "pwsh.exe".into(),
                host_id: None,
                claude: Some(Resume {
                    id: "0b1c-22".into(),
                    cwd: r"C:\work\proj".into(),
                    permission_mode: Some("plan".into()),
                    bypass: true,
                }),
                name_custom: true,
                claude_title: Some("修复登录".into()),
            },
            Saved {
                name: "~".into(),
                cwd: "/home/u".into(),
                command: "ssh u@h:22".into(),
                host_id: Some("h1".into()),
                claude: None,
                name_custom: false,
                claude_title: None,
            },
        ];
        let json = serde_json::to_string_pretty(&items).unwrap();
        assert!(json.contains("\"hostId\": \"h1\""));
        assert_eq!(serde_json::from_str::<Vec<Saved>>(&json).unwrap(), items);
        // 可选字段缺失时为 None
        let min: Vec<Saved> = serde_json::from_str(r#"[{"name":"a","cwd":"C:\\","command":"cmd.exe"}]"#).unwrap();
        assert_eq!((min[0].host_id.clone(), min[0].claude.clone()), (None, None));
        assert_eq!((min[0].name_custom, min[0].claude_title.clone()), (false, None));
        // 旧文件的 claude 不带权限字段
        let old: Resume = serde_json::from_str(r#"{"id":"a","cwd":"C:\\"}"#).unwrap();
        assert_eq!((old.permission_mode, old.bypass), (None, false));
    }

    #[test]
    fn resume_command_rejects_unsafe_ids() {
        let id = |id: &str| resume_command(&Resume { id: id.into(), ..Default::default() });
        assert_eq!(id("6f1e-ab").as_deref(), Some("claude --resume 6f1e-ab\r"));
        assert_eq!(id(""), None);
        assert_eq!(id("x; rm -rf /"), None);
    }

    #[test]
    fn resume_command_permission_mode() {
        let cmd = |mode: Option<&str>, bypass: bool| {
            let r = Resume { id: "a1".into(), cwd: String::new(), permission_mode: mode.map(Into::into), bypass };
            resume_command(&r).unwrap()
        };
        assert_eq!(cmd(None, false), "claude --resume a1\r");
        assert_eq!(cmd(Some("default"), false), "claude --resume a1\r");
        assert_eq!(cmd(Some("plan"), false), "claude --resume a1 --permission-mode plan\r");
        assert_eq!(cmd(Some("acceptEdits"), false), "claude --resume a1 --permission-mode acceptEdits\r");
        assert_eq!(cmd(Some("auto"), false), "claude --resume a1 --permission-mode auto\r");
        assert_eq!(cmd(Some("dontAsk"), false), "claude --resume a1 --permission-mode dontAsk\r");
        assert_eq!(cmd(Some("bypassPermissions"), true), "claude --resume a1 --dangerously-skip-permissions\r");
        // 粘滞的 bypass 与当前模式同时带上
        assert_eq!(cmd(Some("default"), true), "claude --resume a1 --dangerously-skip-permissions\r");
        assert_eq!(
            cmd(Some("acceptEdits"), true),
            "claude --resume a1 --dangerously-skip-permissions --permission-mode acceptEdits\r"
        );
        // 白名单外的值（文件被改动）忽略
        assert_eq!(cmd(Some("plan; rm -rf /"), false), "claude --resume a1\r");
        assert_eq!(cmd(Some("Plan"), false), "claude --resume a1\r");
    }
}
