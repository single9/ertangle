//! daemon 使用的目錄與檔案路徑。

use super::*;

// ---------------------------------------------------------------------------
// 路徑
// ---------------------------------------------------------------------------

pub(crate) fn user_tag() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".into())
}

/// 執行期目錄 (放 socket)。
pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d).join("termade"),
        None => std::env::temp_dir().join(format!("termade-{}", user_tag())),
    }
}

/// 狀態目錄 (放 event log)。
pub fn state_dir() -> PathBuf {
    match std::env::var_os("XDG_STATE_HOME") {
        Some(d) => PathBuf::from(d).join("termade"),
        None => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(".local/state/termade"),
            None => std::env::temp_dir().join("termade-state"),
        },
    }
}

/// 設定目錄 (放 `policy.toml`)。
pub fn config_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) => PathBuf::from(d).join("termade"),
        None => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(".config/termade"),
            None => std::env::temp_dir().join("termade-config"),
        },
    }
}

pub fn socket_path() -> PathBuf {
    if let Some(s) = std::env::var_os("TERMADE_SOCKET") {
        return PathBuf::from(s);
    }
    runtime_dir().join("daemon.sock")
}

pub fn log_path() -> PathBuf {
    state_dir().join("events.jsonl")
}
