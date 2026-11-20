//! 串流文字的行緩衝。
//!
//! 結構化協定 (ACP、Claude stream-json、Codex app-server)都是**一小段一小段**吐字的。
//! 若每個 chunk 都當成一則輸出，transcript 就會變成「一行一字」。
//! [`StreamBuf`] 把 chunk 併回完整行再送出，並在換種類或回合結束時收尾。

const EARLY_FLUSH_CHARS: usize = 120;

/// 把 token 級 chunk 併成完整行的緩衝。
#[derive(Default)]
pub struct StreamBuf {
    /// 目前累積的種類 (例如 `"msg"` / `"think"`)；換種類時會先收尾。
    kind: &'static str,
    text: String,
}

impl StreamBuf {
    /// Prefix each line by kind. Thoughts use the emoji-free `⋯ ` marker.
    fn decorate(kind: &str, line: &str) -> String {
        if kind == "think" {
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
        let mut out = String::new();
        if self.kind != kind {
            out.push_str(&self.take());
            self.kind = kind;
        }
        self.text.push_str(text);
        while let Some(idx) = self.text.find('\n') {
            let line: String = self.text.drain(..=idx).collect();
            out.push_str(&Self::decorate(kind, &line));
        }
        // Long single-paragraph reasoning must stream immediately instead of waiting
        // for the end of the thought block (which may never get a trailing newline).
        if kind == "think" && self.text.chars().count() >= EARLY_FLUSH_CHARS {
            let split = Self::early_flush_split(&self.text, EARLY_FLUSH_CHARS);
            let line: String = self.text.drain(..split).collect();
            out.push_str(&Self::decorate(kind, line.trim_end()));
            out.push('\n');
        }
        out
    }

    /// 收尾：把尚未遇到換行的殘留當成一行送出。
    pub fn flush(&mut self) -> String {
        self.take()
    }

    fn take(&mut self) -> String {
        if self.text.is_empty() {
            return String::new();
        }
        let line = std::mem::take(&mut self.text);
        let mut s = Self::decorate(self.kind, &line);
        s.push('\n');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::StreamBuf;

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
    fn flush_on_empty_is_noop() {
        let mut b = StreamBuf::default();
        assert_eq!(b.flush(), "");
        assert_eq!(b.push("msg", "\n"), "\n");
    }
}
