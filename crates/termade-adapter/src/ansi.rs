//! 極簡 ANSI/escape 序列過濾器。
//!
//! PTY 直通會拿到 agent TUI 的原始終端機輸出（CSI/OSC 控制碼、游標移動、顏色…）。
//! 為了讓 transcript 可讀，我們在 adapter 這層把它們剝掉。
//!
//! 這是**跨 chunk 有狀態**的：escape 序列可能被切在兩次 read 之間。

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    Csi,
    Osc,
    OscEsc,
}

pub struct AnsiStripper {
    state: State,
}

impl Default for AnsiStripper {
    fn default() -> Self {
        Self::new()
    }
}

impl AnsiStripper {
    pub fn new() -> Self {
        AnsiStripper {
            state: State::Ground,
        }
    }

    /// 餵入一段原始位元組，回傳可讀文字（已濾除控制碼）。
    pub fn feed(&mut self, input: &[u8]) -> String {
        let mut out: Vec<u8> = Vec::with_capacity(input.len());
        for &b in input {
            match self.state {
                State::Ground => match b {
                    0x1b => self.state = State::Esc,
                    b'\n' | b'\t' => out.push(b),
                    0x0d => {} // 丟掉 CR
                    0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f => {}
                    _ => out.push(b),
                },
                State::Esc => match b {
                    b'[' => self.state = State::Csi,
                    b']' | b'P' | b'^' | b'_' => self.state = State::Osc,
                    _ => self.state = State::Ground,
                },
                State::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        self.state = State::Ground;
                    }
                }
                State::Osc => match b {
                    0x07 => self.state = State::Ground,
                    0x1b => self.state = State::OscEsc,
                    _ => {}
                },
                State::OscEsc => {
                    self.state = if b == b'\\' {
                        State::Ground
                    } else {
                        State::Osc
                    };
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colors_and_cursor() {
        let mut s = AnsiStripper::new();
        let out = s.feed(b"\x1b[31mred\x1b[0m\x1b[2Jdone");
        assert_eq!(out, "reddone");
    }

    #[test]
    fn strips_osc_title() {
        let mut s = AnsiStripper::new();
        let out = s.feed(b"\x1b]0;title\x07hello");
        assert_eq!(out, "hello");
    }

    #[test]
    fn handles_sequence_split_across_chunks() {
        let mut s = AnsiStripper::new();
        assert_eq!(s.feed(b"a\x1b[3"), "a");
        assert_eq!(s.feed(b"1mb"), "b");
    }

    #[test]
    fn keeps_utf8_and_newlines() {
        let mut s = AnsiStripper::new();
        let out = s.feed("héllo\n世界\r\n".as_bytes());
        assert_eq!(out, "héllo\n世界\n");
    }
}
