# ade — 設計文件

> 目標：用 **Rust + Ratatui** 做一個終端機原生的 ADE（Agent Development Environment）。
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

---

## 0. 一句話定義

`ade` 是一個**常駐 daemon + 可拋棄式 TUI 前端**的系統：

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
GUI ADE 常見問題是「關掉視窗 = 殺掉 agent」。我們把狀態放在 daemon：

- agent 程序的生命週期與 TUI 解耦 → 可以關 TUI、之後再 attach。
- 多個 client（TUI、`ade` CLI、未來的 web）可同時看同一份狀態。
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
                          │              ade (TUI client)                 │
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
              ▲  每個 agent 都可透過 ade MCP tools 或 `ade` CLI 呼叫 daemon
```

**單一執行檔、多種模式**（啟動快、部署簡單）：

```
ade                 # 等同 ade tui：確保 daemon 在跑 → attach
ade daemon          # 前景執行 daemon（--detach 可背景化）
ade tui             # 純前端
ade mcp             # 以 stdio 跑 MCP server（供 agent 子程序當 MCP server 用）
ade send/recv/tasks # CLI，給 shell / agent 用
ade spawn ...       # 建立 session（可指定 agent、worktree、prompt）
```

daemon 的 socket 放 `$XDG_RUNTIME_DIR/ade/daemon.sock`（或 `~/.local/state/ade/`），
`ade tui` 找不到就自動 spawn 一個 daemon（類似 `opencode` 的行為）。

---

## 4. 核心抽象

以 `ade-core` 定義，所有 adapter 與前端共用。

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
```rust
enum Event {
    SessionStarted { id, kind, cwd },
    SessionStatusChanged { id, status },
    MessageDelta { id, text },                 // 串流輸出
    ToolCallStarted { id, name, input },
    ToolCallFinished { id, name, output, ok },
    PermissionRequested { id, req },
    PermissionResolved { id, decision },
    BusMessage { from, to: Target, body },     // agent 之間的訊息
    TaskChanged { task },
    Artifact { id, path, kind },               // 產出的檔案/分支/diff
    SessionExited { id, code },
    CostUpdated { id, cost },
}
```
所有狀態都是 fold(Event) 的結果 → 保證 TUI 重連時能重建畫面。

### 4.4 `Adapter` trait（每個 agent 一種實作）
```rust
#[async_trait]
trait Adapter: Send + Sync {
    fn kind(&self) -> AgentKind;
    /// 探測環境：binary 在不在、版本、是否支援 ACP
    async fn probe(&self) -> ProbeResult;

    /// 啟動一個 session，回傳一個事件流 + 控制把手
    async fn start(&self, spec: SessionSpec) -> Result<AgentHandle>;

    /// 送一段 prompt / 使用者輸入
    async fn prompt(&self, h: &AgentHandle, text: &str) -> Result<()>;

    /// 回應 permission request
    async fn resolve_permission(&self, h: &AgentHandle, id: ReqId, d: Decision) -> Result<()>;

    async fn cancel(&self, h: &AgentHandle) -> Result<()>;
}

