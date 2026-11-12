//! event log 寫入執行緒、輪替與重試。

use super::*;

// ---------------------------------------------------------------------------
// event log 寫入執行緒（T07 / T08）
// ---------------------------------------------------------------------------

pub(crate) const DEFAULT_LOG_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const DEFAULT_LOG_KEEP: usize = 3;
/// log 開檔失敗後，重試開檔的最短間隔。
pub(crate) const LOG_RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// log 輪替設定。
#[derive(Debug, Clone, Copy)]
pub struct LogConfig {
    pub max_bytes: u64,
    pub keep: usize,
}

impl LogConfig {
    /// 讀取 `ADE_LOG_MAX_BYTES`（預設 64 MB）與 `ADE_LOG_KEEP`（預設 3 份）。
    pub fn from_env() -> Self {
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        LogConfig {
            max_bytes: get("ADE_LOG_MAX_BYTES")
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_LOG_MAX_BYTES),
            keep: get("ADE_LOG_KEEP").map_or(DEFAULT_LOG_KEEP, |n| n as usize),
        }
    }
}

pub(crate) enum LogMsg {
    Rec(Box<Record>),
    Flush(Sender<()>),
}

/// 第 `n` 份舊檔路徑：`events.jsonl.<n>`。
pub fn rotated_log_path(path: &Path, n: usize) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

pub(crate) struct LogWriter {
    path: PathBuf,
    cfg: LogConfig,
    file: Option<File>,
    size: u64,
    state: State,
    ok: Arc<AtomicBool>,
    health_tx: Option<Sender<bool>>,
    last_try: Option<std::time::Instant>,
}

impl LogWriter {
    pub(crate) fn open(&mut self) -> bool {
        if self.file.is_some() {
            return true;
        }
        if self
            .last_try
            .is_some_and(|t| t.elapsed() < LOG_RETRY_INTERVAL)
        {
            return false;
        }
        self.last_try = Some(std::time::Instant::now());
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(f) => {
                self.size = f.metadata().map_or(0, |m| m.len());
                self.file = Some(f);
                true
            }
            Err(e) => {
                self.fail(&e);
                false
            }
        }
    }

    pub(crate) fn fail(&mut self, e: &std::io::Error) {
        if self.ok.swap(false, Ordering::SeqCst) {
            tracing::error!("event log 寫入失敗（{:?}）：{e}；之後定期重試", self.path);
            if let Some(tx) = &self.health_tx {
                let _ = tx.send(false);
            }
        }
    }

    pub(crate) fn write_line(&mut self, line: &str) {
        if !self.open() {
            return;
        }
        let Some(f) = self.file.as_mut() else { return };
        let res = f
            .write_all(line.as_bytes())
            .and_then(|_| f.write_all(b"\n"));
        match res {
            Ok(()) => {
                self.size += line.len() as u64 + 1;
                if !self.ok.swap(true, Ordering::SeqCst) {
                    tracing::info!("event log 已恢復寫入");
                    if let Some(tx) = &self.health_tx {
                        let _ = tx.send(true);
                    }
                }
            }
            Err(e) => {
                self.file = None;
                self.fail(&e);
            }
        }
    }

    /// 超過門檻就輪替：舊檔後移、開新檔，新檔第一行寫入目前狀態的 snapshot。
    pub(crate) fn maybe_rotate(&mut self) {
        if self.file.is_none() || self.size < self.cfg.max_bytes {
            return;
        }
        if !fs::metadata(&self.path).is_ok_and(|m| m.is_file()) {
            return;
        }
        let keep = self.cfg.keep.max(1);
        self.file = None;
        let _ = fs::remove_file(rotated_log_path(&self.path, keep));
        for n in (1..keep).rev() {
            let _ = fs::rename(
                rotated_log_path(&self.path, n),
                rotated_log_path(&self.path, n + 1),
            );
        }
        if let Err(e) = fs::rename(&self.path, rotated_log_path(&self.path, 1)) {
            self.fail(&e);
            return;
        }
        self.last_try = None;
        self.size = 0;
        if !self.open() {
            return;
        }
        let snap = Record {
            seq: self.state.seq,
            ts: now_millis(),
            event: Event::Snapshot {
                state: Box::new(self.state.clone()),
            },
        };
        if let Ok(line) = serde_json::to_string(&snap) {
            self.write_line(&redact_secrets(&line));
        }
    }

    pub(crate) fn handle(&mut self, rec: &Record) {
        self.maybe_rotate();
        if let Ok(line) = serde_json::to_string(rec) {
            self.write_line(&redact_secrets(&line));
        }
        self.state.apply(rec);
    }
}

/// 啟動 log 寫入執行緒。`state` 是目前已重播的狀態（供輪替時寫 snapshot）。
pub(crate) fn spawn_log_writer(
    path: PathBuf,
    state: State,
    cfg: LogConfig,
    health_tx: Option<Sender<bool>>,
) -> (Sender<LogMsg>, Arc<AtomicBool>) {
    let (tx, rx) = mpsc::channel::<LogMsg>();
    let ok = Arc::new(AtomicBool::new(true));
    let mut w = LogWriter {
        path,
        cfg,
        file: None,
        size: 0,
        state,
        ok: ok.clone(),
        health_tx,
        last_try: None,
    };
    w.open();
    std::thread::spawn(move || {
        for msg in rx {
            match msg {
                LogMsg::Rec(rec) => w.handle(&rec),
                LogMsg::Flush(done) => {
                    let _ = done.send(());
                }
            }
        }
    });
    (tx, ok)
}
