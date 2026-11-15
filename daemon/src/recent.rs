//! 最近关闭的会话目录：%APPDATA%\tether\recent.json，读写失败只写日志，不影响主流程。

use std::path::PathBuf;

use proto::{RecentItem, SessionInfo, Target};

use crate::now_ms;

/// 最多保留条数
const MAX: usize = 10;

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join(proto::data_dir_name()).join("recent.json"))
}

/// 文件不存在或解析失败时为空列表
pub fn load() -> Vec<RecentItem> {
    let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else { return Vec::new() };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        log!("解析 recent.json 失败：{e}");
        Vec::new()
    })
}

pub fn save(items: &[RecentItem]) {
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
        log!("保存 recent.json 失败：{e}");
    }
}

/// 记录一个被用户关闭的会话：同一目标移到最前，超出上限截断
pub fn push(items: &mut Vec<RecentItem>, info: &SessionInfo) {
    let target = match &info.host_id {
        Some(host_id) => Target::Remote { host_id: host_id.clone(), cwd: Some(info.cwd.clone()) },
        None => Target::Local { cwd: info.cwd.clone() },
    };
    items.retain(|r| r.target != target);
    items.insert(0, RecentItem { target, host: info.host.clone(), closed_at: now_ms() });
    items.truncate(MAX);
    save(items);
}

/// 删除一条；不存在时返回 false
pub fn remove(items: &mut Vec<RecentItem>, target: &Target) -> bool {
    let len = items.len();
    items.retain(|r| &r.target != target);
    if items.len() == len {
        return false;
    }
    save(items);
    true
}
