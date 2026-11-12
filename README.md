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

## 現況（M0–M5 全里程碑已完成並實機驗證）

四家 agent 都有**原生、零額外依賴**的結構化介面，並已實機驗證：

| Agent | 原生協定 | 啟動方式 | 傳輸標記 | 實機狀態 |
| --- | --- | --- | --- | --- |
| **OpenCode** | ACP | `opencode acp` | `acp` | ✅ 雙向結構化（prompt / 串流 / 工具 / 審批 / MCP 注入） |
| **Antigravity** | ACP | `agy acp` | `acp` | ✅ 雙向結構化（與 OpenCode 同方言 / MCP 注入） |
| **Claude Code** | stream-json | `claude -p --input-format stream-json --output-format stream-json --verbose` | `stream-json` | ✅ 雙向結構化（逐字串流、工具、成本、MCP 注入） |
| **Codex** | app-server | `codex app-server`（stdio JSON-RPC） | `app-server` | ⚠️ 握手與 thread 已驗證；回合需 OpenAI 授權 |

> 找不到原生協定、或該 agent 未安裝時，`--transport auto` 會**自動退回 PTY 直通**。

已完成並實機驗證（M0–M5）：

- **daemon（M0/M2/M4/M5）**：Unix domain socket + 選配 TCP 遠端監聽（`--listen host:port --token secret`）、
  增量事件同步（`since`）、事件重播（`ade replay`）與秘密遮罩（`sk-...` / `ghp_...` 自動轉 `<REDACTED>`）、
  訊息匯流排、收件匣游標與阻塞等待（`BusInbox` / `BusWait`）、防呆斷路器（速率限制 + 連續重複迴圈偵測）、任務板。
- **MCP 協作伺服器（`ade mcp`，M2/M3）**：spawn session 時自動注入每個 agent，提供 14 個協作工具
  （`ade_whoami`、`ade_peers`、`ade_send`、`ade_inbox`、`ade_wait`、`ade_ask`、
  `ade_task_list`、`ade_task_create`、`ade_task_update`、`ade_spawn`、`ade_artifact_share`、`ade_status`、`ade_diff`、`ade_merge`）。
- **工作區與 Review 迴圈（M3）**：每個 worktree 獨立 branch（`ade/<session>`）、TUI 即時彩色 Diff 視圖、
  對行下審查註解送回 agent 修改（`ade review` / TUI `c` 鍵）、自動 commit 並合併回主分支（`ade merge` / TUI `M` 鍵）、PR 建立（`ade pr`）與已結束 worktree 清理（`ade gc` / TUI `G` 鍵）。
- **打磨與資源控制（M4）**：命令面板（`/` 或 `Ctrl-P`）、Jump-Anywhere（`Ctrl-K`）、即時成本與 token 統計（`ade cost`）、
  閒置休眠與自動喚醒（`SIGSTOP` / `SIGCONT`、`ade sleep` / `ade wake` / TUI `z` 鍵）、權限規則引擎（`.ade/policy.toml` 或 `~/.config/ade/policy.toml`）。
- **TUI**：三種中央視圖（Overview / Diff / Worktrees）、支援 `@peer` 與 `#channel` 智慧路由輸入框。
  逐字稿會即時渲染 Markdown（標題、粗體／斜體、行內程式碼、清單、引言、分隔線、圍籬程式碼區塊與 GFM 表格），
  並可用 `t` 在渲染後與原始文字之間切換；輸出可翻頁捲動並自動跟隨最新。