struct AgentHandle {
    id: SessionId,
    events: mpsc::Receiver<AgentEvent>,   // adapter 正規化後的事件
    ctl: AdapterCtl,                      // prompt / cancel / permission / mcp 設定
}
```

`Adapter` 把各家協定（ACP、stream-json、app-server…）**正規化成同一組 `AgentEvent`**，
daemon 之上完全不需要知道底下是哪一家。

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
daemon 開一個 MCP server（`ade mcp`）。spawn agent 時，透過該 agent 自己的 MCP 設定把它接上去：

- Claude Code：`--mcp-config <ade.json>`
- Codex：`~/.codex/config.toml` 的 `[mcp_servers.ade]`（或在 session 建立時帶入）
- OpenCode：`opencode.json` 的 `mcp` 欄位（ACP `session/new` 也接受 MCP servers）
- Antigravity：`~/.gemini/antigravity-cli/settings.json` 的 MCP 設定

對 agent 曝露的工具（語意為「協作原語」）：

| MCP tool | 用途 |
| --- | --- |
| `ade_whoami()` | 回傳自己的 session id、role、worktree、branch |
| `ade_peers()` | 列出其他 session（id、role、status、正在做什麼） |
| `ade_send(to, body, kind?)` | 傳訊息給某個 session / channel / broadcast |
| `ade_inbox()` | 讀取尚未處理的訊息 |
| `ade_wait(timeout_ms)` | 阻塞等待下一則訊息（協作交握用） |
| `ade_ask(peer, question, timeout_ms)` | 送出問題並等回覆（request/response） |
| `ade_task_list()` / `ade_task_create()` / `ade_task_update()` | 共用任務看板 |
| `ade_spawn(kind?, prompt, worktree?, role?)` | **agent 產生子 agent**（協調者 → 工作者） |
| `ade_artifact_share(path\|branch\|diff)` | 分享產出（檔案、分支、diff）給他人或看板 |
| `ade_status(text)` | 更新自己在 TUI 上的狀態字串（輕量進度回報） |

> 這是「agent 驅動 orchestration」的關鍵：**協調者 agent 可以直接 `ade_spawn` 出 worker**，
> 而 worker 的審批、狀態、訊息全部回到 TUI，人類可隨時介入。

### 機制 B：`ade` CLI（給 shell / 不支援 MCP 的 agent）
同一批能力也有 CLI 形式，agent 只要會跑 shell 就能用：

```bash
ade whoami
ade send --to worker/checkout "請先跑測試再交棒"
ade inbox --json
ade task create "修正 EU 郵遞區號驗證"
ade spawn --kind codex --worktree --role worker "實作 X，完成後 ade send --to coordinator done"
```

CLI 內部直接打 daemon 的 control socket，零額外成本。

### 機制 C（最低共同標準）：檔案信箱
對於不支援 MCP 又不方便跑 CLI 的 agent，daemon 在每個 worktree 放：

```
.ade/
  mailbox/<session>.jsonl   # 收到的訊息
  tasks.json                # 共用任務快照
  AGENTS.md 片段            # 指示 agent「定期讀 mailbox、用檔案寫回覆」
  outbox/<session>.jsonl    # agent 寫入 → daemon 監看後轉發到 bus
```

搭配 system prompt 注入，agent 用自己的檔案工具讀寫即可參與（`notify` 監看 outbox）。

### 協作模式（由 config 定義 role）
- **Hub-and-spoke**：Coordinator 收需求 → 拆任務 → `ade_spawn` 多個 worker（各自 worktree）→ worker 回報 → Coordinator 整合。
- **Peer-to-peer**：實作者 ↔ 審查者直接對話（`ade_ask` / `ade_send`），人類只旁觀。
- **Channels**：`#build`、`#review`、`#alerts` 廣播。
- **Handoff**：一個 agent 把 session 連同 worktree 交給另一個（換模型/換工具續做）。

### 防呆（避免 agent 無限互傳）
- 每對 session 的訊息速率限制 + 迴圈偵測（相同 body 連續出現 N 次即斷路）。
- 每個 task 的訊息預算（token/回合上限）。
- `ade_wait` 有 timeout，預設不會永久阻塞。

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
事件以 `seq` 單調遞增，log 落地為 `.ade/log/events.jsonl`（可選 rotate / 上限）。

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
  - ade 的語意前綴分色：`💭`（思考，暗色斜體）、`▸`（工具呼叫，琥珀色）、`💬`（審查註解，洋紅）。
  - 支援翻頁捲動與「跟隨最新」（follow-tail），右側顯示捲軸；`t` 可在渲染後與原始文字間切換。
  - Diff 視圖每行附舊／新檔行號邊欄，標題顯示 `+N -M` 統計，`{` / `}` 可跳 hunk。
