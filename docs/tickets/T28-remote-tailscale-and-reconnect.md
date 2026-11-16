# T28 — 遠端連線韌性與 Tailscale 整合：自動偵測 Tailscale 位址、Token 持久化與 TUI 自動斷線重連

優先度：P1　範圍：`termade-daemon`、`termade-tui`、`src/`　相依：T05

Status: ready-for-human

## 問題

目前 M5 雖然具備基礎的 TCP `--listen` 與 `--remote`，但在真實的 Tailscale 跨機與行動網路環境下有三個明顯斷層：

1. **TUI 斷線即殘廢，未使用協定層已具備的增量重連能力**：當行動網路切換、筆電休眠喚醒、或因廣播佇列滿 (`CLIENT_QUEUE_CAP`) 被 daemon 踢線時，TUI 只把 `app.disconnected` 設為 `true` 並要求使用者按 `q` 離開重開，完全沒有利用 `Request::Hello { since, epoch, token }` 自動重連並補齊差異事件。
2. **`validate_listen` 將 Tailscale 介面視為不安全的公網明文 TCP**：Tailscale 的 CGNAT 網段 (`100.64.0.0/10`) 與 IPv6 ULA (`fd7a:115c:a1e0::/48`) 本身已是點對點 WireGuard 加密通道，但目前綁定 Tailscale IP 時會被當成非 loopback 拒絕啟動，逼迫使用者加上 `--insecure-listen-public`，也無法直接寫 `--listen tailscale` 自動綁定。
3. **啟動遠端模式需手動管理 Token 檔**：每次啟動 `--listen` 都必須手動建立 `0600` 的 token 檔或設環境變數，缺少「首次啟動自動產生並持久化至 `~/.local/state/termade/remote.token`」的零設定體驗。

## 證據

- `crates/termade-tui/src/lib.rs:90-106`：`termade_tui::run` 只在啟動時呼叫一次 `Client::connect()` 並送出 `Hello { since: None, epoch: None }`，背景讀取執行緒在 `client.recv()` 失敗時直接結束，關閉 `tx`。
- `crates/termade-tui/src/lib.rs:183-191`、`247-249`：`rx.try_recv()` 收到 `Disconnected` 時只設 `app.disconnected = true` 並顯示「按 q 離開後重新啟動」；收到 `Frame::Ready { epoch, .. }` 時直接以 `..` 忽略了 `epoch`，沒有存進 `App`。
- `crates/termade-daemon/src/handlers/mod.rs:32-60`：daemon 端其實已經完整實作 `epoch` 比對與 `since` 的 `backlog` 增量補齊，連自己主動踢線的註解 (`crates/termade-daemon/src/lib.rs:237`) 都寫著「關閉連線讓 client 重連補齊」，但 TUI 端從未實作重連。
- `crates/termade-daemon/src/server.rs:155-189`：`validate_listen` 只檢查 `a.ip().is_loopback()`，不認得 Tailscale 網段，也不支援 `tailscale` 別名自動解析。
- `crates/termade-daemon/src/server.rs:137-152`：`resolve_token` 若未收到 `--token-file` 或 `TERMADE_TOKEN` 即回傳 `None`，導致 `validate_listen` 直接報錯退出。

## 分析邏輯

1. 對照 `handlers/mod.rs:38-60` 與 `termade-tui/src/lib.rs:245-267`：daemon 的 `Hello` 處理已經準備好增量同步 (`since` + `epoch`)，缺口純粹在 client / TUI 端沒有記錄 `epoch` 與 `state.seq`，也沒有在斷線時重新撥號。
2. Tailscale IP 的判定：Tailscale IPv4 固定分配於 `100.64.0.0/10` (`100.64.0.0` – `100.127.255.255`)，IPv6 固定為 `fd7a:115c:a1e0::/48`。不過 CGNAT 網段在某些 ISP 也可能出現，因此除了判斷 IP 範圍外，若使用者明確傳入 `--listen tailscale` 或 `--listen tailscale:7420`，透過執行 `tailscale ip -4` 取得本機 Tailscale IP 並允許免 `--insecure-listen-public` 綁定是最準確的做法。
3. 未實機確認的部分：本機環境是否已安裝 `tailscale` CLI 與登入；實作時需將 `tailscale ip -4` 呼叫包成可注入或可 fallback 的輔助函式，以便在無 Tailscale 的 CI 環境進行單元測試。

## 實作方式

1. **Tailscale 位址解析與安全放行 (`crates/termade-daemon/src/server.rs`)**：
   - 新增 `resolve_listen_addr(spec: &str) -> Result<(String, bool)>`：
     - 支援 `--listen tailscale` (預設 port `7420`) 與 `--listen tailscale:<port>`，呼叫 `tailscale ip -4` 取第一行 IPv4 組合成 `<ip>:<port>`。
     - 判斷位址是否屬於 Tailscale (`100.64.0.0/10` 或 `fd7a:115c:a1e0::/48` 或由 `tailscale ip` 解析而來)。
   - 在 `validate_listen` 中，對 Loopback 與 Tailscale 網段皆免除 `--insecure-listen-public`，並在啟動時印出清楚的連線資訊。
2. **Token 自動生成與持久化 (`crates/termade-daemon/src/server.rs`、`paths.rs`)**：
   - 新增 `default_token_path() -> PathBuf` (位於 `state_dir().join("remote.token")`)。
   - 當啟用 `--listen` (或未來的 `--web`) 且未指定 `--token-file` / `TERMADE_TOKEN` 時，若 `remote.token` 已存在則直接讀取 (並驗證 `0600` 權限)；若不存在則自動以 `/dev/urandom` (或兩個 UUID v4 拼接) 產生 256-bit 隨機 token，以 `0600` 權限寫入 `remote.token`。
   - 新增 CLI 子命令 `termade remote-info` (或在 `daemon` 啟動時)，印出目前的遠端連線位址與 Token 路徑。
3. **TUI 自動斷線重連與增量補齊 (`crates/termade-tui/src/lib.rs`、`app.rs`)**：
   - 在 `App` 新增 `daemon_epoch: Option<String>`，於收到 `Frame::Ready { epoch, .. }` 時更新。
   - 將 TUI 的連線抽象為可重連通道：背景讀取執行緒在 `client.recv()` 斷線時，送出內部通知讓狀態列顯示「連線中斷，正在重連…」，並以指數退避 (`200ms` → `500ms` → `1s` → `2s`，上限 `5s`) 自動重試 `Client::connect()`。
   - 重連成功後，自動送出 `Request::Hello { client: "tui", since: Some(app.state.seq), token, epoch: app.daemon_epoch.clone() }`，並替換主執行緒的 `writer` 把手，清除 `app.disconnected`。

## 驗收條件

- `termade daemon --listen 127.0.0.1:0` 在未設定 `TERMADE_TOKEN` 時，能自動建立 `0600` 的 `remote.token` 並成功啟動。
- `validate_listen` 對 `100.64.0.0/10` (Tailscale CGNAT) 位址不要求 `--insecure-listen-public`，對一般公網或區網位址 (如 `192.168.1.10:7420`、`0.0.0.0:7420`) 仍要求 `--insecure-listen-public`。
- 整合測試：模擬 TUI 連線後收到數筆 `Record`，強制中斷連線並在 daemon 端產生新事件，重連後帶 `Hello { since, epoch }` 只收到差異 `Record` 且 `State` 完整銜接。
- `cargo test`、`cargo clippy --all-targets -- -D warnings` 全部通過。
