# T44 — 網頁版透過 daemon 取得與 TUI 同源的 model 清單 (`list_models`)

優先度：P2　範圍：`termade-adapter`、`termade-protocol`、`termade-daemon`、`termade-tui`　相依：T43

Status: done

## 問題

1. T43 把網頁版過時的 model 清單換掉後，Codex、OpenCode、Antigravity 只剩「預設」，使用者必須自己輸入 model id；TUI 則會向各 agent 的 CLI 查詢實際可用的清單。
2. Session 控制面板以 `Session.kind` 查清單，但它序列化為 `claude_code`，對不到清單的 `claude` 鍵，Claude session 一律落到 OpenCode 的清單。

## 證據

- 修正前 `crates/termade-tui/src/menu.rs` 的 `query_models`、`parse_models`、`parse_codex_models`、`split_effort_variants` 與 `CLAUDE_MODELS` 只在 TUI crate，daemon 無法使用。
- 修正前 `web/index.html` 的 `openSessionSettings` 以 `s.kind || "opencode"` 呼叫 `renderModelCardList`。

## 分析邏輯

- 模型清單邏輯搬到 `termade-adapter::models`，TUI 與 daemon 共用同一份，不會再有兩份清單需要同步。
- 查 CLI 最多要等 8 秒，daemon 在背景執行緒查詢，避免卡住該連線的其他請求。
- 網頁版連不上 daemon 時仍用內建的離線備援清單。
- 未確認：各 agent CLI 在 daemon 的環境變數 (例如 `PATH`) 下是否都查得到；查不到時清單只會有「預設」與 `config.toml` 裡的 `models`。

## 實作方式

- 新增 `crates/termade-adapter/src/models.rs`，TUI 的 `menu.rs` 改為使用它。
- 協定新增 `Request::ListModels { kind }` 與 `Frame::Models { kind, models: Vec<ModelInfo> }`。
- daemon `handlers::list_models`：Claude 用內建清單，其他 agent 呼叫 `query_models`，再加上 `config.toml` 的 `model`／`models`。
- 網頁版 `renderModelCardList` 依 agent 送一次 `list_models` 並快取，回覆到達後重畫開著的清單；查詢中顯示提示；重新連線時可重送。`normKind` 把 `claude_code` 對應到 `claude`。

## 驗收條件

- 網頁版建立 session 選 Codex／OpenCode 時，列出與 TUI 相同的 model。
- Claude session 的控制面板顯示 Claude 的 model 清單。
- `web_companion` 整合測試透過 WebSocket 送出 `{"op":"list_models","kind":"claude"}` 收到 `Models`。
- `cargo test`、`make pre-commit` 通過。
