# 驗證紀錄

各里程碑的實機驗證結果 (自 `DESIGN.md` 第 13 節移出)。為避免過期，此處不記錄測試數量；請以 `cargo test` 實際結果為準。

## M0 已驗證 (實機)

| 驗證項 | 指令 | 結果 |
| --- | --- | --- |
| daemon 常駐 + socket | `ertangle daemon` / `ertangle sessions` | OK |
| OpenCode ACP 結構化 | `ertangle spawn --kind opencode --prompt "..." --transport acp` | transcript 收到 `hello from ertangle` |
| PTY 萬用後備 | `ertangle spawn --kind opencode --transport pty` | 逐段剝除 ANSI 後可讀 |
| 訊息匯流排 | `ertangle send --to <id>` / `--channel review` | 訊息進 `State.messages` |
| 任務板 | `ertangle task "..."` / `ertangle tasks` | 任務進 `State.tasks` |
| TUI attach | `ertangle` | 三欄 (Sessions / Transcript / Bus)即時更新、`q` 離線不影響 agent |
| 中斷 / 結束 | `ertangle cancel <id>` / `ertangle kill <id>` | ACP `session/cancel`、處理程序 kill；狀態轉 `exited` |
| 關閉 | `ertangle shutdown` | 先結束所有 agent 子處理程序、移除 socket 再離開 |

## M1 已驗證 (實機)

| 驗證項 | 指令 | 結果 |
| --- | --- | --- |
| OpenCode ACP | `ertangle spawn --kind opencode --prompt "..."` | 收到 `ertangle-ok-opencode`，狀態 busy→idle |
| Antigravity ACP | `ertangle spawn --kind agy --prompt "..."` | 收到 `ertangle-ok-agy` (與 OpenCode 同 ACP 方言) |
| Antigravity 工具 | `ertangle spawn --kind agy --prompt "建立 agy-write.txt"` | 檔案寫入成功、transcript 可讀 |
| Claude stream-json | `ertangle spawn --kind claude --prompt "..."` | 收到 `ertangle-ok-claude`、`transport=stream-json`、成本解析正確 |
| Claude 工具 | `ertangle spawn --kind claude --permission bypass --prompt "跑 echo"` | ToolCall `Bash` + tool_result `ertangle-tool-ok` |
| Codex app-server 握手 | `ertangle spawn --kind codex --prompt "..."` | `initialize` + `thread/start` 成功、`transport=app-server`；回合因無授權回 401 |
| 權限政策 | `--permission ask/auto/bypass` | ACP 由 adapter 代核准；Claude 對應 `--permission-mode` / `--dangerously-skip-permissions` |
| transport 別名 | `--transport acp` / `structured` / `pty` | `acp` 與 `structured` 等價 (serde alias) |

## M2 已驗證 (實機)

| 驗證項 | 指令 / 工具 | 結果 |
| --- | --- | --- |
| MCP stdio 協定 | `ertangle mcp --session <id>` | `initialize`、`tools/list` (當時 12 個 `ertangle_*` 工具，現為 14 個)、`tools/call` 正常 |
| 自動注入 MCP | `ertangle spawn --kind claude/opencode/agy` | ACP 透過 `session/new.mcpServers`、Claude 透過 `--mcp-config`、Codex 透過 `-c mcp_servers.ertangle.*` 自動掛載 |
| 收件匣與等待 | `ertangle inbox` / `ertangle wait` / `ertangle ask` | 以 label 或 session id 定址、游標去重、阻塞等待與 request/response 交握正常 |
| 閒置喚醒 | 對 `Idle` session 送 `Question` / `Handoff` | daemon 自動將匯流排訊息轉成 prompt 喚醒對方處理 |
| 防呆斷路 | `BusGuard` | 連續重複訊息達上限 (3 次)觸發迴圈斷路；每分鐘超過 30 則觸發速率限制 |
| 端到端多 Agent 協作 | `coord` 呼叫 `ertangle_task_create` + 2×`ertangle_spawn(worktree=true)` + 2×`ertangle_wait` | `worker/a` 與 `worker/b` 各自於獨立 worktree 寫檔並 `ertangle_send(kind=handoff)` 回報，`coord` 收齊後將兩任務標為 `done` 並更新 `status_note` |

