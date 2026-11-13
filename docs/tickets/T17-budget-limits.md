# T17 — 成本與子 agent 數量上限

優先度：P1　範圍：`termade-core`、`termade-protocol`、`termade-daemon`、`termade-tui`、`src/cmd/`

## 問題

termade 會統計每個 session 的成本，但沒有任何上限：

- 一個 session 可以無限花下去，沒有「最多花多少」的設定。
- agent 能透過 `termade_spawn` 自己產生子 agent，子 agent 又能再產生，數量與層數都沒有限制。
- 目前的防呆只管訊息匯流排（速率限制與重複迴圈偵測），不管花費。

權限政策設成 `auto` 或 `bypass` 放著跑的時候，這是最容易出事的地方。

## 證據

- `crates/termade-core/src/lib.rs:182-187`：`Cost` 有 `usd`、`input_tokens`、`output_tokens`。
- `crates/termade-daemon/src/lib.rs:523-526`：`ingest` 把 `AgentEvent::Cost` 轉成 `Event::Cost` 就結束，沒有比較。
- `crates/termade-daemon/src/bus.rs:18`：`BusGuard` 只處理訊息。
- `crates/termade-daemon/src/mcp.rs:635`：`termade_spawn` 的實作；`handlers/session.rs:18` 的 `handle_spawn` 對有 `parent` 的請求只限制權限，不限制數量。
- `docs/DESIGN.md` 附錄 A：「每任務訊息預算：未實作；只有速率限制與重複迴圈斷路」。
- `crates/termade-daemon/src/config.rs:63-71`：`Config` 沒有限制相關的區段。

## 分析邏輯

1. 從 `Event::Cost` 往下追，確認成本只用來顯示。
2. 各家回報成本的情況：Claude 在每個回合的 `result` 帶 `total_cost_usd`（`claude.rs:263-272`），是**同一個程序內的累計值**。Codex 與 ACP 有沒有回報美金成本**沒有確認**，動手前先搜尋各 adapter 送 `AgentEvent::Cost` 的位置；沒有美金數字的要改用 token 數當上限。
3. `claude --help` 有 `--max-budget-usd`，可以當第二道防線，但觸發時 Claude 怎麼結束（錯誤訊息、結束碼）**沒有實測**。
4. 上限要在 daemon 執行，不能只靠 TUI：TUI 可能沒開，agent 也可能是另一個 agent 產生的。

## 實作方式

1. **設定**（`config.toml`）
   ```toml
   [limits]
   session_usd = 5.0        # 單一 session 的成本上限
   session_tokens = 2000000 # 沒有美金數字的 agent 用這個（輸入＋輸出）
   tree_usd = 20.0          # 一個 session 加上它所有後代的總和
   max_children = 4         # 一個 session 同時 live 的直接子 agent 數
   max_depth = 2            # 人產生的是第 0 層
   warn_percent = 80
   ```
   全部可省略，省略表示不限制。`roles.<role>` 可以覆寫 `session_usd`。repo 的 `.termade/config.toml` 只能調低，不能調高（和 policy 的 `deny` 同一個原則）。
2. **單次覆寫**：`SpawnRequest` 加 `#[serde(default)] budget_usd: Option<f64>`；`Session` 加 `#[serde(default)] budget_usd: Option<f64>`，存解析後的實際上限。入口：`termade spawn --budget 3`、MCP `termade_spawn` 的 `budget_usd`（子 agent 的上限不得高於父 session 剩餘的額度）。
3. **spawn 時檢查**（`handle_spawn`，有 `parent` 時）
   - 沿著 `parent` 往上數層數，超過 `max_depth` 就拒絕。
   - 數 `parent` 目前 live 的直接子 session，達到 `max_children` 就拒絕。
   - 整棵樹的成本已達 `tree_usd` 就拒絕。
   - 錯誤訊息要寫明是哪個上限、目前數值多少，讓發出請求的 agent 能據此調整。
4. **執行中檢查**：`ingest` 送出 `Event::Cost` 之後
   - 達到 `warn_percent`：留一則 `SessionNote`，每次越線只提醒一次（做法比照 `quota_warned`，`lib.rs:281`）。
   - 達到上限：呼叫 `ctl.cancel()` 中斷當前回合，新增 `Event::SessionOverBudget { session, spent, limit }`，`State::apply` 設 `Session.over_budget = true`。
   - 樹的上限：從這個 session 往上找到根，加總整棵樹，超過時中斷樹裡所有 live 的 session。
5. **超過上限之後**：`handle_prompt` 與匯流排的自動喚醒對 `over_budget` 的 session 一律拒絕，回覆說明原因。解除方式是人調高上限：`termade budget <session> <金額>`、TUI `:budget <金額>`。調高後清掉 `over_budget`。agent 不能透過 MCP 調整自己的上限。
6. **Claude 的第二道防線**：有上限時加 `--max-budget-usd`，數值設為上限的 1.2 倍，確保先觸發的是 daemon 的中斷。先實測它觸發時的行為再決定要不要加。
7. **和 T14 的交互**：接續後 Claude 的累計值會歸零。成本必須是「舊值加新程序的累計」，否則接續等於把預算重設。
8. **顯示**：sessions 清單與 `termade cost` 在有上限時顯示 `$1.20/$5`，達警告門檻黃色、超過紅色。

## 驗收條件

- 測試：設定解析；repo 設定調高上限時被忽略，調低時生效。
- 測試：`max_depth`、`max_children` 超過時 spawn 失敗，錯誤訊息含上限名稱；人直接產生的 session 不受 `max_children` 限制。
- 測試：成本越過警告門檻只留一則備註；越過上限會呼叫 `cancel` 並設定 `over_budget`。
- 測試：`over_budget` 的 session 收到 prompt 回錯誤；調高上限後可以再送。
- 測試：子 session 的花費計入父 session 的 `tree_usd`，超過時整棵樹被中斷。
- 測試：`over_budget` 經 log 重播後仍在。
- 測試：舊的 `Session`／`SpawnRequest` JSON 仍可解析。
- 同步 MCP `termade_spawn` 的 schema 與說明、`help.rs` 的「畫面上的標記」、README、`docs/DESIGN.md` 附錄 A。
