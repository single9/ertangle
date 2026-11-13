# T18 — 任務板：依賴解鎖與獨立視圖

優先度：P2　範圍：`termade-daemon`、`termade-tui`

## 問題

任務板有兩個缺口：

1. **`depends_on` 沒有作用。** 建立任務時可以填依賴，資料也存下來了，但沒有任何程式讀它。有依賴的任務一建立就通知負責人開工；前置任務完成時，後續任務的負責人也不會被叫起來。協調者 agent 想排「A 做完再做 B」，實際上只能自己等、自己再發訊息。
2. **TUI 看不清楚任務。** 任務只列在右側 Bus 欄的上半部，每個一行。兩欄與分頁版面會把 Bus 欄藏起來，這時完全看不到任務；也不能在 TUI 裡改任務狀態或看依賴關係。

## 證據

- `crates/termade-daemon/src/handlers/task.rs:10-53`：`handle_create` 把 `depends_on` 存進 `Task`，接著無條件對負責人送出指派訊息。
- `crates/termade-daemon/src/handlers/task.rs:55-115`：`handle_update` 只在「換負責人」時發通知，狀態變成 `done` 時沒有找後續任務。
- 搜尋 `depends_on`：只出現在欄位定義、參數傳遞與 MCP schema，沒有讀取它做判斷的地方。
- `crates/termade-tui/src/draw.rs:1105-1111`：任務畫在 `draw_bus` 裡。
- `crates/termade-tui/src/app.rs:30-34`：`CenterView` 只有 `Transcript`、`Diff`、`Worktrees`。
- `crates/termade-core/src/lib.rs:401-412`：`Task` 沒有專案欄位。

## 分析邏輯

1. 從 `TaskCreate` 一路追到 `handle_create`，再看 `handle_update`，確認依賴只被儲存。
2. `TaskStatus` 已經有 `Blocked`，可以直接拿來表示「在等前置任務」，不必加新狀態。
3. 沒有檢查 `mcp.rs` 裡 `termade_task_list` 回傳的格式是否包含依賴，動手時要一併確認，讓 agent 看得到任務為什麼被擋住。

## 實作方式

### A. 依賴解鎖（daemon）

1. **建立時**（`handle_create`）
   - 驗證 `depends_on` 的每個 id 都存在，不存在就回錯誤。
   - 有任何一個依賴還沒 `done`：任務狀態設為 `blocked`，**不送指派訊息**。回覆裡註明在等哪些任務。
   - 全部已完成或沒有依賴：維持現在的行為。
2. **更新時**（`handle_update`）：某個任務變成 `done` 之後，找出所有 `depends_on` 含它、狀態是 `blocked`、且全部依賴都已 `done` 的任務，逐一：
   - 狀態改為 `todo`，送 `TaskChanged`。
   - 有負責人就送指派訊息。指派訊息的組字串在 `handle_create` 與 `handle_update` 各有一份，先抽成一個函式再用第三次。訊息裡附上各前置任務的標題，以及它們負責人的 label，方便後手去問。
3. **任務被改成依賴別人**：目前 `TaskUpdate` 不能改 `depends_on`，維持不開放，就不需要偵測循環。若之後要開放，再加循環檢查。
4. **已知的模糊地帶**：有依賴的任務若被人手動標成 `blocked`（其他原因），依賴完成時也會被自動解開。可接受，寫進文件；若造成困擾再加一個區分的欄位。
5. **MCP**：`termade_task_list` 的每個任務帶 `depends_on` 與還沒完成的依賴清單。

### B. 任務視圖（TUI）

1. `CenterView` 加 `Tasks`，放進 `Tab`／`g` 的循環；新增 `draw_tasks`。
2. 內容：依狀態分組（doing、review、todo、blocked、done），每列顯示 id 尾碼、狀態、標題、負責人 label、依賴（未完成的用醒目顏色）、多久前更新。下方或右側顯示選取任務的完整說明。
3. 操作：
   - `j`／`k` 移動任務游標（`App` 加 `task_cursor`）。
   - `Enter` 跳到負責人的 session 並切回逐字稿。
   - 改狀態：`:task-status <狀態>` 對選取的任務送 `TaskUpdate`。要不要再綁單鍵，等用過再決定。
   - 建立任務沿用既有的 `:task`。
4. **專案範圍**：`Task` 沒有專案資訊。`scope_here` 開啟時，用負責人或建立者的 session 的 `project_root()` 判斷；兩者都沒有（人建立、還沒指派）的任務一律顯示。
5. Bus 欄保留現有的精簡清單。標題列的 `· N tasks`（`draw.rs:579`）改成只算未完成的。
6. 滑鼠：比照 sessions 欄記下區域，滾輪移動任務游標。沒畫這個視圖時把區域清空。

## 驗收條件

- 測試：依賴未完成時建立的任務是 `blocked`，負責人沒有收到訊息。
- 測試：依賴完成後，任務變 `todo`，負責人收到一則指派訊息，閒置中的負責人被喚醒。
- 測試：有兩個依賴時，只完成一個不會解鎖。
- 測試：`depends_on` 含不存在的 id 時建立失敗。
- 測試：沒有依賴的任務，行為與現在相同。
- TUI 測試（`TestBackend`）：切到任務視圖能看到各狀態分組與任務標題；兩欄與分頁版面下也能看到。
- TUI 測試：`Enter` 後選取的 session 是該任務的負責人。
- TUI 測試：`scope_here` 開啟時，其他專案 session 的任務不顯示。
- 同步 `help.rs`、`COMMANDS` 表、README、MCP 工具說明。