## M3–M5 已驗證 (實機)

| 驗證項 | 指令 / 功能 | 結果 |
| --- | --- | --- |
| Worktree 清單與 Diff | `ertangle worktrees` / `ertangle diff worker/feat` | 正確列出變更檔案數與統一格式 diff (含新增未追蹤檔案) |
| Worktree 自動 Commit + Merge | `ertangle merge worker/feat` | 自動將 worktree 內未 commit 的變更打包 commit 並 merge 回主分支 |
| Worktree 清理 | `ertangle gc` | 自動移除已結束 session 的 `.ertangle/wt/<id>` 與 `ertangle/<id>` 分支 |
| 行內審查註解 (Review 迴圈) | `ertangle review <session> --location ...` / TUI `c` | 記錄 `ReviewCommented` 事件並向該 agent 送出定位修正 prompt |
| 閒置休眠與喚醒 | `ertangle sleep <id>` / `ertangle wake <id>` | 送出 `SIGSTOP` / `SIGCONT`，狀態切換 `sleeping` ↔ `idle`；收到新 prompt 時自動喚醒 |
| 成本統計 | `ertangle cost` | 逐 session 與總計顯示 USD 與 input/output tokens |
| 事件重播與秘密遮罩 | `ertangle replay` | 完整重播 `events.jsonl`；`sk-...` / `ghp_...` 自動遮罩為 `<REDACTED>` |
| 遠端 TCP + Token 連線 | `ertangle daemon --listen 127.0.0.1:17429 --token ...` | 透過 `--remote` + `--token` 建立任務與查詢狀態成功 |
| TUI Markdown 渲染與捲動 | `cargo test -p ertangle-tui`(TestBackend 全畫面渲染) | 標題／粗體／程式碼／表格正確渲染、`t` 切換原始文字、Diff 邊欄與 `+N -M` 統計、翻頁後恢復跟隨 |

## T15 通知 (實機，部分)

環境：Linux、tmux 3.4 (獨立的 tmux server 與獨立的 daemon／設定目錄)、`notify = ["bell", "osc9", "command"]`。

| 驗證項 | 做法 | 結果 |
| --- | --- | --- |
| tmux 內 bell | TUI 在非目前視窗，送 `:notify test` | 該視窗的 `window_bell_flag` 變成 1 |
| tmux 內 `notify_command` | 命令把 `ERTANGLE_NOTIFY_KIND`／`LABEL`／`TEXT` 寫進檔案 | 檔案內容 `done\|ertangle\|ertangle：測試通知` |
| `:notify off`／`:notify` | 在 TUI 輸入 | 狀態列顯示 `通知：關 (bell、osc9、command)` |

沒有驗證：直接在終端機模擬器 (不經 tmux)的 bell 與 `notify_command`；各終端機對 OSC 9 的實際顯示；tmux 是否把 OSC 9 轉給外層終端機；焦點事件在實機終端機的回報；由真實 agent 的回合結束／核准觸發 (這些路徑只有單元測試)。

## T14 Session 接續 (實機，2026-11-13)

版本：Claude Code 2.1.322 (model `haiku`)、OpenCode 2.2.5 (本機 ollama `gemma4:26b-code`)、Antigravity `agy` 1.5.7、codex-cli 0.167.2。
做法：隔離的 daemon (獨立 socket 與 state)，`ertangle spawn --prompt "記住某個字"` → 結束 → `ertangle resume <label> "剛剛要你記的字是什麼？"`，看回答是否正確。

