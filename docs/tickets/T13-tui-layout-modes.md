# T13 — TUI 版面模式：三欄／兩欄／上方 session 分頁

優先度：P2　範圍：`termade-tui`（設定預設值時涉及 T11 的 `config.toml`）

## 問題

TUI 的主區固定是三欄（Sessions／中央／Bus），只會隨終端寬度調整側欄寬度，不能關掉任何一欄。在窄終端（例如 tmux 分割、SSH 手機）或只想專心看一個 session 的逐字稿時，兩個側欄佔掉 46–76 欄，中央區塊最少只保證 20 欄。

需要的模式：

- **兩欄**：左側 sessions＋中央，隱藏 Bus。
- **分頁**：sessions 改成上方一列分頁，中央佔滿整個寬度。

## 證據

- `crates/termade-tui/src/lib.rs:1200-1218`：`draw` 內寫死三個 `Constraint`，側欄寬度依終端寬度取 `(22,24)` 到 `(34,42)`。
- `crates/termade-tui/src/lib.rs:1220-1226`：無條件呼叫 `draw_sessions`、中央視圖、`draw_bus`。
- `App` 沒有版面模式的欄位（`CenterView` 只切換中央的內容）。
- `crates/termade-tui/src/lib.rs:1678-1689`：待審批的請求畫在 `draw_bus` 裡。

## 分析邏輯

1. 讀 `draw` 確認版面是寫死的，再看改動會牽涉到哪些地方。
2. 各繪製函式都只吃一個 `Rect`，所以換版面主要是改 `draw` 裡的切分，繪製函式本身不用動。
3. 兩個要注意的相依：
   - 滑鼠命中測試用的是繪製時記下的 `sessions_area`、`transcript_area`（`lib.rs:762`、`825`）。某一欄沒畫時，它的區域要重設為空，否則點擊會命中上一次的位置。
   - 待審批提示在 Bus 欄。隱藏 Bus 時，審批請求必須在別處看得到，否則 session 會卡在等待審批而使用者不知道。
4. Markdown 快取以寬度為鍵（設計文件 7.3），切版面時寬度改變會自動重算，不需額外處理。
5. 這張票是讀程式碼後的設計，沒有做原型。

## 實作方式

1. **版面模式**：`App` 增加
   ```rust
   enum LayoutMode { Three, Two, Tabs }
   ```
   `draw` 依模式切分主區：
   - `Three`：現狀。
   - `Two`：`[Length(side_l), Min(20)]`，不畫 Bus。
   - `Tabs`：主區上方多切一列高度 1 的分頁列，其下整塊給中央視圖。
2. **分頁列**（新函式 `draw_session_tabs`）：每個 session 一個分頁，顯示狀態符號＋label，選取中的反白，待審批的用醒目顏色。寬度不夠時以選取項為中心水平捲動，兩端顯示 `‹` `›`。標題列現有的 `tab()` 樣式可以重用。
3. **操作**：
   - 新按鍵循環切換版面（建議 `L`；動手前先確認它在 `handle_key` 沒被占用）；命令面板加 `/layout three|two|tabs`。
   - `Tabs` 模式下既有的 `[`／`]` 就是左右切分頁，`j`／`k` 維持同樣的選取語意。
   - 滑鼠點分頁可切換 session：記下每個分頁的欄位範圍，比照 `sessions_area` 的做法。
4. **隱藏的欄位要清區域**：沒畫 sessions 欄時把 `app.sessions_area` 設為 `Rect::default()`，避免舊區域吃到點擊。
5. **Bus 被隱藏時的補償**：
   - 待審批請求改在狀態列或中央區塊上方顯示一行（`⏸ worker/a 要求 Bash … [a]llow [d]eny`），`a`／`A`／`d` 照常運作。
   - 標題列顯示未讀的 bus 訊息數。
   - 加一個暫時顯示 Bus 的切換（例如 `b`），以浮層或暫時恢復右欄的方式呈現。
6. **自動降級**：終端寬度低於某個門檻（例如 84 欄，即現有的最小級距）時，`Three` 自動以 `Two` 呈現；再窄則用 `Tabs`。使用者手動選的模式優先。
7. **記住選擇**：預設值放進 T11 的 `config.toml`（`[tui] layout = "two"`）。T11 完成前先只在執行期切換，不持久化。

## 驗收條件

以既有的 `TestBackend` 全畫面渲染方式寫測試：

- 三種模式各渲染一次：`Two` 沒有 Bus 標題，`Tabs` 沒有 Sessions 欄且第一列主區出現各 session 的 label。
- `Tabs` 模式下中央區塊寬度等於終端寬度。
- 分頁數超過寬度時，選取中的分頁仍在可見範圍內。
- Bus 隱藏時有待審批請求，畫面上看得到提示，且按 `a` 會送出 `Request::Permission`。
- 切換版面後，點擊原本 sessions 欄的位置不會改變選取的 session。
- 寬度 60 欄時 `Three` 自動降級，中央區塊不小於 40 欄。
