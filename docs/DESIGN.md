# termade — 設計文件

> 目標：用 **Rust + Ratatui** 做一個終端機原生的 termade。
> 核心價值是「**多個 agent 互相溝通、協作、平行工作**」，而不是複製 GUI IDE 的圖形功能。

> **已定案的前提（v1）**
> - 平台：**Linux + macOS**（Unix domain socket + PTY）；Windows 之後再補。
> - daemon：**單一常駐 daemon 管所有 repo**（一個使用者一個 daemon，服務多個專案與多個 TUI）。
> - 依賴：**盡量零 Node**。四家 agent 都走原生介面，ACP 的 npm adapter 只當選配。
>   - OpenCode：原生 `opencode acp`
>   - Claude Code：原生 `claude -p --input-format stream-json --output-format stream-json`
>   - Codex：原生 `codex app-server`
>   - Antigravity：原生 `agy acp`（與 OpenCode 同 ACP 方言）
> - M0 先做骨架，且 daemon / adapter 以 **std thread + blocking IO** 實作（暫不引入 async runtime）。

> **閱讀須知（現況 vs. 原始設計）**
> 本文件前半段原寫於動工前。第 4.3、4.4、5（機制 C）、9、11、12 節已依實作改寫為「現況」；
> 仍保留的原始構想會明確標示「未實作」，彙整於文末「附錄 A：原始設計與未實作項目」。
> 各里程碑的實機驗證紀錄移至 [`VERIFICATION.md`](VERIFICATION.md)。

---

## 0. 一句話定義

`termade` 是一個**常駐 daemon + 可拋棄式 TUI 前端**的系統：

- daemon 負責「跑 agent、管 worktree、轉譯各家 agent 協定、當訊息交換中心」。
- TUI 只負責「顯示狀態、收鍵盤輸入、把指令送回 daemon」，因此啟動快、記憶體小、可隨時關掉重開。

它同時對**人**（TUI）與**agent**（MCP / CLI / socket）開介面，所以 agent 可以自己建立 worker、互相傳訊息、回報任務狀態。

---

## 1. 目標與非目標

### 目標
1. 在同一或多個 repo 上，同時跑多個 agent session，互不干擾（git worktree 隔離）。
2. 讓 agent 之間能**交換訊息、共用任務清單、交接工作**，並可由 agent 自行產生子 agent。
3. 支援 **OpenCode / Claude Code / Codex / Antigravity**，且新 agent 只要寫一個 adapter 就能加入。
4. 啟動 < 200ms、閒置記憶體 < 20MB（不含 agent 子程序），可在 SSH / tmux 中運作。
5. 人可隨時「介入、旁觀、接管」任何 session；所有事件可回溯。

### 非目標（明確不做）
- 內建瀏覽器 / mobile app / 桌面 GUI / 圖片 PDF 預覽。
- 自己實作 LLM agent loop（我們只做 orchestrator，不重造模型推理）。
- 遠端 SaaS 控制平面（但架構要留 SSH / remote 的縫）。

---

## 2. 為什麼是這個架構

### 2.1 前端與狀態分離（daemon + client）
GUI termade 常見問題是「關掉視窗 = 殺掉 agent」。我們把狀態放在 daemon：

- agent 程序的生命週期與 TUI 解耦 → 可以關 TUI、之後再 attach。
- 多個 client（TUI、`termade` CLI、未來的 web）可同時看同一份狀態。
- 事件流是 append-only log → 天然可重播、可審計、可 diff。

