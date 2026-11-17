# T36 — 多 Agent 團隊範本 (Playbooks) 與 Session 協作報告匯出 (`:export`)

優先度：P3　範圍：`termade-core`、`termade-daemon`、`termade-tui`、`src/`　相依：T11, T31

Status: ready-for-agent

## 問題

1. **重複啟動多角色團隊需手動逐一設定**：
   - 雖然 T31 已支援在 `:spawn` 指定 `--role coordinator|worker|reviewer`，但若開發者常使用固定的多 Agent 協作模式 (例如「架構師 Coordinator + 實作者 Worker + 審查者 Reviewer」或「雙模型交叉 Code Review」)，每次仍需重複輸入多道 `spawn` 與 `task` 指令。
2. **協作過程與成果無法一鍵匯出為 Markdown 報告**：
   - 當一個 Session (或整個父子協作樹) 完成任務後，開發者常需要將「需求背景、對話摘要、變更統計、各 Session 花費與最終結論」貼到 GitHub PR 描述、Issue 或儲存為專案的技術紀錄 (`docs/adr/`)。目前只能用 `Y` (`app.rs:813`) 複製單一畫面文字，缺乏結構化的 `:export` 與 `termade export` 命令。

## 證據

- `crates/termade-daemon/src/config.rs:115-145`：`Config` 目前包含 `tui`、`web`、`remotes`、`limits`、`handoff`、`collab`、`agents`，尚無 `[playbooks]` 團隊範本定義。
- `crates/termade-tui/src/commands.rs` 與 `src/main.rs`：目前有 `replay` (印出原始 JSONL 事件)，但沒有將 Session 對話、工具呼叫摘要、任務狀態與成本彙整輸出為人類可讀 Markdown 文件的 `export` 子命令。

## UX 設計原則

1. **開箱即用的內建 Playbook + 漸進式選擇器**：
   - 即使使用者沒有在 `config.toml` 手寫任何設定，`termade` 也內建 3 個高實用價值的預設範本：
     - `coord-review`：1 個 Coordinator + 1 個 Reviewer (適合由 Coordinator 動態拆解並派生 Worker，完成後由 Reviewer 把關)。
     - `tdd-pair`：1 個測試工程師 Worker + 1 個實作工程師 Worker + 1 個 Coordinator。
     - `dual-review`：2 個不同 Agent 的 Reviewer 平行審查同一份變更。
   - 在 TUI 輸入 `:playbook` (不帶參數) 或按 `Space s b` 時，直接彈出 Picker 選單列出所有可用範本與簡短說明，選中後輸入一句目標需求即可一鍵啟動。
2. **乾淨可直接貼上 PR 的匯出格式 (`:export`)**：
   - 在 TUI 輸入 `:export` (不帶參數) 時，自動將目前選取 Session (及其子 Session 樹) 匯出至 `.termade/exports/<label>-<timestamp>.md`，並**同時透過 OSC 52 複製到系統剪貼簿**，狀態列提示「`已匯出至 .termade/exports/... 並複製到剪貼簿`」，省去手動開檔複製的步驟。

## 分析邏輯

1. **Playbooks 設定與展開**：
   - 在 `config.toml` 支援定義 `[playbooks.<name>]`：
     ```toml
     [playbooks.tdd]
     description = "TDD 協作團隊 (Coordinator + Test Worker + Impl Worker)"
     [[playbooks.tdd.sessions]]
     label = "coord"
     kind = "claude"
     role = "coordinator"
     worktree = false
     prompt = "請協調團隊完成以下目標：{{goal}}"
     [[playbooks.tdd.sessions]]
     label = "reviewer"
     kind = "opencode"
     role = "reviewer"
     worktree = false
     ```
   - 執行 Playbook 時，先以第一個 `role == coordinator` (或清單第一筆) 為主 Session 建立，其餘成員設定 `parent` 指向主 Session，並將使用者輸入的目標文字替換進 `{{goal}}`。
2. **Session 報告匯出 (`export_markdown`)**：
   - 在 `termade-core` 實作純函式 `State::export_markdown(root_id: &str, include_tree: bool, diff: Option<&str>) -> Result<String>`：
     1. **標題與元資料表**：Session Label、Agent / Model / Effort、Role、Branch、狀態、總耗費 (`$USD` 與 Input/Output Tokens)。
     2. **子 Session 階層表** (當 `include_tree == true` 且有後代時)：列出所有後代 Session 的 Label、Role、Model、狀態與個別花費。
     3. **關聯任務清單**：該 Session 樹建立或負責的 Tasks 與最終狀態。
     4. **對話與執行紀錄**：整理後的逐字稿 (預設摺疊連續工具呼叫，保留失敗工具與關鍵輸出)。
     5. **Git Diff 摘要** (選填)：若傳入 `diff`，附加於報告末尾的 ` ```diff ` 區塊。
     6. **安全過濾**：全篇輸出統一經過 `termade_core::redact_secrets` 遮罩，確保匯出貼到 GitHub PR 時不會外洩金鑰。
3. **未實機確認的部分**：
   - 當 Playbook 指定的某個 `AgentKind` 在本機未安裝時 (且 `transport` 非 `pty`)，若部分建立成功、部分失敗會留下不完整的團隊；因此在啟動前應先檢查所有成員的 `AgentKind` 是否可用。

## 實作方式

1. **Playbook 設定與內建範本 (`crates/termade-daemon/src/config.rs`)**：
   - 新增 `PlaybookConfig` 與 `PlaybookMember`，實作 `builtin_playbooks()` 並允許使用者於 `config.toml` 的 `[playbooks]` 新增或覆寫同名範本。
2. **Markdown 匯出器 (`crates/termade-core/src/lib.rs`)**：
   - 實作 `State::export_markdown`，包含父子樹遞迴收集、逐字稿格式化與 `redact_secrets` 過濾。
3. **TUI 與 CLI 整合 (`crates/termade-tui/src/commands.rs`、`menu.rs`、`src/main.rs`)**：
   - TUI 新增 `:playbook [<name> <goal>]` (無參數時開 Picker) 與 `:export [<path>]`。
   - CLI 新增 `termade playbook <name> "<goal>"` 與 `termade export <session> [--tree] [--diff] [-o <path>]`。

## 驗收條件

- 測試：`State::export_markdown` 對含有父 Session 與 2 個子 Session、任務看板及逐字稿 (含模擬金鑰 `sk-1234567890abcdefghijklmnop`) 的 `State` 匯出 Markdown，驗證輸出包含父子階層表、總成本加總、任務狀態，且金鑰已被遮罩為 `<REDACTED>`。
- 測試：`Config::load` 能正確讀取自訂 `[playbooks]` 並與內建範本合併；替換 `{{goal}}` 後能產生對應的 `SpawnRequest` 清單。
- 同步更新 `help.rs`、`COMMANDS`、`locales/`、`README.md`、`README_EN.md`。
- `make pre-commit` 與 `cargo test` 全數通過。
