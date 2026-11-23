# 改善計畫票

來源：2026-11-13 對 `main`(`9575aa7`)的一次程式碼檢視。每張票各自記錄問題、證據、分析邏輯、實作方式與驗收條件。

## 票一覽

| 票 | 標題 | 優先度 | 相依 |
| --- | --- | --- | --- |
| [T01](T01-daemon-restart-state.md) | daemon 重啟後還原狀態並維持 seq 連續 (已完成) | P0 | — |
| [T02](T02-merge-safety.md) | merge 前置檢查、衝突還原與 auto-commit 範圍 (已完成) | P0 | — |
| [T03](T03-gc-unmerged-work.md) | gc 不得刪除未合併的成果 (已完成) | P0 | — |
| [T04](T04-policy-engine-effective.md) | 讓權限規則引擎在所有政策下都生效 (已完成) | P1 | — |
| [T05](T05-remote-mode-security.md) | 遠端模式與本機 socket 的安全強化 (已完成) | P1 | — |
| [T06](T06-ci-and-integration-tests.md) | CI 與 adapter 整合測試 (已完成) | P1 | — |
| [T07](T07-daemon-concurrency.md) | daemon 併發：鎖內寫檔、無上限佇列、鎖中毒 (已完成) | P2 | T06 |
| [T08](T08-event-log-rotation.md) | event log 輪替與寫入錯誤回報 (已完成) | P2 | T01 |
| [T09](T09-split-large-modules.md) | 拆分 daemon 與 TUI 的大檔 (已完成) | P2 | T06 |
| [T10](T10-docs-and-makefile.md) | 文件與實作同步、Makefile 可攜性 (已完成) | P3 | — |
| [T11](T11-agent-config-and-model.md) | 設定檔與每個 agent 的 model／參數設定 (已完成) | P1 | — |
| [T12](T12-permission-settings.md) | 權限的設定、顯示與子 agent 上限 (已完成) | P1 | T11 |
| [T13](T13-tui-layout-modes.md) | TUI 版面模式：三欄／兩欄／上方 session 分頁 (已完成) | P2 | — (持久化需 T11) |
| [T14](T14-session-resume.md) | Session 接續：處理程序死掉或 daemon 重啟後接回原對話 (已完成；Codex 只驗到協定層，見 `docs/VERIFICATION.md`) | P1 | — |
| [T15](T15-notifications.md) | 回合結束、等待核准、額度用完時通知使用者 (已完成) | P1 | — |
| [T16](T16-transcript-search.md) | 在逐字稿與 diff 裡搜尋 (已完成) | P2 | — |
| [T17](T17-budget-limits.md) | 成本與子 agent 數量上限 (已完成；只用 mock 驗證，未用真實 agent 跑超限) | P1 | — (成本累計與 T14 有交互) |
| [T18](T18-task-board.md) | 任務板：相依性解鎖與獨立視圖 (已完成) | P2 | — |
| [T19](T19-daemon-prompt-queue.md) | 把 prompt 排隊移到 daemon (已完成；超限時保留佇列、調高上限後恢復) | P2 | — (與 T17 有交互) |
| [T20](T20-collab-instructions.md) | 把協作說明注入每個 agent (已完成) | P2 | — |
| [T21](T21-shell-completions.md) | Shell 自動完成 (已完成) | P3 | — (session 自動完成會用到 T14 的 `resume`) |
| [T22](T22-shared-worktree-remove.md) | 共用 worktree 的 session 在分支刪掉後無法移除 (已完成) | P2 | — |
| [T23](T23-surface-agent-errors.md) | agent 回合失敗時把錯誤原因顯示出來 (已完成；TUI 目視未做，見 `docs/VERIFICATION.md`) | P1 | — |
| [T24](T24-handoff-message-forwarding.md) | 交接後，發給舊 session 的訊息轉給接手的 session (已完成) | P2 | — |
| [T25](T25-unread-nudge-on-idle.md) | 收件者忙完後提醒未讀訊息；超限判斷看實際收件者 (已完成) | P2 | T24 |
| [T26](T26-runtime-model-effort.md) | 執行期切換 model／effort，並可從 TUI 寫入預設值 (已完成) | P1 | T11 |
| [T27](T27-i18n-english.md) | 多國語系：以 gettext `.po` 提供英文介面 (已完成) | P2 | — |
| [T28](T28-remote-tailscale-and-reconnect.md) | 遠端連線韌性與 Tailscale 整合：自動偵測 Tailscale 位址、Token 持久化與 TUI 自動斷線重連 (已完成) | P1 | T05 |
| [T29](T29-multi-host-tui.md) | TUI 多主機聚合視圖 (`[remotes]` 設定與跨機 Session 操控) (已完成) | P2 | T28 |
| [T30](T30-mobile-companion-web.md) | Web 版支援 (`--web` 零相依 HTTP/WebSocket 與內嵌 Mobile PWA + 終端機 QR Code) (已完成) | P2 | T28 |
| [T31](T31-role-selection-and-collab-scope.md) | TUI 支援指定 Role 與協作指引英文化分流 (已完成) | P2 | T20, T27 |
| [T32](T32-subagent-and-bg-process-visibility.md) | Subagent 與背景執行程式／長耗時指令的 TUI 即時可觀測性與處理程序群組控管 (已完成) | P1 | — |
| [T33](T33-claude-permission-forwarding-and-mcp-redaction.md) | Claude Code 逐項權限核准轉發與 MCP 回傳全面秘密遮罩 (已完成) | P1 | T04 |
| [T34](T34-conflict-resolution-and-review-loop.md) | 智慧合併衝突解決迴圈、合併前驗證與自動審查守門員 (已完成) | P2 | T02, T18 |
| [T35](T35-diff-file-tree-and-hunk-actions.md) | Diff 視圖檔案清單導覽、單檔摺疊與局部 Hunk 退回 (已完成) | P2 | — |
| [T36](T36-playbooks-and-session-export.md) | 多 Agent 團隊範本 (Playbooks) 與 Session 協作報告匯出 (`:export`) (已完成) | P3 | T11, T31 |
| [T37](T37-token-efficient-agent-communication.md) | 跨 Agent 極簡英文通訊協定與禁止客套往返迴圈 (已完成) | P2 | T20, T31 |
| [T38](T38-quota-aware-spawn-and-ade-skill.md) | 額度感知的 spawn、即時額度警告與可安裝的 termade-ade skill (已完成) | P2 | T20, T37 |
| [T39](T39-kill-pgid-misparse-logout.md) | `kill -SIG -<pgid>` 被誤解析成 `kill -1`，結束 session 時整個桌面被登出 (已完成) | P0 | T32 |
| [T40](T40-web-project-scope-and-overview.md) | Web 版專案範疇切換、QR Code 帶入專案與「總覽」分頁 (已完成) | P2 | T30, T32 |
| [T41](T41-tui-project-panel-and-grid.md) | TUI 右欄改為「Project」面板與中央四格群組檢視 (已完成) | P2 | T13, T32 |
| [T42](T42-tui-skill-toggle-and-command.md) | TUI 設定選單的 `inject_skill` 開關與 `:skill` 指令 (已完成) | P3 | T38 |
| [T43](T43-web-model-catalog.md) | Web 版模型清單過時，與 TUI 不一致 (已完成) | P2 | T30 |
| [T44](T44-web-list-models.md) | 網頁版透過 daemon 取得與 TUI 同源的 model 清單 (`list_models`) (已完成) | P2 | T43 |
| [T45](T45-tui-quota-popup.md) | `:quota` 改為額度浮層，列出已安裝 agent 並以 progress bar 呈現 (已完成) | P3 | — |
| [T46](T46-auto-resume-shared-target-quota-notice.md) | daemon 重啟自動接續、worktree 共用 cargo target、額度提早提醒 (已完成) | P2 | T01, T14, T38 |
| [T47](T47-agent-token-savings.md) | 降低 Agent 的 token 用量：長對話提醒、省 token 提示、注入內容瘦身 (待人工確認) | P2 | T38, T42 |
| [T49](T49-spawn-agent-kind-resolution.md) | Resolve the agent kind of a spawn from config and the parent instead of hard-coding opencode (已完成) | P2 | — |
| [T50](T50-handoff-identity-change.md) | Change a session's identity (role and parent) through a handoff (已完成) | P2 | T49 |
| [T51](T51-antigravity-quota.md) | Show the quota of Gemini (Antigravity) sessions (已完成) | P2 | — |
| [T52](T52-sast-security-hardening.md) | SAST 靜態安全分析修復 (WebSocket 長度上限、常數時間比對、Web 檔案存取隔離、DOM XSS 與暫存檔安全) (已完成) | P1 | — |
| [T53](T53-agent-scoped-quota-warn.md) | 額度警告 (`QUOTA_WARN` / `QUOTA_NOTICE`) 依 Agent 種類精準分發與明確化訊息語意 (已完成) | P2 | T38, T46 |
| [T54](T54-codex-model-list-pipe-deadlock.md) | 修復 Codex 模型清單因 stdout pipe 阻塞而空白 (已完成) | P1 | — |
| [T55](T55-rust-clean-code-conventions.md) | 建立 Rust 程式寫作慣例並減少 production unwrap 與 unsafe | P2 | — |
| [T56](T56-selectable-merge-strategy.md) | 讓使用者選擇 worktree 合併策略 (merge / squash / rebase) | P2 | — |
| [T57](T57-rename-to-ertangle.md) | 專案改名：termade → ertangle (ER=EPR) | P1 | — |
| [T58](T58-resume-picker-scope-and-parent.md) | Scope tabs and parent context in the session resume picker | P2 | — |
| [T59](T59-open-project-from-any-directory.md) | Open a project from any directory instead of starting ertangle inside it | P2 | — |
| [T60](T60-english-first-convention.md) | Adopt English-first tickets and code comments | P3 | — |
| [T61](T61-gemma-comment-translation.md) | Translate legacy code comments to English with gemma4:std | P3 | T60 |
| [T62](T62-question-tool-support-for-all-agents.md) | Render interactive question tool calls for every agent | P1 | — |
| [T63](T63-complete-question-tool-coverage.md) | Complete question tool coverage (web, Claude native, multi-question) | P1 | T62 |
| [T64](T64-web-question-banner-visibility.md) | Show the web question banner outside tab views | P1 | T63 |
| [T65](T65-web-question-card-usability.md) | Make the web pending-question card usable | P1 | T64 |
| [T66](T66-tui-question-picker-usability.md) | Make the TUI question picker usable | P1 | T63 |
| [T67](T67-tui-question-auto-open.md) | Auto-open the TUI question picker, docked above the input | P1 | T66 |
| [T68](T68-web-image-attachments.md) | Send images with prompts from the web UI | P2 | T65 |
| [T69](T69-daemon-restart.md) | Daemon restart that keeps sessions resumable | P2 | — |
| [T70](T70-inbox-cursor-and-dead-assignee.md) | Persist inbox cursors; warn on dead task assignee | P2 | — |
| [T71](T71-web-review-question-stale.md) | Web: question card stays in Review after answering | P1 | — |
| [T72](T72-tui-picker-remote-resolve-and-editing.md) | TUI picker: close on remote answer, editable custom answer | P1 | — |
| [T73](T73-session-cleanup.md) | Session cleanup: auto-archive merged workers, retention, prune | P2 | — |
| [T79](T79-antigravity-quota-pools.md) | Antigravity multi-model quota false positive | P1 | T51, T53 |
| [T74](T74-shell-exec-and-web-commands.md) | Shell command execution in session cwd + web command palette | P2 | — |
| [T75](T75-thinking-display-without-emoji.md) | Emoji-free animated thinking display with folded preview | P2 | — |
| [T76](T76-tui-session-flow-dashboard.md) | TUI session flow dashboard: live session status and message flow | P3 | — |
| [T77](T77-flow-animated-node-graph.md) | Animated node graph for the Flow view | P3 | T76 |
| [T78](T78-command-autocomplete-and-project-folder-picker.md) | Subcommand autocomplete, local folder picker for projects, Space P shortcut | P2 | T59 |
| [T80](T80-dead-peer-message-delivery.md) | Prevent silent delivery to quota-exhausted peers (已完成) | P1 | T24 |
| [T81](T81-quota-reached-reset-stale.md) | Stale `Quota::reached` after rate-limit window resets (已完成) | P1 | — |
| [T82](T82-tui-right-pane-flow-graph.md) | Replace right-pane Bus display with live Agent/Session Flow graph (已完成) | P2 | T77 |
| [T83](T83-web-image-and-artifact-preview.md) | Web UI: Markdown image syntax and agent artifact file preview (已完成) | P1 | — |
| [T84](T84-tui-header-flow-tab-and-quota-filter.md) | TUI header: add `Flow (F)` tab and filter `3p-*` quota windows (已完成) | P2 | T79 |
| [T85](T85-tui-unified-sessions-and-adaptive-flow.md) | TUI: unified `Projects & Sessions` sidebar and adaptive 3-column Flow (已完成) | P2 | T82, T84 |
| [T86](T86-tui-layout-cycling-and-auto-sync.md) | TUI: sync layout cycling (`L` / `b` / `:layout`) with adaptive 3-column Flow (已完成) | P1 | T85 |
| [T87](T87-tui-sidebar-keyboard-navigation.md) | TUI: keyboard navigation and expand/switch for collapsed `Projects & Sessions` (已完成) | P1 | T85 |
| [T88](T88-tui-confirm-resume-on-stopped-prompt.md) | TUI: confirm before resuming and sending prompt to ended/stopped session (已完成) | P1 | — |
| [T89](T89-tui-split-flow-bus-and-orthogonal-links.md) | TUI: vertical split for Flow + Bus in right pane and orthogonal Flow graph links (已完成) | P2 | T82 |
| [T90](T90-sast-web-cors-ws-origin-and-artifact-scope.md) | SAST: restrict CORS, enforce WebSocket Origin check, and tighten artifact file scope (已完成) | P1 | T83 |
| [T91](T91-sast-exec-pgid-timeout-and-secrets-redaction.md) | SAST: kill process group on `handle_exec` timeout and expand `redact_secrets` (已完成) | P1 | T74 |
| [T92](T92-perf-web-markdown-cache-and-tui-flow-allocs.md) | Perf & safety: Web Markdown turn cache (`$` fix) and stack-allocated Flow elbows (已完成) | P2 | T89 |

