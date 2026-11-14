//! 串流文字的行緩衝。
//!
//! 結構化協定 (ACP、Claude stream-json、Codex app-server)都是**一小段一小段**吐字的。
//! 若每個 chunk 都當成一則輸出，transcript 就會變成「一行一字」。
//! [`StreamBuf`] 把 chunk 併回完整行再送出，並在換種類或回合結束時收尾。

/// 把 token 級 chunk 併成完整行的緩衝。
#[derive(Default)]
pub struct StreamBuf {
    /// 目前累積的種類 (例如 `"msg"` / `"think"`)；換種類時會先收尾。
    kind: &'static str,
    text: String,
}

impl StreamBuf {
    /// 依種類替每行加上前綴 (思考用 `💭 `)。
    fn decorate(kind: &str, line: &str) -> String {
        if kind == "think" {
            format!("💭 {line}")
        } else {
            line.to_string()
        }
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
        assert_eq!(b.push("msg", "ok\n"), "💭 hmm\nok\n");
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
