# T32 — Subagent 與背景執行程式／長耗時指令的 TUI 即時可觀測性與處理程序群組控管

優先度：P1　範圍：`termade-core`、`termade-protocol`、`termade-adapter`、`termade-daemon`、`termade-tui`、`src/`　相依：—

Status: done

## 問題

當 Agent 在回合內開啟內部 Subagent (如 Claude Code 的 `Task`／`Agent` 工具、Antigravity 的 `invoke_subagent`)，或執行長耗時指令與背景處理程序 (如 `cargo test`、`npm run dev &`、背景任務) 時，目前 TUI 的提示與管控存在四個明顯盲區：

1. **進行中的 Tool Call 與原生 Subagent 缺乏即時動態提示**：
   - 透過 `termade_spawn` 建立的 `termade` 子 Session 雖會在左側 Sessions 清單縮排顯示 (`↳`)，但 Agent 內建的原生 Subagent (例如 Claude `Task`、Antigravity `invoke_subagent`) 只是一筆普通的 `Event::ToolCall`。
   - 當工具正在執行 (`ok: None`) 時，逐字稿雖會插入一行 `▸ <name>: <summary>`，但若連續呼叫多個工具，預設摺疊模式 (`show_tools = false`) 會將其合併為 `▸ 工具 ×N (✓M) · 最後：…`，看不出**最後這一個工具其實還在執行中 (尚未結束)**，也沒有在左側 Session 列表或逐字稿底部顯示「目前正在執行哪個指令／Subagent + 已耗時幾秒」。
2. **背景殘留處理程序 (Background Child Processes) 完全不可見**：
   - 若 Agent 在 Shell 工具中啟動了背景處理程序 (例如 `nohup`、`&`、dev server 或測試伺服器)，一旦該次 Shell 工具呼叫返回，TUI 就會顯示 `✓` 並將 Session 轉為 `○ Idle`。使用者在 TUI 完全看不出該 Session 底下仍有背景處理程序在消耗 CPU／記憶體或佔用連接埠。
3. **無法在不殺死 Agent 的前提下檢視或單獨清理背景程式**：
   - 當 Agent 啟動了背景伺服器或卡住的子處理程序時，使用者無法在 TUI 查看其 PID 與完整命令列，也無法「只終止背景程式、保留 Agent 對話本身」。
4. **休眠 (`sleep`) 與強制結束 (`kill`) 只打單一 PID，未涵蓋子處理程序樹**：
   - `signal_pid` 僅對 Agent 主處理程序 PID 送出 `SIGSTOP`／`SIGCONT`，`Child::kill()` 也只殺直接子處理程序，Agent 派生的背景執行檔或編譯工作不會一併暫停或清理。

## 證據

- `crates/termade-core/src/lib.rs:976-1014`：`Event::ToolCall` 在 `ok: None` 時只往 `transcripts` 推入一行 `▸ {name}: {sum}`，`Session` 結構 (`lib.rs:245-293`) 並未記錄 `active_tools` (目前執行中的工具名稱與開始時間戳) 或 `bg_procs` (背景子處理程序清單)。
- `crates/termade-tui/src/app.rs:842-885`：`visible_transcript` 在 `show_tools == false` 時，不論最後一筆 `▸ ` 是否已經收到對應的 `  ✓`／`  ✗`，都會被摺進 `▸ 工具 ×{calls} (✓{ok})· 最後：{last}`，沒有標示「進行中」或拆出尚未完成的工具呼叫。
- `crates/termade-tui/src/draw.rs:919-974`：左側 Sessions 清單只顯示 `s.status_note` (由 Agent 主動呼叫 `termade_status` 更新)，當 Agent 卡在長耗時 `Bash` 或原生 `Task`／`invoke_subagent` 時，若未主動呼叫 `termade_status`，側欄只看得到綠燈 `●`，看不出正在跑什麼。
- `crates/termade-adapter/src/lib.rs:161-170`：`signal_pid(pid, sig)` 呼叫 `kill <sig> <pid>` 而非對 Process Group (`-<pgid>`) 或子處理程序樹送訊號；daemon 也沒有探測各 Session 主處理程序底下的背景子處理程序。

## UX 設計原則

為確保終端機畫面維持極簡高資訊密度，不因新增狀態而造成視覺雜訊或操作負擔，必須遵循以下 UX 規範：

1. **平時零干擾 (Zero Clutter by Default)**：
   - 沒有執行中工具或背景處理程序時，介面與現況完全相同，不顯示 `bg:0` 或空白區塊。
   - 左側 Sessions 清單維持固定兩行高度 (`SESSION_ITEM_ROWS = 2`)：
     - **第一行**：僅在 `Busy` 時於標籤後附加精簡活動標籤 (原生 Subagent 顯示 `· sub:<name>`，一般工具顯示 `· run:<name>`；若已有 `status_note` 則優先顯示目前執行的工具以反映即時卡點)。
     - **第二行**：僅在 `bg_procs` 非空時附加 `· bg:1 (node)`；若超過 1 個背景處理程序則摺疊為 `· bg:3 (node…)`，絕不撐破單行寬度。
