//! hub 网页贴图（协议能力 image）：按声明类型校验文件头，保存到 `%TEMP%\tether-img\<uuid>.<ext>`，
//! 启动时及每小时删除超过 24 小时的文件。

use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// 单张图片上限
pub const MAX_BYTES: usize = 10 << 20;
/// 保存的图片保留时长
const KEEP: Duration = Duration::from_secs(24 * 3600);
const CLEAN_EVERY: Duration = Duration::from_secs(3600);

/// 支持的类型 → 扩展名；其它类型为 None
pub fn ext(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// 文件头 magic 与声明的类型一致
pub fn magic_ok(mime: &str, data: &[u8]) -> bool {
    match mime {
        "image/png" => data.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => data.starts_with(b"\xff\xd8\xff"),
        "image/gif" => data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a"),
        "image/webp" => data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP",
        _ => false,
    }
}

pub fn dir() -> PathBuf {
    std::env::temp_dir().join("tether-img")
}

/// 以新文件名（UUID）保存，不覆盖已有文件；返回完整路径
pub fn save(ext: &str, data: &[u8]) -> std::io::Result<PathBuf> {
    let dir = dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.{ext}", uuid::Uuid::new_v4()));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    f.write_all(data)?;
    Ok(path)
}

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
    fn magic_bytes() {
        assert!(magic_ok("image/png", b"\x89PNG\r\n\x1a\nrest"));
        assert!(magic_ok("image/jpeg", b"\xff\xd8\xff\xe0"));
        assert!(magic_ok("image/gif", b"GIF89a..."));
        assert!(magic_ok("image/webp", b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!magic_ok("image/png", b"\xff\xd8\xff\xe0"));
        assert!(!magic_ok("image/webp", b"RIFF\0\0\0\0WAVE"));
        assert!(!magic_ok("image/svg+xml", b"<svg"));
        assert_eq!((ext("image/jpeg"), ext("text/plain")), (Some("jpg"), None));
    }

    #[test]
    fn save_and_clean() {
        let path = save("png", b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(path.starts_with(dir()) && path.extension().unwrap() == "png");
        assert_eq!(std::fs::read(&path).unwrap(), b"\x89PNG\r\n\x1a\n");
        std::fs::remove_file(&path).unwrap();
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
