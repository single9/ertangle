# T30 — 行動端隨身審核器 (`--web` 零相依 HTTP/WebSocket 與內嵌 Mobile PWA + 終端機 QR Code)

優先度：P2　範圍：`termade-daemon`、`termade-tui`、`src/`　相依：T28

Status: ready-for-human

## 問題

開發者坐在電腦前時使用 `termade` TUI 效率極高，但離開座位 (通勤、買咖啡、休息) 時，若 Agent 跑到一半停在 `WaitingApproval` 等待按鍵核准，或完成階段任務等待下一句提示，在手機窄螢幕上透過 SSH 終端機操作三欄式 TUI 體驗極差 (字體過小、虛擬鍵盤遮擋畫面、組合鍵難按)。需要一個專為手機直立螢幕設計的「隨身審核與遙控伴侶 (Mobile Companion)」，同時嚴格遵守專案「不引入 `tokio`、不引入 Node.js 建置鏈、維持單一執行檔」的原則。

## 證據

- `crates/termade-daemon/src/server.rs:10-83`：`handle_conn` 透過 `client_channel` (`ClientTx` / `ClientRx`) 與 `Shared::handle` 處理一行一筆 JSON (`Request` ↔ `Frame`)，邏輯與底層傳輸完全解耦。
- `crates/termade-protocol/src/lib.rs:83-220`：所有行動端需要的操作 (`Hello`、`Prompt`、`Cancel`、`Permission`、`Handoff`、`WorktreeDiff`、`ReviewComment`、`WorktreeMerge`) 都已經是現成的 JSON `Request` 變體。
- `Cargo.toml:48-63`：整個 workspace 維持純同步 `std::thread` 架構，未引入任何 async runtime 或肥大的 HTTP 框架。

## 分析邏輯

1. **如何在不引入 `tokio` 與肥大框架的前提下支援瀏覽器連線**：
   - 瀏覽器需要一個 HTTP `GET /` 來載入 HTML 頁面，以及一條 WebSocket (`GET /ws`) 來雙向收發 JSON。
   - HTTP 與 WebSocket 握手 (`Upgrade: websocket` + `Sec-WebSocket-Accept` 的 SHA-1 Base64) 以及 RFC 6455 的 Text/Close/Ping/Pong frame 讀寫，在阻塞式 `TcpStream` 上只需約 150–200 行純 Rust 程式碼即可完成 (或僅引入極輕量的同步 WebSocket 解析器)，完全不需要 `tokio`、`hyper` 或 `axum`。
   - WebSocket 連線一旦完成握手，每收到一個 Text Frame 就直接丟給 `decode_line::<Request>` 與 `Shared::handle(&tx, req)`；`ClientRx` 收到的每個 `Frame` 轉成 JSON 字串包成 WebSocket Text Frame 寫回，與現有 TCP `handle_conn` 共享 95% 以上的邏輯。
2. **定位邊界 (Mobile Companion vs. Desktop Web IDE)**：
   - 明確不做桌面版複雜多窗格 Web IDE，避免稀釋 TUI 的核心定位。
   - 內嵌的 `index.html` (`< 40 KB`，透過 `include_str!` 編入執行檔) 專注於手機單手大拇指操作：頂部醒目的「待核准請求 (Allow / Always / Deny)」橫幅、即時串流逐字稿、行動版 Diff 檢視 + 點擊行內留註解 + 一鍵 Merge、以及底部 Prompt 輸入框。
3. **安全性考驗**：
   - Token 絕不放在 URL Query (`?token=...`)，因為會留在各層日誌或瀏覽器歷史摘要中；改放在 URL Fragment (`http://<tailscale-ip>:7421/#token=<secret>`)，Fragment 不會隨 HTTP `GET /` 送出，前端 JS 讀取後存入 `localStorage` 並清理網址列，接著透過 WebSocket 的第一個 `Request::Hello { token }` 完成常數時間驗證。
   - 與 `--listen` 相同，`--web` 強制要求認證 Token (可自動讀取或生成 T28 的 `remote.token`)，且非 Loopback / 非 Tailscale 位址要求 `--insecure-listen-public`。
