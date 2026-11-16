//! 終端機剪貼簿輔助 (OSC 52)與區塊複製。

use std::io::{self, Write};
use tr::tr;

use crate::app::{main_pane_text, visible_transcript, App, CenterView};

/// 用 OSC 52 把文字寫進終端機剪貼簿 (SSH / tmux 下也可用，不需終端機原生選取)。
pub(crate) fn write_clipboard(text: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    write!(out, "\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))?;
    out.flush()
}

/// 最小 base64 編碼器 (OSC 52 需要；避免多拉一個相依性)。
pub(crate) fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = ((chunk[0] as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 把 transcript 的可見行切成訊息區塊：每個以 `❯ ` 開頭的使用者提示開啟新區塊，
/// 其後續行 (直到下一個 `❯ `)視為同一則訊息內容；最前面若有非 `❯` 行則自成第一則。
fn transcript_messages(lines: &[String]) -> Vec<Vec<String>> {
    let mut msgs: Vec<Vec<String>> = Vec::new();
    for line in lines {
        if line.starts_with("❯ ") || msgs.is_empty() {
            msgs.push(vec![line.clone()]);
        } else {
            msgs.last_mut().unwrap().push(line.clone());
        }
    }
    msgs.retain(|m| m.iter().any(|l| !l.trim().is_empty()));
    msgs
}

fn transcript_lines(app: &App) -> Vec<String> {
    app.selected_id()
        .and_then(|id| app.state.transcripts.get(&id))
        .map(|buf| visible_transcript(buf.iter(), app.show_thoughts, app.show_tools))
        .unwrap_or_default()
}

/// 複製內容：Transcript 視圖可指定第幾則訊息 (1-based)，未指定則複製最新一則；
/// 其他視圖複製整個主要區塊。
pub(crate) fn copy_main_pane(app: &mut App, message: Option<usize>) {
    let is_transcript = app.view == CenterView::Transcript;
    let (text, label) = if is_transcript {
        let lines = transcript_lines(app);
        let msgs = transcript_messages(&lines);
        if msgs.is_empty() {
            app.status = tr!("沒有可複製的內容");
            return;
        }
        match message {
            Some(n) if n == 0 || n > msgs.len() => {
                app.status = tr!("沒有第 {n} 則訊息 (共 {total} 則)", n = n, total = msgs.len());
                return;
            }
            Some(n) => (msgs[n - 1].join("\n"), tr!("第 {n} 則訊息", n = n)),
            None => {
                let n = msgs.len();
                (msgs[n - 1].join("\n"), tr!("最新訊息 (第 {n} 則)", n = n))
            }
        }
    } else {
        if message.is_some() {
            app.status = tr!("指定訊息編號僅適用於 Transcript 視圖");
            return;
        }
        let text = main_pane_text(app);
        if text.is_empty() {
            app.status = tr!("沒有可複製的內容");
            return;
        }
        (text, tr!("主要區塊"))
    };

    match write_clipboard(&text) {
        Ok(()) => {
            let lines = text.lines().count().max(1);
            app.status = tr!(
                "已複製{label} {lines} 行 (OSC 52；失敗時可用 Shift+拖曳選取)",
                label = label,
                lines = lines
            );
        }
        Err(e) => app.status = tr!("複製失敗：{e}", e = e),
    }
}