- **右**：bus 活動流 + 任務看板（可切 tab）。
- **底**：輸入框。`@session` 指定對象、`#channel` 廣播、`/` 開命令面板（`/spawn`、`/merge`、`/task`）。
- **標題列**：兩行——上排為視圖頁籤與 session/task 數，下排為成本、token 用量與 `md`／`follow` 狀態指示。
- **Diff 視圖**：每個 worktree 的 `git diff`，可對行下註解並送回該 agent（借用 Antigravity/Orca 的 review 迴圈概念）。

### 7.2 關鍵按鍵
| 鍵 | 動作 |
| --- | --- |
| `Ctrl-P` / `Ctrl-K` | 命令面板 / 跳轉任何 session（jump anywhere） |
| `n` | 新 session（精靈：agent、worktree、role、prompt） |
| `Tab` / `g` | 切換 pane / 視圖 |
| `t` | 切換逐字稿顯示（Markdown 渲染 ↔ 原始文字） |
| `PgUp` / `PgDn`（`Ctrl-U` / `Ctrl-D`） | 捲動逐字稿 / diff（捲動即暫停跟隨） |
| `Home` / `End` | 跳到最前 / 回到跟隨最新 |
| `{` / `}` | （Diff）上一個 / 下一個 hunk |
| `Enter` | 送 prompt 給當前 session |
| `a` / `d` | 對待審批按 allow / deny |
| `m` | 開 bus 訊息面板（寄訊息給任一 session/channel） |
| `g` | git/worktree 面板（diff、merge、PR） |
| `x` | 取消當前 session 的回合（cancel） |
| `q` | 離開 TUI（**不**殺 daemon） |

### 7.3 效能策略
- TUI 只持有「渲染所需」的鏡像狀態；逐字稿以 ring buffer（`TRANSCRIPT_CAP`）上限，避免長對話吃記憶體。
- 事件批次渲染（~30–60fps 上限），串流 delta 合併後再重繪。
- 逐字稿 Markdown 渲染結果以 `(session, seq, 寬度, 模式)` 為鍵快取，只有來源或寬度變動時才重算；大量輸出時只取可見視窗。

---

## 8. 工作區與 Git worktree

- 每個「要寫檔的」session 預設配一個 worktree：`git worktree add .ade/wt/<session> -b ade/<label>`。
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
- **Policy engine**（`~/.config/ade/policy.toml`，M4）：
  - 允許：`Read`、`Grep`、`Glob`、`git status`
  - 詢問：檔案寫入、`Bash`、網路
  - 拒絕：`rm -rf`、`git push --force`、讀取 `~/.ssh`、外洩風險樣式
- **預設最小權限**：Claude 用 `--permission-mode` + `--allowedTools`；Codex 用 sandbox + approval policy；
  對「不受信任的 repo」走 `--bare`（不載入 hooks/plugins/CLAUDE.md）。
- **秘密保護**：事件 log 對憑證樣式遮罩；MCP 工具回傳內容做敏感樣式過濾。
- **審計**：所有事件落地成 jsonl，可事後重建「誰在何時批准了什麼」。

---

## 10. 資源與效能目標

| 項目 | 目標 |
| --- | --- |
| `ade` TUI 冷啟動 | < 150 ms |
| TUI 常駐 RSS | < 20 MB |
| daemon 閒置 RSS（不含 agent） | < 25 MB |
| 事件延遲（agent→TUI） | < 50 ms |
| 支援同時 session 數 | 16+（受 agent 本身資源限制） |
| 前端崩潰 | 不影響任何 agent；重啟後自動復原畫面 |

要點：**我們自己的程式碼保持輕量**；真正耗記憶體的是 agent 子程序（Node/Python），
daemon 可選擇性讓閒置 session 進入休眠（保留狀態、暫停子程序）。

---

## 11. 技術選型（crates）