4. **未實機確認的部分**：iOS Safari 與 Android Chrome 在背景分頁休眠再切回前景時，WebSocket `onclose` 觸發重連的速度與 Web Notification API 在非 HTTPS (純 Tailscale HTTP IP) 下的權限行為 (多數行動瀏覽器在 `http://100.x.y.z` 限制 `Notification.requestPermission()`，因此前端需同時提供頁面標題閃爍、音效/震動與醒目視覺橫幅作為後備)。

## 實作方式

1. **Daemon 內建同步 HTTP + WebSocket 端點 (`crates/termade-daemon/src/web.rs`)**：
   - 在 `RemoteOptions` 與 `termade daemon` CLI 新增 `--web <addr>` (支援 `--web tailscale` 自動綁定 `tailscale0` 的 `:7421`，或指定 `host:port`)。
   - 實作輕量 HTTP + RFC 6455 WebSocket 處理器：
     - `GET /`：回傳 `200 OK`、`Content-Type: text/html; charset=utf-8` 與 `CSP` 安全標頭，內容為 `include_str!("web/index.html")`。
     - `GET /manifest.json`：回傳 PWA manifest，支援手機「加入主畫面」全螢幕啟動。
     - `GET /ws`：完成 WebSocket 握手後，套用與 `server.rs:handle_conn` 相同的認證閘門 (首則訊息必須為帶正確 `token` 的 `Request::Hello`，失敗則延遲 `AUTH_FAIL_DELAY` 後關閉)，認證成功後橋接 `ClientTx` / `ClientRx`。
2. **內嵌單檔 Mobile Companion 前端 (`crates/termade-daemon/src/web/index.html`)**：
   - 零外部 CDN 相依 (離線區網/Tailscale 皆可開)，深色終端機風格主題。
   - **四大功能區塊**：
     1. **頂部狀態與核准橫幅 (Approval Banner)**：顯示連線狀態 (支援指數退避自動重連與 `Hello { since, epoch }` 增量同步)、總成本與 Quota 指標；當有 `PermissionRequested` 時置頂彈出核准卡片，一鍵點擊 `Allow` / `Always` / `Deny`。
     2. **Session 切換抽屜 / 頂部膠囊列**：快速切換不同 Session，顯示狀態燈號 (`● Busy`、`! Approval`、`✓ Idle`)。
     3. **主檢視區 (Transcript / Diff / Tasks 切換)**：
        - `Transcript`：即時渲染輸出與摺疊 Tool Call，底部固定輸入框支援送出 Prompt、排入佇列 (`queue`)、中斷 (`Cancel`) 與一鍵 `Handoff`。
        - `Diff`：向 daemon 請求 `WorktreeDiff`，渲染紅綠增刪行；點擊任一行可輸入審查註解 (`ReviewComment`) 送回 Agent，頂部提供 `Merge` 按鈕 (`WorktreeMerge`)。
        - `Tasks`：顯示目前任務看板與負責人狀態。
3. **終端機 QR Code 與連線資訊 (`src/cmd/daemon.rs`、TUI `:remote`)**：
   - 實作輕量 QR Code 矩陣編碼並以 Unicode 半格字元 (`▀`、`▄`、`█`、` `) 渲染至終端機 (或在 TUI 彈出 QR 視窗)。
   - 執行 `termade remote` (或在 TUI 輸入 `:remote`) 時，顯示 `http://<ip>:<port>/#token=<secret>` 並印出 QR Code，手機相機一掃即連。

## 驗收條件

- `termade daemon --web 127.0.0.1:0` 啟動成功，對 `/` 發送 HTTP GET 回傳 `200 OK` 與內嵌 HTML。
- 整合測試：透過 WebSocket 連上 `/ws`，未帶有效 token 時收到錯誤並被斷線；帶有效 token 送出 `Hello` 後，能收到 `Ready` 與 `Snapshot`，並能成功送出 `Prompt` 與 `Permission` 決策。
- WebSocket 斷線後重連並帶上 `Hello { since, epoch }`，能正確收到斷線期間的新 `Record`。
- `cargo test`、`cargo clippy --all-targets -- -D warnings` 全數驗證合格，且編譯產物仍為單一獨立執行檔。
