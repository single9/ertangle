# T23 — agent 回合失敗時把錯誤原因顯示出來

優先度：P1　範圍：`termade-adapter`（`codex.rs`，並檢查 `acp.rs`、`claude.rs`）

## 問題

Codex 登入失效（refresh token 被撤銷）時，`termade spawn --kind codex` 建出來的 session 約 2 秒內變成 `failed`，逐字稿、`status_note`、event log 都沒有任何原因。使用者只能自己另外跑 `codex exec` 才知道要重新 `codex login`。

## 證據

2026-11-13 實際遇到：`t17-budget`、`t19-queue` 兩個 Codex session 的 event log 只有 `user_prompt` → `session_model` → `idle` → `busy` → `failed`，沒有 `output` 或 `session_note`。另外直接執行 `codex exec` 得到 `401 Unauthorized … token_revoked`、「Your access token could not be refreshed because your refresh token was revoked. Please log out and sign in again.」。

- `crates/termade-adapter/src/codex.rs:169-176`：`turn/completed` 的 `turn.error` 不為 null 時只送 `Status(Failed)`，錯誤內容丟掉。
- `crates/termade-adapter/src/codex.rs:205-211`：`error` 通知只寫 `tracing::warn!`；除非 `looks_like_limit` 判定為額度問題，否則不送任何事件給 daemon。
- `crates/termade-adapter/src/codex.rs:381`：`stderr(Stdio::null())`，codex 自己印的錯誤也看不到。`acp.rs:419`、`claude.rs:444` 同樣把 stderr 丟掉。
- 對照：`claude.rs:219-224` 至少在 `is_error` 時取了 `result` 文字判斷額度，但非額度的錯誤同樣只送 `Failed`。

## 分析邏輯

1. 從 event log 的順序看，握手（`initialize`、`thread/start`）有成功（有 `session_model`、轉 `idle`），失敗發生在第一個回合，所以錯誤應該走 `error` 通知或 `turn.error`。**沒有確認是哪一條**，也沒抓 app-server 的原始輸出。
2. 不管哪一條，都只差「把文字往上送」。daemon 已經有 `AgentEvent::Output` 與 `SessionNote` 可以用，不必改協定。
3. stderr 改成收集最後幾行，在處理程序異常結束時附上，可以涵蓋握手前就掛掉的情況。

## 實作方式

1. 動手前先抓一次真實輸出：在 Codex 未登入的環境用 `codex app-server` 手動送 `initialize` → `thread/start` → `turn/start`，記下錯誤出現在哪個訊息、欄位長什麼樣，寫進 `docs/VERIFICATION.md`。
2. `codex.rs`：
   - `error` 通知：不是額度問題時，送 `AgentEvent::Output("⚠ codex 錯誤：<message>")`，並設成 status note（取 `error.message`，沒有就用整個 JSON 的前 300 字）。
   - `turn/completed` 帶 `turn.error`：同樣把訊息送出，再送 `Failed`。同一個回合已經由 `error` 通知送過同樣文字時不要重複。
   - 認得出是認證錯誤（401、`token_revoked`、`unauthorized`）時，note 加上「請執行 `codex login`」。
3. stderr：三個 adapter 改成 `Stdio::piped()`，用執行緒讀進固定大小的環狀緩衝（例如最後 20 行），`Exited(code)` 且 code 非 0 時把這幾行當成 `Output` 送出。注意讀取執行緒不能因為 agent 大量輸出 stderr 而卡住 pipe。
4. `claude.rs`：`is_error` 但不是額度問題時，把 `result` 文字送成 `Output`。

## 驗收條件

- 測試（假 codex 腳本）：回合送出 `error` 通知後 `turn/completed` 帶 error，session 變 `failed`，逐字稿有錯誤文字、`status_note` 有內容，而且只出現一次。
- 測試：認證錯誤的 note 含 `codex login`。
- 測試：假 agent 往 stderr 印字後以非 0 結束，逐字稿看得到最後幾行。
- 測試：agent 大量寫 stderr（例如 1 MB）時不會卡住。
- 手動驗證：在 Codex 未登入的環境 spawn 一個 codex session，TUI 上看得到原因。
