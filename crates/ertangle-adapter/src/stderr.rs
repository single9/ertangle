//! 子處理程序 stderr 的收集：固定大小的環狀緩衝，只留最後幾行。
//!
//! stderr 必須持續讀取，否則 agent 大量輸出時 pipe 塞滿會把 agent 卡死。
//! 處理程序以非 0 結束時，把這幾行當成 `Output` 送出，讓使用者看得到原因。

use crate::ansi::AnsiStripper;
use crate::AgentEvent;
use std::collections::VecDeque;
use std::io::Read;
use std::process::ChildStderr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tr::tr;

/// 最多保留的行數。
const MAX_LINES: usize = 20;
/// 單行最多保留的位元組數 (超過的部分丟掉)。
const MAX_LINE_BYTES: usize = 500;

#[derive(Default)]
struct Inner {
    lines: VecDeque<String>,
    cur: Vec<u8>,
}

impl Inner {
    fn push_text(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\n' {
                self.end_line();
            } else if self.cur.len() < MAX_LINE_BYTES {
                let mut buf = [0u8; 4];
                self.cur
                    .extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
    }

    fn end_line(&mut self) {
        let line = String::from_utf8_lossy(&self.cur).trim_end().to_string();
        self.cur.clear();
        if line.is_empty() {
            return;
        }
        if self.lines.len() == MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }
}

/// stderr 的尾巴。
pub struct StderrTail {
    inner: Arc<Mutex<Inner>>,
    done: Mutex<Receiver<()>>,
}

impl StderrTail {
    /// 啟動讀取執行緒，持續把 stderr 排乾。
    pub fn spawn(mut stderr: ChildStderr) -> Arc<StderrTail> {
        let inner = Arc::new(Mutex::new(Inner::default()));
        let (done_tx, done_rx) = mpsc::channel();
        let shared = inner.clone();
        std::thread::spawn(move || {
            let mut strip = AnsiStripper::new();
            let mut buf = [0u8; 8192];
            // 非 UTF-8 邊界被切開的位元組交給 lossy 處理即可；這裡只求不卡 pipe。
            while let Ok(n) = stderr.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let text = strip.feed(buf.get(..n).unwrap_or_default());
                shared
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_text(&text);
            }
            shared.lock().unwrap_or_else(|e| e.into_inner()).end_line();
            // 讀取執行緒結束通知，接收端逾時放棄時忽略
            let _ = done_tx.send(());
        });
        Arc::new(StderrTail {
            inner,
            done: Mutex::new(done_rx),
        })
    }

    /// 目前保留的行 (最舊在前)。
    pub fn lines(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lines
            .iter()
            .cloned()
            .collect()
    }

    /// 處理程序結束後呼叫：短暫等讀取執行緒收尾；非 0 結束且有內容時送出 `Output`。
    pub fn report_exit(&self, code: Option<i32>, tx: &Sender<AgentEvent>) {
        // 等待背景 stderr 讀取結束，逾時或無訊息皆繼續回報
        let _ = self
            .done
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recv_timeout(Duration::from_millis(500));
        if !matches!(code, Some(c) if c != 0) {
            return;
        }
        let lines = self.lines();
        if lines.is_empty() {
            return;
        }
        // 接收端若已關閉則忽略輸出事件
        let _ = tx.send(AgentEvent::Output(tr!(
            "⚠ 處理程序以結束碼 {code} 結束，stderr 最後 {count} 行：\n{lines}\n",
            code = code.unwrap_or(-1),
            count = lines.len(),
            lines = lines.join("\n")
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_last_lines_and_truncates_long_ones() {
        let mut i = Inner::default();
        for n in 0..50 {
            i.push_text(&format!("line {n}\n"));
        }
        i.push_text(&"x".repeat(5000));
        i.end_line();
        assert_eq!(i.lines.len(), MAX_LINES);
        assert_eq!(i.lines.front().unwrap(), "line 31");
        assert_eq!(i.lines.back().unwrap().len(), MAX_LINE_BYTES);
    }
}
