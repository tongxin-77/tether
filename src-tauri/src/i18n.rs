//! GUI 进程（Rust 侧）返回给前端的错误文字的多语言词条。
//! 语言：启动时取 daemon 持久化的 config.json 中的 locale，没有则按 Windows 显示语言；
//! 前端经 `send` 发出 SetLocale 时同步更新（见 lib.rs 的 send），无需单独的 command。

use std::{
    fmt::Display,
    sync::atomic::{AtomicU8, Ordering},
};

use proto::i18n::{Entry, Locale, fill, lookup};

/// 取当前语言的词条并替换占位符：`tr!("key")`、`tr!("key", name = value, ...)`
macro_rules! tr {
    ($key:literal) => {
        $crate::i18n::tr($key, &[])
    };
    ($key:literal, $($name:ident = $val:expr),+ $(,)?) => {
        $crate::i18n::tr($key, &[$((stringify!($name), &$val as &dyn std::fmt::Display)),+])
    };
}

static CURRENT: AtomicU8 = AtomicU8::new(Locale::ZhCn as u8);

pub fn set(code: &str) {
    CURRENT.store(Locale::parse(code) as u8, Ordering::Relaxed);
}

/// 启动时确定初始语言：config.json 的 locale（GUI 上次设置并由 daemon 保存）→ Windows 显示语言
pub fn init() {
    let saved = std::env::var_os("APPDATA")
        .map(|d| std::path::PathBuf::from(d).join(proto::data_dir_name()).join("config.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("locale")?.as_str().map(String::from));
    set(&saved.unwrap_or_else(system_locale));
}

/// Windows 显示语言（LANGID）→ 语言代码
fn system_locale() -> String {
    let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    let (primary, sub) = (id & 0x3ff, id >> 10);
    match (primary, sub) {
        // LANG_CHINESE：SUBLANG_CHINESE_TRADITIONAL / HONGKONG / MACAU 为繁体
        (0x04, 0x01 | 0x03 | 0x05) => "zh-TW",
        (0x04, _) => "zh-CN",
        (0x11, _) => "ja",
        _ => "en",
    }
    .into()
}

pub fn tr(key: &'static str, args: &[(&str, &dyn Display)]) -> String {
    fill(lookup(TABLE, Locale::from_index(CURRENT.load(Ordering::Relaxed)), key), args)
}

/// 词条表：(键, [zh-CN, en, zh-TW, ja])
static TABLE: &[Entry] = &[
    ("daemon.unreachable", ["无法连接后台进程 tetherd", "Cannot connect to the background service tetherd", "無法連線背景服務 tetherd", "バックグラウンド サービス tetherd に接続できません"]),
    ("daemon.not_connected", ["未连接后台进程", "Not connected to the background service", "未連線背景服務", "バックグラウンド サービスに接続していません"]),
    ("daemon.open_failed", ["无法打开后台进程 {pid}", "Cannot open background service process {pid}", "無法開啟背景服務程序 {pid}", "バックグラウンド サービスのプロセス {pid} を開けません"]),
    ("daemon.kill_failed", ["结束后台进程 {pid} 失败", "Failed to end background service process {pid}", "結束背景服務程序 {pid} 失敗", "バックグラウンド サービスのプロセス {pid} を終了できませんでした"]),
    ("git.dir_not_found", ["目录不存在：{dir}", "Directory does not exist: {dir}", "目錄不存在：{dir}", "ディレクトリが存在しません：{dir}"]),
    ("git.not_found", ["未找到 git，请安装 Git 并加入 PATH", "git not found; install Git and add it to PATH", "找不到 git，請安裝 Git 並加入 PATH", "git が見つかりません。Git をインストールして PATH に追加してください"]),
    ("git.run_failed", ["运行 git 失败：{error}", "Failed to run git: {error}", "執行 git 失敗：{error}", "git を実行できませんでした：{error}"]),
    ("git.exit_failed", ["git 执行失败（{status}）", "git failed ({status})", "git 執行失敗（{status}）", "git の実行に失敗しました（{status}）"]),
    ("git.bad_branch", ["无效的分支名：{name}", "Invalid branch name: {name}", "無效的分支名稱：{name}", "無効なブランチ名：{name}"]),
    ("git.bad_hash", ["无效的提交哈希：{hash}", "Invalid commit hash: {hash}", "無效的提交雜湊：{hash}", "無効なコミット ハッシュ：{hash}"]),
    ("claude.no_home", ["未找到用户目录", "User profile folder not found", "找不到使用者目錄", "ユーザー フォルダーが見つかりません"]),
    ("image.too_large", ["图片超过 {mb} MB", "Image is larger than {mb} MB", "圖片超過 {mb} MB", "画像が {mb} MB を超えています"]),
    ("image.incomplete", ["剪贴板图片数据不完整", "Clipboard image data is incomplete", "剪貼簿圖片資料不完整", "クリップボードの画像データが不完全です"]),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// 词条表自检；lib.rs 中 tr! 用到的键都在表中，表中每个键都被用到
    #[test]
    fn table_consistent_and_used() {
        if let Err(e) = proto::i18n::check(TABLE) {
            panic!("{e}");
        }
        let src = include_str!("lib.rs");
        let used: Vec<&str> = src.match_indices("tr!(\"").map(|(i, _)| src[i + 5..].split('"').next().unwrap()).collect();
        for key in &used {
            assert!(TABLE.iter().any(|(k, _)| k == key), "词条表中没有 {key}");
        }
        for (key, _) in TABLE {
            assert!(used.contains(key), "词条 {key} 未被使用");
        }
    }

    #[test]
    fn english_lookup() {
        assert_eq!(fill(lookup(TABLE, Locale::En, "git.bad_hash"), &[("hash", &"zz")]), "Invalid commit hash: zz");
    }
}
