# T14 — Session 接續（resume）：agent 程序死掉或 daemon 重啟後接回原對話

優先度：P1　範圍：`termade-core`、`termade-protocol`、`termade-adapter`、`termade-daemon`、`termade-tui`、`src/cmd/session.rs`

## 問題

agent 程序一旦結束（當掉、被 kill、daemon 重啟），該 session 的對話就接不回來：

- daemon 重啟時，log 裡還在跑的 session 一律標成 `Failed`。
- 沒有任何入口能讓已結束的 session 用原本的對話繼續。
- 唯一的補救是 handoff，但那是開一個新對話加上簡報，原本的 context 會掉。

三家 agent 都有原生的接續機制，只是 termade 沒有記下接續所需的 id，也沒有走那條路徑。

## 證據

- `crates/termade-daemon/src/lib.rs:356-371`：`restore_with` 把仍 live 的 session 收斂成 `Failed`，註解寫明「重啟後子程序已不受管」。
- `crates/termade-core/src/lib.rs:232-262`：`Session` 沒有存 agent 原生 session id 的欄位。
- `crates/termade-adapter/src/claude.rs:76-84`：`on_system` 只從 `init` 取 `model`；同一則訊息的 `session_id`（檔頭註解 `claude.rs:10` 有列出）被忽略。
- `crates/termade-adapter/src/codex.rs:443-449`：thread id 只存在 `conn.thread`（記憶體）。
- `crates/termade-adapter/src/acp.rs:477-480`：`sessionId` 同樣只在記憶體。
- 在 `crates/` 與 `src/` 搜尋 `resume`、`thread/resume`、`session/load`：非測試程式碼沒有結果。

## 分析邏輯

1. 從 `restore_with` 確認重啟後的行為，再往 adapter 追「原生 id 有沒有往上送」：`AgentEvent`（`crates/termade-adapter/src/lib.rs:31-57`）沒有對應的變體，所以 daemon 無從得知。
2. 各家接續方式，本機查到的：
   - **Claude**：`claude --help` 列出 `-r, --resume [value]`、`--session-id <uuid>`、`--fork-session`。已確認旗標存在；**未確認**它和 `-p --input-format stream-json` 併用時的行為，也未確認接續後 `session_id` 會不會換新。
   - **Codex**：`codex --help` 有互動用的 `resume` 子命令。app-server 是否有 `thread/resume`、參數長什麼樣，**未確認**，動手前要先查 app-server 的協定定義。
   - **OpenCode／Antigravity（ACP）**：ACP 規格有 `session/load`，要 `initialize` 回應的 `agentCapabilities.loadSession` 為真才能用；**未確認**這兩家有沒有宣告。`session/load` 會把歷史以 `session/update` 重播一次，載入期間的輸出必須丟掉，否則逐字稿會重複。
3. 沿用同一個 termade session id 比開新 session 合理：label、worktree 分支、被指派的 task、收件匣游標都綁在這個 id 上。
4. 這張票只讀了程式碼與 `--help`，沒有實際跑過任何一家的接續。

## 實作方式

1. **記下原生 id**
   - `AgentEvent` 加 `NativeId(String)`；`Event` 加 `SessionNativeId { session, native_id }`；`Session` 加 `#[serde(default)] native_id: Option<String>`；`State::apply` 寫入。
   - Claude：`on_system` 的 `init` 送出 `session_id`。每次收到都更新（接續後可能換 id）。
   - Codex：`thread/start` 拿到 thread id 後送出。
   - ACP：`session/new` 拿到 `sessionId` 後送出，同時記下 `loadSession` 能力（可放進 `native_id` 之外的另一個欄位，或沒有能力時就不送 id，讓「沒有 id」等於「不能接續」）。
2. **adapter 支援接續**：`SessionSpec` 加 `resume: Option<String>`。
   - Claude：命令列加 `--resume <id>`（組參數的位置在 `claude.rs:395-420`）。
   - Codex：有 `resume` 時改呼叫接續用的方法取代 `thread/start`（`codex.rs:443`）。
   - ACP：有 `resume` 時改呼叫 `session/load` 取代 `session/new`（`acp.rs:472`），載入完成前收到的 `session/update` 不往上送。
   - PTY，以及不支援的組合：在 `crates/termade-adapter/src/lib.rs` 的 `spawn()` 開頭明確報錯，不要靜默開新對話。
3. **daemon**
   - 先把 `handle_spawn`（`handlers/session.rs:18-226`）後半「組 `SessionSpec` → `spawn_adapter` → 登記 `SessionEntry` → 起 ingest 執行緒」抽成一個函式，spawn 與 resume 共用。
   - 新增 `Request::Resume { session, prompt: Option<String> }` 與 `handle_resume`：
     - session 必須存在、不是 live、有 `native_id`，否則回明確錯誤。
     - `cwd`、`branch`、`repo_root`、`kind`、`permission`、`model` 取自既有的 `Session`；其餘（`bin`、`args`、`env`、工具限制）重新走 `Config::resolve`。`effort` 沒存在 `Session` 上，用設定檔解析的值。
     - 成功後送 `SessionStatus::Idle`；有 `prompt` 就當作第一則輸入。
     - 不送 `SessionStarted`（session 還在 state 裡）。
   - daemon 重啟：仍把孤兒標成 `Failed`（程序確實不在了），但加一則 `SessionNote`「daemon 重啟，可用 :resume 接續」。**不要自動接續**，那會在使用者沒注意時動用額度。
4. **入口**
   - CLI：`termade resume <session-or-label> [prompt]`。
   - TUI：`:resume`、Space 選單 session 層加一項（先確認該層的 `key` 沒重複）。對已結束的 session 送 prompt 時，目前回「找不到 session」（`handle_prompt` 的 `None` 分支），改成提示可以 `:resume`。
   - MCP：先不開放給 agent，避免 agent 自己把被人中止的同伴叫回來。
5. **和既有行為的交互**
   - `prune_ended_sessions`（`lib.rs:378`）會在重啟時清掉超過 1 小時、沒有分支的已結束 session，清掉後就不能接續。保留這個行為，寫進 README。
   - Claude 的成本是程序內累計（`claude.rs:263-272` 取 `total_cost_usd`）。接續後是新程序，數字會從頭算，`Event::Cost` 會把舊值蓋掉。接續時要把舊的 `Session.cost` 當基底相加（T17 的預算也依賴這點）。
   - 交接（handoff）不變：接續是同一個 agent 接回原對話，交接是換 agent。

## 驗收條件

- 測試：`SessionNativeId` 事件寫入 state，且經過 `flush_log()` 後重播仍在。
- 測試：舊的 `Session` JSON（沒有 `native_id`）仍可解析。
- 測試：Claude adapter 在 `resume` 有值時，組出的參數含 `--resume <id>`。
- 測試：對 live 的 session、或沒有 `native_id` 的 session 要求接續，回傳錯誤且狀態不變。
- 整合測試（擴充 `tests/fake_claude.sh` 讓它接受 `--resume` 並在 `init` 回報 id）：spawn → kill → resume 後，session id 不變、狀態回到 `Idle`、原本的逐字稿還在、worktree 分支沒被動到。
- 測試：接續後的成本是舊值加新值。
- 實機驗證三家各一次，結果記進 `docs/VERIFICATION.md`；沒驗證過的寫明未驗證。
- 依 `docs/MAINTAINING.md` 同步：`SessionSpec`／`Session` 的所有 literal、`handlers/mod.rs` 的 `match`、`COMMANDS` 表、`help.rs`、README。
