# 驗證紀錄

各里程碑的實機驗證結果（自 `DESIGN.md` 第 13 節移出）。為避免過期，此處不記錄測試數量；請以 `cargo test` 實際結果為準。

## M0 已驗證（實機）

| 驗證項 | 指令 | 結果 |
| --- | --- | --- |
| daemon 常駐 + socket | `termade daemon` / `termade sessions` | OK |
| OpenCode ACP 結構化 | `termade spawn --kind opencode --prompt "..." --transport acp` | transcript 收到 `hello from termade` |
| PTY 萬用後備 | `termade spawn --kind opencode --transport pty` | 逐段剝除 ANSI 後可讀 |
| 訊息匯流排 | `termade send --to <id>` / `--channel review` | 訊息進 `State.messages` |
| 任務板 | `termade task "..."` / `termade tasks` | 任務進 `State.tasks` |
| TUI attach | `termade` | 三欄（Sessions / Transcript / Bus）即時更新、`q` 離線不影響 agent |
| 中斷 / 結束 | `termade cancel <id>` / `termade kill <id>` | ACP `session/cancel`、程序 kill；狀態轉 `exited` |
| 關閉 | `termade shutdown` | 先結束所有 agent 子程序、移除 socket 再離開 |

## M1 已驗證（實機）

| 驗證項 | 指令 | 結果 |
| --- | --- | --- |
| OpenCode ACP | `termade spawn --kind opencode --prompt "..."` | 收到 `termade-ok-opencode`，狀態 busy→idle |
| Antigravity ACP | `termade spawn --kind agy --prompt "..."` | 收到 `termade-ok-agy`（與 OpenCode 同 ACP 方言） |
| Antigravity 工具 | `termade spawn --kind agy --prompt "建立 agy-write.txt"` | 檔案寫入成功、transcript 可讀 |
| Claude stream-json | `termade spawn --kind claude --prompt "..."` | 收到 `termade-ok-claude`、`transport=stream-json`、成本解析正確 |
| Claude 工具 | `termade spawn --kind claude --permission bypass --prompt "跑 echo"` | ToolCall `Bash` + tool_result `termade-tool-ok` |
| Codex app-server 握手 | `termade spawn --kind codex --prompt "..."` | `initialize` + `thread/start` 成功、`transport=app-server`；回合因無授權回 401 |
| 權限政策 | `--permission ask/auto/bypass` | ACP 由 adapter 代核准；Claude 對應 `--permission-mode` / `--dangerously-skip-permissions` |
| transport 別名 | `--transport acp` / `structured` / `pty` | `acp` 與 `structured` 等價（serde alias） |

## M2 已驗證（實機）

| 驗證項 | 指令 / 工具 | 結果 |
| --- | --- | --- |
| MCP stdio 協定 | `termade mcp --session <id>` | `initialize`、`tools/list`（當時 12 個 `termade_*` 工具，現為 14 個）、`tools/call` 正常 |
| 自動注入 MCP | `termade spawn --kind claude/opencode/agy` | ACP 透過 `session/new.mcpServers`、Claude 透過 `--mcp-config`、Codex 透過 `-c mcp_servers.termade.*` 自動掛載 |
| 收件匣與等待 | `termade inbox` / `termade wait` / `termade ask` | 以 label 或 session id 定址、游標去重、阻塞等待與 request/response 交握正常 |
| 閒置喚醒 | 對 `Idle` session 送 `Question` / `Handoff` | daemon 自動將匯流排訊息轉成 prompt 喚醒對方處理 |
| 防呆斷路 | `BusGuard` | 連續重複訊息達上限（3 次）觸發迴圈斷路；每分鐘超過 30 則觸發速率限制 |
| 端到端多 Agent 協作 | `coord` 呼叫 `termade_task_create` + 2×`termade_spawn(worktree=true)` + 2×`termade_wait` | `worker/a` 與 `worker/b` 各自於獨立 worktree 寫檔並 `termade_send(kind=handoff)` 回報，`coord` 收齊後將兩任務標為 `done` 並更新 `status_note` |

## M3–M5 已驗證（實機）

| 驗證項 | 指令 / 功能 | 結果 |
| --- | --- | --- |
| Worktree 清單與 Diff | `termade worktrees` / `termade diff worker/feat` | 正確列出變更檔案數與統一格式 diff（含新增未追蹤檔案） |
| Worktree 自動 Commit + Merge | `termade merge worker/feat` | 自動將 worktree 內未 commit 的變更打包 commit 並 merge 回主分支 |
| Worktree 清理 | `termade gc` | 自動移除已結束 session 的 `.termade/wt/<id>` 與 `termade/<id>` 分支 |
| 行內審查註解（Review 迴圈） | `termade review <session> --location ...` / TUI `c` | 記錄 `ReviewCommented` 事件並向該 agent 送出定位修正 prompt |
| 閒置休眠與喚醒 | `termade sleep <id>` / `termade wake <id>` | 送出 `SIGSTOP` / `SIGCONT`，狀態切換 `sleeping` ↔ `idle`；收到新 prompt 時自動喚醒 |
| 成本統計 | `termade cost` | 逐 session 與總計顯示 USD 與 input/output tokens |
| 事件重播與秘密遮罩 | `termade replay` | 完整重播 `events.jsonl`；`sk-...` / `ghp_...` 自動遮罩為 `<REDACTED>` |
| 遠端 TCP + Token 連線 | `termade daemon --listen 127.0.0.1:17429 --token ...` | 透過 `--remote` + `--token` 建立任務與查詢狀態成功 |
| TUI Markdown 渲染與捲動 | `cargo test -p termade-tui`（TestBackend 全畫面渲染） | 標題／粗體／程式碼／表格正確渲染、`t` 切換原始文字、Diff 邊欄與 `+N -M` 統計、翻頁後恢復跟隨 |

