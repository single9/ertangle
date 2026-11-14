# T16 — 在逐字稿與 diff 裡搜尋

優先度：P2　範圍：`termade-tui`

## 問題

TUI 沒有搜尋。長對話裡要找某個檔名、錯誤訊息或先前的決定，只能用 `PgUp`／`PgDn` 翻。逐字稿每個 session 最多保留 4000 行，翻找很花時間。diff 視圖也一樣，只能逐 hunk 跳。

## 證據

- `crates/termade-tui/src/app.rs:10-24`：`Mode` 沒有搜尋模式。
- `crates/termade-tui/src/input.rs:187`：Normal 模式的 `/` 已經綁給命令列 (和 `:` 相同)，所以不能照 vim 的習慣用 `/`。
- `crates/termade-tui/src/commands.rs` 的 `COMMANDS` 表沒有搜尋相關命令。
- `crates/termade-core/src/lib.rs:715`：`TRANSCRIPT_CAP = 4000`。

## 分析邏輯

1. 搜尋的對象要是**畫面上實際顯示的行** (Markdown 渲染、工具摺疊、隱藏思考行之後的結果)，這樣符合項的行號才能直接當捲動位置用。逐字稿的顯示行由 `build_transcript_lines`(`draw.rs:783`)產生並放在 `App.md_cache`，捲動位置是 `App.transcript_scroll`(`Option<usize>`，`None` 表示跟隨最新)。
2. 按鍵：Normal 模式目前沒用到 `f` (這次檢查 `input.rs` 的 `KeyCode::Char` 清單得到的結果，動手前再確認一次)。`n`／`N` 不能拿來跳下一個符合項，`n` 已經是建立 session。
3. 沒有讀 `MdCache` 的內部結構與 `draw_transcript` 的細節，下面第 2 步的做法要對照實際結構調整。

## 實作方式

1. **狀態**：`App` 加
   ```rust
   struct Search { query: String, matches: Vec<usize>, current: usize }
   search: Option<Search>
   ```
   `matches` 是顯示行的索引。`Mode` 加 `Search`。
2. **比對**：對顯示行的純文字 (把 `Line` 的各個 span 內容接起來)做子字串比對。查詢全小寫時不分大小寫，含大寫時區分 (smartcase)。逐字稿內容或寬度改變、快取重建時 (`transcript_stamp` 改變)重算 `matches`，並盡量停在原本那一行附近。
3. **操作**
   - Normal 模式 `f` 或 `:find <文字>` 進入搜尋，輸入框標題顯示「搜尋」。
   - 邊打邊找：每次輸入更新 `matches`，跳到游標位置之後的第一個符合項。
   - 搜尋模式內：`Enter`／`↓` 下一個，`↑` 上一個，到底後繞回開頭；`Esc` 離開並停在目前位置。
   - 離開後保留上一次的查詢，再按 `f` 時預填。
   - 跳轉是設定 `transcript_scroll = Some(行號)`，等同手動捲動，會暫停跟隨。
4. **顯示**
   - 符合的行加底色，目前這一個用更醒目的顏色。第一版整行標示即可；只標出符合的片段需要切 span，可以之後再做。
   - 狀態列顯示 `3/17`；沒有符合時顯示「找不到」。
5. **Diff 視圖**：同一套狀態，比對 `App.diff_lines`，跳轉改成設定 `diff_cursor`。
6. **限制，寫進說明**：只搜尋看得到的內容。被摺疊的工具呼叫、隱藏的思考行、超過 4000 行已被捨棄的舊內容都不在範圍內。要找舊內容用 `termade replay`。

## 驗收條件

以既有的 `TestBackend` 渲染方式寫測試：

- 輸入查詢後，畫面捲到第一個符合的行，狀態列顯示 `1/N`。
- `Enter` 依序前進並在最後一個之後繞回第一個；`↑` 反向。
- 全小寫查詢能找到大寫內容；含大寫的查詢不會。
- 沒有符合項時不改變捲動位置。
- 搜尋期間有新輸出進來，`matches` 會更新，目前位置不會跳走。
- Diff 視圖下搜尋會移動 `diff_cursor`。
- `Esc` 後 `j`／`k`／`n` 等 Normal 按鍵恢復原本的作用。
- 同步 `help.rs`、`COMMANDS` 表、README 按鍵表。
