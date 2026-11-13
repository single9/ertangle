# T11 — 設定檔與每個 agent 的 model／參數設定

優先度：P1　範圍：`termade-core`、`termade-protocol`、`termade-adapter`、`termade-daemon`、`termade-tui`、`src/main.rs`

## 問題

目前沒有任何方式指定 agent 要用哪個 model，也沒有設定檔可以放每個 agent 的預設值：

- spawn 的請求沒有 model 欄位，CLI、TUI、MCP 三個入口都無法指定。
- adapter 有「額外命令列參數」的欄位，但 daemon 永遠傳空陣列，所以也不能用它繞過。
- agent 回報的實際 model 被 daemon 丟掉，`Session.model` 永遠是 `None`。
- 整個專案唯一的設定檔是 `policy.toml`；沒有地方設定預設 agent、預設 model、執行檔路徑、額外參數或環境變數。

多 agent 協作的常見用法（協調者用大模型、worker 用便宜的模型）因此做不到。

## 證據

- `crates/termade-protocol/src/lib.rs:38-61`：`SpawnRequest` 的欄位是 `kind`、`cwd`、`label`、`role`、`prompt`、`worktree`、`transport`、`permission`、`parent`，沒有 `model`。
- `crates/termade-adapter/src/lib.rs:51-68`：`SessionSpec` 有 `args` 與 `env`，沒有 `model`。
- `crates/termade-daemon/src/lib.rs:1337`：`args: vec![]`。
- `crates/termade-daemon/src/lib.rs:475`：`AgentEvent::Model(_) => return`。
- `crates/termade-daemon/src/lib.rs:1301`：`model: None`；之後沒有任何地方寫入。
- `crates/termade-adapter/src/claude.rs:73-74`、`codex.rs:434-435`：adapter 有送出 `AgentEvent::Model`。
- `grep '\.toml'`：只有 `policy.toml` 的讀取。

## 分析邏輯

1. 從三個 spawn 入口（`src/main.rs` 的 `Spawn`、TUI 的 `submit_spawn`、MCP 的 `termade_spawn`）往下追到 `SpawnRequest`，確認沒有 model 欄位。
2. 再追到 `SessionSpec` 找有沒有其他通道；`args` 欄位存在且各 adapter 都有 `cmd.args(&spec.args)`，但 daemon 端寫死空陣列。
3. 反方向追 `Session.model`：adapter 送出 → daemon `ingest` 直接 `return`。所以連顯示都沒有。
4. 各 agent 指定 model 的方式，本機查到的：
   - **Claude**：`claude --help` 列出 `--model <model>`。已確認。
   - **OpenCode**：`opencode acp --help` 沒有 model 旗標。要走 ACP 協定內的 model 選擇，或以設定檔／環境變數注入。未確認。
   - **Codex**：本機未安裝。`thread/start` 應可帶 `model`，或以 `-c model=...` 覆寫。未確認。
   - **Antigravity**：`agy --help` 沒有列出 model 相關選項。未確認。

## 實作方式

1. **設定檔 `config.toml`**（`~/.config/termade/config.toml`，repo 的 `.termade/config.toml` 可覆寫）：
   ```toml
   [defaults]
   agent = "claude"
   permission = "auto"

   [agents.claude]
   bin = "claude"            # 執行檔路徑，可省略
   model = "sonnet"
   args = []                 # 附加在 adapter 自己的參數之後
   env = { }

   [agents.codex]
   model = "..."

   [roles.coordinator]       # 依角色覆寫
   agent = "claude"
   model = "opus"

   [roles.worker]
   model = "haiku"
   ```
   解析順序：spawn 請求的明確值 → `roles.<role>` → `agents.<kind>` → `defaults`。

   目前的 `policy.toml` 是手寫的行解析器（`PolicyEngine::parse_toml`），撐不住巢狀表格。這裡建議引入 `toml` crate；T04 的結構化規則也會需要它。
2. **協定**：`SpawnRequest` 與 `SessionSpec` 增加 `model: Option<String>`。`SpawnRequest` 另加 `args: Vec<String>` 供一次性覆寫。兩者都 `#[serde(default)]`，舊 client 不受影響。
3. **各 adapter 套用 model**：
   - Claude：加 `--model <model>`。
   - Codex：`thread/start` 帶 `model`。先以實機或文件確認欄位名稱。
   - OpenCode／Antigravity（ACP）：先確認 `session/new` 回應是否列出可用 model 與對應的設定方法；若協定內沒有，退而以環境變數或設定檔注入。確認之前，對這兩家指定 model 時回傳明確錯誤，不要靜默忽略。
   - PTY：只透過 `args` 傳遞，不做轉換。
4. **回報實際 model**：新增 `Event::SessionModel { session, model }`（或沿用既有的狀態事件），`ingest` 不再丟棄 `AgentEvent::Model`，`State::apply` 寫入 `Session.model`。
5. **入口**：
   - CLI：`termade spawn --model <m>`，以及 `-- <額外參數>`。
   - MCP：`termade_spawn` 增加 `model`；`termade_peers`／`termade_whoami` 回傳 model。
   - TUI：`n` 的輸入支援 `claude:opus 幫我…` 或 `claude --model opus 幫我…`（擇一，建議前者，較短）；sessions 欄與標題列顯示 model。
6. **執行期切換 model** 不在這張票：各家支援程度不同，先做啟動時指定。

## 驗收條件

- 測試：設定檔解析與四層覆寫順序。
- 測試：Claude adapter 在有 model 時產生的命令列含 `--model <m>`（以 T06 的假 agent 或直接檢查組出的參數）。
- 測試：收到 `AgentEvent::Model` 後 `State` 中該 session 的 `model` 有值。
- 測試：舊格式的 `SpawnRequest` JSON（沒有 `model`、`args`）仍可解析。
- 對尚未支援的 agent 指定 model 時得到明確錯誤。
- `termade sessions` 與 TUI 顯示每個 session 的 model。
