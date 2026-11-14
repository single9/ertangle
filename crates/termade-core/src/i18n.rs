//! 介面語言：以 gettext `.po` 翻譯使用者看得到的字串。
//!
//! 原始碼裡的繁體中文就是 `msgid`，各語言的翻譯放在 repo 根目錄的 `locales/<語言>.po`，
//! 以 `include_str!` 內嵌進執行檔。字串用 [`tr::tr!`] 包起來；沒有翻譯 (或 `msgstr` 為空)
//! 時一律退回原文。
//!
//! `tr` 依 crate 分開登記翻譯器 (以 `module_path!()` 的第一段當 domain)，所以切換語言時
//! 要對每個會呼叫 `tr!` 的 crate 都登記一次，見 [`DOMAINS`]。

use std::borrow::Cow;

#[doc(hidden)]
pub use tr as __tr;

/// 標記要翻譯但先不翻的字串字面值 (gettext 的 `N_`)：給 `&'static str` 表格用，
/// 讓 `xtr -k tr_noop` 抽得到；顯示時再用 [`tr_dyn!`] 翻譯。
#[macro_export]
macro_rules! tr_noop {
    ($s:literal) => {
        $s
    };
}

/// 翻譯執行時期才知道的字串 (通常來自 [`tr_noop!`] 標記的表格)，回傳 `String`。
/// 不是字面值，xtr 抽不到，所以原文一定要在別處用 `tr_noop!` 或 `tr!` 標記過。
#[macro_export]
macro_rules! tr_dyn {
    ($s:expr) => {
        $crate::i18n::__tr::internal::with_translator(module_path!(), |t| {
            t.translate($s, None).into_owned()
        })
    };
}
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

/// 介面語言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// 繁體中文 (原文，不需要翻譯檔)。
    ZhTw,
    En,
}

/// 可選的語言：`(設定值, 顯示名稱)`。顯示名稱用該語言自己的寫法，不翻譯。
pub const LANGUAGES: [(Language, &str, &str); 2] = [
    (Language::ZhTw, "zh-TW", "繁體中文"),
    (Language::En, "en", "English"),
];

/// 會呼叫 `tr!` 的 crate (底線形式的 crate 名稱)。新增 crate 時要加進來。
const DOMAINS: [&str; 5] = [
    "termade",
    "termade_core",
    "termade_adapter",
    "termade_daemon",
    "termade_tui",
];

const EN_PO: &str = include_str!("../../../locales/en.po");

static CURRENT: AtomicU8 = AtomicU8::new(0);

impl Language {
    /// 設定檔或環境變數的值 → 語言。`zh`、`zh_TW.UTF-8`、`en_US` 這類寫法都認得。
    pub fn parse(s: &str) -> Option<Language> {
        let s = s.trim().to_ascii_lowercase().replace('_', "-");
        let base = s.split(['.', '@']).next().unwrap_or("");
        if base.starts_with("zh") {
            Some(Language::ZhTw)
        } else if base == "en" || base.starts_with("en-") {
            Some(Language::En)
        } else {
            None
        }
    }

    /// 寫進設定檔的值。
    pub fn code(self) -> &'static str {
        match self {
            Language::ZhTw => "zh-TW",
            Language::En => "en",
        }
    }

    fn po(self) -> Option<&'static str> {
        match self {
            Language::ZhTw => None,
            Language::En => Some(EN_PO),
        }
    }
}

/// 決定語言：設定值 (`auto` 或空白視同未設定) → `LC_ALL`／`LC_MESSAGES`／`LANG` → 繁體中文。
pub fn resolve(setting: Option<&str>) -> Language {
    resolve_with(setting, |k| std::env::var(k).ok())
}

fn resolve_with(setting: Option<&str>, env: impl Fn(&str) -> Option<String>) -> Language {
    if let Some(l) = setting
        .filter(|s| !s.trim().is_empty() && !s.trim().eq_ignore_ascii_case("auto"))
        .and_then(Language::parse)
    {
        return l;
    }
    // 依 POSIX 優先序取第一個有值的變數；`C`／`POSIX` 之類認不得的值就用預設。
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| env(k).filter(|v| !v.is_empty()))
        .and_then(|v| Language::parse(&v))
        .unwrap_or(Language::ZhTw)
}

/// 目前的介面語言。
pub fn current() -> Language {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Language::En,
        _ => Language::ZhTw,
    }
}

/// 切換介面語言，之後所有 `tr!` 立即生效。翻譯檔解析失敗時維持原文並回傳錯誤。
pub fn set_language(lang: Language) -> Result<(), String> {
    let Some(src) = lang.po() else {
        for d in DOMAINS {
            tr::internal::unset_translator(d);
        }
        CURRENT.store(0, Ordering::Relaxed);
        return Ok(());
    };
    let catalog = catalog(lang, src)?;
    for d in DOMAINS {
        tr::internal::set_translator(d, Shared(catalog.clone()));
    }
    CURRENT.store(lang as u8, Ordering::Relaxed);
    Ok(())
}

