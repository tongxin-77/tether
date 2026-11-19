//! 贴图文件（hub 网页贴图与 GUI 粘贴剪贴板图片共用）：按类型校验文件头，保存到 `%TEMP%\tether-img\<uuid>.<ext>`。
//! 过期文件由 daemon 清理。

use std::{io::Write, path::PathBuf};

/// 单张图片上限
pub const MAX_BYTES: usize = 10 << 20;

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
    fn save_unique() {
        let a = save("png", b"\x89PNG\r\n\x1a\n").unwrap();
        let b = save("png", b"x").unwrap();
        assert!(a.starts_with(dir()) && a.extension().unwrap() == "png" && a != b);
        assert!(uuid::Uuid::parse_str(a.file_stem().unwrap().to_str().unwrap()).is_ok());
        assert_eq!(std::fs::read(&a).unwrap(), b"\x89PNG\r\n\x1a\n");
        std::fs::remove_file(&a).unwrap();
        std::fs::remove_file(&b).unwrap();
    }
}
