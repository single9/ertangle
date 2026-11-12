# 研究：各 agent 的 model／sandbox 指定與審批轉發

查證日期：2026-11-13。標示規則：【已確認】= 有官方文件網址或本機 `--help` 輸出為據；【未確認】= 未能取得一手來源，不可當作事實。
注意：查證過程中遇到使用額度上限，部分文件未能抓取，這些項目一律標【未確認】並列在「待補」。

## (1) Codex app-server `thread/start` 的 model 與 sandbox

- 【未確認】欄位名稱與合法值。已抓取 https://raw.githubusercontent.com/openai/codex/main/codex-rs/app-server/README.md，該頁摘要未列出 `model`、`sandbox`、`approvalPolicy` 的定義；`codex-rs/app-server-protocol/src/protocol/v2.rs` 該路徑回 404；https://learn.chatgpt.com/docs/app-server（由 developers.openai.com/codex/app-server 308 轉址而來）因額度用盡未抓到。本機無 `codex` 指令，無法用 `codex app-server generate-json-schema` 產生 schema。
- 【已確認】README 提到 `thread/start` 與 `turn/start` 的其他可帶欄位：`personality`、`disabledPluginIds`、`daybreakEnabled`（需 `experimentalApi`），`turn/start` 可帶 `environments`；`thread/settings/update` 可改變未來 turn 的設定、`cwd` 可帶入。來源同上 README。
- 建議做法（待驗證）：實作前在有 codex 的機器執行 `codex app-server generate-json-schema --out <dir>`（此子命令存在與否【未確認】），直接讀 `ThreadStartParams` 的 schema 取得 `model`、`sandbox`、`approvalPolicy` 的確切名稱與列舉值。憑印象 sandbox 值為 `read-only`／`workspace-write`／`danger-full-access`，但這是【未確認】。

## (2) OpenCode、Antigravity 的 ACP `session/new` 選 model

### ACP 協定本身
- 【已確認】`session/new` 必填 `cwd`、`mcpServers`；選填 `systemPrompt`、`additionalDirectories`（需 agent 宣告 sessionCapabilities）；**沒有 model 參數**。來源：https://agentclientprotocol.com/protocol/session-setup
- 【已確認】model 屬於 session config option：agent 在 session setup 結果回傳 `configOptions`（例：`id:"model"`, `category:"model"`, `type:"select"`, `currentValue`, `options:[{value,name}]`）；client 用 `session/set_config_option`（`sessionId`、`configId`、`value`）修改，回應為完整 configOptions。來源：https://agentclientprotocol.com/protocol/session-config-options 。文件範例只示範 `mode`，model 的請求為依相同模式推得（【未確認】實際是否被各 agent 實作）。
- 結論：選 model 的標準路徑是 `session/new` 之後送 `session/set_config_option`（configId 取自回傳的 category=model 項目），前提是該 agent 有回傳該選項。

### OpenCode
- 【已確認】`opencode acp` 子命令存在，flag 只有 `--login`（`opencode acp --help`）。ACP 文件未描述 model 選擇：https://opencode.ai/docs/acp/
- 【已確認】設定檔 `model` 鍵，格式 `provider/model`，且設定值可用 `{env:VAR}` 引用環境變數（未設定則為空字串）。來源：https://opencode.ai/docs/config/ 。本機 `~/.config/opencode/opencode.json` 為 `"model": "ollama/gemma4:26b-code"`。`opencode models` 可列出可用 model id（本機輸出為 `ollama/gemma4:*`）。
- 【未確認】`OPENCODE_CONFIG`、`OPENCODE_CONFIG_CONTENT` 是否存在（config 文件頁未提及）；`opencode acp` 是否回傳 model 的 configOptions 或支援 `session/set_model`。
- 可行做法：ade 啟動 `opencode acp` 時，於子行程 cwd 放專案層 `opencode.json`（含 `"model": "provider/model"`），或用 `{env:ADE_MODEL}` 搭配環境變數；並先以 ACP 實測 `session/new` 回應有無 model configOptions。

