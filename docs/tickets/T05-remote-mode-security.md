# T05 — 遠端模式與本機 socket 的安全強化

優先度：P1　範圍：`termade-daemon`、`src/main.rs`、`README.md`

## 問題

能連上 daemon 的人可以 spawn `--permission bypass` 的 agent，等同以 daemon 使用者的身分執行任意指令。目前遠端模式對這個等級的權限保護不足：

- TCP 是明文，token 與所有 prompt、輸出都可被同網段側錄。
- token 以 `--token` 命令列參數傳入，同機的其他使用者用 `ps` 看得到。
- token 比對不是常數時間，沒有失敗次數限制或延遲。
- README 的範例是 `--listen 0.0.0.0:7420`。
- 指定 `--listen` 但沒有 token 時，TCP 連線不需認證。

本機方面，沒有 `XDG_RUNTIME_DIR` 時 socket 放在 `/tmp/termade-$USER/`，目錄以預設權限建立，也不檢查擁有者。

## 證據

- `crates/termade-daemon/src/lib.rs:1725-1742`：`TcpListener::bind` 後直接明文處理。
- `crates/termade-daemon/src/lib.rs:1646`：`authenticated = !require_auth || shared.auth_token.is_none()`，沒有 token 就視為已認證。
- `crates/termade-daemon/src/lib.rs:1657`、`557`：以 `==`／`!=` 比對字串。
- `src/main.rs:27-29`、`43-45`：`--token` 參數。
- `README.md:163`：`termade daemon --listen 0.0.0.0:7420 --token <secret>`。
- `crates/termade-daemon/src/lib.rs:52-57`、`1686-1688`：`/tmp` 後備路徑與 `create_dir_all`。

## 分析邏輯

1. 先確定連上 daemon 能做什麼：`Request::Spawn` 接受 `permission: bypass`，所以控制通道的權限等於 shell。
2. 以這個等級檢視 TCP 路徑：傳輸是否加密、憑證如何傳遞與比對、預設值是否安全。
3. `1646` 行的邏輯讀起來是「需要認證，但沒設定 token 就放行」，這是最嚴重的一項，因為它是靜默的。
4. `/tmp` 後備：其他使用者若先建立 `/tmp/termade-<受害者>`，就擁有該目錄，可以替換裡面的 socket，讓 client 連到假的 daemon。這是常見的 `/tmp` 佔位問題，是推論，沒有實測。在有 `XDG_RUNTIME_DIR` 的系統（多數 Linux 桌面）不受影響。
5. 常數時間比對在明文 TCP 下是次要問題，一併修掉成本很低。

## 實作方式

1. **`--listen` 必須有 token**：沒有 token 就拒絕啟動並說明原因。
2. **預設只綁 loopback**：`--listen` 的位址不是 loopback 時，要求額外的 `--insecure-listen-public` 旗標，並在啟動時印警告。README 改成推薦 SSH 轉發：
   ```bash
   ssh -L 7420:127.0.0.1:7420 host   # 遠端：termade daemon --listen 127.0.0.1:7420
   termade --remote 127.0.0.1:7420 ...
   ```
3. **token 不走命令列**：改為 `TERMADE_TOKEN` 環境變數或 `--token-file <path>`（檢查檔案權限為 `0600`）。`--token` 保留一版並印棄用警告。
4. **比對與節流**：常數時間比對（逐位元組 XOR 累加，不需新相依性）；認證失敗後延遲回覆並關閉連線。
5. **本機 socket 目錄**：建立時設為 `0700`；若目錄已存在，檢查擁有者是自己且權限不對其他人開放，否則拒絕啟動。
6. **不在這張票做 TLS**：SSH 轉發已涵蓋需求，引入 TLS 會帶進不小的相依性，與專案「相依性少」的方向衝突。若日後需要直連再另開票。

## 驗收條件

- `termade daemon --listen 127.0.0.1:0`（無 token）啟動失敗並有清楚訊息。
- `--listen 0.0.0.0:...` 無額外旗標時啟動失敗。
- 測試：錯誤 token 的 TCP 連線收到錯誤後被關閉，之後的請求不被處理。
- 測試：socket 目錄權限為 `0700`；預先建立成 `0777` 的目錄會讓啟動失敗。
- README 不再出現 `0.0.0.0` 的範例。
