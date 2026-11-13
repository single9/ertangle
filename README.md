# termade — 終端機原生的多 agent 開發環境

一個以終端機為核心的 **termade**：讓多個 coding agent
（OpenCode / Claude Code / Codex / Antigravity）在同一個 repo 上平行工作、彼此溝通、互相協作。

- **TUI 前端**（Rust + Ratatui）：啟動快、記憶體足跡小、SSH 友善，沒有厚重的 GUI。
- **常駐 daemon**：真正持有 agent 程序、訊息匯流排與工作區；TUI 只是可隨時重啟的「顯示器」。
- **互通層**：以各家 agent 的**原生結構化協定**為主（零額外依賴），PTY 直通當萬用後備。

完整設計請見 [`docs/DESIGN.md`](docs/DESIGN.md)。

## 為什麼不是「又一個 GUI IDE」

Orca 這類 termade 把 worktree、瀏覽器、diff、mobile 全部塞進一個 Electron/原生 app。
我們只要它的**協作核心**，其餘交給終端機與 agent 自己的 CLI：

| 需求 | 本專案的做法 |
| --- | --- |
| 快速啟動、低記憶體 | Rust + Ratatui 前端，常駐成本 < 20 MB；agent 程序才是耗資源的，且可休眠 |
| 不用圖形化功能 | 全部以鍵盤驅動，並提供 `termade` CLI 與控制 socket，讓 agent 也能驅動環境 |
| Agent 互相溝通協作 | daemon 內的 message bus + task board，透過 **MCP** 注入每個 agent |
| 平行不打架 | 每個 session 一個 `git worktree` + branch |
| 支援多種 agent | Adapter 抽象層（ACP / stream-json / app-server / headless / PTY） |

## 現況（M0–M5 功能已實作；部分路徑尚未實機驗證）

四家 agent 都有**原生、零額外依賴**的結構化介面，實機驗證狀態如下：

| Agent | 原生協定 | 啟動方式 | 傳輸標記 | 實機狀態 |
| --- | --- | --- | --- | --- |
| **OpenCode** | ACP | `opencode acp` | `acp` | ✅ 雙向結構化（prompt / 串流 / 工具 / 審批 / MCP 注入） |
| **Antigravity** | ACP | `agy acp` | `acp` | ✅ 雙向結構化（與 OpenCode 同方言 / MCP 注入） |
| **Claude Code** | stream-json | `claude -p --input-format stream-json --output-format stream-json --verbose` | `stream-json` | ✅ 雙向結構化（逐字串流、工具、成本、MCP 注入） |
| **Codex** | app-server | `codex app-server`（stdio JSON-RPC） | `app-server` | ⚠️ 握手與 thread 已驗證；回合需 OpenAI 授權 |

> **尚未驗證或未實作**：Codex 回合（需 OpenAI 授權）、ACP 逐項審批（受測 agent 預設自行核准）、
> Claude 逐項審批（未轉發到 TUI）、`outbox/` 監看、其他 MCP 工具回傳的秘密過濾。
> 與原始設計的差異彙整於 [`docs/DESIGN.md`](docs/DESIGN.md) 附錄 A。

> 找不到原生協定、或該 agent 未安裝時，`--transport auto` 會**自動退回 PTY 直通**。

已實作（M0–M5；驗證紀錄見 [`docs/VERIFICATION.md`](docs/VERIFICATION.md)）：

- **daemon（M0/M2/M4/M5）**：Unix domain socket + 選配 TCP 遠端監聽（`--listen 127.0.0.1:port`，需 `TERMADE_TOKEN` 或 `--token-file`，建議搭配 SSH 轉發）、
  增量事件同步（`since`）、事件重播（`termade replay`）與秘密遮罩（`sk-...` / `ghp_...` 自動轉 `<REDACTED>`）、
  訊息匯流排、收件匣游標與阻塞等待（`BusInbox` / `BusWait`）、防呆斷路器（速率限制 + 連續重複迴圈偵測）、任務板。
  `termade_send` 直接訊息與廣播（`*`）都會喚醒狀態為 `idle` 或休眠中的收件者；指派任務（`termade_task_create` / 改派負責人）也會自動喚醒對方。