| 用途 | crate |
| --- | --- |
| TUI | `ratatui` + `crossterm` |
| async runtime | `tokio` |
| ACP client | `agent-client-protocol`（runtime）/ `agent-client-protocol-schema` |
| MCP server | `rmcp`（官方 Rust MCP SDK）或手寫 stdio JSON-RPC |
| 序列化 | `serde` / `serde_json` |
| 本地 socket | `tokio::net::UnixListener`（或 `interprocess` 跨平台） |
| PTY fallback | `portable-pty`（wezterm）或 `nix`/`tokio::process` |
| git | `git2`（或直接 shell out `git`；worktree 用 CLI 較穩） |
| CLI | `clap` |
| 設定路徑 | `etcetera` / `directories` |
| 日誌 | `tracing` + `tracing-subscriber` |
| 檔案監看（mailbox） | `notify` |
| id / 時間 | `uuid` / `jiff`（或 `chrono`） |
| 錯誤 | `anyhow`（應用層）/ `thiserror`（library 層） |

---

## 12. Repo 結構（Cargo workspace）

```
ade-rust/
├─ Cargo.toml                # workspace
├─ crates/
│  ├─ ade-core/              # 領域型別、Event、fold/state、bus、task
│  ├─ ade-protocol/          # control API 與 bus 的 wire 型別
│  ├─ ade-adapter/           # Adapter trait + 各 agent 實作
│  │   ├─ acp.rs             #   ACP client（opencode / agy）
│  │   ├─ claude.rs          #   Claude stream-json
│  │   ├─ codex.rs           #   Codex app-server
│  │   ├─ stream.rs          #   共用 token→行 串流緩衝（StreamBuf）
│  │   ├─ pty.rs             #   PTY 直通萬用後備
│  │   └─ ansi.rs            #   ANSI 控制碼剝除
│  ├─ ade-daemon/            # supervisor、socket server、MCP server、worktree
│  ├─ ade-tui/               # ratatui 前端
│  │   ├─ lib.rs             #   三欄版面、事件迴圈、視圖切換
│  │   └─ markdown.rs        #   零依賴 Markdown → Ratatui 行渲染器
│  └─ ade-cli/               # `ade` binary（tui/daemon/mcp/send/spawn/...）
└─ docs/
   ├─ DESIGN.md              # 本文件
   └─ PROTOCOL.md            # control API + MCP tools 規格（後續）
```

單一 binary `ade`，子命令分派到各 crate；`ade mcp` 也可獨立當 MCP server 執行檔。

---

## 13. 里程碑

**M0 — 骨架可跑（已完成，見 `crates/`）**
Cargo workspace + `ade-core` 型別 + `ade-protocol` 控制 API + daemon（Unix socket、event log、訊息匯流排）
+ `ade-tui`（attach、即時事件、輸入）+ `ade` CLI；
adapter 實作 **PTY 直通**（對任何 agent 都有效）與 **OpenCode ACP**（原生結構化）。

M0 已驗證（實機）：

| 驗證項 | 指令 | 結果 |
| --- | --- | --- |
| daemon 常駐 + socket | `ade daemon` / `ade sessions` | OK |
| OpenCode ACP 結構化 | `ade spawn --kind opencode --prompt "..." --transport acp` | transcript 收到 `hello from ade` |
| PTY 萬用後備 | `ade spawn --kind opencode --transport pty` | 逐段剝除 ANSI 後可讀 |
| 訊息匯流排 | `ade send --to <id>` / `--channel review` | 訊息進 `State.messages` |
| 任務板 | `ade task "..."` / `ade tasks` | 任務進 `State.tasks` |
| TUI attach | `ade` | 三欄（Sessions / Transcript / Bus）即時更新、`q` 離線不影響 agent |
| 中斷 / 結束 | `ade cancel <id>` / `ade kill <id>` | ACP `session/cancel`、程序 kill；狀態轉 `exited` |
| 關閉 | `ade shutdown` | 先結束所有 agent 子程序、移除 socket 再離開 |
| 測試 | `cargo test` | 14 passed |

