# termade — 終端機原生的多 Agent 開發協作環境

[English](README_EN.md) | [繁體中文](README.md)

> **讓多個 Coding Agent 在同一個 Repo 內平行工作、自主溝通、互相協同開發的終端機環境**。

`termade` 是一個以終端機為核心的多 Agent 協同開發平台。支援 **OpenCode**、**Claude Code**、**Codex** 與 **Antigravity** 等現代 Coding Agent，讓它們各自在獨立的 Git Worktree 內平行寫程式，透過 MCP 訊息匯流排與任務看板互相溝通，並由開發者透過鍵盤驅動的 TUI 介面進行即時審查與安全合併。

---

## 為什麼選擇 termade？ (終端機原生 vs. GUI IDE)

[Orca](https://github.com/stablyai/orca) 等 GUI IDE 將 Worktree、瀏覽器、Diff 視圖與終端機整合在同一個桌面應用程式中。`termade` 的哲學是**只聚焦協作與排程核心，其餘交還給終端機與各家 Agent 的原生 CLI**：

| 特性 | 本專案的做法 |
| --- | --- |
| **超輕量與極速** | Rust + Ratatui 開發，常駐記憶體成本 `< 20 MB`，瞬間啟動，SSH 遠端連線零卡頓 |
| **Daemon 常駐架構** | Daemon 負責真正持有 Agent 子程序、訊息匯流排與狀態；TUI 只是隨時可重開、重新連線的顯示端 |
| **平行作業不衝突** | 每個 Agent Session 自動配置獨立的 `git worktree` 與分支 (`termade/<session>`) |
| **零相依原生通訊** | 優先採用各 Agent 的結構化協定 (ACP / stream-json / app-server)，無需額外 Wrapper；未安裝時自動退回 PTY 直通 |
| **結構化協同機制** | 透過 Daemon 注入 MCP 伺服器，讓 Agent 能彼此通訊、委派任務、發布進度、排隊等待 |
| **全鍵盤與腳本驅動** | 完整 TUI 快速鍵、Helix 風格選單 (which-key)、命令列，以及功能對等的 `termade` CLI |

---

## 支援的 Agent 與連線協定

各家 Agent 均以其**官方原生、零額外相依**的結構化介面連接：

| Agent | 原生協定 | 預設啟動命令 | 傳輸標記 | 支援狀態與特性 |
| --- | --- | --- | --- | --- |
| **OpenCode** | ACP | `opencode acp` | `acp` | ✅ 完整雙向結構化 (串流、工具呼叫、權限核准轉發、MCP 注入) |
| **Antigravity** | ACP | `agy acp` | `acp` | ✅ 完整雙向結構化 (ACP 方言相容、MCP 注入) |
| **Claude Code** | stream-json | `claude -p --input-format stream-json --output-format stream-json --verbose` | `stream-json` | ✅ 逐字串流、工具呼叫、成本統計、5h/7d 額度回報、MCP 注入 |
| **Codex** | app-server | `codex app-server` | `app-server` | ⚠️ 握手與 Thread 已驗證；執行回合需先於本機執行 `codex login` |
| **萬用後備** | PTY | 各 Agent 互動 CLI | `pty` | ✅ 找不到原生協定或未安裝時，`--transport auto` 自動退回 PTY 直通 |

> [!NOTE]
> - **Codex 授權**：`codex app-server` 的通訊協定完整支援，但實際發送 prompt 需要有效的 OpenAI 登入。未授權時 session 會變更為 `failed` 並提示執行 `codex login`。
> - **PTY 限制**：PTY 模式下全螢幕 TUI 輸出會去除 ANSI 控制碼作為盡力而為的文字記錄，建議優先使用結構化原生協定。
> - 完整驗證紀錄與實機測試版本請參閱 [`docs/VERIFICATION.md`](docs/VERIFICATION.md)；架構設計細節請見 [`docs/DESIGN.md`](docs/DESIGN.md)。

---

## 快速開始

### 1. 安裝與建置

```bash
# 1. 複製並編譯單一執行檔
git clone <repo-url> ade-rust && cd ade-rust
cargo build --release

# 2. 安裝至 ~/.cargo/bin
cargo install --path .

# 或使用 Makefile 安裝至 ~/.local/bin/termade
make install

# 3. 安裝 Shell 自動補齊腳本 (支援 bash / zsh / fish，並能動態補齊 session label)
make install-completions
# 亦可手動輸出：termade completions <bash|zsh|fish>
```

### 2. 啟動與第一個任務

在任何 Git 專案目錄下執行：

```bash
termade
```

- 若 Daemon 尚未執行，`termade` 會**自動在背景啟動 Daemon** 並進入 TUI。
- 在 TUI 中按下 **`n`**，依序選擇 Agent (如 `opencode`)、Model (如預設或建議模型)，並輸入提示：
  ```
  幫我把測試修好，並修正任何編譯錯誤
  ```
- Agent 會在專屬 Worktree (`termade/session-xxx`) 開始工作。你可以在中央視圖即時觀察 Markdown 串流逐字稿、切換到 Diff 視圖審查程式碼變更，並一鍵完成合併。

---

## 核心功能與工作流程

### 1. 工作區隔離與審查迴圈 (Review Loop)

- **Worktree 獨立分支**：每個 session 使用專屬 `git worktree` 與 `termade/<session>` 分支，互不干擾。
- **即時彩色 Diff**：在 TUI 按 `Tab` 切換至 Diff 視圖，按 `{` / `}` 跳轉 hunk。
- **行內註解審查 (Review Comment)**：在 Diff 視圖按 `c`，直接對該行留下修改建議 (例如：「請補上邊界條件檢查」)，系統會自動將註解送回該 Agent 要求修正。
- **安全合併 (Merge)**：按 `M` 或執行 `termade merge`，系統會先執行 pre-check，自動 commit worktree 變更並合併回主分支。
- **清理 (GC)**：按 `G` 或執行 `termade gc`，自動清除已結束且已合併的 Worktree，絕不誤刪未合併成果。

### 2. 跨 Agent 協作匯流排與任務看板 (MCP & Bus)

Daemon 會在啟動 session 時自動注入包含 **14 個協作工具**的 stdio MCP 伺服器：

| 類別 | 工具名稱 | 功能 |
| --- | --- | --- |
| **自我與環境** | `termade_whoami`, `termade_peers` | 查詢自己的 session ID、工作區分支，以及其他同伴資訊 |
| **訊息傳遞** | `termade_send`, `termade_inbox`, `termade_wait`, `termade_ask` | 發送訊息、非同步讀取收件匣、阻塞等待新訊息、同步問答 |
| **任務看板** | `termade_task_list`, `termade_task_create`, `termade_task_update` | 建立協作任務、指定負責人、支援 `depends_on` 前置相依性 |
| **衍生與產出** | `termade_spawn`, `termade_artifact_share`, `termade_status` | 派生子 Agent、分享產出物、廣播進度狀態 |
| **Git 整合** | `termade_diff`, `termade_merge` | 查詢自身分支與主分支 diff、請求合併 |

- **空閒提醒 (Unread Nudge on Idle)**：
  - 當 Agent 正忙於執行回合時，發來的直接訊息 (或廣播) 只會暫存在收件匣，**不打斷運算**。
  - 當 Agent 結束回合回到 `idle` 且佇列中的指令處理完畢後，系統會將未讀訊息合併為一則提醒，每則訊息**僅提醒一次**。
  - 已透過 `termade_wait` / `termade_ask` 取走或超過成本預算的 session 則不予打擾。
- **任務相依性 (Task Dependencies)**：
  - 建立任務時可帶 `--depends-on <id>`，在前置任務全數 `done` 之前，任務維持 `blocked` 且不打擾負責人。
  - 前置任務完成後自動轉為 `todo`，並主動發信喚醒指派的負責人。
- **信箱後備**：所有訊息同步寫入專案目錄下的 `.termade/mailbox/<session>.jsonl`。

### 3. 額度監控、智慧交接 (Handoff) 與接續 (Resume)

- **額度監視**：
  - 即時解析 Agent 回報的額度狀態 (Claude 的 5 小時／7 天視窗與重置倒數；Codex 的 rateLimits)。
  - 標題列即時標示最吃緊視窗 (`≥ 70%` 黃色、`≥ 90%` 紅色)；輸入 `:quota` 可查看完整視窗資訊。
- **智慧交接 (Handoff)**：
  - 當 Agent 回報額度用完、被限流，或使用者手動要求 (`:handoff <agent> [備註]`)：
  - 舊 Agent 停止執行，工作區未 commit 變更自動 commit；
  - Daemon 彙整任務背景、指派任務、最後輸出與完整 Diff 生成交接簡報；
  - 新 Agent **直接沿用同一個工作目錄與分支**接手；
  - 之後發往舊 session 的直接訊息會**自動改投給新接手者**。
- **對話接續 (Resume)**：
  - 若 Agent 程序異常中斷或 Daemon 重啟，按下 `Space s r` 或執行 `termade resume <session> [prompt]`，即可喚回**原本的對話**。
  - 利用各家原生的對話回復機制 (Claude `--resume`、Codex `thread/resume`、ACP `session/load`)，保持同一個 session ID、逐字稿與分支。

### 4. 資源控管與休眠 (Budget & Limits)

- **成本上限設定**：
  - 支援單一 session 預算 (`session_usd`)、子樹總額上限 (`tree_usd`)、Token 上限 (`session_tokens`)，以及最大子 Agent 數量與層級深度。
  - 達到警戒線 (預設 80%) 黃色警告，超過上限立即阻斷執行並暫停排隊的 prompt，使用 `:budget <金額>` 調高後自動恢復。
- **閒置休眠**：
  - 按 `z` 或執行 `termade sleep <session>`，發送 `SIGSTOP` 暫停 Agent 程序節省系統資源；按 `z` 或收到訊息時發送 `SIGCONT` 自動喚醒。

### 5. 權限政策與安全防護 (Policy Engine)

- 提供跨 Agent 的統一控制旋鈕 `--permission <ask|auto|bypass>`：
  - `ask`：所有工具呼叫轉交由使用者在 TUI 逐一按 `a` (允許) / `A` (一律允許) / `d` (拒絕) 核准。
  - `auto` (預設)：安全工具自動核准。
  - `bypass`：跳過權限確認。
- **規則引擎**：支援全域 `~/.config/termade/policy.toml` 與專案 `.termade/policy.toml`，以 `deny` (黑名單) → `allow` (白名單) → session 政策順序過濾指令。

---

## TUI 終端介面操作指南

### 1. 版面與檢視

- **版面模式**：按 `L` 循環切換「兩欄」 (Sessions + 中央)、「分頁」 (頂部 Tabs + 中央) 與「三欄」 (Sessions + 中央 + Bus 訊息監控)。
- **中央視圖**：按 `Tab` 依序切換四大視圖：
  1. **Overview**：即時串流逐字稿，支援 Markdown 渲染 (標題、清單、表格、程式碼區塊等)。
  2. **Diff**：即時彩色 Git Diff 視圖，支援行級審查註解。
  3. **Worktrees**：檢視目前所有由 termade 建立的 Git Worktree 狀態與磁碟路徑。
  4. **Tasks**：協作看板視圖，任務依狀態 (doing、review、todo、blocked、done) 分組，顯示前置任務相依性 (`⧗`)。按 `Enter` 直達負責人 Session。

### 2. 快速鍵速查表

#### 導航、選單與核心動作

| 按鍵 | 動作 |
| --- | --- |
| `Space` | 開啟 **Which-key 選單** (`s` session、`g` git、`v` 檢視、`m` 訊息、`?` 說明) |
| `:` 或 `/` | 開啟**命令列** (支援 `Tab` 自動補齊) |
| `n` | 建立新 Session (依序選擇 Agent、Model 與輸入 Prompt；`Ctrl-W` 切換 Worktree) |
| `i` / `Enter` | 聚焦輸入框，對選取的 Session 發送 prompt (亦支援 `@peer` 或 `#channel` 傳訊) |
| `j` / `k` 或 `↑` / `↓` | 移動選取 Session；在 Diff 視圖移動行游標；在 Tasks 視圖移動任務游標 |
| `[` / `]` | 快速跳至上一個 / 下一個 Session |
| `Ctrl-K` | **Jump-Anywhere**：依關鍵字模糊搜尋並跳轉 Session |
| `q` / `Q` | `q` 關閉 TUI (Daemon 與 Agent 續跑)；`Q` 關閉 Daemon (等同 `:shutdown`) |

#### 逐字稿、搜尋與複製

| 按鍵 | 動作 |
| --- | --- |
| `t` | 切換逐字稿顯示模式：Markdown 渲染 ↔ 原始文字 |
| `T` (或 `:thoughts`) | 顯示 / 隱藏 Agent 的思考行 (`💭`，預設隱藏以加速渲染) |
| `O` (或 `:tools`) | 展開 / 摺疊工具呼叫紀錄 (連續成功呼叫預設摺疊) |
| `f` (或 `:find <文字>`) | **即時搜尋**逐字稿或 Diff (邊打邊找，`Enter` / `↓` 下一個、`↑` 上一個、`Esc` 退出) |
| 滑鼠拖曳 | 在逐字稿中央區塊拖曳選取，放開即透過 **OSC 52** 複製至系統剪貼簿 |
| `Y` (或 `/copy`) | 複製目前中央區塊的完整文字 (不含側欄與邊框) |
| `F2` | 切換滑鼠擷取模式 (關閉後可使用終端機原生選取，或使用 `Shift`+拖曳) |
| `PgUp` / `PgDn` | 向上 / 向下翻頁捲動逐字稿 (捲動時暫停自動跟隨) |
| `Home` / `End` | 跳至頂部 / 回到底部並恢復跟隨最新輸出 (`▼ follow`) |
| `P` | 切換專案範圍：只看本專案 (`here`) ↔ 顯示全部專案 Session (`all`) |

#### Prompt 輸入與排隊

| 按鍵 / 語法 | 功能說明 |
| --- | --- |
| `@檔名` | **檔案附件模糊搜尋**：輸入 `@` 片段，自動在專案內搜尋檔案並補齊，送出時自動內嵌文字 (上限 256 KB) 或圖片 (上限 5 MB) |
| `Ctrl-V` | 貼上系統剪貼簿內的圖片 (Linux 需安裝 `wl-paste` 或 `xclip`) |
| 拖曳檔案 | 直接將檔案從檔案總管拖進終端機，自動轉換為 `@路徑` 語法 |
| `Ctrl-J` / `Shift-Enter` | 輸入框換行 (輸入框隨行數自動伸展，最多 8 行) |
| `Alt-Enter` | **將 Prompt 排入佇列** (Session 忙碌時排入 Daemon 佇列，閒置時依序自動送出) |
| `↑` / `↓` | 在空輸入框時切換歷史輸入指令 (持久化儲存於本機) |

#### 審查、Git 與生命週期控制

| 按鍵 | 動作 |
| --- | --- |
| `c` | (Diff 視圖) 針對當前選取的行留下 Review 審查註解並送回 Agent 修改 |
| `M` | 將選取 Session 的 Worktree 分支安全合併回主分支 (可先用 `:merge --dry-run` 預覽) |
| `G` | 清理 (GC) 已結束且已合併的 Worktree |
| `z` | 切換暫停休眠 (`SIGSTOP`) 與喚醒 (`SIGCONT`) |
| `x` | 中斷 Agent 當前回合運算 (Cancel) |
| `K` | 強制結束選取的 Agent 子程序 (Kill) |
| `a` / `A` / `d` | 審核請求：允許一次 / 本回合一律允許 / 拒絕 |

### 3. TUI 命令列 (`:command`) 速查

按 `:` 進入命令列，支援 `Tab` 自動補齊：

- `:spawn <agent>[:model] <prompt>` — 啟動新 Agent Session (如 `:spawn claude:opus 修復連線逾時`)
- `:handoff <agent> [備註]` — 將當前 Session 交接給另一位 Agent 接手
- `:resume [prompt]` — 接續已結束或中斷的 Session 對話
- `:queue` / `:unqueue` / `:queue clear` — 檢視、取出或清空待送佇列
- `:rm` / `:clean` — 移除已結束的 Session / 清除所有已結束項目
- `:budget <金額>` — 查看或調高當前 Session 的美元預算上限
- `:quota` — 查詢所有 Agent 帳號的額度視窗與重置倒數
- `:task <標題>` / `:tasks` — 建立新任務 / 切換到任務看板視圖
- `:task-status <todo|doing|blocked|review|done>` — 修改選取任務的狀態
- `:notify on|off|test` — 開關桌面通知或測試發送
- `:shutdown` — 關閉共用 Daemon 與所有專案的 Agent (安全保護：需輸入 `:shutdown!` 確認)

---

## CLI 命令參考

`termade` 提供與 TUI 對等的完整 CLI 工具：

```bash
# Session 管理
termade sessions [--json]                         # 列出所有 session (別名 termade peers)
termade whoami [--session <id>] [--json]          # 查看當前 session 資訊
termade spawn --kind <agent> [--label <label>] [--worktree] [--prompt <prompt>]
termade resume <session-or-label> ["接續 prompt"] # 接續已中斷的 session
termade handoff <session-or-label> --to <agent> [--note "交接補充"]
termade sleep <session> ; termade wake <session>  # 暫停 / 喚醒 Agent 程序
termade cancel <session> ; termade kill <session> # 中斷回合 / 強制中止程序
termade rm <session> [--force]                    # 移除已結束的 session 紀錄

# 提示詞與佇列
termade prompt [--queue] <session-or-label> "提示詞"
termade queue <session-or-label> [--clear]        # 查看或清空待送佇列

# Worktree 與程式碼審查
termade worktrees                                 # 列出所有由 termade 管理的 worktree
termade diff <session-or-label>                   # 檢視指定 session 的 git diff
termade review <session-or-label> --location <file>:<line> "審查意見"
termade merge <session-or-label> [--dry-run]      # 合併分支回主工作區
termade pr <session-or-label> --title "PR 標題"   # 透過 gh 建立 Pull Request
termade gc [--session <id>]                       # 清理已結束且已合併的 worktrees

# 跨 Agent 訊息與任務看板
termade send <session-or-label> "訊息內容"         # 發送訊息給指定 agent
termade inbox [--session <id>]                    # 檢視收件匣
termade tasks                                     # 列出所有任務與相依性狀態
termade task "任務標題" [--assignee <label>] [--depends-on <id>]
termade task-update <id> --status <status>        # 更新任務狀態 (todo/doing/blocked/review/done)

# 成本、額度與審核記錄
termade quota [--json]                            # 查詢帳號額度視窗與重置倒數
termade cost [--json]                             # 統計各 session 與整體 token / 美元花費
termade budget <session-or-label> <金額>          # 設定或調高預算上限
termade replay [--since <seq>] [--json]           # 重播與審核 events.jsonl 事件記錄

# 遠端 Daemon 與 Shell 補齊
termade daemon --listen 127.0.0.1:7420            # 啟動 Daemon 並監聽遠端 (搭配 TOKEN)
termade --remote 127.0.0.1:7420                   # 連線至遠端 Daemon
termade completions <bash|zsh|fish>               # 產生 Shell 自動補齊腳本
```

---

## 設定檔與環境變數

### 設定檔：`config.toml`

`termade` 依序讀取全域設定 (`~/.config/termade/config.toml`) 並由專案設定 (`<repo>/.termade/config.toml`) 覆寫：

```toml
[tui]
layout = "two"                    # 預設版面："two" | "three" | "tabs"
notify = ["bell", "command"]      # 通知方式："bell" | "osc9" | "command" (可複選)
notify_command = "notify-send termade \"$TERMADE_NOTIFY_TEXT\""
notify_min_turn_secs = 10         # 短於此秒數的回合完成不觸發通知

[limits]
session_usd = 5.0                 # 單一 Session 成本上限 (美元)
tree_usd = 15.0                   # 父子 Agent 樹累計成本上限
warn_percent = 80                 # 成本與額度警告門檻百分比
max_children = 5                  # 單一 Session 允許派生的最大子 Agent 數
max_depth = 3                     # 子 Agent 派生深度上限

[handoff]
warn_percent = 90                 # 額度消耗達 90% 時提示建議 :handoff
auto = false                      # 額度用盡時是否自動交接 (預設 false 避免非預期費用)
fallback = ["codex", "claude:sonnet"]

[collab]
instructions = true               # 是否自動向 Agent 注入 termade 協作工具使用規範
extra = "請優先遵循專案的 CONTRIBUTING.md 規範"
```

> [!TIP]
> 專案內可建立 `.termade/instructions.md` (上限 8 KB)，內容會在啟動時自動附加至 Agent 的 System Prompt 底部。

### 支援的環境變數

| 環境變數 | 預設值 | 說明 |
| --- | --- | --- |
| `XDG_RUNTIME_DIR` | `/tmp/termade-$USER/` | Daemon Unix domain socket 存放目錄 |
| `XDG_STATE_HOME` | `~/.local/state/` | 事件持久化日誌路徑 (`$XDG_STATE_HOME/termade/events.jsonl`) |
| `TERMADE_LOG` | `warn` | Tracing 日誌過濾等級 (例如 `TERMADE_LOG=debug`) |
| `TERMADE_TOKEN` | — | 遠端連線認證 Token (建議搭配 SSH 轉發避免明文傳輸) |
| `TERMADE_REMOTE` | — | 預設遠端 Daemon 位址 (等同 `--remote` 參數) |

---

## 進階架構與相關文件

- [`docs/DESIGN.md`](docs/DESIGN.md) — 系統架構、事件同步協定、狀態機與 MCP 規格定義。
- [`docs/VERIFICATION.md`](docs/VERIFICATION.md) — 實機驗證成果、各家 Agent 相容性測試紀錄。
- [`docs/MAINTAINING.md`](docs/MAINTAINING.md) — 開發者維護指南與修改同步檢查清單。
- [`docs/tickets/`](docs/tickets/) — 系統重構、功能擴充與改善計畫票紀錄 (T01–T25 全數完成)。

---

## 授權條款

本專案依 [MIT 授權條款](LICENSE) 開源。

---

## 致謝

`termade` 的發想來自 [Orca](https://github.com/stablyai/orca) (MIT 授權)，其 full handoff 與 review 迴圈的概念啟發了本專案的設計。本專案為獨立實作，未複製其程式碼。
