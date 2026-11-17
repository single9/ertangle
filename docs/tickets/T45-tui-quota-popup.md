# T45 — `:quota` 改為額度浮層，列出已安裝 agent 並以 progress bar 呈現

優先度：P3　範圍：`termade-tui`　相依：—

Status: done

## 問題

`:quota` 只把所有額度擠成一行寫進狀態列，訊息很快被其他狀態蓋掉，也看不出哪些已安裝的 agent 沒有額度資料；用量只有百分比數字，不直觀。

## 證據

- 修正前 `crates/termade-tui/src/commands.rs` 的 `"quota"` 分支把 `Quota::summary` 以 `   |   ` 串接後寫入 `app.status`。
- `termade_adapter::supports_structured` 可判斷本機是否裝有各 agent 的 CLI。

## 分析邏輯

- 只有 claude 與 codex 會回報額度，其他 agent 列出但標示「尚無額度資料」，讓使用者知道不是漏掉。
- 已過重置時間的視窗顯示為 0%，避免把舊的高用量當成現況。
- 偵測 agent 要查 `PATH`，只在開啟浮層時做一次；額度數字則每次重畫時讀最新狀態，開著也會即時更新。

## 實作方式

- 新增 `crates/termade-tui/src/quota.rs` 與 `Mode::Quota`：每個 agent 一個區塊，標題列含「✗額度已滿」與更新時間，每個視窗一列 20 格 progress bar (<70% 綠、<90% 黃、其餘紅)、百分比與重置倒數。
- 開啟方式：`:quota`、which-key `Space s q`、滑鼠點擊標題列的額度摘要 (`app.quota_area`)。
- Esc、Enter、Space、q 關閉。

## 驗收條件

- `:quota` 彈出浮層，列出本機已安裝的 agent；沒有額度資料的 agent 有提示。
- progress bar 長度與用量成比例，超出 0–100% 會夾住。
- `cargo test -p termade-tui` (含 bar、浮層內容與渲染測試) 與 `make pre-commit` 通過。