### 2.2 為什麼選 ACP 當主要整合面
[Agent Client Protocol](https://agentclientprotocol.com)（Zed 發起，JetBrains 共同推動）正好就是「**編輯器/客戶端 ↔ coding agent**」的標準，用 JSON-RPC over stdio，具備我們要的一切：

- `initialize` / `session/new` / `session/prompt` / `session/cancel`
- 串流事件（assistant 訊息、tool call 開始/結束）
- **server→client 的 permission request**（審批）
- 可傳入 **MCP servers**（`session/new` 時帶入）→ 我們能把 message bus 注入 agent
- 官方 **Rust crates**：`agent-client-protocol`（runtime）與 `agent-client-protocol-schema`（型別）

各家現況：

| Agent | ACP 途徑 | 原生程式化途徑（fallback） |
| --- | --- | --- |
| **OpenCode** | 原生 `opencode acp` | `opencode serve`（HTTP + SSE + OpenAPI）、`opencode run --format json` |
| **Claude Code** | 官方 adapter `claude-agent-acp`（包 Claude Agent SDK） | `claude -p --input-format stream-json --output-format stream-json --verbose`（雙向持久 session） |
| **Codex** | 社群 ACP adapter | `codex app-server`（JSON-RPC，thread/turn/item、approval、auth）、`codex exec --json` |
| **Antigravity** | 原生 `agy acp`（已實測，與 OpenCode 同方言） | `agy -p "prompt"`（headless print）、`--mode`、可管線化 JSON metadata、Antigravity SDK |

> 策略（零 Node 版）：**原生協定優先**，ACP 為選配。
> OpenCode 與 Antigravity 的原生 ACP（`opencode acp` / `agy acp`）不需額外 npm 套件，且是最結構化的兩條，
> 因此走 ACP；Claude 走原生 stream-json，Codex 走 `codex app-server`。
> 若連原生協定都沒有，最後手段是 PTY 直通（raw terminal passthrough）——這條路對「任何」CLI agent 都有效。

---

## 3. 系統架構

```
                          ┌───────────────────────────────────────────────┐
                          │              termade (TUI client)                 │
                          │   ratatui · 只渲染 + 收鍵盤 · 可隨時重啟       │
                          └───────────────┬───────────────────────────────┘
                                          │ control socket (Unix, NDJSON / JSON-RPC)
                          ┌───────────────▼───────────────────────────────┐
                          │                aded (daemon)                  │
                          │  ┌───────────┐ ┌──────────┐ ┌──────────────┐  │
                          │  │ Event log │ │ Message  │ │ Task board   │  │
                          │  │ append-   │ │ bus      │ │ (共享任務)    │  │
                          │  │ only      │ │(channels)│ │              │  │
                          │  └───────────┘ └──────────┘ └──────────────┘  │
                          │  ┌──────────────────────────────────────────┐ │
                          │  │ Session supervisor                       │ │
                          │  │ worktree mgr · permission policy · cost  │ │
                          │  └──────────────────────────────────────────┘ │
                          │  ┌──────────────┐   ┌──────────────────────┐  │
                          │  │ MCP server   │   │ Adapter registry     │  │
                          │  │ (bus tools)  │   │ acp | claude | ...   │  │
                          │  └──────┬───────┘   └──────────┬───────────┘  │
                          └─────────┼──────────────────────┼──────────────┘
                                    │ MCP (stdio)          │ spawn + 協定
              ┌─────────────────────┼──────────────────────┼──────────────────────┐
              │                     │                      │                      │
        ┌─────▼─────┐        ┌──────▼──────┐        ┌──────▼──────┐        ┌──────▼──────┐
        │ opencode  │        │ claude code │        │   codex     │        │ antigravity │
        │ acp       │        │ acp / stream│        │ app-server  │        │ agy / acp   │
        └───────────┘        └─────────────┘        └─────────────┘        └─────────────┘
              ▲  每個 agent 都可透過 termade MCP tools 或 `termade` CLI 呼叫 daemon
```

**單一執行檔、多種模式**（啟動快、部署簡單）：

```
termade                 # 等同 termade tui：確保 daemon 在跑 → attach
termade daemon          # 前景執行 daemon（--detach 可背景化）
termade tui             # 純前端
termade mcp             # 以 stdio 跑 MCP server（供 agent 子程序當 MCP server 用）
termade send/recv/tasks # CLI，給 shell / agent 用
termade spawn ...       # 建立 session（可指定 agent、worktree、prompt）
```

daemon 的 socket 放 `$XDG_RUNTIME_DIR/termade/daemon.sock`（或 `~/.local/state/termade/`），
`termade tui` 找不到就自動 spawn 一個 daemon（類似 `opencode` 的行為）。

---

## 4. 核心抽象

以 `termade-core` 定義，所有 adapter 與前端共用。

### 4.1 `AgentKind`
```rust
enum AgentKind { OpenCode, ClaudeCode, Codex, Antigravity, Custom(String) }
```

### 4.2 `Session`
一個 agent 的執行實例。
```rust
struct Session {
    id: SessionId,            // "ses_..."
    kind: AgentKind,
    label: String,            // 人類可讀，例如 "worker/checkout"
    cwd: PathBuf,             // 可能是 worktree 路徑
    worktree: Option<WorktreeRef>,
    role: Role,               // Coordinator | Worker | Reviewer | Custom
    status: SessionStatus,    // Starting | Idle | Busy | WaitingApproval | Exited | Failed
    model: Option<String>,
    cost: Option<Cost>,       // 由 claude/opencode 回報
    parent: Option<SessionId>,// 誰 spawn 的（agent 或人）
}
```

### 4.3 `Event`（append-only log 的元素）
定義於 `crates/termade-core/src/lib.rs`，序列化為 `{"kind": "snake_case", ...}`，外層 `Record { seq, ts, event }`：
```rust
enum Event {
    SessionStarted { session },
    SessionStatus { session, status },
    SessionLabel { session, label },
    SessionNote { session, note },
    UserPrompt { session, text },
    Output { session, text },                  // 串流輸出
    ToolCall { session, name, input, ok, output },
    PermissionRequested { request },
    PermissionResolved { session, req, decision },
    BusMessage { message },                    // agent 之間的訊息
    TaskChanged { task },
    ArtifactShared { artifact },               // 產出的檔案/分支/diff
    ReviewCommented { review },
    SessionExited { session, code },
    Cost { session, cost },
}
```
所有狀態都是 fold(Event) 的結果（`State`）→ TUI 重連時能重建畫面。

### 4.4 Adapter 與 `AgentControl`（現況：同步、std thread）
`crates/termade-adapter/src/lib.rs` 以**同步 trait** 實作，沒有 async runtime：

```rust
trait AgentControl: Send + Sync {
    fn send_input(&self, text: &str) -> Result<()>;
    fn cancel(&self) -> Result<()>;
    fn kill(&self) -> Result<()>;
    fn sleep(&self) -> Result<()>;   // SIGSTOP，預設空實作
    fn wake(&self) -> Result<()>;    // SIGCONT，預設空實作
    fn resolve_permission(&self, req: &str, decision: Decision) -> Result<()>;
    fn transport(&self) -> &'static str;
}
```
啟動時由 adapter 以 `SessionSpec`（kind、cwd、權限政策、`mcp_exe`、初始 prompt…）建立子程序與讀取執行緒，
把各家協定（ACP、stream-json、app-server、PTY）**正規化成同一組 `AgentEvent`**，經 channel 送給 daemon。
daemon 之上完全不需要知道底下是哪一家。實作位於 `acp.rs`、`claude.rs`、`codex.rs`、`pty.rs`。

### 4.5 訊息與任務
```rust
struct BusMessage {
    id: MsgId,
    from: SessionId,
    to: Target,             // Direct(SessionId) | Channel(String) | Broadcast
    kind: MsgKind,          // Chat | Handoff | Question | Answer | Status | Artifact
    body: String,
    reply_to: Option<MsgId>,
    ts: Timestamp,
}

struct Task {
    id: TaskId,
    title: String,
    detail: String,
    status: TaskStatus,     // Todo | Doing | Blocked | Review | Done
    assignee: Option<SessionId>,
    depends_on: Vec<TaskId>,
    artifacts: Vec<ArtifactRef>,
}
```

---

## 5. Agent 之間如何溝通與協作（本專案的核心）

提供**三種互補機制**，由強到弱，確保任何 agent 都能參與。

### 機制 A（主要）：把 bus 做成 MCP server，注入每個 agent
daemon 開一個 MCP server（`termade mcp`）。spawn agent 時，透過該 agent 自己的 MCP 設定把它接上去：

- Claude Code：`--mcp-config <termade.json>`
- Codex：`~/.codex/config.toml` 的 `[mcp_servers.termade]`（或在 session 建立時帶入）
- OpenCode：`opencode.json` 的 `mcp` 欄位（ACP `session/new` 也接受 MCP servers）
- Antigravity：`~/.gemini/antigravity-cli/settings.json` 的 MCP 設定

對 agent 曝露的工具（語意為「協作原語」）：

| MCP tool | 用途 |
| --- | --- |
| `termade_whoami()` | 回傳自己的 session id、role、worktree、branch |
| `termade_peers()` | 列出其他 session（id、role、status、正在做什麼） |
| `termade_send(to, body, kind?)` | 傳訊息給某個 session / channel / broadcast |
| `termade_inbox()` | 讀取尚未處理的訊息 |
| `termade_wait(timeout_ms)` | 阻塞等待下一則訊息（協作交握用） |
| `termade_ask(peer, question, timeout_ms)` | 送出問題並等回覆（request/response） |
| `termade_task_list()` / `termade_task_create()` / `termade_task_update()` | 共用任務看板。`depends_on` 未完成的任務建立為 `blocked` 且不通知負責人，前置任務全部 `done` 後由 daemon 改回 `todo` 並送指派訊息；`task_list` 的 `waiting_on` 列出還沒完成的前置任務 |
| `termade_spawn(kind?, prompt, worktree?, role?)` | **agent 產生子 agent**（協調者 → 工作者） |
| `termade_artifact_share(path\|branch\|diff)` | 分享產出（檔案、分支、diff）給他人或看板 |
| `termade_status(text)` | 更新自己在 TUI 上的狀態字串（輕量進度回報） |

> 這是「agent 驅動 orchestration」的關鍵：**協調者 agent 可以直接 `termade_spawn` 出 worker**，
> 而 worker 的審批、狀態、訊息全部回到 TUI，人類可隨時介入。

### 機制 B：`termade` CLI（給 shell / 不支援 MCP 的 agent）
同一批能力也有 CLI 形式，agent 只要會跑 shell 就能用：

```bash
termade whoami
termade send --to worker/checkout "請先跑測試再交棒"
termade inbox --json
termade task create "修正 EU 郵遞區號驗證"
termade spawn --kind codex --worktree --role worker "實作 X，完成後 termade send --to coordinator done"
```

CLI 內部直接打 daemon 的 control socket，零額外成本。

### 機制 C（最低共同標準）：檔案信箱
對於不支援 MCP 又不方便跑 CLI 的 agent，daemon 會把收到的訊息同步寫到：

```
<cwd>/.termade/mailbox/<session>.jsonl   # 收到的訊息（daemon 單向寫入，append_mailbox）
```

**未實作**：原設計的 `outbox/<session>.jsonl`（agent 寫入、daemon 以 `notify` 監看後轉發到 bus）、
`tasks.json` 快照與 `AGENTS.md` 片段注入。目前 agent 若要回覆，仍須透過 MCP 或 `termade` CLI。

### 協作模式（由 config 定義 role）
- **Hub-and-spoke**：Coordinator 收需求 → 拆任務 → `termade_spawn` 多個 worker（各自 worktree）→ worker 回報 → Coordinator 整合。
- **Peer-to-peer**：實作者 ↔ 審查者直接對話（`termade_ask` / `termade_send`），人類只旁觀。
- **Channels**：`#build`、`#review`、`#alerts` 廣播。
- **Handoff**（僅有 `kind=handoff` 訊息類型，會喚醒對方；**未實作**連同 worktree 轉交 session）：一個 agent 把 session 連同 worktree 交給另一個（換模型/換工具續做）。

### 防呆（避免 agent 無限互傳）
- 每對 session 的訊息速率限制 + 迴圈偵測（相同 body 連續出現 N 次即斷路）。
- 每個 task 的訊息預算（token/回合上限）：**未實作**。
- `termade_wait` 有 timeout，預設不會永久阻塞。

---

## 6. Control API（daemon ↔ client）

- 傳輸：Unix domain socket，`newline-delimited JSON`（先求簡單；未來可換 JSON-RPC 2.0）。
- 語意：client 訂閱事件流 + 送命令。

```
→ {"op":"hello","client":"tui","since":12345}     # since = 從哪個 seq 之後補事件
← {"ev":"event","seq":12346,"body":{...}}          # 事件串流（server push）
→ {"op":"spawn","kind":"claude","prompt":"...","worktree":true,"role":"worker","transport":"auto","permission":"auto"}
→ {"op":"prompt","session":"ses_1","text":"..."}
→ {"op":"permission","session":"ses_1","req":"r7","decision":"allow"}
→ {"op":"bus.send","to":{"channel":"review"},"body":"..."}
→ {"op":"task.update","id":"t3","status":"done"}
```

TUI 斷線重連時帶 `since`，daemon 從 event log 補齊 → 畫面無縫重建。
事件以 `seq` 單調遞增，log 落地為 `.termade/log/events.jsonl`（可選 rotate / 上限）。

---

## 7. TUI 設計（Ratatui）

### 7.1 版面
```
┌ Sessions ────────┬─ Transcript: worker/checkout ─────────┬─ Bus / Activity ──┐
│ ▾ repo acme      │ ▍prompt: 修正 EU 郵遞區號驗證         │ 12:01 coord→w1   │
│   ● main         │ ▸ Read validators.ts                  │ 12:02 w1: done    │
│   ▾ fix/eu-zip   │ ▸ Edit validators.ts (+18 -3)         │ 12:03 w1→coord    │
│     ● worker/…   │ ✓ 27 tests pass                        │ ...               │
│     ○ reviewer/… │                                       │                   │
│   ▾ feat/cart    │                                       │                   │
│     ⏸ worker/…   │                                       │                   │
├──────────────────┴───────────────────────────────────────┴───────────────────┤
│ [Overview][Diff][Tasks][Logs]                                                 │
│ > @worker/checkout 請把 reviewer 的意見套用                                   │
└───────────────────────────────────────────────────────────────────────────────┘
```

- **左**：repo → worktree → session 樹；狀態以顏色/符號表示（●忙 ○閒 ⏸待審批 ✗失敗）。
- **中**：選定 session 的對話/工具呼叫/diff（結構化渲染；無法結構化時 fallback 成 PTY 原始輸出）。
  - 逐字稿會即時渲染 **Markdown**：標題、粗體／斜體／刪除線、行內程式碼、連結、無序／有序／工作清單、引言、分隔線、圍籬程式碼區塊（含語言標籤與底色）、GFM 表格（含對齊）。
  - termade 的語意前綴分色：`❯`（使用者 prompt，綠色）、`💭`（思考，暗色斜體）、`▸`（工具呼叫，琥珀色）、`💬`（審查註解，洋紅）。
  - 支援翻頁捲動與「跟隨最新」（follow-tail），右側顯示捲軸；滾輪可捲動，滑鼠拖曳可直接選取逐字稿（只含中央區塊）並以 OSC 52 複製；`F2` 可關閉滑鼠擷取改用終端原生選取；`t` 可在渲染後與原始文字間切換。
  - `💭` 思考行預設隱藏（`T` 或 `/thoughts` 切換），避免長篇思考拖慢渲染。
  - `Y` 或 `/copy` 以 OSC 52 複製中央主要區塊（不含側欄與邊框），在 SSH / tmux 下也不需終端原生選取。
  - Diff 視圖每行附舊／新檔行號邊欄，標題顯示 `+N -M` 統計，`{` / `}` 可跳 hunk。
- **右**：bus 活動流 + 任務看板（可切 tab）。
- **Tasks 視圖**（中央，`Tab` 或 `:tasks`）：任務依狀態分組，顯示負責人與前置任務，下方是選取任務的說明；`Enter` 跳到負責人，`:task-status` 改狀態。`Task` 沒有專案欄位，只看本專案時用負責人或建立者的 session 判斷，兩者都沒有就顯示。
- **底**：輸入框。`@session` 指定對象、`#channel` 廣播、`/` 開命令面板（`/spawn`、`/merge`、`/task`）。
- **標題列**：兩行——上排為視圖頁籤與 session/task 數，下排為成本、token 用量與 `md`／`think`／`mouse`／`follow` 狀態指示。
- **Diff 視圖**：每個 worktree 的 `git diff`，可對行下註解並送回該 agent（借用 Antigravity/Orca 的 review 迴圈概念）。

### 7.2 關鍵按鍵
（與 `README.md`「TUI 按鍵」同步；以 README 為準。）

| 鍵 | 動作 |
| --- | --- |
| `j` / `k`、↑ / ↓ | 選擇 session（在 Diff 視圖則為移動 diff 行游標） |
| 滑鼠拖曳 | 在逐字稿上選取（只含中央區塊，不含側欄與邊框），放開即以 OSC 52 複製 |
| 滑鼠滾輪 | 捲動逐字稿（Diff 視圖則移動 diff 游標） |
| `F2` | 切換滑鼠擷取（預設開啟；關閉後可用終端原生拖曳選取，`Shift`+拖曳在開啟時也可原生選取） |
| `[` / `]` | 在任何視圖切換上一個 / 下一個 session |
| `Tab` / `g` | 切換中央視圖（Overview → Diff → Worktrees → Tasks） |
| `t` | 切換逐字稿顯示：Markdown 渲染 ↔ 原始文字 |
| `T`（或 `/thoughts`） | 顯示 / 隱藏 agent 的思考行（`💭`；預設隱藏以減少渲染量） |
| `Y` 或 `/copy` | 用 OSC 52 複製中央主要區塊（逐字稿 / diff / worktrees），不含側欄與邊框 |
| `PgUp` / `PgDn`（或 `Ctrl-U` / `Ctrl-D`） | 逐字稿 / diff 翻頁捲動（捲動後暫停跟隨，狀態列會顯示 `⏸ paused`） |
| `Home` / `End` | 跳到最前 / 回到跟隨最新輸出（`▼ follow`） |
| `{` / `}` | （Diff 視圖）跳到上一個 / 下一個 hunk（`@@`） |
| `i` / `Enter` | 對選取 session 送 prompt（支援 `@peer 訊息`、`#channel 訊息`、`/命令`） |
| `←` / `→`、`Home` / `End`、`Delete` | 在輸入框移動游標 / 跳到頭尾 / 刪除字元（支援中文等寬字元與 bracketed paste） |
| `n` | 建立 session：`<agent> <prompt>`（`Ctrl-W` 切換是否開 worktree） |
| `m` | 送訊息到匯流排（agent 之間溝通） |
| `c` | （Diff 視圖）對目前選取的 diff 行下審查註解並送回 agent 修改 |
| `M` / `G` | 合併選取 session 的 worktree 分支回主工作區 / 清理已結束 worktree |
| `z` | 切換選取 session 的休眠（`SIGSTOP`）/ 喚醒（`SIGCONT`） |
| `/` 或 `Ctrl-P` | 開啟命令面板（`spawn` / `merge` / `diff` / `gc` / `sleep` / `wake` / `task` / `copy` / `shutdown`） |
| `Ctrl-K` | Jump-Anywhere：關鍵字快速跳轉至任何 session |
| `a` / `A` / `d` | 允許 / 一律允許 / 拒絕待審批請求 |
| `x` / `K` | 中斷當前回合 / 結束 session |
| `r` | 向 daemon 重新要快照与重整當前視圖 |
| `q` | 離開 TUI（daemon 與 agent 續跑） |
| `Q` | 關閉 daemon：結束所有 agent、移除 socket（等同 `/shutdown`） |

### 7.3 效能策略
- TUI 只持有「渲染所需」的鏡像狀態；逐字稿以 ring buffer（`TRANSCRIPT_CAP`）上限，避免長對話吃記憶體。
- 事件批次渲染（~30–60fps 上限），串流 delta 合併後再重繪。
- 逐字稿 Markdown 渲染結果以 `(session, seq, 寬度, 模式)` 為鍵快取，只有來源或寬度變動時才重算；大量輸出時只取可見視窗。

---

## 8. 工作區與 Git worktree

- 每個「要寫檔的」session 預設配一個 worktree：`git worktree add .termade/wt/<session> -b termade/<label>`。
- 唯讀 session（reviewer）可共用主 checkout（唯讀）。
- daemon 提供：列出 worktree、取 diff、合併回主分支、開 PR（呼叫 `gh` 或 `git`）。
- 衝突處理：以「一個 worktree 一個 branch」為原則，避免同時改同一檔；需要整合時由 coordinator/人類決定。
- 清理：session 結束後 worktree 預設保留（供檢視），可 `/gc` 清除。

---

## 9. 權限、審批與安全

- **統一審批通道**：ACP 的 permission request、Codex 的 approval、Antigravity 的 confirm
  全部轉成 `PermissionRequested` 事件 → TUI 跳出 `[a]llow / [d]eny / [A]lways` 提示。
- **統一權限政策（M1 已實作）**：`PermissionPolicy`（`Ask` / `Auto` / `Bypass`）由 client 指定，
  adapter 翻譯成各家原生機制：

  | 政策 | ACP（OpenCode / Antigravity） | Claude Code | Codex |
  | --- | --- | --- | --- |
  | `Ask` | 轉發 `session/request_permission` | `--permission-mode manual` | `approvalPolicy: untrusted` |
  | `Auto`（預設） | adapter 代核准（`allow_once`） | `--permission-mode acceptEdits` | `approvalPolicy: on-request` |
  | `Bypass` | adapter 代核准（`allow_always`） | `--dangerously-skip-permissions` | `approvalPolicy: never` |
- **Policy engine**（`.termade/policy.toml` 或 `~/.config/termade/policy.toml`；生效範圍與缺口見 T04）：
  - 允許：`Read`、`Grep`、`Glob`、`git status`
  - 詢問：檔案寫入、`Bash`、網路
  - 拒絕：`rm -rf`、`git push --force`、讀取 `~/.ssh`、外洩風險樣式
- **預設最小權限**：Claude 用 `--permission-mode` + `--allowedTools`；Codex 用 sandbox + approval policy；
  對「不受信任的 repo」走 `--bare`（不載入 hooks/plugins/CLAUDE.md）。
- **秘密保護**：`redact_secrets` 目前只套用在寫入事件 log 與 `termade_diff` 的回傳；**其他 MCP 工具的回傳尚未做敏感樣式過濾**（原設計項目，未實作）。
- **審計**：所有事件落地成 jsonl，可事後重建「誰在何時批准了什麼」。

---

## 10. 資源與效能目標

| 項目 | 目標 |
| --- | --- |
| `termade` TUI 冷啟動 | < 150 ms |
| TUI 常駐 RSS | < 20 MB |
| daemon 閒置 RSS（不含 agent） | < 25 MB |
| 事件延遲（agent→TUI） | < 50 ms |
| 支援同時 session 數 | 16+（受 agent 本身資源限制） |
| 前端崩潰 | 不影響任何 agent；重啟後自動復原畫面 |

要點：**我們自己的程式碼保持輕量**；真正耗記憶體的是 agent 子程序（Node/Python），
daemon 可選擇性讓閒置 session 進入休眠（保留狀態、暫停子程序）。

---

## 11. 技術選型（現況）

實際依賴（`Cargo.toml` workspace）：

| 用途 | crate / 做法 | 備註 |
| --- | --- | --- |
| TUI | `ratatui` + `crossterm`、`unicode-width` | Markdown 渲染器自寫（`markdown.rs`） |
| 並行模型 | `std::thread` + `std::sync::mpsc`，blocking IO | 不使用 `tokio`：連線數少、避免 async 傳染 adapter |
| ACP client | 手寫 JSON-RPC（`acp.rs`） | 不用 `agent-client-protocol`：只需子集，且避免跟著 schema 升版 |
| MCP server | 手寫 stdio JSON-RPC（`mcp.rs`） | 不用 `rmcp`：只需 `initialize`／`tools/list`／`tools/call` |
| 序列化 | `serde` / `serde_json` | |
| 本地 socket | `std::os::unix::net::UnixListener` | 遠端為 `std::net::TcpListener` + token |
| PTY fallback | `portable-pty` | |
| git | 以子程序呼叫 `git`（worktree 用 CLI 較穩） | 不用 `git2` |
| CLI | `clap`（derive） | |
| 日誌 | `tracing` + `tracing-subscriber` | |
| id | `uuid` | 時間戳直接用 `SystemTime`，不用 `jiff`／`chrono` |
| 路徑 | 自行解析 `XDG_*` 環境變數 | 不用 `etcetera`／`directories` |
| 錯誤 | `anyhow`／`thiserror` | |

未採用且無對應程式碼：`notify`（見 5 的機制 C）、`tokio`、`rmcp`、`git2`、`jiff`、`etcetera`。

---

## 12. Repo 結構（Cargo workspace）

```
termade-rust/
├─ Cargo.toml                # workspace
├─ crates/
│  ├─ termade-core/              # 領域型別、Event、fold/state、bus、task
│  ├─ termade-protocol/          # control API 與 bus 的 wire 型別
│  ├─ termade-adapter/           # AgentControl trait + 各 agent 實作
│  │   ├─ acp.rs             #   ACP client（opencode / agy）
│  │   ├─ claude.rs          #   Claude stream-json
│  │   ├─ codex.rs           #   Codex app-server
│  │   ├─ stream.rs          #   共用 token→行 串流緩衝（StreamBuf）
│  │   ├─ pty.rs             #   PTY 直通萬用後備
│  │   └─ ansi.rs            #   ANSI 控制碼剝除
│  ├─ termade-daemon/            # supervisor、socket server、worktree（lib.rs）、MCP server（mcp.rs）
│  └─ termade-tui/               # ratatui 前端
│      ├─ lib.rs             #   版面、事件迴圈、視圖切換
│      └─ markdown.rs        #   零依賴 Markdown → Ratatui 行渲染器
├─ src/                      # `termade` binary（tui/daemon/mcp/send/spawn/...）
│  └─ main.rs                #   根目錄 package 入口，支援 `cargo install --path .`
└─ docs/
   ├─ DESIGN.md              # 本文件
   ├─ VERIFICATION.md        # 各里程碑的實機驗證紀錄
   └─ tickets/               # 改善計畫票
```

單一 binary `termade`，子命令分派到各 crate；`termade mcp` 也可獨立當 MCP server 執行檔。

---

## 13. 里程碑

**M0 — 骨架可跑（已完成，見 `crates/`）**
Cargo workspace + `termade-core` 型別 + `termade-protocol` 控制 API + daemon（Unix socket、event log、訊息匯流排）
+ `termade-tui`（attach、即時事件、輸入）+ `termade` CLI；
adapter 實作 **PTY 直通**（對任何 agent 都有效）與 **OpenCode ACP**（原生結構化）。

M0 已驗證：見 [`VERIFICATION.md`](VERIFICATION.md)。

**M1 — 四家 agent 都能跑（原生，零 Node）（已完成，見 `crates/`）**
Claude（`claude -p` stream-json 雙向）、Codex（`codex app-server`）、Antigravity（`agy acp`）。
統一審批通道與權限政策（`PermissionPolicy`）。共用的串流行緩衝抽到 `termade-adapter::stream`。

M1 已驗證：見 [`VERIFICATION.md`](VERIFICATION.md)。

M1 未完成項（受環境限制，程式路徑已就緒）：
- **Codex 回合**：本機無 OpenAI 授權，`turn/start` 回 401；授權後應可直接運作。
- **ACP 逐項審批實測**：受測的 OpenCode / Antigravity 在預設設定下會自行核准工具，未發出
  `session/request_permission`；轉發程式路徑已完成但少有机会實測。
- **Claude 逐項審批**：`-p` 模式不會穩定發出可轉發的審批請求，留待 M2 以 MCP 權限工具處理。

**M2 — 協作核心（已完成，見 `crates/termade-daemon/src/mcp.rs`）**
Message bus + task board + **`termade mcp` server** + `termade` CLI；agent 可 `send/inbox/wait/ask/task/spawn/artifact/status`。
端到端 demo：coordinator 用 `termade_spawn` 產生兩個 worker，各自 worktree，完成後回報並整合。

M2 已驗證：見 [`VERIFICATION.md`](VERIFICATION.md)。

**M3 — 工作區與 review（已完成，見 `crates/termade-daemon` 與 `crates/termade-tui`）**
worktree 管理（`termade worktrees` / `termade gc`）、彩色 diff 視圖（含未追蹤新檔預覽，`termade diff` / TUI `g` 鍵）、行內註解送回 agent（`termade review` / TUI `c` 鍵）、自動 commit 並 merge 回主工作區（`termade merge` / TUI `M` 鍵）/ 開 PR（`termade pr`）。

**M4 — 打磨（已完成，見 `crates/termade-core`、`crates/termade-daemon`、`crates/termade-tui`）**
命令面板（`/` 或 `Ctrl-P`）、Jump-Anywhere（`Ctrl-K`）、成本/用量追蹤（`termade cost` 與 TUI 標題列）、閒置休眠與自動喚醒（`SIGSTOP`/`SIGCONT`，`termade sleep`/`termade wake`/TUI `z` 鍵）、事件重播与增量同步（`termade replay`、`Hello.since`）、Policy 規則引擎（`policy.toml`）與秘密自動遮罩（`redact_secrets`）。

**TUI 檢視區與 Markdown 渲染（已完成，見 `crates/termade-tui/src/markdown.rs`）**
零依賴 Markdown 渲染器（標題／粗體／斜體／刪除線／行內程式碼／連結／清單／引言／分隔線／圍籬程式碼／GFM 表格）、
語意前綴分色（`💭`／`▸`／`💬`）、逐字稿翻頁捲動 + 跟隨最新 + 捲軸 + `t` 原始／渲染切換、
Diff 視圖舊新行號邊欄與 `+N -M` 統計與 `{`/`}` hunk 跳轉、響應式三欄寬度與兩行標題列。

**M5 — 遠端（已完成，見 `crates/termade-daemon` 與 `src/main.rs`）**
daemon 支援 `--listen <host:port> --token <secret>`（TCP + token 驗證），client 支援 `--remote <host:port> --token <secret>`（或 `TERMADE_REMOTE` / `TERMADE_TOKEN` 環境變數）與 SSH attach，讓遠端機器上的 agent 也能被本地 TUI 或 CLI 管理。

M3–M5 已驗證：見 [`VERIFICATION.md`](VERIFICATION.md)。

---

## 14. 風險與開放問題

| 風險 | 說明 / 對策 |
| --- | --- |
| ACP adapter 品質不一 | 以 `probe()` 偵測能力；ACP 不穩就退回原生 headless；adapter 可個別關閉 |
| Antigravity 的程式化介面較新 | 先以 `agy -p` headless 起步，ACP 支援成熟後切換 |
| 各 agent 對 MCP 設定方式不同 | adapter 負責「注入 MCP 設定」；同時保留 CLI 與檔案信箱作為退路 |
| agent 互傳訊息造成無限迴圈 / 成本爆炸 | 速率限制、迴圈偵測、每任務預算、`termade_wait` timeout |
| 多 agent 同時改同檔 | 一 worktree 一 branch；整合交給 coordinator/人 |
| 授權與合規 | 各 agent 用自己的訂閱/金鑰（BYO）；我們不代理也不儲存憑證 |
| TUI 對超長輸出的效能 | 虛擬捲動 + ring buffer + delta 合併 |
| Windows | 先以 Unix socket / PTY 為主；Windows 用 named pipe 與 ConPTY 後補 |

### 已定案的開放問題
1. **主要平台**：先 Linux/macOS（Unix socket、PTY），Windows 之後再補。
2. **daemon 形態**：單一常駐 daemon 管所有 repo。
3. **ACP adapter 是否可接受 Node 依賴**：盡量零 Node，四家都走原生介面。
4. **MCP 還是 CLI 優先**：M2 先做 MCP（`termade mcp`），CLI 與檔案信箱作為退路。
5. **是否要 agent 自主 `termade_spawn`**：M2 開啟，但加上速率限制與迴圈偵測（見風險表）。
6. **預設權限政策**：`Auto`（不中斷流程）；需要逐項審批時用 `Ask`，全開用 `Bypass`。
   M2 再以 MCP 權限工具補上 Claude / Codex 的逐項轉發（**Claude 仍未實作**，Codex 有轉發路徑但未實機驗證）。

---

## 15. 範例流程（端到端）

> 使用者在 TUI 對 coordinator 說：「修正 EU 結帳的郵遞區號驗證，加測試，開 PR。」

1. TUI `spawn` coordinator（Claude Code，role=coordinator，共用主 checkout）。
2. Coordinator 透過 MCP `termade_task_create` 建 2 個任務，`termade_spawn` 出：
   - worker/impl（Codex，`--worktree`，branch `termade/eu-zip`）
   - worker/test（OpenCode，`--worktree`，branch `termade/eu-zip-tests`）
3. 兩個 worker 各自改檔、跑測試；TUI 左側即時顯示狀態，右側顯示 bus 訊息。
4. worker/test 用 `termade_ask(peer="worker/impl", ...)` 問介面細節；impl 用 `termade_send` 回覆。
5. worker/impl 觸發 `Bash` 寫入 → daemon 送 `PermissionRequested` → TUI 跳提示 → 使用者按 `a` 允許。
6. 兩個 worker `termade_task_update(status=done)` 並 `termade_send(to=coordinator, kind=handoff)`。
7. Coordinator 用 `termade_wait` 收到、跑整合測試、`termade_artifact_share(diff)`。
8. 使用者在 TUI 的 Diff 視圖審查、留一行註解 → 送回 coordinator → coordinator 修掉 → `termade merge` / 開 PR。

整個過程：人只按了幾個鍵；agent 之間的溝通、分工、交接全走 bus；所有事件可回溯。

---

## 附錄 A：原始設計與未實作項目

| 項目 | 原設計 | 現況 |
| --- | --- | --- |
| async runtime | `tokio`、`async_trait Adapter` | 未採用；同步 `AgentControl` + std thread（4.4、11） |
| `outbox/` 監看 | daemon 以 `notify` 監看 agent 寫入並轉發 | 未實作；只有 `mailbox/` 單向寫入 |
| MCP 回傳過濾 | 所有 MCP 工具回傳做敏感樣式過濾 | 僅 log 與 `termade_diff` 套用 `redact_secrets` |
| 每任務訊息預算 | token／回合上限 | 未實作；只有速率限制與重複迴圈斷路 |
| Handoff | 連同 worktree 交給另一個 agent | 只有 `handoff` 訊息類型 |
| `tasks.json`、`AGENTS.md` 注入 | 檔案信箱的一部分 | 未實作 |
| Claude 逐項審批 | 轉成 `PermissionRequested` | 未實作（`claude.rs` 不轉發） |
| Codex 回合、ACP 逐項審批 | 實機驗證 | 程式路徑就緒，未實機驗證 |
| `docs/PROTOCOL.md` | control API + MCP 工具規格 | 未撰寫；以 `termade-protocol` 的 `Request`／`Frame` 與 `mcp.rs` 為準 |