- **MCP 協作伺服器（`termade mcp`，M2/M3）**：spawn session 時自動注入每個 agent，提供 14 個協作工具
  （`termade_whoami`、`termade_peers`、`termade_send`、`termade_inbox`、`termade_wait`、`termade_ask`、
  `termade_task_list`、`termade_task_create`、`termade_task_update`、`termade_spawn`、`termade_artifact_share`、`termade_status`、`termade_diff`、`termade_merge`）。
- **工作區與 Review 迴圈（M3）**：每個 worktree 獨立 branch（`termade/<session>`）、TUI 即時彩色 Diff 視圖、
  對行下審查註解送回 agent 修改（`termade review` / TUI `c` 鍵）、自動 commit 並合併回主分支（`termade merge` / TUI `M` 鍵）、PR 建立（`termade pr`）與已結束 worktree 清理（`termade gc` / TUI `G` 鍵）。
- **打磨與資源控制（M4）**：命令面板（`/` 或 `Ctrl-P`）、Jump-Anywhere（`Ctrl-K`）、即時成本與 token 統計（`termade cost`）、
  閒置休眠與自動喚醒（`SIGSTOP` / `SIGCONT`、`termade sleep` / `termade wake` / TUI `z` 鍵）、權限規則引擎（`.termade/policy.toml` 或 `~/.config/termade/policy.toml`）。
  規則引擎是防呆而非安全邊界：決策順序為 `deny`（子字串比對）→ `allow`（工具名稱或指令整詞前綴）→ session 政策（`auto`／`bypass` 自動允許）→ 詢問使用者；repo 的 `.termade/policy.toml` 只能新增 `deny`，不能移除全域規則。字串比對可被改寫指令繞過，真正的隔離要靠 agent 自己的 sandbox。ACP、Codex 與 Claude 都會經過它；Claude 是透過 `--permission-prompt-tool=mcp__termade__termade_approve` 轉發（非 bypass 時啟用），回應格式依官方 SDK 文件實作，但**尚未用真實 claude 驗證**，逾時 300 秒視為拒絕。
- **TUI**：三種中央視圖（Overview / Diff / Worktrees）、支援 `@peer` 與 `#channel` 智慧路由輸入框。
  逐字稿會顯示使用者送出的 prompt（`❯` 前綴）並即時渲染 Markdown（標題、粗體／斜體、行內程式碼、清單、引言、分隔線、圍籬程式碼區塊與 GFM 表格），
  可用 `t` 在渲染後與原始文字之間切換；支援滑鼠拖曳選取（只複製中央區塊）、滾輪捲動、OSC 52 複製，以及帶可見游標與中文 IME 的輸入框。agent 的 `💭` 思考行預設隱藏，按 `T` 顯示。
- **檔案信箱後備**：自動同步寫入 `<cwd>/.termade/mailbox/<session>.jsonl`。

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
cargo build --release          # 產出單一執行檔 target/release/termade
cargo test                     # 全部單元測試

./target/release/termade           # 啟動 TUI（會自動確保 daemon 在跑）
./target/release/termade daemon    # 或前景跑 daemon
```

第一次使用：在 TUI 按 `n` 建立 session，輸入 `opencode 幫我把測試修好`。

### TUI 按鍵

| 鍵 | 動作 |
| --- | --- |
| `j` / `k`、↑ / ↓ | 選擇 session（在 Diff 視圖則為移動 diff 行游標） |
| 滑鼠拖曳 | 在逐字稿上選取（只含中央區塊，不含側欄與邊框），放開即以 OSC 52 複製 |
| 滑鼠滾輪 | 捲動逐字稿（Diff 視圖則移動 diff 游標） |
| `F2` | 切換滑鼠擷取（預設開啟；關閉後可用終端原生拖曳選取，`Shift`+拖曳在開啟時也可原生選取） |
| `[` / `]` | 在任何視圖切換上一個 / 下一個 session |
| `Tab` / `g` | 切換中央視圖（Overview ↔ Diff ↔ Worktrees） |
| `t` | 切換逐字稿顯示：Markdown 渲染 ↔ 原始文字 |
| `T`（或 `/thoughts`） | 顯示 / 隱藏 agent 的思考行（`💭`；預設隱藏以減少渲染量） |
| `O`（或 `:tools`） | 展開 / 摺疊工具呼叫（預設把連續的呼叫與成功結果摺成一行，失敗行保留） |
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

> 預設啟用滑鼠擷取：可直接在逐字稿上拖曳選取，放開即以 OSC 52 複製（只含中央區塊）；滾輪捲動逐字稿。需要終端原生選取時按 `F2` 關閉擷取，或直接 `Shift`+拖曳。任何時候都可用 `Y` / `/copy` 複製整個主要區塊。agent 的 `💭` 思考行預設隱藏，按 `T` 顯示。

## 安裝方式

```bash
# 本地安裝（在專案根目錄執行，編譯並安裝 termade 到 ~/.cargo/bin）
cargo install --path .

