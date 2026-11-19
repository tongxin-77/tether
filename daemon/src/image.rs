//! hub 网页贴图（协议能力 image）：按声明类型校验文件头，保存到 `%TEMP%\tether-img\<uuid>.<ext>`（GUI 粘贴的剪贴板图片也存于此），
//! 启动时及每小时删除超过 24 小时的文件。

use std::{
    path::Path,
    time::{Duration, SystemTime},
};

/// 类型校验、保存路径与上限与 GUI 粘贴图片共用，见 proto::image
pub use proto::image::{MAX_BYTES, dir, ext, magic_ok, save};

/// 保存的图片保留时长
const KEEP: Duration = Duration::from_secs(24 * 3600);
const CLEAN_EVERY: Duration = Duration::from_secs(3600);

/// 删除 dir 中修改时间早于 keep 之前的文件
fn clean(dir: &Path, keep: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let old = e.metadata().ok().filter(|m| m.is_file()).and_then(|m| m.modified().ok()).and_then(|t| now.duration_since(t).ok());
        if old.is_some_and(|age| age > keep) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// 启动时及之后每小时清理一次（在 tokio 运行时中调用）
pub fn start_cleanup() {
    tokio::spawn(async {
        let mut tick = tokio::time::interval(CLEAN_EVERY);
        loop {
            tick.tick().await;
            let _ = tokio::task::spawn_blocking(|| clean(&dir(), KEEP)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_expired() {
        // 清理在独立目录中测试（不影响并行测试保存在 tether-img 中的文件）：未过期的保留，过期的删除
        let tmp = std::env::temp_dir().join(format!("tether-img-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let f = tmp.join("a.png");
        std::fs::write(&f, b"x").unwrap();
        clean(&tmp, KEEP);
        assert!(f.exists());
        std::thread::sleep(Duration::from_millis(20));
        clean(&tmp, Duration::from_millis(10));
        assert!(!f.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
