# ade — Agent Development Environment (TUI)

一個以終端機為核心的 **ADE（Agent Development Environment）**：讓多個 coding agent
（OpenCode / Claude Code / Codex / Antigravity）在同一個 repo 上平行工作、彼此溝通、互相協作。

- **TUI 前端**（Rust + Ratatui）：啟動快、記憶體足跡小、SSH 友善，沒有厚重的 GUI。
- **常駐 daemon**：真正持有 agent 程序、訊息匯流排與工作區；TUI 只是可隨時重啟的「顯示器」。
- **互通層**：以各家 agent 的**原生結構化協定**為主（零額外依賴），PTY 直通當萬用後備。

完整設計請見 [`docs/DESIGN.md`](docs/DESIGN.md)。

## 為什麼不是「又一個 GUI IDE」

Orca 這類 ADE 把 worktree、瀏覽器、diff、mobile 全部塞進一個 Electron/原生 app。
我們只要它的**協作核心**，其餘交給終端機與 agent 自己的 CLI：

| 需求 | 本專案的做法 |
| --- | --- |
| 快速啟動、低記憶體 | Rust + Ratatui 前端，常駐成本 < 20 MB；agent 程序才是耗資源的，且可休眠 |
| 不用圖形化功能 | 全部以鍵盤驅動，並提供 `ade` CLI 與控制 socket，讓 agent 也能驅動環境 |
| Agent 互相溝通協作 | daemon 內的 message bus + task board，透過 **MCP** 注入每個 agent |
| 平行不打架 | 每個 session 一個 `git worktree` + branch |
| 支援多種 agent | Adapter 抽象層（ACP / stream-json / app-server / headless / PTY） |

## 現況（M1：四家 agent 原生介面）

四家 agent 都有**原生、零額外依賴**的結構化介面，並已實機驗證：

| Agent | 原生協定 | 啟動方式 | 傳輸標記 | 實機狀態 |
| --- | --- | --- | --- | --- |
| **OpenCode** | ACP | `opencode acp` | `acp` | ✅ 雙向結構化（prompt / 串流 / 工具 / 審批） |
| **Antigravity** | ACP | `agy acp` | `acp` | ✅ 雙向結構化（與 OpenCode 同方言） |
| **Claude Code** | stream-json | `claude -p --input-format stream-json --output-format stream-json --verbose` | `stream-json` | ✅ 雙向結構化（逐字串流、工具、成本） |
| **Codex** | app-server | `codex app-server`（stdio JSON-RPC） | `app-server` | ⚠️ 握手與 thread 已驗證；回合需 OpenAI 授權 |

> 找不到原生協定、或該 agent 未安裝時，`--transport auto` 會**自動退回 PTY 直通**。

已完成並實機驗證：

- **daemon**：Unix domain socket、newline-delimited JSON 控制 API、事件記錄（`events.jsonl`）、
  訊息匯流排、任務板、`git worktree` 建立。
- **adapter**：四家原生 adapter + PTY 萬用後備（輸出會剝除 ANSI 控制碼）。
- **統一權限政策**：`--permission ask|auto|bypass`，對應各 agent 的原生權限機制。
- **TUI**：三欄（Sessions / Transcript / Bus）、輸入模式、審批、中斷/結束。
- **CLI**：`ade spawn/prompt/send/task/sessions/...`，讓 shell 與 agent 也能驅動環境。

尚未實作（見里程碑）：`ade mcp` 協作工具、worktree diff/review 視圖、Claude/Codex 的逐項審批轉發。

### 權限政策

`--permission` 是跨 agent 的統一旋鈕：

| 政策 | OpenCode / Antigravity（ACP） | Claude Code | Codex |
| --- | --- | --- | --- |
| `ask` | 轉發 `session/request_permission` 給 client | `--permission-mode manual` | `approvalPolicy: untrusted` |
| `auto`（預設） | adapter 代為核准（`allow_once`） | `--permission-mode acceptEdits` | `approvalPolicy: on-request` |
| `bypass` | adapter 代為核准（`allow_always`） | `--dangerously-skip-permissions` | `approvalPolicy: never` |

> 註：M1 尚未把 Claude / Codex 的審批請求轉成 TUI 提示（Claude 在 `-p` 模式不會穩定發出可轉發的
> 請求；Codex 需要授權才能走到審批步驟）。ACP 的轉發通道已完成，但受測的 OpenCode / Antigravity
> 在預設設定下會自行核准工具，因此實測少見審批事件。

### M1 已知限制

- **PTY transcript 是盡力而為**：全螢幕 TUI（互動模式）是螢幕導向輸出，我們剝掉 ANSI 控制碼後
  只能還原出零碎文字。要可讀的結構化輸出請用原生協定。
- **`--worktree` 需要 git repo**：非 git 目錄會回報錯誤並中止 spawn。
- **Codex 需 OpenAI 授權**：`codex app-server` 的 `initialize` / `thread/start` 免授權即可完成，
  但送出回合會回 `401 Unauthorized`；授權後即可正常使用。
- **Claude 取消是盡力而為**：Claude 沒有公開的取消 API，`cancel` 送 `control_request: interrupt`，
  不保證立即生效。

## 建置與使用

```bash
cargo build --release          # 產出單一執行檔 target/release/ade
cargo test                     # 19 個單元測試

./target/release/ade           # 啟動 TUI（會自動確保 daemon 在跑）
./target/release/ade daemon    # 或前景跑 daemon
```

第一次使用：在 TUI 按 `n` 建立 session，輸入 `opencode 幫我把測試修好`。

### TUI 按鍵

| 鍵 | 動作 |
| --- | --- |
| `j` / `k`、↑ / ↓ | 選擇 session |
| `i` / `Enter` | 對選取 session 送 prompt |
| `n` | 建立 session：`<agent> <prompt>`（`Ctrl-W` 切換是否開 worktree） |
| `m` | 送訊息到匯流排（agent 之間溝通） |
| `a` / `d` | 允許 / 拒絕待審批請求 |
| `x` / `K` | 中斷當前回合 / 結束 session |
| `r` | 向 daemon 重新要快照 |
| `q` | 離開 TUI（daemon 與 agent 續跑） |

### CLI

```bash
ade sessions [--json]                       # 列出 sessions（含 transport）
ade spawn --kind claude --permission auto --label worker/a --worktree --prompt "..."
ade prompt <session-id> "再跑一次測試"
ade send --to <session-id> "請先跑測試"     # 或 --channel review / --broadcast
ade task "修正 EU 郵遞區號驗證" --detail "..."
ade tasks
ade cancel <session-id> ; ade kill <session-id>
ade shutdown
```

- `--kind`：`opencode` | `claude` | `codex` | `antigravity`（別名 `agy` / `gemini`）。
- `--transport`：`auto`（預設）/ `structured`（別名 `acp`）/ `pty`。
- `--permission`：`ask` | `auto`（預設）| `bypass`。

### 環境變數

- `XDG_RUNTIME_DIR`：socket 位置（預設 `$XDG_RUNTIME_DIR/ade/daemon.sock`，否則 `/tmp/ade-$USER/`）。
- `XDG_STATE_HOME`：事件記錄位置（預設 `~/.local/state/ade/events.jsonl`）。
- `ADE_LOG`：tracing 過濾（例如 `ADE_LOG=debug`）。
