# T04 — 讓權限規則引擎在所有政策下都生效

優先度：P1　範圍：`termade-core`、`termade-adapter`、`termade-daemon`

## 問題

`policy.toml` 的 `deny` 規則在預設設定下擋不到任何東西：

- 預設政策是 `auto`，ACP adapter 在這個政策下會自己核准，請求不會送到 daemon 的 `PolicyEngine`。
- Claude adapter 完全不轉發核准。
- 規則是子字串比對，`allow` 可以被輕易誤觸發。
- 規則只在 daemon 啟動時讀一次，而且只看 daemon 的啟動目錄。

README 與設計文件把它描述成已完成的「權限規則引擎」，使用者會以為 `rm -rf /`、`git push --force` 已被擋下。

## 證據

- `crates/termade-adapter/src/acp.rs:159-167`：`auto_approves()` 時直接 `respond` 並 `return`，不送 `AgentEvent::Permission`。
- `crates/termade-daemon/src/lib.rs:429-447`：`PolicyEngine::evaluate` 只在收到 `AgentEvent::Permission` 時被呼叫。
- `crates/termade-adapter/src/claude.rs:267-269`：`resolve_permission` 直接 `bail!`。
- `crates/termade-adapter/src/codex.rs:132`、`307`：Codex 有轉發與回覆路徑（README 註明未實機驗證）。
- `crates/termade-core/src/lib.rs:891-904`：`combined.contains(pat)`；預設 `allow` 含 `"Read"`、`"Grep"`、`"Glob"`。
- `crates/termade-daemon/src/lib.rs:93-103`、`1704`：`load_policy(&default_cwd)` 只在啟動時呼叫一次。

## 分析邏輯

1. 從 `PolicyEngine::evaluate` 往回找唯一的呼叫點（`ingest`），再找哪些路徑會產生 `AgentEvent::Permission`。
2. ACP 的 `on_permission` 在送事件之前就依 session 政策短路，所以 `auto`／`bypass` 下 daemon 看不到請求。
3. 各 adapter 的情況：ACP 只有 `ask` 會到 daemon；Codex 由 `approvalPolicy` 決定是否發出請求；Claude 沒有路徑。
4. 比對語意：`deny` 先於 `allow` 檢查，所以 `allow` 的誤觸發不會蓋過 `deny`。但在 `ask` 模式下，任何標題或輸入內容含 `Read` 的請求（例如指令中出現 `README`）會被自動放行而不問人。
5. 沒有實機驗證各 agent 實際送出的 `title`／`rawInput` 格式，比對規則的誤觸發程度是從程式碼推論。

## 實作方式

1. **daemon 成為唯一決策點**：ACP adapter 不再自行短路，一律送出 `AgentEvent::Permission`。`ingest` 的決策順序：
   1. 命中 `deny` → 拒絕。
   2. 命中 `allow` → 允許。
   3. session 政策是 `auto`／`bypass` → 依政策自動允許。
   4. 否則發 `PermissionRequested` 給人。

   `ingest` 需要知道 session 的 `PermissionPolicy`。`Session` 目前沒有這個欄位（只有 `cwd`、`branch` 等），要在 spawn 時存進 `Session` 或 `SessionEntry`。
2. **規則改成結構化比對**：
   - 分開 `tool`（比對工具名稱，完全相等）與 `pattern`（比對輸入內容）。
   - 內容比對至少做到以空白切詞後的前綴比對，避免 `Read` 命中 `README`。
   - 保留舊格式的解析，啟動時對舊格式印警告。
3. **每個 repo 各自的規則**：以 session 的 repo 根目錄載入 `.termade/policy.toml`，疊在全域規則之上；`deny` 取聯集，repo 層級不能移除全域的 `deny`。
4. **Claude**：研究以 `--permission-prompt-tool` 指向 `termade mcp` 的一個核准工具，讓 `-p` 模式的核准走到 daemon。本機 `claude --help` 確認有 `--permission-prompt-tool` 與 `--permission-prompts host`，但工具的輸入輸出格式仍要先查證再實作；若不可行，就在 spawn Claude 且 `deny` 非空時於 TUI 顯示「規則引擎不適用於此 session」。
5. **文件誠實**：README 說明規則引擎是防呆而非安全邊界，字串比對可以被改寫指令繞過；真正的隔離要靠 agent 自己的 sandbox。

## 驗收條件

- 測試：session 政策為 `auto`，請求命中 `deny` → 被拒絕且有 `PermissionResolved` 事件。
- 測試：session 政策為 `auto`，請求未命中任何規則 → 自動允許。
- 測試：`ask` 模式下 `cat README.md` 不會因 `allow` 的 `Read` 而被自動放行。
- 測試：repo 層級規則無法取消全域 `deny`。
- README 的權限章節與實際行為一致。