## Language convention

From T58 onward, new tickets and inline code comments are written in English. Older Chinese tickets stay as historical records. See `docs/agents/issue-tracker.md`.

T01–T10 是修正既有行為；T11–T13 是缺少的功能，於 2026-11-13 追加。

T14–T21 也是缺少的功能，於 2026-11-13 對 `main`(`f3fc576`)再檢視一次後追加。這一批只讀了程式碼與各 agent 的 `--help`，沒有實機重現；每張票的「分析邏輯」都列出了沒有確認的部分，動手前先查證。

優先度：P0 會造成資料遺失或狀態錯亂；P1 是安全或防止回歸；P2 是穩定度與可維護性；P3 是整理。

## 建議順序

1. T02、T03：改動小、彼此獨立，先擋住會弄丟成果的路徑。
2. T01：資料正確性的核心，改動最大。
3. T06：之後的重構 (T07、T09)需要它當安全網。
4. T11：設定檔與 model。它引入的 `config.toml` 是 T12 與 T04 的基礎。
5. T12、T04、T05：權限與安全。
6. T07、T08、T09、T10。

T14–T21 已於 2026-11-13 全部合併；T22、T23 是合併過程中實際遇到的問題，同日追加。各票未做的手動／實機驗證見 `docs/VERIFICATION.md`。