### Antigravity（本機指令 `agy`）
- 【已確認】`agy acp`（Start the Agent Client Protocol stdio server）旗標中**沒有 `--model`**；有 `--agent`、`--add-dir`、`--dangerously-skip-permissions`、`--sandbox`、`--gemini-dir`、`--app-data-dir`、`--process-per-session`（`agy acp --help`）。
- 【已確認】`agy` 主命令（print 模式等）有 `--model`、`--mode`（accept-edits/auto/plan）、`--effort`、`--settings`（JSON 物件，僅限該 session）；`agy models` 列出 id，例如 `gemini-4-argon-medium`、`claude-opus-5-5-high`、`claude-sonnet-5-5-low`（`agy --help`、`agy models`）。
- 【未確認】`agy acp` 是否在 `session/new` 回傳 model configOptions；是否讀取環境變數或 `--settings` 以外的設定檔指定 model。`agy acp` 沒有列 `--settings`，故 ACP 模式下不能假設該旗標可用。
- 可行做法：先實測 `agy acp` 的 `session/new` 回應；若無 model 選項，備案為以 `agy --print --model <id>` 非 ACP 路徑，或 `--agent` 指定預先定義好 model 的 agent（`agy agents` 可列出，agent 是否可綁定 model【未確認】）。

## (3) Claude Code `-p` 的 `--permission-prompt-tool` 與 `--permission-prompts host`

- 【已確認】`claude --help`：`--permission-prompts <target>` 「Who answers permission prompts with --print: "host" (the SDK host or --permission-prompt-tool) or "none" (nobody: anything that would prompt is denied automatically; the permission mode still decides everything else)」，選項 `host`、`none`，預設 `host`。
- 【已確認】`--permission-mode` 選項：acceptEdits、auto、bypassPermissions、manual、dontAsk、plan；`--mcp-config <configs...>`、`--strict-mcp-config`、`--input-format stream-json`（僅 `--print`）、`--allowedTools`／`--disallowedTools`。
- 【已確認】本機 help 輸出只在 `--permission-prompts` 說明中提到 `--permission-prompt-tool`，**沒有獨立的旗標說明列**（grep 無獨立條目），因此該旗標的確切參數格式無本機一手來源。
- 【未確認】輸入輸出格式。官方頁面 https://code.claude.com/docs/en/agent-sdk/permissions 因額度用盡未能抓取（已追蹤轉址至此）。憑記憶，MCP 工具名稱格式為 `mcp__<server>__<tool>`，工具收到 `{tool_name, input, tool_use_id?}`，回傳 JSON 字串：允許 `{"behavior":"allow","updatedInput":{...}}`、拒絕 `{"behavior":"deny","message":"..."}`。以上為【未確認】，實作前必須對照官方頁面或實測。

### 轉發給 `ade mcp` 的可行做法（設計建議，待實測）
1. ade 啟動 `claude -p --input-format stream-json --output-format stream-json --mcp-config '<json 指向 ade mcp>' --strict-mcp-config --permission-prompt-tool mcp__ade__approve --permission-prompts host`。
2. `ade mcp` 暴露一個 `approve` 工具：收到工具名稱與輸入後，透過 daemon 轉成 TUI 審批事件，阻塞等待使用者決定，再回傳 allow／deny JSON。
3. 需實測項目：逾時行為、`updatedInput` 是否必填、`--permission-mode` 為 manual／dontAsk 時是否仍呼叫該工具、`--permission-prompts none` 時確實全部自動拒絕。

## 待補清單
1. Codex：`ThreadStartParams` 的 model／sandbox／approval 欄位與列舉（讀 schema 或 learn.chatgpt.com/docs/app-server）。
2. Claude：permission-prompt-tool 的 request／response schema（code.claude.com/docs/en/agent-sdk/permissions）。
3. OpenCode／agy：實跑 ACP `session/new`，觀察有無 model configOptions。
