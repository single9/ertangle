# T52：SAST 靜態安全分析修復 (WebSocket 長度上限、常數時間比對、Web 檔案存取隔離、DOM XSS 與暫存檔安全)

Status: ready-for-agent

## 問題

針對專案執行靜態安全分析 (SAST) 後，發現以下 5 項安全風險：

1. **[高風險] WebSocket Frame 未檢查 64-bit 承載長度上限 (OOM DoS)**
   - 位置：`crates/termade-daemon/src/web.rs` (`read_ws_frame`)
   - 說明：當 `len == 127` 時，直接將 64-bit 長度轉為 `usize` 並配置 `vec![0u8; len]`，攻擊者（含未認證連線）可傳送超大長度觸發記憶體耗盡崩潰 (OOM panic)。
2. **[中風險] `token_matches` 常數時間比對的長度截斷 (`as u8`)**
   - 位置：`crates/termade-daemon/src/server.rs` (`token_matches`)
   - 說明：`let mut diff = (a.len() ^ b.len()) as u8;` 將長度 XOR 截斷為 8 位元。若長度差為 256 的倍數且多出的位元組為 `\0`，長度檢查將失效。
3. **[中風險] Web `/file` 端點缺乏目錄白名單隔離 (Arbitrary File Read)**
   - 位置：`crates/termade-daemon/src/web.rs` (`serve_file`)
   - 說明：若傳入絕對路徑（如 `/etc/passwd` 或 `~/.ssh/id_rsa`）或透過 `..` 跳脫目錄，`serve_file` 僅檢查檔案是否存在與大小上限便回傳內容。
4. **[中風險] Web 前端 `escapeHtml` 未轉義引號與 Markdown 連結未攔截 `javascript:` 偽協議 (DOM XSS)**
   - 位置：`crates/termade-daemon/src/web/index.html` (`escapeHtml` 與 `renderMarkdown`)
   - 說明：`escapeHtml` 未轉義 `"` 與 `'`，在 `onclick="openFileActionSheet('${escapeHtml(rawPath)}', ...)"` 等屬性上下文中可被單引號跳脫；Markdown 連結對非 `http://`、`https://`、`file://` 的網址直接渲染 `<a href="...">`，允許 `javascript:` 偽協議。
5. **[低風險] 剪貼簿圖片暫存檔路徑可預測且未使用獨占建立 (Predictable Temp File)**
   - 位置：`crates/termade-tui/src/attach.rs` (`clipboard_image`)
   - 說明：直接以 `std::fs::write` 寫入 `/tmp/termade-clip-{ts}.png`，在多使用者環境下檔名可預測且未防範 symlink 競態與檔案權限問題。

## 實作方式

1. **WebSocket Frame 長度上限 (`crates/termade-daemon/src/web.rs`)**：
   - 定義常數 `const MAX_WS_PAYLOAD_LEN: usize = 30 * 1024 * 1024;` (30 MB)。
   - 在 `read_ws_frame` 解析完 `len` 後、配置 `vec![0u8; len]` 前，檢查 `if len > MAX_WS_PAYLOAD_LEN { bail!("WebSocket 承載過大 (超過 30MB 限制)"); }`（使用 `u64` 比對再轉 `usize`，避免 32-bit 平台轉型截斷）。
2. **`token_matches` 長度比對修正 (`crates/termade-daemon/src/server.rs`)**：
   - 將 `diff` 型別改為 `usize`：`let mut diff: usize = a.len() ^ b.len();`，迴圈中 `diff |= (a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0)) as usize;`，回傳 `diff == 0`。
3. **`serve_file` 目錄白名單隔離 (`crates/termade-daemon/src/web.rs`)**：
   - 收集允許存取的根目錄清單 (`allowed_roots: Vec<PathBuf>`)：包含 `inner.default_cwd` 以及所有 session 的 `cwd` 與 `repo_root`（皆經 `canonicalize()` 解析為真實路徑）。
   - 將目標 `resolved_path` 執行 `canonicalize()`（若失敗或非檔案則回傳 404）。
   - 檢查 `resolved_path` 是否滿足 `allowed_roots.iter().any(|root| resolved_path.starts_with(root))`。若不在白名單範圍內，回傳 `HTTP/1.1 403 Forbidden`（內文：`拒絕存取：路徑不在工作區範圍內`）。
   - 抽出純函式 `is_path_allowed(canonical_target: &Path, allowed_roots: &[PathBuf]) -> bool` 以便撰寫單元測試。
4. **Web 前端 XSS 防護 (`crates/termade-daemon/src/web/index.html`)**：
   - 更新 `escapeHtml(str)`：除了 `&`, `<`, `>` 外，一併將 `"` 替換為 `&quot;`、將 `'` 替換為 `&#39;`。
   - 在 `renderMarkdown` 的連結解析中，除了 `http://`、`https://`、`file://` 以及頁面錨點 `#` 或純相對路徑外，若網址包含協議前綴（如 `^[a-zA-Z][a-zA-Z0-9+.-]*:`，例如 `javascript:`、`data:`、`vbscript:`），一律不渲染為 `<a>`，僅輸出純文字 `${label}`。
5. **剪貼簿圖片暫存檔安全強化 (`crates/termade-tui/src/attach.rs`)**：
   - 優先使用 `XDG_RUNTIME_DIR` (通常為 `/run/user/<uid>`，權限 `0700`) 或退回 `std::env::temp_dir()` 下的專屬子目錄，並搭配隨機 ID (`termade_core::new_id("clip")`) 產生檔名 `termade-{id}.png`。
   - 在 Unix 系統上使用 `std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)` 獨占建立並寫入檔案，防止符號連結競態攻擊與其他使用者讀取。

## 驗收條件

- `crates/termade-daemon/src/web.rs` 新增單元測試驗證：
  - `read_ws_frame` 收到超過 30MB 的長度宣告時立即回傳 `Err` 且不會配置超大記憶體。
  - `is_path_allowed` 允許工作區子路徑、阻擋 `/etc/passwd` 等工作區外路徑。
- `crates/termade-daemon/src/server.rs` (或 `lib.rs` 測試模組) 驗證 `token_matches` 在長度相差 256 時回傳 `false`。
- `make pre-commit` (`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`make i18n-check`) 與 `cargo test` 全數通過。