**M1 — 四家 agent 都能跑（原生，零 Node）（已完成，見 `crates/`）**
Claude（`claude -p` stream-json 雙向）、Codex（`codex app-server`）、Antigravity（`agy acp`）。
統一審批通道與權限政策（`PermissionPolicy`）。共用的串流行緩衝抽到 `ade-adapter::stream`。

M1 已驗證（實機）：

| 驗證項 | 指令 | 結果 |
| --- | --- | --- |
| OpenCode ACP | `ade spawn --kind opencode --prompt "..."` | 收到 `ade-ok-opencode`，狀態 busy→idle |
| Antigravity ACP | `ade spawn --kind agy --prompt "..."` | 收到 `ade-ok-agy`（與 OpenCode 同 ACP 方言） |
| Antigravity 工具 | `ade spawn --kind agy --prompt "建立 agy-write.txt"` | 檔案寫入成功、transcript 可讀 |
| Claude stream-json | `ade spawn --kind claude --prompt "..."` | 收到 `ade-ok-claude`、`transport=stream-json`、成本解析正確 |
| Claude 工具 | `ade spawn --kind claude --permission bypass --prompt "跑 echo"` | ToolCall `Bash` + tool_result `ade-tool-ok` |
| Codex app-server 握手 | `ade spawn --kind codex --prompt "..."` | `initialize` + `thread/start` 成功、`transport=app-server`；回合因無授權回 401 |
| 權限政策 | `--permission ask/auto/bypass` | ACP 由 adapter 代核准；Claude 對應 `--permission-mode` / `--dangerously-skip-permissions` |
| transport 別名 | `--transport acp` / `structured` / `pty` | `acp` 與 `structured` 等價（serde alias） |
| 測試 / lint | `cargo test` / `cargo clippy --all-targets` | 19 passed / 無警告 |

M1 未完成項（受環境限制，程式路徑已就緒）：
- **Codex 回合**：本機無 OpenAI 授權，`turn/start` 回 401；授權後應可直接運作。
- **ACP 逐項審批實測**：受測的 OpenCode / Antigravity 在預設設定下會自行核准工具，未發出
  `session/request_permission`；轉發程式路徑已完成但少有机会實測。
- **Claude 逐項審批**：`-p` 模式不會穩定發出可轉發的審批請求，留待 M2 以 MCP 權限工具處理。

**M2 — 協作核心（已完成，見 `crates/ade-daemon/src/mcp.rs`）**
Message bus + task board + **`ade mcp` server** + `ade` CLI；agent 可 `send/inbox/wait/ask/task/spawn/artifact/status`。
端到端 demo：coordinator 用 `ade_spawn` 產生兩個 worker，各自 worktree，完成後回報並整合。

M2 已驗證（實機）：

| 驗證項 | 指令 / 工具 | 結果 |
| --- | --- | --- |
| MCP stdio 協定 | `ade mcp --session <id>` | `initialize`、`tools/list`（12 個 `ade_*` 工具）、`tools/call` 正常 |
| 自動注入 MCP | `ade spawn --kind claude/opencode/agy` | ACP 透過 `session/new.mcpServers`、Claude 透過 `--mcp-config`、Codex 透過 `-c mcp_servers.ade.*` 自動掛載 |
| 收件匣與等待 | `ade inbox` / `ade wait` / `ade ask` | 以 label 或 session id 定址、游標去重、阻塞等待與 request/response 交握正常 |
| 閒置喚醒 | 對 `Idle` session 送 `Question` / `Handoff` | daemon 自動將匯流排訊息轉成 prompt 喚醒對方處理 |
| 防呆斷路 | `BusGuard` | 連續重複訊息達上限（3 次）觸發迴圈斷路；每分鐘超過 30 則觸發速率限制 |
| 端到端多 Agent 協作 | `coord` 呼叫 `ade_task_create` + 2×`ade_spawn(worktree=true)` + 2×`ade_wait` | `worker/a` 與 `worker/b` 各自於獨立 worktree 寫檔並 `ade_send(kind=handoff)` 回報，`coord` 收齊後將兩任務標為 `done` 並更新 `status_note` |
| 測試 / lint | `cargo test` / `cargo clippy --all-targets -- -D warnings` | 22 passed / 無警告 |

