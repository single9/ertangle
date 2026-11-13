# T12 — 權限的設定、顯示與子 agent 上限

優先度：P1　範圍：`termade-core`、`termade-protocol`、`termade-adapter`、`termade-daemon`、`termade-tui`　相依：T11（設定檔）

T04 處理的是「規則引擎有沒有被呼叫到」。這張票處理的是「權限能在哪裡設定、設定後看不看得到、agent 能不能自己放寬」。

## 問題

1. **TUI 不能選權限**：從 TUI 建立的 session 一律是預設的 `auto`。要用 `ask` 或 `bypass` 只能改用 CLI。
2. **沒有預設值設定**：不能設定「這個 repo 一律 `ask`」或「reviewer 角色一律唯讀」。
3. **權限不在狀態裡**：`Session` 沒有記錄它的權限政策，TUI 與 `termade sessions` 看不出哪個 session 是 `bypass`，事後審計也查不到。
4. **agent 可以自行放寬權限**：MCP 的 `termade_spawn` 接受 `permission: "bypass"`。一個以 `ask` 啟動的 agent 可以 spawn 一個 `bypass` 的子 agent，讓它去做自己不被允許的事。
5. **只有三段式旋鈕**：沒有辦法限制可用工具、sandbox 範圍或唯讀模式。設計文件第 9 節提到的 `--allowedTools`、sandbox、`--bare` 都沒有做成可設定的選項。

## 證據

- `crates/termade-tui/src/lib.rs:1151`：`permission: Default::default()`；`submit_spawn` 只解析 `<agent> <prompt>`。
- `crates/termade-core/src/lib.rs` 的 `Session`：欄位有 `cwd`、`branch`、`role`、`model` 等，沒有 `permission`。
- `crates/termade-daemon/src/mcp.rs:305-309`：`termade_spawn` 的 schema 列出 `"enum": ["ask", "auto", "bypass"]`；`613-616` 直接採用。
- `crates/termade-daemon/src/lib.rs:1264-1345`：`handle_spawn` 沒有比較 `spawn.permission` 與父 session 的權限。
- `crates/termade-adapter/src/claude.rs:305`：唯一的工具限制是固定加上 `--allowedTools=mcp__termade__*`（放行 termade 自己的 MCP 工具）。
- `grep 'sandbox\|disallowedTools\|--bare'`：沒有結果。
- `crates/termade-adapter/src/codex.rs:420-425`：`thread/start` 只帶 `cwd` 與 `approvalPolicy`。

## 分析邏輯

1. 列出設定權限的所有入口（CLI、TUI、MCP、設定檔），逐一看能設什麼：CLI 可以，TUI 不行，MCP 可以且沒有上限，設定檔不存在。
2. 第 4 點是從信任邊界看的：`ask` 的意義是「這個 agent 的動作要人同意」。若它能產生不需同意的子 agent 並用訊息指揮它，這個保證就不成立。目前 `handle_spawn` 只把 `parent` 用來記錄關係，沒有用來限制。這是讀程式碼得出的，沒有實機演示。
3. 第 5 點對照設計文件第 9 節「預設最小權限」的描述與程式碼中實際出現的旗標。
4. `--permission-mode manual` 我原本懷疑不是合法值，查本機 `claude --help` 後確認是（選項為 `acceptEdits`、`auto`、`bypassPermissions`、`manual`、`dontAsk`、`plan`），所以對應表沒有問題。

## 實作方式

1. **權限寫進狀態**：`Session` 增加 `permission: PermissionPolicy`（`#[serde(default)]`，舊 log 可重播）。TUI 的 sessions 欄對 `bypass` 用醒目顏色標示，`termade sessions` 多一欄。
2. **子 agent 不得高於父 agent**：定義順序 `ask < auto < bypass`。`handle_spawn` 在 `parent` 存在時把請求的權限壓到不超過父 session，並在回覆中註明被調降。人類從 CLI／TUI 發出的 spawn（沒有 `parent`）不受限。

   注意 `parent` 目前由 client 自己填（CLI 讀 `$TERMADE_SESSION`，MCP 由 `termade mcp --session` 帶入）。agent 可以直接跑 `termade spawn --permission bypass` 並清掉環境變數來規避，所以這是防呆，不是安全邊界；完整的做法需要 daemon 能辨識連線來自哪個 session，另開票處理。
3. **預設值**（放進 T11 的 `config.toml`）：
   ```toml
   [defaults]
   permission = "auto"
   max_agent_permission = "auto"   # agent 經 MCP spawn 時的上限

   [roles.reviewer]
   permission = "ask"
   read_only = true

   [agents.claude]
   allowed_tools = ["Read", "Grep", "Glob", "Bash(git diff:*)"]
   disallowed_tools = []

   [agents.codex]
   sandbox = "workspace-write"     # read-only | workspace-write | danger-full-access
   ```
4. **細部選項的對應**：
   - Claude：`allowed_tools`／`disallowed_tools` → `--allowedTools`／`--disallowedTools`（與既有的 `mcp__termade__*` 合併）；`read_only` → `--permission-mode plan`。
   - Codex：`sandbox` → `thread/start` 的 sandbox 參數；`read_only` → `read-only`。欄位名稱要先以實機或文件確認，本機沒有安裝 Codex。
   - ACP（OpenCode／Antigravity）：協定沒有對應的啟動參數，`read_only` 由 daemon 端達成——T04 完成後，在 `ingest` 對寫入類的審批請求自動拒絕。
   - 某個 agent 不支援某選項時，spawn 回傳明確錯誤，不要靜默忽略。
5. **TUI**：`n` 的建立流程增加權限切換（比照既有的 `Ctrl-W` 切換 worktree，例如 `Ctrl-E` 在 `ask`／`auto`／`bypass` 間循環），輸入框標題顯示目前選擇。選到 `bypass` 時送出前要再按一次確認。
6. **Claude 的逐項審批**：本機 `claude --help` 有 `--permission-prompts host` 與 `--permission-prompt-tool`，T04 第 4 步提到的做法可行性較高了。實作留在 T04，這裡只確認 `ask` 對 Claude 最終要走那條路。

## 驗收條件

- 測試：父 session 為 `ask`，經由帶 `parent` 的請求 spawn `bypass` → 子 session 的權限為 `ask`，回覆含調降說明。
- 測試：沒有 `parent` 的 spawn 可以取得 `bypass`。
- 測試：舊的 `events.jsonl`（`Session` 沒有 `permission`）可以重播。
- 測試：`roles.reviewer.permission` 生效，且可被 spawn 請求的明確值覆寫。
- 測試：Claude 的 `allowed_tools` 與固定的 `mcp__termade__*` 一起出現在命令列。
- TUI 能在建立 session 時選權限，並在清單中看得出 `bypass` 的 session。