| 驗證項 | 做法 | 結果 |
| --- | --- | --- |
| Claude `--resume` 搭配 stream-json | `ertangle kill` → `ertangle resume cl`，再送 prompt | 答出原本的字；`init` 的 `session_id` 接續前後相同；agent 不重播歷史；成本由 $0.0083 變 $0.0088 (舊值加新值) |
| Antigravity ACP `session/load` | `ertangle kill` → `ertangle resume ag "<問題>"` | 答出原本的字；`initialize` 宣告 `loadSession: true`；載入時重播的 user／agent 訊息沒有進逐字稿 |
| OpenCode ACP `session/load`(daemon 重啟) | `ertangle shutdown` → 重新啟動 daemon → `ertangle resume oc "<問題>"` | 重啟後三個 session 都是 failed 且備註「daemon 重啟，可用 :resume 接續」，沒有自動接續；接續後答出原本的字，備註清除 |
| 對已結束的 session 送 prompt | `ertangle prompt oc hi` | 回「已結束；可用 :resume…接續原對話」 |
| 對執行中的 session 接續 | `ertangle resume cl` (狀態 busy) | 回「仍在執行，不需要接續」 |
| Codex `thread/resume` (只到協定層) | 直接對 `codex app-server` 送 `thread/start` → 另一個處理程序 `thread/resume` | 回傳同一個 thread id，且帶回先前的 1 個回合；本機 codex 的登入已失效 (回合回 `unauthorized`)，所以**沒有驗證**模型接續後的回答 |

未驗證：

- Codex 經由 `ertangle resume` 的完整流程 (需要有效登入)，以及接續後 `thread/tokenUsage/updated` 的 `total` 是否包含接續前的用量。目前假設包含，所以 Codex 不另外加上舊值；如果實際上是從 0 算，接續後顯示的 token 會偏少。
- `thread/resume` 帶 `excludeTurns: true`：欄位取自 0.167.2 的 schema，沒有在較舊的 codex 上試過。
- TUI 的 `:resume` 與 `Space s r` 只跑了自動測試，沒有在終端機裡手動操作。
- 對話很長時 ACP `session/load` 的耗時 (逾時設為 180 秒；實測只用了一個回合的短對話)。

## 協作說明注入 (T20)實機驗證

以只回傳 `initialize.instructions` 的探測 MCP server 單獨測試第一層是否進入模型 context，並以隔離 daemon 啟動子 session (`--parent coord`、prompt 不含 `ertangle_send` 提示)驗證主動回報：

| Agent | 第一層 (MCP `initialize.instructions`) | 第二層 (adapter 補強) | 子 agent 主動回報實測 |
| --- | --- | --- | --- |
| **Claude Code** | ✅ 有效 (模型可直接讀到 `initialize.instructions`) | `--append-system-prompt` | ✅ 完成 `17+25=42` 後主動呼叫 `ertangle_send(to="coord")` 回報 |
| **OpenCode** | ✅ 有效 (`opencode acp` 會將 `initialize.instructions` 放進 context) | ACP 首則 prompt 前置 (`UserPrompt` 事件不含) | ✅ 主動呼叫 `ertangle_status` 與 `ertangle_send(to="coord")` 回報 |
| **Antigravity** | ❌ 無效 (`agy acp` 忽略 `initialize.instructions`) | ✅ ACP 首則 prompt 前置生效 (`UserPrompt` 事件不含) | ✅ 靠第二層前置說明主動呼叫 `ertangle_status` 與 `ertangle_send(to="coord")` 回報 |
| **Codex** | 未單獨實測 (受限於回合授權) | `thread/start` 的 `developerInstructions`(schema 查證支援，握手成功) | ⚠️ `initialize` + `thread/start` (含 `developerInstructions`)成功；`turn/start` 因本機無有效授權停止 |

## Agent 錯誤顯示 (T23)

2026-11-13，codex-cli 0.167.2，登入已失效 (refresh token 被撤銷)。直接對 `codex app-server` 送 `initialize` → `thread/start` → `turn/start`，握手都成功，`turn/start` 的回應也是成功 (`turn.error: null`、`status: inProgress`)。錯誤在回合開始後約 0.7 秒才出現，**兩條路徑都會來，內容相同**，順序為：

1. `thread/status/changed`(`status.type = "systemError"`)
2. `error` 通知：`params.error = {"message": "Your access token could not be refreshed because your refresh token was revoked. Please log out and sign in again.", "codexErrorInfo": "unauthorized", "additionalDetails": null, "misalignment": null}`，另有 `willRetry: false`、`threadId`、`turnId`
3. `turn/completed`：`turn.status = "failed"`，`turn.error` 與上面的 `params.error` 同形同內容

