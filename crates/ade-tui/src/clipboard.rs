//! 終端剪貼簿輔助（OSC 52）與區塊複製。

use std::io::{self, Write};

use crate::app::{main_pane_text, App};

/// 用 OSC 52 把文字寫進終端剪貼簿（SSH / tmux 下也可用，不需終端原生選取）。
pub(crate) fn write_clipboard(text: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    write!(out, "\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))?;
    out.flush()
}

/// 最小 base64 編碼器（OSC 52 需要；避免多拉一個依賴）。
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

/// 複製中央主要區塊（`Y` 或 `/copy`），成功後在狀態列回報行數。
pub(crate) fn copy_main_pane(app: &mut App) {
    let text = main_pane_text(app);
    if text.is_empty() {
        app.status = "沒有可複製的內容".into();
        return;
    }
    match write_clipboard(&text) {
        Ok(()) => {
            let lines = text.lines().count().max(1);
            app.status = format!("已複製主要區塊 {lines} 行（OSC 52；失敗時可用 Shift+拖曳選取）");
        }
        Err(e) => app.status = format!("複製失敗：{e}"),
    }
}
