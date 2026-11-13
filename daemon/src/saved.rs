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
}

/// Claude Code 的 session_id 与其启动目录（`--resume` 按项目目录查找对话）
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resume {
    pub id: String,
    pub cwd: String,
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

/// 恢复时输入 shell 的命令；session_id 含意外字符（文件被改动）时不输入
pub fn resume_command(id: &str) -> Option<String> {
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then(|| format!("claude --resume {id}\r"))
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
                claude: Some(Resume { id: "0b1c-22".into(), cwd: r"C:\work\proj".into() }),
            },
            Saved { name: "~".into(), cwd: "/home/u".into(), command: "ssh u@h:22".into(), host_id: Some("h1".into()), claude: None },
        ];
        let json = serde_json::to_string_pretty(&items).unwrap();
        assert!(json.contains("\"hostId\": \"h1\""));
        assert_eq!(serde_json::from_str::<Vec<Saved>>(&json).unwrap(), items);
        // 可选字段缺失时为 None
        let min: Vec<Saved> = serde_json::from_str(r#"[{"name":"a","cwd":"C:\\","command":"cmd.exe"}]"#).unwrap();
        assert_eq!((min[0].host_id.clone(), min[0].claude.clone()), (None, None));
    }

    #[test]
    fn resume_command_rejects_unsafe_ids() {
        assert_eq!(resume_command("6f1e-ab").as_deref(), Some("claude --resume 6f1e-ab\r"));
        assert_eq!(resume_command(""), None);
        assert_eq!(resume_command("x; rm -rf /"), None);
    }
}