注意 `error` 通知的訊息在 `params.error.message` (不是 `params.message`)。stderr 只有 tracing 日誌 (含 ANSI 色碼)，例如 `401 Unauthorized … auth error code: token_revoked`，沒有人類可讀的錯誤摘要。

自動測試 (假 codex／假 agent，`crates/ertangle-adapter/tests/codex_fake.rs`)：`error` 通知＋`turn/completed` 只回報一次；認證錯誤附 `codex login`；非 0 結束帶出 stderr 最後幾行 (ANSI 已剝除)；stderr 灌 20000 行不會卡住握手。

未驗證：
- 沒有在 TUI 裡目視確認錯誤文字與 status note 的呈現；也沒有用真實 `ertangle spawn --kind codex` 跑完整條路徑。
- `willRetry: true` 的 `error` 通知目前直接略過 (不顯示)，沒有真實樣本。
- Claude `is_error` 與 ACP 的錯誤只改了程式，沒有專屬測試；stderr 收集對它們共用同一個實作 (只有 codex 有端到端測試)。

## T22、T23 實機 (2026-11-13)

| 驗證項 | 做法 | 結果 |
| --- | --- | --- |
| T22：清掉舊版留下、分支已刪的 handoff session | 新版 daemon 啟動後對 8 筆卡住的 session 執行一般 `ertangle rm` (不加 `--force`) | 全部移除；沒有殘留 worktree 或 `ertangle/ses_*` 分支 |
| T23：Codex 登入失效時顯示原因 | 未登入環境 `ertangle spawn --kind codex --prompt "say hi"` | session 變 `failed`；逐字稿與 status note 各一次「codex 錯誤：Your access token could not be refreshed… (請執行 `codex login`)」 |

沒有驗證：TUI 畫面目視 (只看了 `ertangle sessions` 與 event log)。

## T24 交接後訊息轉送 (實機，2026-11-13)

opencode (本機 gemma4)在暫存 git repo：`spawn hx-a` → `handoff hx-a --to opencode` → `ertangle send --to hx-a "ping-t24…"`。

| 驗證項 | 結果 |
| --- | --- |
| `session_handed_off` 事件 | 有，`from` 舊 session、`to` 新 session |
| 送給舊 label 的訊息 | log 裡 `to` 是接手的 session，body 開頭「(原收件者 hx-a，已交接)」 |
| 傳送端回覆 | 「收件者 hx-a 已交接，訊息改投給接手的 hx-a→opencode (…)」 |

沒有驗證：接手者正忙時訊息只留在收件匣、不會喚醒 (既有的匯流排規則，非 T24 範圍)；自動交接路徑。

## T25 忙碌時收到訊息的補提醒 (實機，2026-11-14)

opencode (本機 gemma4)：`spawn nx --prompt "用 shell 執行 sleep 25…"`，忙碌中 `ertangle send --to nx "ping-t25：請用 ertangle_send 回覆 human 一句 pong"`。

| 驗證項 | 結果 |
| --- | --- |
| 忙碌時送達 | 不打斷，只留在收件匣 |
| 回到 idle | 收到一則「[ertangle 未讀訊息提醒] … id=msg_… 來自 human (chat)」 |
| agent 反應 | 呼叫 `ertangle_inbox` 後以 `ertangle_send` 回覆 human「pong」(`reply_to` 指向原訊息) |

沒有驗證：調高成本上限後的補提醒 (只有單元測試)。

## T76 Flow 儀表板 (實機，2026-11-21)

環境：Linux、tmux 3.4 (獨立的 tmux server，`XDG_RUNTIME_DIR`／`XDG_STATE_HOME`／`XDG_CONFIG_HOME` 指向暫存目錄的獨立 daemon)、opencode (本機 gemma4)。session 樹：`coord` (coordinator)、`worker/a`、`worker/b`、已 `kill` 的 `worker/old`；任務 T1 指派給 `worker/a`，T2 指派給 `worker/b` 並相依 T1 (blocked)；訊息含 handoff、status×2、`#review` 頻道、broadcast、question。

