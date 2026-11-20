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
        Some(d) => PathBuf::from(d).join("ertangle"),
        None => std::env::temp_dir().join(format!("ertangle-{}", user_tag())),
    }
}

/// 舊名稱 (termade) 的目錄名；相容層保留至 v1.0。
const LEGACY_DIR_NAME: &str = "termade";

/// 新目錄不存在而舊目錄存在時，把舊目錄搬到新位置 (記錄一行 log)；
/// 搬移失敗則退回舊路徑，讓既有資料仍可讀取。
fn migrate_legacy_dir(new: PathBuf, legacy: PathBuf) -> PathBuf {
    if new.exists() || !legacy.is_dir() {
        return new;
    }
    match std::fs::rename(&legacy, &new) {
        Ok(()) => {
            tracing::info!("已把舊目錄 {legacy:?} 搬移到 {new:?}");
            new
        }
        Err(e) => {
            tracing::warn!("搬移舊目錄 {legacy:?} 失敗，繼續使用舊路徑：{e}");
            legacy
        }
    }
}

/// 狀態目錄路徑 (純計算，不碰檔案系統)：回傳 (新目錄, 舊目錄)。
fn state_dirs(xdg: Option<PathBuf>, home: Option<PathBuf>) -> (PathBuf, PathBuf) {
    let base = match (xdg, home) {
        (Some(d), _) => d,
        (None, Some(h)) => h.join(".local/state"),
        (None, None) => {
            let t = std::env::temp_dir();
            return (t.join("ertangle-state"), t.join("termade-state"));
        }
    };
    (base.join("ertangle"), base.join(LEGACY_DIR_NAME))
}

/// 設定目錄路徑 (純計算)：回傳 (新目錄, 舊目錄)。
fn config_dirs(xdg: Option<PathBuf>, home: Option<PathBuf>) -> (PathBuf, PathBuf) {
    let base = match (xdg, home) {
        (Some(d), _) => d,
        (None, Some(h)) => h.join(".config"),
        (None, None) => {
            let t = std::env::temp_dir();
            return (t.join("ertangle-config"), t.join("termade-config"));
        }
    };
    (base.join("ertangle"), base.join(LEGACY_DIR_NAME))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// 狀態目錄 (放 event log)；必要時自動搬移舊的 termade 目錄。
pub fn state_dir() -> PathBuf {
    let (new, legacy) = state_dirs(env_path("XDG_STATE_HOME"), env_path("HOME"));
    migrate_legacy_dir(new, legacy)
}

/// 設定目錄 (放 `policy.toml`)；必要時自動搬移舊的 termade 目錄。
pub fn config_dir() -> PathBuf {
    let (new, legacy) = config_dirs(env_path("XDG_CONFIG_HOME"), env_path("HOME"));
    migrate_legacy_dir(new, legacy)
}

pub fn socket_path() -> PathBuf {
    if let Some(s) = ertangle_core::env_var_os("ERTANGLE_SOCKET") {
        return PathBuf::from(s);
    }
    runtime_dir().join("daemon.sock")
}

pub fn log_path() -> PathBuf {
    state_dir().join("events.jsonl")
}

/// 預設遠端 Token 路徑 (~/.local/state/ertangle/remote.token)。
pub fn default_token_path() -> PathBuf {
    state_dir().join("remote.token")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(ertangle_core::new_id(tag));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dirs_use_xdg_then_home() {
        let (n, l) = state_dirs(Some("/x".into()), Some("/h".into()));
        assert_eq!((n, l), ("/x/ertangle".into(), "/x/termade".into()));
        let (n, l) = config_dirs(None, Some("/h".into()));
        assert_eq!(
            (n, l),
            ("/h/.config/ertangle".into(), "/h/.config/termade".into())
        );
    }

    #[test]
    fn migrates_legacy_dir_once() {
        let base = tmp("mig");
        let (new, legacy) = (base.join("ertangle"), base.join("termade"));
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("events.jsonl"), "x").unwrap();
        assert_eq!(migrate_legacy_dir(new.clone(), legacy.clone()), new);
        assert!(new.join("events.jsonl").is_file());
        assert!(!legacy.exists());
        // 第二次不再動作。
        assert_eq!(migrate_legacy_dir(new.clone(), legacy), new);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn keeps_new_dir_when_both_exist() {
        let base = tmp("both");
        let (new, legacy) = (base.join("ertangle"), base.join("termade"));
        std::fs::create_dir_all(&new).unwrap();
        std::fs::create_dir_all(&legacy).unwrap();
        assert_eq!(migrate_legacy_dir(new.clone(), legacy.clone()), new);
        assert!(legacy.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn falls_back_to_legacy_when_rename_fails() {
        let base = tmp("fail");
        let legacy = base.join("termade");
        std::fs::create_dir_all(&legacy).unwrap();
        // 新路徑的父目錄不存在，rename 必然失敗。
        let new = base.join("missing-parent").join("ertangle");
        assert_eq!(migrate_legacy_dir(new, legacy.clone()), legacy);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn no_legacy_returns_new() {
        let base = tmp("none");
        let new = base.join("ertangle");
        assert_eq!(migrate_legacy_dir(new.clone(), base.join("termade")), new);
        let _ = std::fs::remove_dir_all(&base);
    }
}
