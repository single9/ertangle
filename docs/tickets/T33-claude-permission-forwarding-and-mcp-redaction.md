# T33 — Claude Code 逐項權限核准轉發與 MCP 回傳全面秘密遮罩

優先度：P1　範圍：`termade-core`、`termade-protocol`、`termade-adapter`、`termade-daemon`　相依：T04

Status: done

## 問題

對照 `docs/DESIGN.md` 第 9 節與附錄 A，目前權限與安全機制仍有兩個尚未補齊的缺口：

1. **Claude Code (`stream-json`) 尚未轉發逐項權限核准**：
   - OpenCode / Antigravity (`acp.rs:186-208`) 與 Codex (`codex.rs:108-144`) 都會將工具執行核准請求轉成 `AgentEvent::Permission`，交由 Daemon 的 `PolicyEngine` 過濾並在 `ask` 模式下彈出 TUI / Web 核准橫幅。
   - 但 `claude.rs` 在 `-p` (`stream-json`) 模式下沒有掛載 `--permission-prompt-tool`，`resolve_permission` 直接回傳錯誤 (`claude.rs:348`)。這導致 Claude Session 在 `ask` 模式下無法於 TUI 互動核准，且 `policy.toml` 的動態規則無法在執行期攔截 Claude 的單一工具呼叫。
2. **MCP 工具回傳值未全面套用秘密遮罩 (`redact_secrets`)**：
   - 目前 `redact_secrets` 只在寫入 `events.jsonl` (`logwriter.rs`) 與 `termade_diff` (`mcp.rs:365`) 時呼叫。
   - 若某個 Agent 讀取了含金鑰的檔案並透過 `termade_send`、`termade_artifact_share` 傳送，其他 Agent 透過 `termade_inbox`、`termade_wait`、`termade_ask` 讀取時，MCP 回傳內容尚未經過 `redact_secrets` 過濾。

## 證據

- `crates/termade-adapter/src/claude.rs:348-350`：`resolve_permission` 直接 `bail!("claude stream-json 尚不支援逐項核准轉發")`。
- `crates/termade-adapter/src/claude.rs:371-415`：組裝 `claude -p` 參數時，僅傳入 `--permission-mode`，未傳入 `--permission-prompt-tool` 與 `--permission-prompts host`。
- `docs/research/agent-model-and-approval.md:31-41`：已記錄透過 `--permission-prompt-tool mcp__termade__termade_approve --permission-prompts host` 轉發至 `termade mcp` 的設計方案。
- `crates/termade-daemon/src/mcp.rs:365`：整個 `mcp.rs` 只有 `termade_diff` 呼叫了 `redact_secrets`。
- `docs/DESIGN.md:422`、`592`、`596`：明確標註「其他 MCP 工具的回傳尚未做敏感樣式過濾」與「Claude 逐項核准未實作」。

## 分析邏輯

1. **Claude 權限轉發路徑 (透過 `termade mcp` 橋接 Daemon)**：
   - Claude Code CLI 支援 `--permission-prompt-tool <mcp_tool_name> --permission-prompts host`。當 `termade` 啟動 `claude` 時已透過 `--mcp-config` 注入 `termade mcp`，因此可在 `termade mcp` 中新增 `termade_approve` 工具供 Claude 呼叫。
   - 當 Claude 需要權限時會呼叫 `mcp__termade__termade_approve`，傳入 `{ "tool_name": "...", "input": {...} }`。
   - `termade mcp` 收到後，透過 Unix socket 向 Daemon 發送 `Request::McpPermission { session, tool_name, input }` 並阻塞等待回應 `Frame::PermissionDecision { decision }`：
     1. Daemon 先以該 Session 的 `PolicyEngine` (含專案與全域 `policy.toml`) 評估 `evaluate(&tool_name, &input_str)`。
     2. 若命中 `deny` → 立即回傳 `Decision::Deny`。
     3. 若命中 `allow` 或該 Session 政策為 `Auto` / `Bypass` → 立即回傳 `Decision::Allow`。
     4. 若政策為 `Ask` → 建立 `PermissionRequest`，將回呼通道存入 `SharedInner.mcp_permissions: HashMap<String, ClientTx>`，發出 `Event::PermissionRequested` 讓 TUI / Web 顯示核准橫幅；待使用者按下 `a` / `A` / `d` (`handle_permission`) 時喚醒該 `ClientTx` 回傳決策。
   - `termade mcp` 收到 `Decision` 後，回傳 Claude SDK 預期的 JSON 字串：
     - 允許：`{"behavior": "allow", "updatedInput": <原 input 物件>}`
     - 拒絕：`{"behavior": "deny", "message": "User denied permission in termade"}`