**M3 — 工作區與 review（已完成，見 `crates/ade-daemon` 與 `crates/ade-tui`）**
worktree 管理（`ade worktrees` / `ade gc`）、彩色 diff 視圖（含未追蹤新檔預覽，`ade diff` / TUI `g` 鍵）、行內註解送回 agent（`ade review` / TUI `c` 鍵）、自動 commit 並 merge 回主工作區（`ade merge` / TUI `M` 鍵）/ 開 PR（`ade pr`）。

**M4 — 打磨（已完成，見 `crates/ade-core`、`crates/ade-daemon`、`crates/ade-tui`）**
命令面板（`/` 或 `Ctrl-P`）、Jump-Anywhere（`Ctrl-K`）、成本/用量追蹤（`ade cost` 與 TUI 標題列）、閒置休眠與自動喚醒（`SIGSTOP`/`SIGCONT`，`ade sleep`/`ade wake`/TUI `z` 鍵）、事件重播与增量同步（`ade replay`、`Hello.since`）、Policy 規則引擎（`policy.toml`）與秘密自動遮罩（`redact_secrets`）。

**TUI 檢視區與 Markdown 渲染（已完成，見 `crates/ade-tui/src/markdown.rs`）**
零依賴 Markdown 渲染器（標題／粗體／斜體／刪除線／行內程式碼／連結／清單／引言／分隔線／圍籬程式碼／GFM 表格）、
語意前綴分色（`💭`／`▸`／`💬`）、逐字稿翻頁捲動 + 跟隨最新 + 捲軸 + `t` 原始／渲染切換、
Diff 視圖舊新行號邊欄與 `+N -M` 統計與 `{`/`}` hunk 跳轉、響應式三欄寬度與兩行標題列。

**M5 — 遠端（已完成，見 `crates/ade-daemon` 與 `crates/ade-cli`）**
daemon 支援 `--listen <host:port> --token <secret>`（TCP + token 驗證），client 支援 `--remote <host:port> --token <secret>`（或 `ADE_REMOTE` / `ADE_TOKEN` 環境變數）與 SSH attach，讓遠端機器上的 agent 也能被本地 TUI 或 CLI 管理。

M3–M5 已驗證（實機）：

| 驗證項 | 指令 / 功能 | 結果 |
| --- | --- | --- |
| Worktree 清單與 Diff | `ade worktrees` / `ade diff worker/feat` | 正確列出變更檔案數與統一格式 diff（含新增未追蹤檔案） |
| Worktree 自動 Commit + Merge | `ade merge worker/feat` | 自動將 worktree 內未 commit 的變更打包 commit 並 merge 回主分支 |
| Worktree 清理 | `ade gc` | 自動移除已結束 session 的 `.ade/wt/<id>` 與 `ade/<id>` 分支 |
| 行內審查註解（Review 迴圈） | `ade review <session> --location ...` / TUI `c` | 記錄 `ReviewCommented` 事件並向該 agent 送出定位修正 prompt |
| 閒置休眠與喚醒 | `ade sleep <id>` / `ade wake <id>` | 送出 `SIGSTOP` / `SIGCONT`，狀態切換 `sleeping` ↔ `idle`；收到新 prompt 時自動喚醒 |
| 成本統計 | `ade cost` | 逐 session 與總計顯示 USD 與 input/output tokens |
| 事件重播與秘密遮罩 | `ade replay` | 完整重播 `events.jsonl`；`sk-...` / `ghp_...` 自動遮罩為 `<REDACTED>` |
| 遠端 TCP + Token 連線 | `ade daemon --listen 127.0.0.1:17429 --token ...` | 透過 `--remote` + `--token` 建立任務與查詢狀態成功 |
| TUI Markdown 渲染與捲動 | `cargo test -p ade-tui`（TestBackend 全畫面渲染） | 標題／粗體／程式碼／表格正確渲染、`t` 切換原始文字、Diff 邊欄與 `+N -M` 統計、翻頁後恢復跟隨 |
| 測試 / lint | `cargo test` / `cargo clippy --all-targets -- -D warnings` | 32 passed / 無警告 |

