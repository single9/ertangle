//! `--help` 的翻譯。clap 的說明文字來自 doc comment，`xtr` 抽不到，所以分兩步：
//!
//! 1. `termade __help-strings` 走一遍命令樹，把所有說明文字印成 `help_strings.rs`
//!    (每條包成 `tr_noop!`，xtr 才抽得到)；`scripts/i18n-extract.sh` 會重新產生它。
//! 2. 執行時期用 [`localize`] 把命令樹裡的說明文字逐條換成翻譯 (沒有翻譯就保持原文)。

use clap::Command;
use std::collections::BTreeSet;

mod help_strings;

/// 把整棵命令樹的說明文字換成目前語言的翻譯。
pub(crate) fn localize(cmd: Command) -> Command {
    let tr = |s: String| termade_core::tr_dyn!(s.as_str());
    let mut cmd = cmd;
    if let Some(a) = cmd.get_about().map(|s| s.to_string()) {
        cmd = cmd.about(tr(a));
    }
    if let Some(a) = cmd.get_long_about().map(|s| s.to_string()) {
        cmd = cmd.long_about(tr(a));
    }
    let ids: Vec<_> = cmd.get_arguments().map(|a| a.get_id().clone()).collect();
    for id in ids {
        cmd = cmd.mut_arg(id, |mut a| {
            if let Some(h) = a.get_help().map(|s| s.to_string()) {
                a = a.help(tr(h));
            }
            if let Some(h) = a.get_long_help().map(|s| s.to_string()) {
                a = a.long_help(tr(h));
            }
            a
        });
    }
    let subs: Vec<String> = cmd
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();
    for name in subs {
        cmd = cmd.mut_subcommand(name, localize);
    }
    cmd
}

fn collect(cmd: &Command, out: &mut BTreeSet<String>) {
    for s in [cmd.get_about(), cmd.get_long_about()]
        .into_iter()
        .flatten()
    {
        out.insert(s.to_string());
    }
    for a in cmd.get_arguments() {
        for s in [a.get_help(), a.get_long_help()].into_iter().flatten() {
            out.insert(s.to_string());
        }
    }
    for s in cmd.get_subcommands() {
        collect(s, out);
    }
}

/// 印出 `help_strings.rs` 的內容 (`termade __help-strings > src/cmd/helptext/help_strings.rs`)。
pub(crate) fn dump(cmd: &Command) {
    let mut set = BTreeSet::new();
    collect(cmd, &mut set);
    println!("//! 由 `termade __help-strings` 產生，請勿手改；用 `make i18n-extract` 更新。");
    println!("//! 只是讓 xtr 抽得到 clap 的說明文字，執行時期不使用。");
    println!();
    println!("#[allow(dead_code)]");
    println!("pub(super) const HELP_STRINGS: &[&str] = &[");
    for s in set {
        println!("    termade_core::tr_noop!({s:?}),");
    }
    println!("];");
}

#[cfg(test)]
mod tests {
    use super::*;
    use termade_core::i18n::{set_language, Language};

    #[test]
    fn localize_translates_about_args_and_subcommands() {
        let cmd = Command::new("t").about("啟動 TUI（預設行為）").subcommand(
            Command::new("s")
                .about("列出所有 session")
                .arg(
                    clap::Arg::new("a")
                        .long("a")
                        .help("附加檔案或圖片（可重複）"),
                )
                .arg(clap::Arg::new("b").long("b").help("沒有翻譯的說明")),
        );
        set_language(Language::En).unwrap();
        let l = localize(cmd.clone());
        set_language(Language::ZhTw).unwrap();
        assert_eq!(
            l.get_about().unwrap().to_string(),
            "Start the TUI (default behavior)"
        );
        let s = l.find_subcommand("s").unwrap();
        assert_eq!(s.get_about().unwrap().to_string(), "List all sessions");
        let help = |c: &Command, id: &str| {
            c.get_arguments()
                .find(|a| a.get_id() == id)
                .and_then(|a| a.get_help())
                .map(|h| h.to_string())
        };
        assert_eq!(
            help(s, "a").as_deref(),
            Some("Attach files or images (repeatable)")
        );
        // 沒有翻譯的退回原文
        assert_eq!(help(s, "b").as_deref(), Some("沒有翻譯的說明"));
    }
}
