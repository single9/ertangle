# T43 — Web 版模型清單過時，與 TUI 不一致

優先度：P2　範圍：`termade-daemon` (`web/index.html`)　相依：T30

Status: done

## 問題

手機版建立 session 或切換模型時，列出的是寫死且已過時的模型 (例如 `claude-3-7-sonnet`、`gpt-4o`)，選了之後 agent 可能無法啟動或用到非預期的模型；TUI 則使用另一份較新的清單，兩邊不一致。

## 證據

- `crates/termade-daemon/src/web/index.html` 的 `MODEL_CATALOG` 內含 `claude-3-7-sonnet`、`claude-3-5-haiku`、`o1`、`gpt-4o` 等。
- `crates/termade-tui/src/menu.rs:413` 的 `CLAUDE_MODELS` 是 Claude 的內建清單；其他 agent 由 `query_models` 向各自 CLI 查詢。

## 分析邏輯

- Claude 沒有列出模型的 CLI，只能內建；網頁版應與 `CLAUDE_MODELS` 相同。
- 其他 agent 的模型清單需要向 CLI 查詢，網頁版目前沒有對應的協定請求；寫死的猜測清單比沒有更糟，先改為「預設 + 自訂輸入」。
- 後續：`Request::ListModels` 讓網頁向 daemon 取得與 TUI 相同的清單，見 T44。

## 實作方式

- `MODEL_CATALOG.claude` 改為與 `CLAUDE_MODELS` 相同的別名與完整 id。
- 其他 agent 只保留「預設 (依設定檔)」，並引導使用自訂模型輸入。
- `menu.rs` 的 `CLAUDE_MODELS` 註解提醒同步更新網頁版。

## 驗收條件

- 網頁版不再出現 Claude 3.x、GPT-4o 等過時模型。
- Claude 清單與 TUI 一致。
- `make pre-commit` 通過。