- **檔案信箱後備**：自動同步寫入 `<cwd>/.ade/mailbox/<session>.jsonl`。

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
| `j` / `k`、↑ / ↓ | 選擇 session（在 Diff 視圖則為移動 diff 行游標） |
| `[` / `]` | 在任何視圖切換上一個 / 下一個 session |
| `Tab` / `g` | 切換中央視圖（Overview ↔ Diff ↔ Worktrees） |
| `t` | 切換逐字稿顯示：Markdown 渲染 ↔ 原始文字（方便複製） |
| `PgUp` / `PgDn`（或 `Ctrl-U` / `Ctrl-D`） | 逐字稿 / diff 翻頁捲動（捲動後暫停跟隨，狀態列會顯示 `⏸ paused`） |
| `Home` / `End` | 跳到最前 / 回到跟隨最新輸出（`▼ follow`） |
| `{` / `}` | （Diff 視圖）跳到上一個 / 下一個 hunk（`@@`） |
| `i` / `Enter` | 對選取 session 送 prompt（支援 `@peer 訊息`、`#channel 訊息`、`/命令`） |
| `n` | 建立 session：`<agent> <prompt>`（`Ctrl-W` 切換是否開 worktree） |
| `m` | 送訊息到匯流排（agent 之間溝通） |
| `c` | （Diff 視圖）對目前選取的 diff 行下審查註解並送回 agent 修改 |
| `M` / `G` | 合併選取 session 的 worktree 分支回主工作區 / 清理已結束 worktree |
| `z` | 切換選取 session 的休眠（`SIGSTOP`）/ 喚醒（`SIGCONT`） |
| `/` 或 `Ctrl-P` | 開啟命令面板（`spawn` / `merge` / `diff` / `gc` / `sleep` / `wake` / `task` / `shutdown`） |
| `Ctrl-K` | Jump-Anywhere：關鍵字快速跳轉至任何 session |
| `a` / `A` / `d` | 允許 / 一律允許 / 拒絕待審批請求 |
| `x` / `K` | 中斷當前回合 / 結束 session |
| `r` | 向 daemon 重新要快照与重整當前視圖 |
| `q` | 離開 TUI（daemon 與 agent 續跑） |
| `Q` | 關閉 daemon：結束所有 agent、移除 socket（等同 `/shutdown`） |

### CLI

```bash
ade sessions [--json]                       # 列出 sessions（別名 ade peers）
ade whoami [--session <id>] [--json]        # 查詢當前 session 資訊
ade spawn --kind claude --permission auto --label worker/a --worktree --prompt "..."
ade prompt <session-or-label> "再跑一次測試"
ade send --to <session-or-label> "請先跑測試" # 或 --channel review / --broadcast / --kind handoff
ade inbox [--session <id>] [--all] [--json] # 讀取未讀訊息並推進游標
ade wait [--from <peer>] [--timeout-ms 30000]
ade ask <peer> "請問驗證函式的簽名？"        # 送出問題並阻塞等待回覆
ade task "修正 EU 郵遞區號驗證" --detail "..." --assignee worker/a
ade task-update <task-id> --status done
ade tasks [--json]
ade status "正在跑整合測試"
ade artifact src/zip.rs --kind file --note "新增驗證模組"
ade worktrees [--json]                      # 列出所有受管 git worktrees
ade diff <session-or-label>                 # 查看該 session 工作區的 git diff
ade review <session-or-label> --location src/lib.rs:42 "請補上空字串檢查"
ade merge <session-or-label>                # 將該 session 的 worktree 分支合併回主工作區
ade pr <session-or-label> --title "..."     # 透過 gh 建立 Pull Request
ade gc [--session <id>]                     # 清理已結束 session 的 worktrees
ade sleep <session> ; ade wake <session>    # 暫停 / 喚醒閒置 agent 子程序
ade cost [--json]                           # 查看各 session 與總計的成本與 token 用量
ade replay [--since <seq>] [--json]         # 重播與審計 events.jsonl
ade daemon --listen 0.0.0.0:7420 --token <secret> # 啟動含遠端 TCP 監聽的 daemon
ade --remote <host:port> --token <secret> ...     # 連線至遠端 daemon
ade mcp [--session <id>]                    # stdio MCP 協作伺服器（由 adapter 自動注入）
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