---

## 14. 風險與開放問題

| 風險 | 說明 / 對策 |
| --- | --- |
| ACP adapter 品質不一 | 以 `probe()` 偵測能力；ACP 不穩就退回原生 headless；adapter 可個別關閉 |
| Antigravity 的程式化介面較新 | 先以 `agy -p` headless 起步，ACP 支援成熟後切換 |
| 各 agent 對 MCP 設定方式不同 | adapter 負責「注入 MCP 設定」；同時保留 CLI 與檔案信箱作為退路 |
| agent 互傳訊息造成無限迴圈 / 成本爆炸 | 速率限制、迴圈偵測、每任務預算、`ade_wait` timeout |
| 多 agent 同時改同檔 | 一 worktree 一 branch；整合交給 coordinator/人 |
| 授權與合規 | 各 agent 用自己的訂閱/金鑰（BYO）；我們不代理也不儲存憑證 |
| TUI 對超長輸出的效能 | 虛擬捲動 + ring buffer + delta 合併 |
| Windows | 先以 Unix socket / PTY 為主；Windows 用 named pipe 與 ConPTY 後補 |

### 已定案的開放問題
1. **主要平台**：先 Linux/macOS（Unix socket、PTY），Windows 之後再補。
2. **daemon 形態**：單一常駐 daemon 管所有 repo。
3. **ACP adapter 是否可接受 Node 依賴**：盡量零 Node，四家都走原生介面。
4. **MCP 還是 CLI 優先**：M2 先做 MCP（`ade mcp`），CLI 與檔案信箱作為退路。
5. **是否要 agent 自主 `ade_spawn`**：M2 開啟，但加上速率限制與迴圈偵測（見風險表）。
6. **預設權限政策**：`Auto`（不中斷流程）；需要逐項審批時用 `Ask`，全開用 `Bypass`。
   M2 再以 MCP 權限工具補上 Claude / Codex 的逐項轉發。

---

## 15. 範例流程（端到端）

> 使用者在 TUI 對 coordinator 說：「修正 EU 結帳的郵遞區號驗證，加測試，開 PR。」

1. TUI `spawn` coordinator（Claude Code，role=coordinator，共用主 checkout）。
2. Coordinator 透過 MCP `ade_task_create` 建 2 個任務，`ade_spawn` 出：
   - worker/impl（Codex，`--worktree`，branch `ade/eu-zip`）
   - worker/test（OpenCode，`--worktree`，branch `ade/eu-zip-tests`）
3. 兩個 worker 各自改檔、跑測試；TUI 左側即時顯示狀態，右側顯示 bus 訊息。
4. worker/test 用 `ade_ask(peer="worker/impl", ...)` 問介面細節；impl 用 `ade_send` 回覆。
5. worker/impl 觸發 `Bash` 寫入 → daemon 送 `PermissionRequested` → TUI 跳提示 → 使用者按 `a` 允許。
6. 兩個 worker `ade_task_update(status=done)` 並 `ade_send(to=coordinator, kind=handoff)`。
7. Coordinator 用 `ade_wait` 收到、跑整合測試、`ade_artifact_share(diff)`。
8. 使用者在 TUI 的 Diff 視圖審查、留一行註解 → 送回 coordinator → coordinator 修掉 → `ade merge` / 開 PR。

整個過程：人只按了幾個鍵；agent 之間的溝通、分工、交接全走 bus；所有事件可回溯。
