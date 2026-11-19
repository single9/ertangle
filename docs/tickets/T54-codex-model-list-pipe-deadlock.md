# T54 — 修復 Codex 模型清單因 stdout pipe 阻塞而空白

優先度：P1　範圍：`termade-adapter` (`models.rs`)　相依：—

## 問題

TUI 與網頁版選取 Codex 的 model 時，清單只剩「預設 (依 config.toml)」，`codex` 的模型完全不出現；`opencode` 與 `agy` 則正常。

## 證據

- `crates/termade-adapter/src/models.rs` 原本先以 `try_wait()` 等行程結束，才用 `read_to_string` 讀 stdout。
- `codex debug models` 的輸出約 263 KB (內含完整模型與設定說明)，遠大於 pipe buffer (常見 64 KB)；寫入阻塞使行程永不結束，8 秒後被 kill 並回傳空清單。
- 臨時探針呼叫 `query_models(AgentKind::Codex)`：修正前耗時 8.02 秒且回傳 0 筆，修正後耗時 0.05 秒回傳 3 筆 (`gpt-6-luna`、`gpt-5.6-terra`、`gpt-5.6-luna`)。
- `opencode models` 只有 585 bytes，所以原本的順序讀取沒有觸發阻塞。

## 分析邏輯

- 已確認 `parse_codex_models` 對目前 `{"models":[{slug, display_name, visibility}]}` 格式與 `visibility == "list"` 的篩選正確 (本機有 3 筆)。
- 已確認 `codex app-server` 的 `thread/start` 有帶 `model`，問題不在模型傳遞。
- 未確認：其他平台的 pipe buffer 實際大小；修正採背景執行緒並行讀取，不依賴特定 buffer 大小。

## 實作方式

- 抽出 `capture_stdout`：以 `Stdio::piped()` 啟動子行程後，立刻在背景執行緒把 stdout 讀到 `Vec<u8>`，主執行緒再以 `try_wait()` 搭配 8 秒期限輪詢；逾時就 kill 並回傳 `None`。
- 用 `String::from_utf8_lossy` 容忍非 UTF-8 位元組；背景執行緒以 `recv_timeout(1s)` 收尾，避免子行程的子行程仍持有 stdout 時卡住。
- `query_models` 改呼叫 `capture_stdout`，其餘解析邏輯不變。
- 新增 `capture_stdout_drains_large_output` 測試，用大於 pipe buffer 的輸出驗證不會逾時。

## 驗收條件

- `query_models(AgentKind::Codex)` 在本機回傳 3 筆模型且耗時遠低於 8 秒。
- `make pre-commit` 與 `cargo test -p termade-adapter` 通過。