| 驗證項 | 做法 | 結果 |
| --- | --- | --- |
| 開啟 Flow | 一般模式按 `F` | 中央窗格標題 `Flow`，摘要列、節點板、邊列表三區都有內容 |
| 節點板 | 目視 | 依父子關係縮排；`coord` 收到訊息後顯示 `✉2`；`worker/a`、`worker/b` 顯示 `T:1`；已結束 session 折疊成 `… 1 ended (E to show)` |
| `E` 顯示已結束 | 按 `E` | `worker/old` 以 `✓` 出現在節點板，再按一次折疊回去 |
| 依賴邊 | 目視 | `worker/b ┄┄blocked by┄┄▶ worker/a`，附任務 id |
| 訊息邊 | 目視 | `*` (broadcast)、`#review` (頻道)、`status×2` 合併計數、預覽與相對時間正確 |
| 閃爍標記 | 新送一則 question 後 0.4 秒與 3 秒各擷取一次 | 0.4 秒時有 `⚡`，3 秒後消失 |
| 游標與 `Enter` | `j` 後 `Enter` | 反白移到 `worker/a`；切回 Transcript 並選取 `worker/a` |
| `Esc` | Flow 中按 `Esc` | 回到開啟前的視圖 |
| 80×24 | `tmux resize-window -x 80 -y 24` | 窄版版面：隱藏多數標記與預覽，不會 panic |

修正：初版摘要列在 80 與 120 欄寬都被截斷，任務數看不到；改成省略為 0 的計數。80×24 時最後一項仍可能被截斷 (以 `…` 結尾)。

沒有驗證：`⧗` 佇列標記 (opencode 回合結束太快，無法讓 prompt 留在佇列，只有單元測試)；`?`／`!perm` 標記；多主機時的主機名稱；edge 上限 40 條 (只有單元測試)。

另外發現 (非 T76 範圍)：連續對同一個 opencode session 送兩個 prompt 時，daemon log 出現 `Session already has an active ACP prompt`，session 狀態回到 `idle` 且佇列是空的，第二個 prompt 疑似遺失而不是排入佇列。

## T77 Flow 動態節點圖 (實機，2026-11-21)

環境同 T76 (獨立 tmux server 與暫存目錄 daemon、opencode 本機 gemma4)。session 樹：`coord` 下有 `worker/a`、`worker/b`、`worker/c`，`worker/a` 下有 `tester/a2`，另有一個沒有 parent 的 `tester/a1`；T4 指派給 `worker/b` 並相依 `worker/a` 的 T3。

| 驗證項 | 做法 | 結果 |
| --- | --- | --- |
| 樹狀版面 | 120×40 按 `F` | 三層方框，父節點置中於子節點上方，層與層之間以 Braille 線相連；游標節點為雙線框 |
| 依賴邊 | 目視 | `worker/b` 與 `worker/a` 之間是紅色虛線 |
| 訊息封包 | `send --from coord --to worker/c` 後 0.6 秒擷取 | `◆` 帶殘影在 `coord` 往 `worker/c` 的連線上 |
| broadcast | `send --from coord --broadcast` 後 0.5 秒擷取 | 畫面上同時有 5 個 `◆`，對應其他 5 個節點 |
| 呼吸效果 | 每 0.3 秒擷取忙碌節點的框線顏色 | 256 色索引依序變化 22 → 34 → 40 → 28 |
| 80×24 | `resize-window -x 80 -y 24` | 三層樹仍以圖形顯示 (層間 1 列)，邊列表因高度不足省略 |
| `l` 切換 | 按 `l` 兩次 | 切到 T76 清單再切回圖形 |
| 游標與 `Enter` | `j` 後 `Enter` | 雙線框移到 `worker/a`；切回 Transcript 並選取 `worker/a` |

修正：一開始方框內的狀態文字 (例如 `busy`) 與狀態符號重複，還會被截斷；改成只有沒有其他標記時才顯示狀態文字。層間最少列數從 2 改成 1，3 層的樹才能在 80×24 顯示。

沒有驗證：`WaitingApproval` 的閃爍 (只有單元測試)；頻道訊息 (設計上不畫封包，只出現在邊列表)；60 ms 重繪頻率對遠端 SSH 頻寬的實際影響。