2. **秘密遮罩範圍**：
   - 在 `crates/termade-daemon/src/mcp.rs` 的 `dispatch_tool` / `call_tool` 統一出口，對最終要回傳給 Agent 的文字內容套用 `termade_core::redact_secrets`，確保 `termade_inbox`、`termade_wait`、`termade_ask`、`termade_task_list`、`termade_diff` 等所有工具輸出都不會洩漏明文 API Key (`sk-...`、`ghp_...`、`AKIA...` 等)。
3. **未實機確認的部分**：
   - 本機 `claude` 版本對 `--permission-prompt-tool` 回傳 JSON 的格式 (是否包在 MCP `content: [{"type": "text", "text": "{...}"}]` 內)，實作時需以本機 `claude -p` 實測確認。

## 實作方式

1. **Protocol 與 Daemon 核准等待通道 (`crates/termade-protocol/src/lib.rs`、`crates/termade-daemon/src/handlers/session.rs`)**：
   - `Request` 新增 `McpPermission { session: SessionId, title: String, detail: String }`。
   - `Frame` 新增 `PermissionDecision { decision: Decision }`。
   - `SharedInner` 新增 `mcp_perms: HashMap<String, ClientTx>`。
   - 在 `handle_mcp_permission` 中執行與 `ingest` `AgentEvent::Permission` 完全一致的 `PolicyEngine` 決策順序；需要詢問使用者時發出 `Event::PermissionRequested` 並將 `reply` 存入 `mcp_perms`。
   - 在 `handle_permission` 中，若 `req` 存在於 `mcp_perms`，則直接透過 `reply.send(Frame::PermissionDecision { decision })` 回覆 `termade mcp`，並發出 `Event::PermissionResolved`。
2. **MCP `termade_approve` 工具與 Claude Adapter 參數 (`crates/termade-daemon/src/mcp.rs`、`crates/termade-adapter/src/claude.rs`)**：
   - `mcp.rs` 的 `tools_list` 與 `call_tool` 新增 `termade_approve`，解析 `tool_name` (或 `name`) 與 `input`，向 Daemon 請求 `McpPermission` 並格式化回傳 JSON。
   - `claude.rs` 在 `mcp_exe.is_some()` 且 (`spec.permission == PermissionPolicy::Ask` 或設定需要規則引擎攔截) 時，附加 `--permission-prompt-tool mcp__termade__termade_approve --permission-prompts host`。
3. **MCP 回傳統一秘密遮罩 (`crates/termade-daemon/src/mcp.rs`)**：
   - 在 `mcp.rs` 將工具回傳文字包裝進 JSON-RPC response 前，統一呼叫 `termade_core::redact_secrets(&text)`。

## 驗收條件

- 測試：模擬 `termade mcp` 送出 `Request::McpPermission`：
  - 命中 `policy.toml` 的 `deny` 時立即收到 `Decision::Deny` 且有 `PermissionResolved` 事件。
  - 在 `Ask` 政策下產生 `PermissionRequested` 事件，送出 `Request::Permission` 後 `McpPermission` 請求能收到對應的 `Decision::Allow` / `Decision::Deny`。
- 測試：透過 `termade_send` 傳送含 `sk-1234567890abcdefghijklmnop` 的訊息，收件者呼叫 `termade_inbox` 與 `termade_wait` 取得的內容已遮罩為 `<REDACTED>`。
- 同步更新 `docs/DESIGN.md` 第 9 節與附錄 A、`README.md`、`README_EN.md`。
- `make pre-commit` 與 `cargo test` 全數通過。