2. **語意化警示分級 (不亂亮紅黃燈)**：
   - 當 Session 處於 `● Busy` 時，底下有子處理程序屬於正常現象，`bg:N` 維持低調的預設暗灰色 (`DarkGray`)。
   - **只有當 Session 已經回到 `○ Idle` (回合結束)，底下卻仍有殘留的背景處理程序時**，第二行與底部 Footer 的 `bg:N` 才轉為**黃色警示色 (`Yellow`)**，精準提示「回合已結束但有背景程式仍在跑」。
3. **漸進式揭露：互動式處理程序檢視浮層 (`:ps` / `Space s p`)**：
   - 側欄與底部 Footer 只顯示極簡摘要 (`bg:2 (node…)`)；想看細節或砍程式時，按 **`Space s p`** (Which-key：`Space` → `s` Session → `p` 處理程序)、輸入 **`:ps`**、或用**滑鼠點擊底部 Footer 的 `bg:N` 區段**，即彈出置中浮層面板 (與既有的 Picker / Settings 浮層風格一致)。
   - 浮層內部分為上下兩區 (空區自動隱藏)：
     - **[執行中工具 / Subagent]**：顯示完整工具名稱、目標參數摘要、已執行秒數 (`14s`)。
     - **[背景子處理程序]**：以對齊表格列出 `PID`、`執行時間`、`命令列摘要` (例如 `48291  02:15  node vite --port 5173`)。
   - **浮層內單鍵直覺操作 (不誤傷 Agent 本體)**：
     - `j` / `k` 或 `↑` / `↓`：移動選取列。
     - `x` (或 `d`)：單獨終止選取的背景子處理程序 (`SIGTERM`；若對同一個仍在跑的 PID 再按一次則送 `SIGKILL`)，**完全不影響 Agent 主處理程序與對話狀態**。
     - `X`：一鍵清理該 Session 所有殘留的背景子處理程序 (`bg-clean`)。
     - `Esc` / `q`：關閉浮層。

## 分析邏輯

1. **區分「正在跑的工具」與「已完成的工具」**：
   - Adapter 在工具開始時送 `AgentEvent::ToolCall { name, input, ok: None, output: None }`，結束時送 `AgentEvent::ToolCall { ok: Some(bool), .. }`。
   - 在 `State::apply` 中，為每個 `Session` 維護 `active_tools: Vec<ActiveTool>` (含 `name`、`summary`、`started_ms`、`is_subagent`)：收到 `ok: None` 時推入，收到 `ok: Some(_)` 或 `SessionStatus` 變為非 `Busy`／非 `WaitingApproval` 時移除。
   - 當 `name` 為 `Task`、`Agent`、`dispatch_agent`、`invoke_subagent`、`spawn_agent` 等原生 Subagent 工具時，標記 `is_subagent = true`。
2. **背景處理程序探測 (Linux / macOS)**：
   - Daemon 持有每個活躍 Session 的主處理程序 `pid: u32`。
   - 透過輕量級系統查詢 (Linux 讀取 `/proc/<pid>/task/<tid>/children` 與 `/proc/<cpid>/cmdline`，macOS 使用 `pgrep -P` 與 `ps -o pid=,etime=,comm=,args=`)，定期 (每 3 秒，以及回合轉為 `Idle` 瞬間) 收集該 Session 的後代處理程序清單 `Vec<BgProc>` (`pid`、`name`、`cmdline`、`elapsed_secs`)。
   - 自動過濾掉 `termade mcp` 自身以及目前正在前台同步執行中的 `Bash` 子處理程序 (或於 `Idle` 時全數列出)，僅在清單變動時發出 `Event::SessionBgProcs` 事件，避免無意義刷屏。
3. **處理程序群組 (Process Group) 與單別終止管控**：
   - 在 Unix 上啟動 Agent 子處理程序時，透過 `CommandExt::process_group(0)` 讓每個 Session 成為獨立的 Process Group Leader (`pgid == pid`)。
   - 對整個 Session 執行 `sleep`、`wake` 與 `kill` 時，改對 `-<pgid>` 送出訊號，確保 Agent 派生的所有子處理程序一併暫停、喚醒或清理。
   - 當使用者在 `:ps` 浮層單獨終止某個背景處理程序 (`Request::KillBgProc { session, pid: Some(target_pid), force }`) 時，Daemon 驗證該 `target_pid` 確實屬於該 Session 的後代處理程序樹 (防止誤殺無關系統處理程序)，再對該 `target_pid` 送出 `SIGTERM`／`SIGKILL`。
