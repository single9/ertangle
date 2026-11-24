//! 串流文字的行緩衝。
//!
//! 結構化協定 (ACP、Claude stream-json、Codex app-server)都是**一小段一小段**吐字的。
//! 若每個 chunk 都當成一則輸出，transcript 就會變成「一行一字」。
//! [`StreamBuf`] 把 chunk 併回完整行再送出，並在換種類或回合結束時收尾。

const EARLY_FLUSH_CHARS: usize = 120;

/// 一次可送出的文字。`append` 為 true 時，`text` 的第一段要接在上一次
/// 送出、尚未換行的那一行後面 (見 [`StreamBuf::idle_flush_chunk`])。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    pub append: bool,
}

/// 把 token 級 chunk 併成完整行的緩衝。
#[derive(Default)]
pub struct StreamBuf {
    /// 目前累積的種類 (例如 `"msg"` / `"think"`)；換種類時會先收尾。
    kind: &'static str,
    text: String,
    /// 已經以「未換行」的形式送出過半行 (見 [`Self::idle_flush_chunk`])，
    /// 下一次送出的第一段必須接在那一行後面，且不能再加思考前綴。
    open: bool,
}

impl StreamBuf {
    /// Prefix each line by kind. Thoughts use the emoji-free `⋯ ` marker; a
    /// continuation of an already-prefixed line never gets a second prefix.
    fn decorate(kind: &str, line: &str, continuation: bool) -> String {
        if kind == "think" && !continuation {
            format!("⋯ {line}")
        } else {
            line.to_string()
        }
    }

    /// Pick a byte index to split an over-long thought without a newline.
    /// Prefer the last whitespace near the limit so words stay intact; the returned
    /// index is past that whitespace so the remainder never starts with a space.
    fn early_flush_split(text: &str, limit: usize) -> usize {
        let cut = text
            .char_indices()
            .nth(limit)
            .map_or(text.len(), |(i, _)| i);
        let head = &text[..cut];
        head.char_indices()
            .rev()
            .find(|(_, ch)| ch.is_whitespace())
            .map(|(i, ch)| i + ch.len_utf8())
            .filter(|end| *end >= cut.saturating_sub(24))
            .unwrap_or(cut)
    }

    /// 併入一段 chunk，回傳可送出的完整行 (可能多行、含換行)。
    pub fn push(&mut self, kind: &'static str, text: &str) -> String {
        self.push_chunk(kind, text).text
    }

    /// 同 [`Self::push`]，但保留「接在上一行後面」的資訊。
    pub fn push_chunk(&mut self, kind: &'static str, text: &str) -> Chunk {
        let append = self.open;
        let mut out = String::new();
        if self.kind != kind {
            out.push_str(&self.take().text);
            self.kind = kind;
        }
        // Only the first emitted segment can continue the open line; a kind switch
        // above has already closed it.
        let mut cont = self.open;
        self.text.push_str(text);
        while let Some(idx) = self.text.find('\n') {
            let line: String = self.text.drain(..=idx).collect();
            out.push_str(&Self::decorate(kind, &line, cont));
            cont = false;
            self.open = false;
        }
        // Long single-paragraph reasoning must stream immediately instead of waiting
        // for the end of the thought block (which may never get a trailing newline).
        if kind == "think" && self.text.chars().count() >= EARLY_FLUSH_CHARS {
            let split = Self::early_flush_split(&self.text, EARLY_FLUSH_CHARS);
            let line: String = self.text.drain(..split).collect();
            out.push_str(&Self::decorate(kind, line.trim_end(), cont));
            out.push('\n');
            self.open = false;
        }
        Chunk {
            append: append && !out.is_empty(),
            text: out,
        }
    }

    /// 收尾：把尚未遇到換行的殘留當成一行送出。
    pub fn flush(&mut self) -> String {
        self.take().text
    }

    /// 同 [`Self::flush`]，但保留「接在上一行後面」的資訊。
    pub fn flush_chunk(&mut self) -> Chunk {
        self.take()
    }

    /// 串流停頓時把殘留文字**不加換行**送出，避免把一行 (或一個 Markdown
    /// 表格列 / 行內程式碼) 從中間切斷；後續片段會以 `append` 接回同一行。
    pub fn idle_flush_chunk(&mut self) -> Chunk {
        if self.text.is_empty() {
            return Chunk::default();
        }
        let line = std::mem::take(&mut self.text);
        let append = self.open;
        self.open = true;
        Chunk {
            text: Self::decorate(self.kind, &line, append),
            append,
        }
    }