### T14–T21 的建議順序

1. T15：改動只在 TUI，最小，日常使用最有感。
2. T14：影響最大。先做「記下原生 id」(第 1 步)，那一步獨立、沒有風險，之後的 session 才有 id 可以接續。
3. T17：放著讓 agent 自己跑之前要有的保護。排在 T14 之後，成本累計的處理才不用改兩次。
4. T18 的 A (相依性解鎖)與 T20：兩者一起決定 agent 之間能不能不靠人就接力下去。
5. T19、T16、T18 的 B (任務視圖)、T21。

## 這次檢視怎麼做的

**讀了什麼**
- 全部：`README.md`、`docs/DESIGN.md`、`Cargo.toml`、`Makefile`。
- `crates/termade-daemon/src/lib.rs`：路徑、`replay_log`、`Shared::emit`／`ingest`、`Hello`／`Prompt`／`WorktreeMerge`／`WorktreeGc` 處理、所有 git 輔助函式、`handle_conn`、`run_with_options`。
- `crates/termade-core/src/lib.rs`：`PolicyEngine`、`redact_secrets`。
- `crates/termade-adapter/src/acp.rs`：`on_permission`。其餘 adapter 只用 grep 看核准相關的行。
- 其餘檔案只看了函式／型別大綱與行數。