# 亦可使用 Makefile 安裝至 ~/.local/bin/termade
make install
```

### CLI

```bash
termade sessions [--json]                       # 列出 sessions（別名 termade peers）
termade whoami [--session <id>] [--json]        # 查詢當前 session 資訊
termade spawn --kind claude --permission auto --label worker/a --worktree --prompt "..."
termade prompt <session-or-label> "再跑一次測試"
termade send --to <session-or-label> "請先跑測試" # 或 --channel review / --broadcast / --kind handoff
termade inbox [--session <id>] [--all] [--json] # 讀取未讀訊息並推進游標
termade wait [--from <peer>] [--timeout-ms 30000]
termade ask <peer> "請問驗證函式的簽名？"        # 送出問題並阻塞等待回覆
termade task "修正 EU 郵遞區號驗證" --detail "..." --assignee worker/a
termade task-update <task-id> --status done
termade tasks [--json]
termade status "正在跑整合測試"
termade artifact src/zip.rs --kind file --note "新增驗證模組"
termade worktrees [--json]                      # 列出所有受管 git worktrees
termade diff <session-or-label>                 # 查看該 session 工作區的 git diff
termade review <session-or-label> --location src/lib.rs:42 "請補上空字串檢查"
termade merge <session-or-label>                # 將該 session 的 worktree 分支合併回主工作區
termade pr <session-or-label> --title "..."     # 透過 gh 建立 Pull Request
termade gc [--session <id>]                     # 清理已結束 session 的 worktrees
termade sleep <session> ; termade wake <session>    # 暫停 / 喚醒閒置 agent 子程序
termade cost [--json]                           # 查看各 session 與總計的成本與 token 用量
termade replay [--since <seq>] [--json]         # 重播與審計 events.jsonl
# 遠端：建議只綁 loopback，再用 SSH 轉發（TCP 為明文）
TERMADE_TOKEN=<secret> termade daemon --listen 127.0.0.1:7420   # 或 --token-file <path>（0600）
ssh -L 7420:127.0.0.1:7420 host                         # 本機端轉發
TERMADE_TOKEN=<secret> termade --remote 127.0.0.1:7420 ...      # 連線至遠端 daemon
termade mcp [--session <id>]                    # stdio MCP 協作伺服器（由 adapter 自動注入）
termade cancel <session-id> ; termade kill <session-id>
termade shutdown
```

- `--kind`：`opencode` | `claude` | `codex` | `antigravity`（別名 `agy` / `gemini`）。
- `--transport`：`auto`（預設）/ `structured`（別名 `acp`）/ `pty`。
- `--permission`：`ask` | `auto`（預設）| `bypass`。

### 環境變數

- `XDG_RUNTIME_DIR`：socket 位置（預設 `$XDG_RUNTIME_DIR/termade/daemon.sock`，否則 `/tmp/termade-$USER/`）。
- `XDG_STATE_HOME`：事件記錄位置（預設 `~/.local/state/termade/events.jsonl`）。
- `TERMADE_LOG`：tracing 過濾（例如 `TERMADE_LOG=debug`）。

### TUI 操作

* `Space`：Helix 風格選單（which-key），按 `s` session、`g` git／worktree、`v` view、`m` message；`Backspace` 返回、`Esc` 關閉。
* `n`（或 `Space s n`）：依序選 agent → model → 輸入 prompt。model 清單含預設、內建建議（Claude 的 sonnet／opus／haiku）與 `config.toml` 的 `[agents.<名稱>] models = [...]`；清單沒有符合時，Enter 會使用你輸入的值。
* `:`（或 `/`）：命令列，`Tab` 補全。常用：`spawn claude:opus 做某事`、`rm [session] [--force]`、`clean`（清除所有已結束的 session）、`kill`、`merge --dry-run`、`gc`、`layout`。
* 啟動失敗的 session 會自動移除（含剛建立的 worktree 與分支）；已結束的 session 可用 `:rm`／`Space s x` 或 `termade rm <session>` 移除。
* effort（推理強度，low／medium／high／xhigh／max）：`termade spawn --effort high`、MCP `termade_spawn` 的 `effort`、TUI 選擇器（Claude、Codex 會多一步）。也可寫進 `config.toml` 的 `[agents.claude] effort = "medium"` 或 `[roles.reviewer] effort = "high"`（請求 > 角色 > agent）。Claude 對應 `--effort`，Codex 對應 `thread/start` 的 `config.model_reasoning_effort`；ACP agent 只有在 `session/new` 回報了 effort 選項時才能設，否則會明確報錯。
* 檔案／圖片附件：在 prompt 裡寫 `@` 加檔名片段，會在專案內（選取 session 的工作目錄，用 `git ls-files`，排除 .gitignore）做模糊搜尋並列出建議，`↑↓` 選、`Tab` 採用；只打檔名（如 `@main.rs`）而專案內唯一符合時，送出時會自動找到；`~/`、`/` 開頭的字面路徑用 `Tab` 做路徑補全，含空白用 `@"my file.png"`，送出時文字檔內容、圖片會一起附上；`Space m f` 會預填 `@`；把檔案拖進終端機（貼上路徑）會自動轉成 `@路徑`；`Ctrl-V` 貼剪貼簿圖片（Linux 需要 `wl-paste` 或 `xclip`）。開頭的 `@名稱` 若是存在的檔案就視為附件，否則仍是傳給 peer 的訊息。CLI：`termade prompt <session> -a 圖.png "說明"`、`termade spawn … -a 檔案`。
  限制：圖片上限 5 MB、文字檔內嵌上限 256 KB（超過只帶路徑）；其他二進位檔只帶路徑。Claude／Codex／Antigravity 以圖片區塊送出，OpenCode 的 ACP 沒宣告圖片支援時改成路徑說明（實測它的 gemma 模型看不到圖）。
* 交接（額度用完時）：Claude／ACP／Codex 回報額度用完或被限流時，session 會標成 failed 並註明「額度用完」。用 `termade handoff <session> --to codex[:model] [--note "補充"]`、TUI 的 `Space s h` 或 `:handoff` 交接：舊 session 若還在跑會先停掉，worktree 的未 commit 變更自動 commit，daemon 用事件紀錄產生簡報（原始任務、追加指示、指派的 task、最後輸出、git 現況）加上完整 diff 附件，新 agent 沿用**同一個工作目錄與分支**接手。兩個 session 共用 worktree 時，`gc`／`rm` 不會刪掉仍在使用中的那個。目前只有手動觸發，沒有自動 fallback。
* 額度查詢：agent 自己回報帳號額度，termade 只是顯示（不估算）。Claude 在每個回合的串流裡帶 5 小時／7 天視窗用量與重置時間；Codex 在啟動時用 `account/rateLimits/read` 查、之後收更新通知。標題列顯示最吃緊的視窗（≥70% 黃、≥90% 或已滿紅），`:quota` 或 `termade quota [--json]` 看全部視窗與重置倒數。Claude 的資料要有一個回合跑過才有；OpenCode／Antigravity 目前沒有額度資訊來源。
* 額度提醒與自動交接（`config.toml`）：`[handoff] warn_percent = 90` 時，agent 帳號額度越過門檻，該 agent 還在跑的 session 會留下「建議 :handoff」的備註（每次越線只提醒一次）。`auto = true` 加 `fallback = ["codex", "claude:sonnet"]` 則在額度用完時自動交接給第一個可用的候選（跳過同一個 agent、額度也滿了的、連續交接超過 3 次的）；預設關閉，因為會動用另一邊的額度。
* context 用量：agent 回報的對話佔用 token 數與模型視窗（Claude 由每則訊息的 usage 加上 `result.modelUsage.contextWindow`；Codex 由 `thread/tokenUsage/updated`；ACP 若送 `usage_update` 也會用）。sessions 清單顯示 `ctx 4%`（≥80% 黃、≥92% 紅），transcript 標題與 `termade sessions` 顯示 `41k/1M 4%`。視窗大小要 agent 回報過才知道，之前只顯示 token 數。

開發者：改動按鍵、命令、協定或設定檔時需要手動同步的清單見 [docs/MAINTAINING.md](docs/MAINTAINING.md)。
