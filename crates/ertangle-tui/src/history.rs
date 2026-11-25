//! 輸入歷史 (↑↓ 回顧、跨重開儲存)。

use std::io::Write;
use std::path::PathBuf;

/// 每種歷史最多保留幾筆。
const MAX_ENTRIES: usize = 500;

/// 一種輸入的歷史 (prompt 或命令)。
#[derive(Default)]
pub(crate) struct History {
    entries: Vec<String>,
    /// 目前回顧到哪一筆；`None` = 在草稿上。
    pos: Option<usize>,
    /// 開始回顧前正在輸入的內容。
    draft: String,
    path: Option<PathBuf>,
}

impl History {
    /// 從檔案載入 (每行一個 JSON 字串)；`path` 為 `None` 時只存在記憶體 (測試用)。
    pub(crate) fn load(path: Option<PathBuf>) -> Self {
        let mut entries: Vec<String> = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| {
                s.lines()
                    .filter_map(|l| serde_json::from_str::<String>(l).ok())
                    .collect()
            })
            .unwrap_or_default();
        if entries.len() > MAX_ENTRIES {
            entries.drain(..entries.len() - MAX_ENTRIES);
        }
        History {
            entries,
            pos: None,
            draft: String::new(),
            path,
        }
    }

    /// 記一筆已送出的輸入。空白與和上一筆相同的不記。
    pub(crate) fn push(&mut self, text: &str) {
        self.pos = None;
        let t = text.trim();
        if t.is_empty() || self.entries.last().map(String::as_str) == Some(t) {
            return;
        }
        self.entries.push(t.to_string());
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        if let Some(p) = &self.path {
            if let Some(dir) = p.parent() {
                // 目錄建立失敗後續開啟檔案會處理
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
            {
                // 歷史記錄寫入失敗不影響主流程
                let _ = writeln!(f, "{}", serde_json::Value::String(t.to_string()));
            }
        }
    }

    /// 往前一筆 (較舊)。`current` 是輸入框目前的內容，第一次回顧時當草稿存起來。
    pub(crate) fn prev(&mut self, current: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.pos {
            None => {
                self.draft = current.to_string();
                self.entries.len().saturating_sub(1)
            }
            Some(0) => 0,
            Some(i) => i.saturating_sub(1),
        };
        self.pos = Some(next);
        self.entries.get(next).cloned()
    }

    /// 往後一筆 (較新)；越過最新一筆就回到草稿。
    pub(crate) fn next(&mut self) -> Option<String> {
        let i = self.pos?;
        if i + 1 < self.entries.len() {
            self.pos = Some(i + 1);
            self.entries.get(i + 1).cloned()
        } else {
            self.pos = None;
            Some(std::mem::take(&mut self.draft))
        }
    }

    pub(crate) fn reset(&mut self) {
        self.pos = None;
    }
}

/// 歷史檔位置。
pub(crate) fn history_path(kind: &str) -> PathBuf {
    ertangle_daemon::state_dir().join(format!("history-{kind}.jsonl"))
}
