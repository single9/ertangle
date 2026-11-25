//! 過濾漏進輸入的終端機回應序列。
//!
//! 有時終端機會回傳 Device Attributes 之類的回應 (例如 Secondary DA `ESC [ > 64;2500;0 c`)，
//! 查詢可能來自其他程式或 SSH 延遲。crossterm 0.29 不認得 `ESC [ >` 開頭的 CSI，
//! 解析失敗後剩下的位元組會變成一串字元按鍵，直接打進過濾框或輸入框。
//! 這些回應會在同一次讀取中一起抵達，所以把「同一批立即可讀的事件」中長得像回應的連續字元丟掉。

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// 字元按鍵 (只允許 Shift)；其他按鍵回 `None`。
fn plain_char(ev: &Event) -> Option<char> {
    match ev {
        Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if (*modifiers - KeyModifiers::SHIFT).is_empty() => Some(*c),
        _ => None,
    }
}

fn is_plain_esc(ev: &Event) -> bool {
    matches!(
        ev,
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if modifiers.is_empty()
    )
}

/// 是否為 Device Attributes 回應的殘骸：`[>64;2500;0c`、`>1;10;0c`、`64;2500;0c`、`[?62;22c`…
/// 至少要有兩個以 `;` 分隔的數字欄位，避免誤吞一般輸入。
pub(crate) fn is_device_attributes_reply(s: &str) -> bool {
    let s = s.strip_prefix('[').unwrap_or(s);
    let s = s
        .strip_prefix(|c| matches!(c, '>' | '?' | '='))
        .unwrap_or(s);
    let Some(body) = s.strip_suffix('c') else {
        return false;
    };
    let fields: Vec<&str> = body.split(';').collect();
    fields.len() >= 2
        && fields
            .iter()
            .all(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
}

/// 從同一批事件中移除終端機回應：連續的字元按鍵若整段符合回應格式就丟掉，
/// 緊接在前面的單獨 `Esc` (回應的 ESC 與其餘位元組分開抵達時產生) 也一起丟掉。
pub(crate) fn strip_terminal_replies(events: Vec<Event>) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::with_capacity(events.len());
    let mut i = 0;
    while i < events.len() {
        let Some(ev) = events.get(i) else { break };
        if plain_char(ev).is_none() {
            out.push(ev.clone());
            i += 1;
            continue;
        }
        let end = events
            .get(i..)
            .and_then(|slice| slice.iter().position(|e| plain_char(e).is_none()))
            .map_or(events.len(), |n| i + n);
        let slice = events.get(i..end).unwrap_or(&[]);
        let run: String = slice.iter().filter_map(plain_char).collect();
        if is_device_attributes_reply(&run) {
            if out.last().is_some_and(is_plain_esc) {
                out.pop();
            }
        } else {
            out.extend(slice.iter().cloned());
        }
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<Event> {
        s.chars()
            .map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
            .collect()
    }

    fn esc() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
    }

    fn typed(events: &[Event]) -> String {
        events.iter().filter_map(plain_char).collect()
    }

    #[test]
    fn recognizes_device_attributes_replies() {
        for s in [
            "[>64;2500;0c",
            ">64;2500;0c",
            "64;2500;0c",
            "[?62;22c",
            ">1;10;0c",
        ] {
            assert!(is_device_attributes_reply(s), "{s}");
        }
    }

    #[test]
    fn keeps_ordinary_text() {
        for s in [
            "abc",
            "2c",
            "[c",
            "1;c",
            ";2c",
            "64;2500;0",
            "a;b;c",
            "gemini-4c",
        ] {
            assert!(!is_device_attributes_reply(s), "{s}");
        }
    }

    #[test]
    fn drops_reply_and_leading_esc() {
        let mut evs = vec![esc()];
        evs.extend(chars("[>64;2500;0c"));
        let out = strip_terminal_replies(evs);
        assert!(out.is_empty(), "left {} events", out.len());
    }

    #[test]
    fn keeps_esc_without_reply() {
        let mut evs = vec![esc()];
        evs.extend(chars("hi"));
        let out = strip_terminal_replies(evs);
        assert_eq!(out.len(), 3);
        assert!(is_plain_esc(&out[0]));
    }

    #[test]
    fn keeps_typed_text_around_non_char_events() {
        let mut evs = chars("gem");
        evs.push(Event::FocusGained);
        evs.extend(chars("64;2500;0c"));
        evs.push(Event::FocusLost);
        evs.extend(chars("ini"));
        let out = strip_terminal_replies(evs);
        assert_eq!(typed(&out), "gemini");
        assert_eq!(out.len(), 8);
    }
}
