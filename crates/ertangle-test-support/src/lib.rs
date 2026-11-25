//! Test-only helpers for temporary directories that cannot leak between runs.
//!
//! Each test runs on its own thread. Guards kept in a thread-local are dropped
//! when that thread finishes, so `TempDir` removes the directory even if the
//! test panics.

use std::cell::RefCell;
use std::path::PathBuf;

thread_local! {
    static GUARDS: RefCell<Vec<tempfile::TempDir>> = const { RefCell::new(Vec::new()) };
}

/// Create a unique temporary directory kept alive until the current test thread exits.
pub fn temp_dir(tag: &str) -> PathBuf {
    // This crate is only used as a dev-dependency; failing to create a test
    // directory should fail the test loudly instead of silently sharing state.
    #[expect(
        clippy::expect_used,
        reason = "測試專用 crate，建立暫存目錄失敗應直接中止測試"
    )]
    let dir = tempfile::Builder::new()
        .prefix(&format!("{tag}_"))
        .tempdir()
        .expect("create test temp dir");
    // macOS 的 /var 是 /private/var 的符號連結；回傳正規化路徑，避免與
    // 產品程式 canonicalize 後的路徑不一致 (例如 check 的輸出化簡)。
    let path = dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| dir.path().to_path_buf());
    GUARDS.with(|guards| guards.borrow_mut().push(dir));
    path
}
