# T26 — 執行期切換 model／effort，並可從 TUI 寫入預設值

優先度：P1　範圍：`termade-core`、`termade-protocol`、`termade-adapter`、`termade-daemon`、`termade-tui`、`src/cmd`　相依：T11

T11 只做到啟動時指定 model（「執行期切換」明確排除）。這張票補上：session 執行中隨時換 model 與 effort，以及不必手改 `config.toml` 就能調整預設值。

## 做法

- 協定：`Request::SessionConfig { session, model, effort }`；`Session.effort` 與 `Event::SessionEffort` 記錄目前值（舊 log 沒有此欄位，`#[serde(default)]`）。
- `AgentControl::set_config(model, effort)`：預設回傳「不支援」錯誤，不靜默忽略。下一個回合起生效。
  - Claude：`control_request` 的 `set_model`（實機確認會回傳解析後的 model）；effort 送 `apply_flag_settings` 的 `effortLevel`。
  - Codex：存起來，之後每個 `turn/start` 帶 `model`／`effort`（以 `codex app-server generate-json-schema` 確認兩者都是 `TurnStartParams` 欄位）。
  - ACP（OpenCode、Antigravity）：`session/set_config_option`，值先對照 agent 回報的 `configOptions`，不在清單內回傳可用清單。
  - PTY：不支援。
- 入口：TUI `:model [名稱]`、`:effort [等級]`（不帶參數顯示目前值與可選項）、Space `s m`／`s e`；CLI `termade model <session> [model] [--effort <level>]`。
- 預設值：TUI `:set [--repo] <agent|permission|model|effort> <值>` 寫入 `config.toml`（`model`／`effort` 寫進選取 session 的 `[agents.<kind>]`；空值＝刪除；`--repo` 寫 `.termade/config.toml`）。只影響之後新建立的 session。`Config::set_value` 以 TOML 重新序列化，檔案內的註解不會保留。

## 驗證

- 單元測試：`Config::set_value` 建立／覆寫／刪除；daemon 的 `SessionConfig` 成功時寫入 `model`／`effort`、adapter 失敗時狀態不變；Claude `set_model` 訊息格式。
- 實機：Claude 切到 `haiku` 後下一個回合實際回報 `claude-haiku-5-5`；OpenCode ACP 切到 `ollama/gemma4:12b` 成功，未知 model 與不支援的 effort 都回明確錯誤。
- 未驗證：Claude `apply_flag_settings` 的 `effortLevel` 只確認回應 `success`，沒有實際比對推理強度是否改變；Codex 的 `turn/start` 帶 `model`／`effort` 只做了 schema 與單元層檢查，沒有實機回合（本機 Codex 無授權）。

## 追加：agent 自行建立 session 的防呆、系統提示、原生斜線指令

- `[agent_spawn]`（只認全域設定，repo 的設定檔不能放寬）：
  - `worktree`（預設 `true`）：帶 `parent` 的 spawn 一律用獨立 worktree；不在 git repo 內就略過。
  - `max_effort`（預設 `medium`）、`allowed_models`（子字串比對，空＝不限）：定義免確認範圍。
  - `confirm`：`costly`（預設，超出範圍才彈出核准請求問使用者，拒絕或 5 分鐘逾時＝失敗）、`always`（每次都問）、`never`（effort 調降到上限，model 不在清單就拒絕）。
  - MCP `termade_spawn` 的 model／effort 說明與協調者的協作說明都提醒：沒有特別需要就不要指定。
- `[collab] prompt_file`：全域的 ADE 系統提示檔，附加在每個 agent 的協作說明最前面（相對路徑以設定目錄為準；過長會截斷）。
- 原生斜線指令：agent 回報的指令存進 `Session.slash_commands`（Claude `init.slash_commands`、ACP `available_commands_update`）。TUI 不認得的 `/名稱` 原樣轉發給選取的 session；`//名稱` 強制轉發（即使與 TUI 命令同名）；命令提示會列出 agent 原生指令。Codex 與 PTY 沒有回報清單，仍會轉發但不保證被識別。

驗證（實機）：`confirm = never` 時 `--model opus` 被拒、`haiku --effort max` 被調降為 `medium` 且自動使用 worktree；`costly` 時超標請求會產生「建立子 agent」核准請求（內容含原因與任務）。未驗證：在 TUI 裡核准後 session 實際建立的流程、Claude `/compact` 之類指令轉發後的實際行為（`/context` 已在 stream-json 直接測過可用）。
