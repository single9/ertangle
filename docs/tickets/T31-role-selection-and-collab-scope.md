# T31 — TUI 支援指定 Role 與協作指引英文化分流

優先度：P2　範圍：`termade-tui`、`termade-daemon`　相依：T20, T27

Status: done

## 問題

1. **TUI 建立 Session 時無法指定 Coordinator**：TUI (`:spawn` 與 Spawn Modal) 原本硬編碼 `role: Role::Worker`，使用者無法在介面上直接建立協調者 Session，與 `docs/DESIGN.md` 中由 TUI spawn coordinator 的架構衝突。
2. **Worker 缺乏行為邊界指引**：在非 Coordinator 的一般 worker session 中，若未明確約束權責，模型在具有 MCP `termade_spawn` 工具清單時可能誤調用並自行衍生平行處理程序，造成額度無節制燃燒。
3. **Prompt 語言語境不一致**：MCP 工具說明與參數均為純英文，但先前 `collab.rs` 協作指引為繁體中文，中英夾雜會降低各大模型對負向約束與工具邊界的遵循度 (Instruction Following)，且不利於多語言專案。

## 解決方案

1. **TUI 全面支援指定 Role**：
   - 互動選單 (Spawn Modal)：`App` 狀態新增 `spawn_role`（預設 `worker`），支援以 **`Ctrl-R`** 循環切換 `worker` ↔ `coordinator` ↔ `reviewer`。
   - 命令列模式 (`:spawn`)：解析 `--role <role>` 或 `-r <role>`（如 `:spawn --role coordinator claude:opus "任務"`）。
   - 送出 `SpawnRequest` 時正確帶入指定的角色。
2. **協作指引全面英文化與依角色精準分流**：
   - 通用規範英文化：包含 Reporting、Progress、Tasks、Questions、Worktree 等說明。
   - Coordinator 專屬指引：引導使用 `termade_spawn` 與 `termade_task_create` 進行任務拆解與相依性排程，並注意成本與模型選擇。
   - Worker / Reviewer 專屬指引：注入嚴格 Scope 約束，明確指示未經使用者明確同意不要自行呼叫 `termade_spawn` 建立子 session，遇阻礙一律透過 `termade_send` 向父 session 回報。
3. **驗證與測試**：
   - 補齊 `crates/termade-daemon/src/collab.rs` 與 `crates/termade-tui/src/tests.rs` 單元測試。
   - 新增 `make pre-commit` 一鍵檢查目標與本地 Git pre-commit hook。