    fn take(&mut self) -> Chunk {
        let append = std::mem::take(&mut self.open);
        if self.text.is_empty() {
            // Close a dangling partial line with a bare newline.
            return if append {
                Chunk {
                    text: "\n".to_string(),
                    append: true,
                }
            } else {
                Chunk::default()
            };
        }
        let line = std::mem::take(&mut self.text);
        let mut s = Self::decorate(self.kind, &line, append);
        s.push('\n');
        Chunk { text: s, append }
    }
}

#[cfg(test)]
mod tests {
    use super::{Chunk, StreamBuf};

    #[test]
    fn coalesces_token_chunks_into_lines() {
        let mut b = StreamBuf::default();
        assert_eq!(b.push("msg", "Hel"), "");
        assert_eq!(b.push("msg", "lo"), "");
        assert_eq!(b.push("msg", " world\n"), "Hello world\n");
        assert_eq!(b.push("msg", "next"), "");
        assert_eq!(b.flush(), "next\n");
    }

    #[test]
    fn prefixes_thoughts_and_flushes_on_kind_change() {
        let mut b = StreamBuf::default();
        assert_eq!(b.push("think", "hmm"), "");
        assert_eq!(b.push("msg", "ok\n"), "⋯ hmm\nok\n");
    }

    #[test]
    fn flushes_long_thought_without_newline() {
        let mut b = StreamBuf::default();
        let chunk = "a".repeat(130);
        let out = b.push("think", &chunk);
        assert!(out.starts_with("⋯ "), "{out:?}");
        assert!(out.ends_with('\n'));
        assert_eq!(out.chars().count(), 123); // prefix (2) + 120 chars + newline
        assert_eq!(b.flush(), format!("⋯ {}\n", "a".repeat(10)));
    }

    #[test]
    fn flushes_long_thought_at_word_boundary() {
        let mut b = StreamBuf::default();
        let chunk = "word ".repeat(30);
        let out = b.push("think", &chunk);
        let body = out.strip_prefix("⋯ ").unwrap().trim_end_matches('\n');
        assert!(body.chars().count() <= 120, "{body:?}");
        assert!(body.ends_with("word"), "{body:?}");
        assert!(!b.text.is_empty());
        assert!(!b.flush().starts_with("⋯  "));
    }

    #[test]
    fn handles_multiple_lines_in_one_chunk() {
        let mut b = StreamBuf::default();
        assert_eq!(b.push("msg", "a\nb\nc"), "a\nb\n");
        assert_eq!(b.flush(), "c\n");
    }

    #[test]
    fn idle_flush_emits_partial_line_without_newline_then_appends() {
        let mut b = StreamBuf::default();
        assert_eq!(b.push_chunk("msg", "foo `00:03"), Chunk::default());
        let first = b.idle_flush_chunk();
        assert_eq!(
            first,
            Chunk {
                text: "foo `00:03".into(),
                append: false
            }
        );
        // Nothing buffered: a second idle flush is a no-op and keeps the line open.
        assert_eq!(b.idle_flush_chunk(), Chunk::default());
        let rest = b.push_chunk("msg", ":00` bar\nnext");
        assert_eq!(
            rest,
            Chunk {
                text: ":00` bar\n".into(),
                append: true
            }
        );
        assert_eq!(
            b.flush_chunk(),
            Chunk {
                text: "next\n".into(),
                append: false
            }
        );
    }

    #[test]
    fn closing_an_open_line_with_nothing_buffered_sends_bare_newline() {
        let mut b = StreamBuf::default();
        b.push_chunk("msg", "abc");
        b.idle_flush_chunk();
        assert_eq!(
            b.flush_chunk(),
            Chunk {
                text: "\n".into(),
                append: true
            }
        );
        assert_eq!(b.flush_chunk(), Chunk::default());
    }

    #[test]
    fn continuation_of_thought_has_no_second_prefix() {
        let mut b = StreamBuf::default();
        b.push_chunk("think", "hmm");
        assert_eq!(b.idle_flush_chunk().text, "⋯ hmm");
        let c = b.push_chunk("think", " more\n");
        assert_eq!(
            c,
            Chunk {
                text: " more\n".into(),
                append: true
            }
        );
    }

    #[test]
    fn kind_switch_closes_open_line_then_starts_new() {
        let mut b = StreamBuf::default();
        b.push_chunk("think", "hmm");
        b.idle_flush_chunk();
        let c = b.push_chunk("msg", "ok\n");
        assert_eq!(
            c,
            Chunk {
                text: "\nok\n".into(),
                append: true
            }
        );
    }

    #[test]
    fn flush_on_empty_is_noop() {
        let mut b = StreamBuf::default();
        assert_eq!(b.flush(), "");
        assert_eq!(b.push("msg", "\n"), "\n");
    }
}
