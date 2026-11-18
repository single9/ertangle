//! 端對端：切換語言後 `tr!` 的結果。放在獨立的測試執行檔，因為語言是全域狀態，
//! 跟其他斷言中文字串的單元測試同時跑會互相干擾。

use termade_core::i18n::{self, Language};

/// 以 `termade_tui` 的 domain 翻譯 (跟 TUI 裡的 `tr!` 走同一條路徑)。
fn tui(msgid: &str) -> String {
    tr::internal::with_translator("termade_tui", |t| t.translate(msgid, None).into_owned())
}

#[test]
fn switch_language_round_trip() {
    assert_eq!(tui("介面語言"), "介面語言");

    i18n::set_language(Language::En).unwrap();
    assert_eq!(i18n::current(), Language::En);
    assert_eq!(tui("介面語言"), "Language");
    // 具名佔位符保留給 runtime_format 代入。
    assert_eq!(tui("{agent} 的預設 model"), "{agent} model");
    assert_eq!(
        tr::runtime_format!(tui("{agent} 的預設 model"), agent = "claude"),
        "claude model"
    );
    // 缺譯退回原文。
    assert_eq!(tui("沒有這條翻譯"), "沒有這條翻譯");

    i18n::set_language(Language::ZhTw).unwrap();
    assert_eq!(i18n::current(), Language::ZhTw);
    assert_eq!(tui("介面語言"), "介面語言");
}