/// 解析過的翻譯檔只做一次。
fn catalog(lang: Language, src: &'static str) -> Result<Arc<Catalog>, String> {
    static EN: OnceLock<Result<Arc<Catalog>, String>> = OnceLock::new();
    let cell = match lang {
        Language::En => &EN,
        Language::ZhTw => unreachable!("繁體中文沒有翻譯檔"),
    };
    cell.get_or_init(|| Catalog::parse(src).map(Arc::new))
        .clone()
}

/// 一份語言的翻譯：`(msgctxt, msgid)` → 單數或各複數形。
///
/// 不用 `tr::PoTranslator`：它的 `from_vec_u8` 把內容交給 rspolib 的 `byte_content`，
/// 但 rspolib 讀 `.po` 時不看這個欄位，結果永遠是空的翻譯表 (tr 0.1.11／rspolib 0.1.2)。
/// 自己建表也順便略過 fuzzy、過時 (`#~`) 與空白的 `msgstr`，這些一律退回原文。
struct Catalog {
    map: HashMap<(Option<String>, String), Vec<String>>,
}

impl Catalog {
    fn parse(src: &str) -> Result<Catalog, String> {
        let file = rspolib::pofile(src).map_err(|e| e.to_string())?;
        let map = file
            .entries
            .into_iter()
            .filter(|e| !e.obsolete && !e.flags.iter().any(|f| f == "fuzzy"))
            .filter_map(|e| {
                let forms = if e.msgid_plural.is_some() {
                    e.msgstr_plural
                } else {
                    e.msgstr.into_iter().collect()
                };
                (!forms.is_empty() && forms.iter().all(|f| !f.is_empty()))
                    .then_some(((e.msgctxt, e.msgid), forms))
            })
            .collect();
        Ok(Catalog { map })
    }

    fn get(&self, msgid: &str, context: Option<&str>) -> Option<&[String]> {
        self.map
            .get(&(context.map(str::to_string), msgid.to_string()))
            .map(Vec::as_slice)
    }
}

/// 多個 domain 共用同一份翻譯。
struct Shared(Arc<Catalog>);

impl tr::Translator for Shared {
    fn translate<'a>(&'a self, string: &'a str, context: Option<&'a str>) -> Cow<'a, str> {
        match self.0.get(string, context) {
            Some([t, ..]) => Cow::Borrowed(t),
            _ => Cow::Borrowed(string),
        }
    }

    /// 複數形只支援 `nplurals=2; plural=(n != 1)` (英文)；之後加入其他規則的語言時要擴充。
    fn ntranslate<'a>(
        &'a self,
        n: u64,
        singular: &'a str,
        plural: &'a str,
        context: Option<&'a str>,
    ) -> Cow<'a, str> {
        let idx = usize::from(n != 1);
        match self.0.get(singular, context).and_then(|f| f.get(idx)) {
            Some(t) => Cow::Borrowed(t),
            None => Cow::Borrowed(if n == 1 { singular } else { plural }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_locale_strings() {
        assert_eq!(Language::parse("zh-TW"), Some(Language::ZhTw));
        assert_eq!(Language::parse("zh_TW.UTF-8"), Some(Language::ZhTw));
        assert_eq!(Language::parse("en"), Some(Language::En));
        assert_eq!(Language::parse("en_US.UTF-8"), Some(Language::En));
        assert_eq!(Language::parse("C"), None);
        assert_eq!(Language::parse("fr_FR"), None);
    }

    #[test]
    fn resolve_order() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        let none = env(&[]);
        assert_eq!(resolve_with(None, none), Language::ZhTw);
        assert_eq!(resolve_with(Some("en"), none), Language::En);
        let en = env(&[("LANG", "en_US.UTF-8")]);
        assert_eq!(resolve_with(Some("auto"), en), Language::En);
        assert_eq!(resolve_with(Some("zh-TW"), en), Language::ZhTw);
        // LC_ALL 優先於 LANG。
        let mixed = env(&[("LC_ALL", "zh_TW.UTF-8"), ("LANG", "en_US.UTF-8")]);
        assert_eq!(resolve_with(None, mixed), Language::ZhTw);
        // 認不得的值 (C) 用預設，不往下找。
        let c = env(&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")]);
        assert_eq!(resolve_with(None, c), Language::ZhTw);
    }

    #[test]
    fn embedded_po_parses() {
        assert!(catalog(Language::En, EN_PO).is_ok());
    }

    #[test]
    fn empty_msgstr_falls_back() {
        let po = "msgid \"\"\nmsgstr \"Content-Type: text/plain; charset=UTF-8\\n\"\n\n\
                  msgid \"甲\"\nmsgstr \"\"\n\nmsgid \"乙 {n}\"\nmsgstr \"B {n}\"\n\n\
                  #, fuzzy\nmsgid \"丙\"\nmsgstr \"C\"\n";
        let t = Shared(Arc::new(Catalog::parse(po).unwrap()));
        use tr::Translator;
        assert_eq!(t.translate("甲", None), "甲");
        assert_eq!(t.translate("乙 {n}", None), "B {n}");
        assert_eq!(t.translate("沒有這條", None), "沒有這條");
        // fuzzy 項目不採用。
        assert_eq!(t.translate("丙", None), "丙");
    }
}