4. **未實機確認的部分**：
   - 各家 Agent (Claude、OpenCode、Antigravity、Codex) 在放入獨立 Process Group (`process_group(0)`) 後，是否會影響其內部對子處理程序的控制 (需於四家 Adapter 整合測試確認)。
   - Claude 平行 `tool_use` 時 `on_tool_results` 目前未帶回 `tool_use_id` (`claude.rs:205`)，可以 FIFO 佇列配對並在回合轉為 `Idle` 時兜底清空。

## 實作方式

1. **核心資料結構與事件 (`crates/termade-core/src/lib.rs`、`crates/termade-protocol/src/lib.rs`)**：
   - 新增 `ActiveTool { name: String, summary: String, started_ms: u64, is_subagent: bool }` 與 `BgProc { pid: u32, name: String, cmdline: String }`。
   - 在 `Session` 新增 `active_tools: Vec<ActiveTool>` 與 `bg_procs: Vec<BgProc>` (`#[serde(default)]`)。
   - 新增 `Event::SessionBgProcs { session: SessionId, procs: Vec<BgProc> }`。
   - 新增 `Request::KillBgProc { session: SessionId, pid: Option<u32>, force: bool }` (`pid: None` 代表清理該 Session 底下所有背景子處理程序)。
2. **TUI 即時呈現與 `:ps` 檢視浮層 (`crates/termade-tui/`)**：
   - **逐字稿摺疊優化 (`app.rs:visible_transcript`)**：若最後一筆 `▸ ` 工具呼叫尚未完成 (`active_tools` 非空)，不摺入已完成的統計行，而是獨立顯示於最下方：`▸ [執行中] <name>: <summary> (12s)`；若為原生 Subagent 則顯示 `▸ [subagent] <name>: <summary> (12s)`。
   - **左側 Sessions 清單與底部 Footer (`draw.rs`)**：
     - 第一行於 `Busy` 時附加 `· sub:<name>` 或 `· run:<name>`。
     - 第二行與逐字稿底部 Footer 於 `bg_procs` 非空時顯示 `bg:<N> (<name>)` (`Idle` 時以黃色高亮)，並支援滑鼠點擊 Footer 上的 `bg:<N>` 直接打開 `:ps` 浮層。
   - **互動式處理程序浮層 (`draw.rs`、`input.rs`、`menu.rs`、`commands.rs`)**：
     - Which-key 新增 `Space s p` (`處理程序與 Subagent`)，命令列新增 `:ps` 與 `:bg-kill [pid|--all]`。
     - 浮層支援 `j`/`k` 選擇、`x` 終止單一背景 PID、`X` 清理全部背景子處理程序、`Esc` 關閉。
3. **Daemon 處理程序樹巡檢與 Process Group 控制 (`crates/termade-daemon/`、`crates/termade-adapter/`)**：
   - Adapter 啟動子處理程序時設定 `process_group(0)`，`sleep`/`wake`/`kill` 同步作用於整個處理程序群組。
   - Daemon 背景執行緒定期掃描活躍 Session 的後代處理程序樹 (排除 `termade mcp`)，有異動時廣播 `Event::SessionBgProcs`。
   - 實作 `KillBgProc` 處理器與 CLI `termade ps [session]`、`termade bg-kill <session> [--pid <pid> | --all]`。

## 驗收條件

- 當 Agent 呼叫長耗時 `Bash` 或原生 Subagent (`Task`／`invoke_subagent`) 且尚未返回時，即使在預設工具摺疊模式下，TUI 逐字稿底部與左側 Session 列都能即時看到正在執行的工具／Subagent 名稱與耗時。
- 當 Agent 在背景啟動長駐處理程序 (如 `sleep 60 &`) 並結束當前回合回到 `Idle` 時，TUI 的 Session 次要資訊列與底部 Footer 會以黃色警示色顯示 `bg:1 (sleep)`。
- 在 TUI 按 `Space s p` 或輸入 `:ps` 可開啟處理程序檢視浮層，列出背景處理程序的 PID 與命令列；按 `x` 能單獨終止該背景處理程序，且 Agent Session 本體維持存活可繼續對話。
- 對含有背景子處理程序的 Session 執行 `termade kill` 或 `termade sleep`，其衍生的子處理程序能一併被終止或暫停。
- `make pre-commit` (`cargo fmt`、`cargo clippy`、`make i18n-check`) 與 `cargo test` 全數通過。

## Comments

- 2026-11-17：已於 `f53e4cb` 實作。該 commit 的 `signal_pgid` 以 `kill <sig> -<pgid>` 送訊號，在 procps-ng 4.0.4 下被解析成 `kill -1`，結束 session 時會把整個桌面登出；已於 T39 (`79d9029`) 修正。
