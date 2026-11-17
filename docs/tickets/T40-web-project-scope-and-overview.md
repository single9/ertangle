# T40 — Web 版專案範疇切換、QR Code 帶入專案與「總覽」分頁

優先度：P2　範圍：`termade-daemon` (`web/index.html`)、`termade-tui`　相依：T30, T32

Status: done

## 問題

1. 手機版看不到可選的專案，專案切換也不正常：專案名稱只取路徑最後一段，同名資料夾會混在一起；篩選只存在 Session 分頁，對話、任務、核准都不受影響；篩選按鈕以字串拼接 `onclick='${p}'`，名稱含引號就壞掉。
2. 從 TUI 掃 QR Code 進入後，不一定是使用者在 TUI 看的那個專案。
3. 手機畫面缺少專案層級的資訊：看不到協作者與工作者的關係、誰在做哪個任務、執行中的工具／subagent、背景處理程序。
4. 前端 `applyRecord` 只處理少數事件，`active_tools`、背景處理程序、Bus 訊息、佇列等變動不會即時反映。

## 證據

- 修正前 `web/index.html` 的 `getProjectName()` 以 `split("/")` 取最後一段；`renderSessionsList()` 用字串拼接 `onclick`。
- 修正前 `crates/termade-tui/src/commands.rs` 的 `:web` 只產生 `?token=` 網址，不含專案。
- `termade-core` 的 `Session` 已有 `repo_root`、`active_tools`、`bg_procs`、`parent` 等欄位可直接使用。

## 分析邏輯

- 專案以完整根目錄 (`repo_root ?? cwd`) 辨識，只在顯示時取最後一段並在重名時加上上層資料夾。
- QR Code 網址仍沿用已驗證可掃描的 `?token=` 形式，只多加 `&project=<百分比編碼>`；前端讀取後清掉網址參數。
- 未確認：超長路徑使 QR Code 版本變大後在小螢幕的掃描成功率。

## 實作方式

- 頁首專案名稱可點選開啟專案清單 (含 session 數)，選擇存入 `localStorage`；Session、任務、Bus、核准橫幅與預設 session 都依所選專案過濾。改用 `data-*` 屬性與 `escapeHtml`。
- TUI `:web`／`:web qr` 在 `scope_here` 時附加 `&project=` (`commands.rs` 的 `percent_encode`)；前端選取該專案第一個執行中 session，優先協作者。
- 新增預設的「總覽」分頁 (`renderOverview`)：協作者 → 工作者樹狀卡片 (角色、狀態、`status_note`、執行中工具與已耗時、subagent、背景處理程序、手上的任務、最新 Bus 訊息)；寬螢幕以最多四格顯示各 session 最近逐字稿，手機改為可橫向滑動。
- `applyRecord` 對齊 `State::apply`：工具開始／結束維護 `active_tools`、背景處理程序、Bus 訊息、超出預算／額度、佇列。

## 驗收條件

- 多個專案時頁首可切換，且各分頁內容只顯示所選專案。
- 在 TUI 的 A 專案執行 `:web qr` 掃描後，手機直接停在 A 專案。
- `cargo test -p termade-daemon --test web_companion` 與 `make pre-commit` 通過。
- 實作 commit：`8a0948b`。
