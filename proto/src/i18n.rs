//! 界面语言：代码解析（回退链）与词条表查找。词条表每项为 (键, [zh-CN, en, zh-TW, ja])，
//! 四种语言同处一行，键集合天然一致；占位符写作 `{name}`，由 [`fill`] 替换。

use std::fmt::Display;

/// 支持的语言；数值即词条表中译文的下标
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale {
    ZhCn = 0,
    En = 1,
    ZhTw = 2,
    Ja = 3,
}

impl Locale {
    pub const ALL: [Locale; 4] = [Locale::ZhCn, Locale::En, Locale::ZhTw, Locale::Ja];

    /// 语言代码（BCP 47）→ 支持的语言：精确匹配 → 同语种（zh-HK / zh-MO / zh-Hant* → zh-TW，其余 zh* → zh-CN，
    /// ja* → ja）→ 其它一律 en
    pub fn parse(code: &str) -> Locale {
        let c = code.trim().replace('_', "-").to_ascii_lowercase();
        let lang = c.split('-').next().unwrap_or("");
        match lang {
            "zh" if c.contains("hant") || ["zh-tw", "zh-hk", "zh-mo"].iter().any(|p| c.starts_with(p)) => Locale::ZhTw,
            "zh" => Locale::ZhCn,
            "ja" => Locale::Ja,
            _ => Locale::En,
        }
    }

    pub fn code(self) -> &'static str {
        ["zh-CN", "en", "zh-TW", "ja"][self as usize]
    }

    pub fn from_index(i: u8) -> Locale {
        Locale::ALL.get(i as usize).copied().unwrap_or(Locale::ZhCn)
    }
}

/// 词条：键与四种语言的译文（顺序同 [`Locale`]）；译文为空串表示缺失，按 en → zh-CN 回退
pub type Entry = (&'static str, [&'static str; 4]);

/// 查词条：缺失时按 en 再按 zh-CN 回退；未知键原样返回键名
pub fn lookup(table: &[Entry], locale: Locale, key: &'static str) -> &'static str {
    let Some((_, texts)) = table.iter().find(|(k, _)| *k == key) else { return key };
    [locale, Locale::En, Locale::ZhCn].iter().map(|l| texts[*l as usize]).find(|t| !t.is_empty()).unwrap_or(key)
}

/// 依次把模板中的 `{name}` 替换为对应值
pub fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = template.to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), &value.to_string());
    }
    out
}

/// 模板中的占位符名（排序去重）
pub fn placeholders(s: &str) -> Vec<&str> {
    let mut names: Vec<&str> = s
        .split('{')
        .skip(1)
        .filter_map(|p| p.split_once('}').map(|(n, _)| n))
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// 词条表自检（供各进程的单元测试调用）：键不重复、每种语言都有译文、各语言占位符与 zh-CN 一致
pub fn check(table: &[Entry]) -> Result<(), String> {
    let mut errors = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (key, texts) in table {
        if !seen.insert(*key) {
            errors.push(format!("重复的键 {key}"));
        }
        let base = placeholders(texts[0]);
        for l in Locale::ALL {
            let t = texts[l as usize];
            if t.trim().is_empty() {
                errors.push(format!("{key} 缺少 {} 译文", l.code()));
            } else if placeholders(t) != base {
                errors.push(format!("{key} 的 {} 占位符 {:?} 与 zh-CN {:?} 不一致", l.code(), placeholders(t), base));
            }
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors.join("\n")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fallback_chain() {
        for (code, want) in [
            ("zh-CN", Locale::ZhCn),
            ("zh-SG", Locale::ZhCn),
            ("zh-Hans-CN", Locale::ZhCn),
            ("zh", Locale::ZhCn),
            ("zh-TW", Locale::ZhTw),
            ("zh-HK", Locale::ZhTw),
            ("zh-MO", Locale::ZhTw),
            ("zh-Hant", Locale::ZhTw),
            ("zh_TW", Locale::ZhTw),
            ("ja", Locale::Ja),
            ("ja-JP", Locale::Ja),
            ("en", Locale::En),
            ("en-GB", Locale::En),
            ("fr-FR", Locale::En),
            ("", Locale::En),
        ] {
            assert_eq!(Locale::parse(code), want, "{code}");
        }
        for l in Locale::ALL {
            assert_eq!(Locale::parse(l.code()), l);
            assert_eq!(Locale::from_index(l as u8), l);
        }
    }

    #[test]
    fn lookup_and_fill() {
        const T: &[Entry] = &[("a", ["甲 {n}", "A {n}", "", "エー {n}"])];
        assert_eq!(lookup(T, Locale::ZhTw, "a"), "A {n}");
        assert_eq!(lookup(T, Locale::Ja, "a"), "エー {n}");
        assert_eq!(lookup(T, Locale::En, "missing"), "missing");
        assert_eq!(fill("{x} / {y} / {x}", &[("x", &1), ("y", &"b")]), "1 / b / 1");
        assert_eq!(placeholders("{b}{a}{b} {} {not a name}"), ["a", "b"]);
    }

    #[test]
    fn check_reports_problems() {
        assert!(check(&[("a", ["{x}", "{x}", "{x}", "{x}"])]).is_ok());
        assert!(check(&[("a", ["{x}", "{y}", "{x}", "{x}"])]).is_err());
        assert!(check(&[("a", ["x", "", "x", "x"])]).is_err());
        assert!(check(&[("a", ["x", "x", "x", "x"]), ("a", ["x", "x", "x", "x"])]).is_err());
    }
}
