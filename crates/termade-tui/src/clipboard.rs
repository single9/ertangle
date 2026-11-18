//! 剪貼簿支援：原生工具鏈 (wl-copy / xclip / xsel)與 OSC 52 (支援 tmux passthrough)。

use std::io::{self, Write};
use tr::tr;

use crate::app::{main_pane_text, visible_transcript, App, CenterView};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClipBackend {
    WlCopy,
    Xclip,
    Xsel,
    Osc52,
}

impl std::fmt::Display for ClipBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WlCopy => write!(f, "wl-copy"),
            Self::Xclip => write!(f, "xclip"),
            Self::Xsel => write!(f, "xsel"),
            Self::Osc52 => write!(f, "OSC 52"),
        }
    }
}

#[allow(dead_code)]
pub(crate) fn choose_native(
    remote: bool,
    wayland: bool,
    have: impl Fn(&str) -> bool,
) -> Option<ClipBackend> {
    if remote {
        return None;
    }
    if wayland && have("wl-copy") {
        return Some(ClipBackend::WlCopy);
    }
    if have("xclip") {
        return Some(ClipBackend::Xclip);
    }
    if have("xsel") {
        return Some(ClipBackend::Xsel);
    }
    None
}

pub(crate) fn osc52_sequence(text: &str, in_tmux: bool) -> Vec<u8> {
    let b64 = base64_encode(text.as_bytes());
    if in_tmux {
        format!("\x1bPtmux;\x1b\x1b]52;c;{b64}\x07\x1b\\").into_bytes()
    } else {
        format!("\x1b]52;c;{b64}\x07").into_bytes()
    }
}

fn is_remote() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_TTY").is_some()
        || std::env::var_os("TMUX").is_some()
}

fn is_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}

fn try_command(program: &str, args: &[&str], text: &str) -> bool {
    use std::process::{Command, Stdio};
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(text.as_bytes()).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
    }

    match child.wait() {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

/// 將文字寫入系統剪貼簿。
///
/// 優先偵測環境：若為遠端 (SSH / tmux)則優先使用 OSC 52；否則優先嘗試本機工具 (wl-copy / xclip / xsel)，
/// 失敗或不可用時回退至 OSC 52。
pub(crate) fn write_clipboard(text: &str) -> io::Result<ClipBackend> {
    let remote = is_remote();
    let tmux = is_tmux();
    let wayland = is_wayland();

    if !remote {
        if wayland && try_command("wl-copy", &[], text) {
            return Ok(ClipBackend::WlCopy);
        }
        if try_command("xclip", &["-selection", "clipboard"], text) {
            return Ok(ClipBackend::Xclip);
        }
        if try_command("xsel", &["--clipboard", "--input"], text) {
            return Ok(ClipBackend::Xsel);
        }
    }

    let seq = osc52_sequence(text, tmux);
    let mut out = io::stdout().lock();
    out.write_all(&seq)?;
    out.flush()?;
    Ok(ClipBackend::Osc52)
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

/// 最小 base64 解碼器 (測試與驗證用)。
#[allow(dead_code)]
pub(crate) fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for &b in input.as_bytes() {
        if b == b'=' {
            break;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
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
    let active = app
        .selected_id()
        .and_then(|id| app.state.sessions.get(&id))
        .map(|s| s.active_tools.as_slice())
        .unwrap_or(&[]);
    app.selected_id()
        .and_then(|id| app.state.transcripts.get(&id))
        .map(|buf| {
            visible_transcript(buf.iter(), app.show_thoughts, app.show_tools, active)
                .into_iter()
                .map(|c| c.into_owned())
                .collect()
        })
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
                app.status = tr!(
                    "沒有第 {n} 則訊息 (共 {total} 則)",
                    n = n,
                    total = msgs.len()
                );
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
        Ok(backend) => {
            let lines = text.lines().count().max(1);
            app.status = tr!(
                "已複製{label} {lines} 行 ({backend})",
                label = label,
                lines = lines,
                backend = backend
            );
        }
        Err(e) => app.status = tr!("複製失敗：{e}", e = e),
    }
}
