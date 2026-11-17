# T41 — TUI 右欄改為「Project」面板與中央四格群組檢視

優先度：P2　範圍：`termade-tui`　相依：T13, T32

Status: done

## 問題

1. 三欄版面的右欄原本是 Bus／Tasks 訊息流，對使用者不是必要資訊；使用者需要的是整個專案的狀態：協作者與工作者之間的工作分派、背景 subagent、執行中的程式。
2. 中央一次只能看一個 session 的逐字稿，多個 agent 同時運作時要一直切換才看得到整體情形。

## 證據

- 修正前 `crates/termade-tui/src/draw.rs` 的 `LayoutMode::Three` 右欄固定呼叫 `draw_bus`。
- `Session.active_tools`、`Session.bg_procs`、`Session.parent` 與 `state.tasks` 的 `assignee` 已提供所需資料，只是沒有集中呈現。

## 分析邏輯

- 平時零干擾：沒有執行中工具或背景處理程序時對應區段不顯示。
- Bus 仍保留，可用 `b` 或 `:bus` 切回，不移除既有功能。
- 未確認：在 80 欄以下的窄終端機，四格版面每格寬度是否足夠閱讀 (目前依寬度改為 1×2 或 2×1)。

## 實作方式

- 新增 `crates/termade-tui/src/project.rs`：`project_panel_lines` 組出協作樹 (狀態燈號、角色、標籤、`status_note`、doing／blocked／review 任務)、執行中區段 (工具名稱、摘要、已耗時，subagent 標 `sub:`；背景處理程序在 session 閒置時以黃色顯示)、最新 Bus 訊息。
- 新增 `CenterView::Grid`：`:grid`、`V`、`Space v g` 切換；一頁最多 `GRID_PAGE = 4` 格，依選取的 session 翻頁，選取格加亮，`Enter` 回單一逐字稿。
- 三欄版面說明改為 Sessions／中央／Project。

## 驗收條件

- 三欄版面右欄顯示目前專案的協作樹與執行中資訊，`b` 可切回 Bus。
- 五個以上 session 時，`:grid` 每頁四格並隨選取翻頁。
- `cargo test -p termade-tui` (含 grid 分頁與面板內容測試) 與 `make pre-commit` 通過。
- 實作 commit：`d1a84f1`。