**跑了什麼**
- `cargo test`：58 個通過 (adapter 12、core 5、daemon 7、protocol 3、tui 31、binary 0)。
- `cargo clippy --all-targets -- -D warnings`：無警告。
- `cargo fmt --check`：通過。
- 沒有實際啟動 daemon 或任何 agent；所有問題都是從程式碼推論，沒有實機重現。

**判斷方式**
- 先比對設計文件的承諾 (事件可重播、重連無縫、規則引擎擋危險指令、遠端可用)與程式碼實際走的路徑，找落差。
- 再看會動到使用者資料的地方 (git 操作、event log)在失敗時留下什麼狀態。
- 最後看量化指標：檔案行數、測試分布、`unwrap` 數量。

**沒有檢視的範圍**
- `termade-tui` 的互動邏輯與 `markdown.rs` 的渲染正確性。
- `claude.rs`、`codex.rs`、`pty.rs` 的協定處理細節。
- `mcp.rs` 的工具實作。
- 效能目標 (冷啟動、RSS、事件延遲)沒有量測。

## 更正紀錄

檢視當下的口頭結論說「Claude 和 Codex 都不轉發核准」。寫票時再看程式碼，Codex 其實有轉發路徑 (`codex.rs:132` 送出 `AgentEvent::Permission`，`codex.rs:307` 實作 `resolve_permission`)，只是 README 註明尚未實機驗證。不轉發的只有 Claude (`claude.rs:267-269`)。T04 以更正後的版本為準。
