//! 輸入歷史（↑↓ 回顧、跨重開保存）與 prompt 排隊（session 忙碌時先排著，閒下來自動送）。

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use termade_core::{Attachment, SessionId, SessionStatus};

/// 每種歷史最多保留幾筆。
const MAX_ENTRIES: usize = 500;
/// 送出排隊 prompt 後，最久等多久才當作 session 已經開始處理。
const INFLIGHT_TIMEOUT: Duration = Duration::from_secs(3);

/// 一種輸入的歷史（prompt 或命令）。
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
    /// 從檔案載入（每行一個 JSON 字串）；`path` 為 `None` 時只存在記憶體（測試用）。
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
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
            {
                let _ = writeln!(f, "{}", serde_json::Value::String(t.to_string()));
            }
        }
    }

    /// 往前一筆（較舊）。`current` 是輸入框目前的內容，第一次回顧時當草稿存起來。
    pub(crate) fn prev(&mut self, current: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.pos {
            None => {
                self.draft = current.to_string();
                self.entries.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.pos = Some(next);
        Some(self.entries[next].clone())
    }

    /// 往後一筆（較新）；越過最新一筆就回到草稿。
    pub(crate) fn next(&mut self) -> Option<String> {
        let i = self.pos?;
        if i + 1 < self.entries.len() {
            self.pos = Some(i + 1);
            Some(self.entries[i + 1].clone())
        } else {
            self.pos = None;
            Some(std::mem::take(&mut self.draft))
        }
    }

    pub(crate) fn reset(&mut self) {
        self.pos = None;
    }
}

/// 一則排隊中的 prompt（附件在排隊時就讀好，避免送出時檔案已不見）。
pub(crate) struct Queued {
    pub(crate) text: String,
    pub(crate) attachments: Vec<Attachment>,
}

/// 各 session 的排隊 prompt。
#[derive(Default)]
pub(crate) struct PromptQueues {
    queues: HashMap<SessionId, VecDeque<Queued>>,
    /// 剛送出一則、還沒看到 session 轉為忙碌的 session。
    inflight: HashMap<SessionId, Instant>,
}

impl PromptQueues {
    pub(crate) fn push(&mut self, id: &str, q: Queued) {
        self.queues.entry(id.to_string()).or_default().push_back(q);
    }

    pub(crate) fn len(&self, id: &str) -> usize {
        self.queues.get(id).map_or(0, VecDeque::len)
    }

    pub(crate) fn texts(&self, id: &str) -> Vec<String> {
        self.queues
            .get(id)
            .map(|q| q.iter().map(|x| x.text.clone()).collect())
            .unwrap_or_default()
    }

    /// 取消最後排入的一則。
    pub(crate) fn pop_last(&mut self, id: &str) -> Option<Queued> {
        self.queues.get_mut(id)?.pop_back()
    }

    pub(crate) fn clear(&mut self, id: &str) -> usize {
        self.queues.remove(id).map_or(0, |q| q.len())
    }

    /// 依各 session 目前狀態，取出現在該送的下一則：閒置、且沒有剛送出還在等的。
    /// 已結束或消失的 session 會丟掉它的佇列，回傳丟掉的筆數供提示。
    pub(crate) fn take_ready(
        &mut self,
        status_of: impl Fn(&str) -> Option<SessionStatus>,
        now: Instant,
    ) -> (Vec<(SessionId, Queued)>, usize) {
        let mut ready = Vec::new();
        let mut dropped = 0;
        let ids: Vec<SessionId> = self.queues.keys().cloned().collect();
        for id in ids {
            match status_of(&id) {
                None | Some(SessionStatus::Exited | SessionStatus::Failed) => {
                    dropped += self.clear(&id);
                    self.inflight.remove(&id);
                }
                Some(SessionStatus::Idle) => {
                    if let Some(t) = self.inflight.get(&id) {
                        if now.duration_since(*t) < INFLIGHT_TIMEOUT {
                            continue;
                        }
                    }
                    self.inflight.remove(&id);
                    if let Some(q) = self.queues.get_mut(&id).and_then(VecDeque::pop_front) {
                        self.inflight.insert(id.clone(), now);
                        ready.push((id, q));
                    }
                }
                Some(_) => {
                    // 已經在處理（忙碌／等審批…）：送出的那則已被接手。
                    self.inflight.remove(&id);
                }
            }
        }
        self.queues.retain(|_, q| !q.is_empty());
        (ready, dropped)
    }
}

/// 歷史檔位置。
pub(crate) fn history_path(kind: &str) -> PathBuf {
    termade_daemon::state_dir().join(format!("history-{kind}.jsonl"))
}
