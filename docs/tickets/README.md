# 改善計畫票

來源：2026-11-13 對 `main`（`9575aa7`）的一次程式碼檢視。每張票各自記錄問題、證據、分析邏輯、實作方式與驗收條件。

## 票一覽

| 票 | 標題 | 優先度 | 相依 |
| --- | --- | --- | --- |
| [T01](T01-daemon-restart-state.md) | daemon 重啟後還原狀態並維持 seq 連續 | P0 | — |
| [T02](T02-merge-safety.md) | merge 前置檢查、衝突還原與 auto-commit 範圍 | P0 | — |
| [T03](T03-gc-unmerged-work.md) | gc 不得刪除未合併的成果 | P0 | — |
| [T04](T04-policy-engine-effective.md) | 讓權限規則引擎在所有政策下都生效 | P1 | — |
| [T05](T05-remote-mode-security.md) | 遠端模式與本機 socket 的安全強化 | P1 | — |
| [T06](T06-ci-and-integration-tests.md) | CI 與 adapter 整合測試 | P1 | — |
| [T07](T07-daemon-concurrency.md) | daemon 併發：鎖內寫檔、無上限佇列、鎖中毒 | P2 | T06 |
| [T08](T08-event-log-rotation.md) | event log 輪替與寫入錯誤回報 | P2 | T01 |
| [T09](T09-split-large-modules.md) | 拆分 daemon 與 TUI 的大檔 | P2 | T06 |
| [T10](T10-docs-and-makefile.md) | 文件與實作同步、Makefile 可攜性 | P3 | — |
| [T11](T11-agent-config-and-model.md) | 設定檔與每個 agent 的 model／參數設定 | P1 | — |
| [T12](T12-permission-settings.md) | 權限的設定、顯示與子 agent 上限 | P1 | T11 |
| [T13](T13-tui-layout-modes.md) | TUI 版面模式：三欄／兩欄／上方 session 分頁 | P2 | —（持久化需 T11） |

T01–T10 是修正既有行為；T11–T13 是缺少的功能，於 2026-11-13 追加。

優先度：P0 會造成資料遺失或狀態錯亂；P1 是安全或防止回歸；P2 是穩定度與可維護性；P3 是整理。

## 建議順序

1. T02、T03：改動小、彼此獨立，先擋住會弄丟成果的路徑。
2. T01：資料正確性的核心，改動最大。
3. T06：之後的重構（T07、T09）需要它當安全網。
4. T11：設定檔與 model。它引入的 `config.toml` 是 T12 與 T04 的基礎。
5. T12、T04、T05：權限與安全。
6. T07、T08、T09、T10。

## 這次檢視怎麼做的

**讀了什麼**
- 全部：`README.md`、`docs/DESIGN.md`、`Cargo.toml`、`Makefile`。
- `crates/termade-daemon/src/lib.rs`：路徑、`replay_log`、`Shared::emit`／`ingest`、`Hello`／`Prompt`／`WorktreeMerge`／`WorktreeGc` 處理、所有 git 輔助函式、`handle_conn`、`run_with_options`。
- `crates/termade-core/src/lib.rs`：`PolicyEngine`、`redact_secrets`。
- `crates/termade-adapter/src/acp.rs`：`on_permission`。其餘 adapter 只用 grep 看審批相關的行。
- 其餘檔案只看了函式／型別大綱與行數。

**跑了什麼**
- `cargo test`：58 個通過（adapter 12、core 5、daemon 7、protocol 3、tui 31、binary 0）。
- `cargo clippy --all-targets -- -D warnings`：無警告。
- `cargo fmt --check`：通過。
- 沒有實際啟動 daemon 或任何 agent；所有問題都是從程式碼推論，沒有實機重現。

**判斷方式**
- 先比對設計文件的承諾（事件可重播、重連無縫、規則引擎擋危險指令、遠端可用）與程式碼實際走的路徑，找落差。
- 再看會動到使用者資料的地方（git 操作、event log）在失敗時留下什麼狀態。
- 最後看量化指標：檔案行數、測試分布、`unwrap` 數量。

**沒有檢視的範圍**
- `termade-tui` 的互動邏輯與 `markdown.rs` 的渲染正確性。
- `claude.rs`、`codex.rs`、`pty.rs` 的協定處理細節。
- `mcp.rs` 的工具實作。
- 效能目標（冷啟動、RSS、事件延遲）沒有量測。

## 更正紀錄

檢視當下的口頭結論說「Claude 和 Codex 都不轉發審批」。寫票時再看程式碼，Codex 其實有轉發路徑（`codex.rs:132` 送出 `AgentEvent::Permission`，`codex.rs:307` 實作 `resolve_permission`），只是 README 註明尚未實機驗證。不轉發的只有 Claude（`claude.rs:267-269`）。T04 以更正後的版本為準。
