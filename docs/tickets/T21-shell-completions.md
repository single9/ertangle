# T21 — Shell 自動完成

優先度：P3　範圍：`src/main.rs`、`src/cmd/`、`Cargo.toml`、`Makefile`

## 問題

`termade` 的 CLI 有三十個左右的子命令，多數要帶 session id 或 label，但沒有 shell 自動完成。子命令名稱、旗標、`--kind` 的可用值、session 的 label 都要靠記憶或先跑 `termade sessions` 查。

## 證據

- `Cargo.toml:46`：相依性 `clap` 4 (`derive`)，沒有 `clap_complete`。
- `src/main.rs:17-35`：`Cli` 與 `Cmd` 用 derive 定義，沒有產生自動完成的子命令。
- `Makefile`：安裝目標只複製執行檔。

## 分析邏輯

1. 子命令與旗標的靜態自動完成，`clap_complete` 可以從既有的 derive 定義直接產生，改動很小。
2. session 名稱的自動完成需要在按 Tab 時問 daemon。`clap_complete` 的動態自動完成目前在 `unstable-dynamic` feature 後面，介面可能變動，所以分成第二階段，並且用自己的隱藏子命令實作，不需要那個 feature。
3. 自動完成時不能有副作用：一般的 CLI 子命令會在 daemon 沒跑時自動啟動它 (`ensure_daemon`)。按 Tab 不該啟動 daemon。**沒有逐一確認**哪些子命令會走 `ensure_daemon`，動手時看 `crates/termade-daemon/src/client.rs`。

## 實作方式

### 第一階段：靜態自動完成

1. 加相依性 `clap_complete = "4"`。
2. 新增子命令 `termade completions <bash|zsh|fish>`，用 `clap_complete::generate` 把腳本印到 stdout。
3. `--kind`、`--transport`、`--permission`、`--effort` 目前若是自由字串，改成 clap 的 `value_parser` 列出可用值，自動完成才會帶出來。`--kind` 要保留別名 (`agy`、`gemini`)。
4. `Makefile` 加 `install-completions` 目標，依使用者目前的 shell 寫入對應位置 (zsh：`~/.local/share/zsh/site-functions/_termade`；bash：`~/.local/share/bash-completion/completions/termade`；fish：`~/.config/fish/completions/termade.fish`)。不要放進預設的 `install`。
5. README「安裝方式」加一段說明。

### 第二階段：session 名稱

1. 新增隱藏子命令 `termade __complete sessions`：
   - socket 不存在就直接結束、不輸出，**不啟動 daemon**。
   - 連線逾時設短 (200 ms)，失敗一樣不輸出。
   - 每行輸出一個 label；沒有 label 的輸出 session id。只列目前目錄所屬專案的 session，和 TUI 的預設範圍一致。
2. 在產生的腳本後面附加一小段各 shell 的函式，讓接受 session 參數的子命令 (`prompt`、`diff`、`merge`、`kill`、`resume` 等)呼叫 `termade __complete sessions`。zsh 與 bash 先做，fish 視需要。
3. 第二階段可以獨立於第一階段之後再做。

## 驗收條件

- 測試：`termade completions zsh`、`bash`、`fish` 各自輸出非空內容，且含 `spawn`、`merge` 等子命令名稱。
- 測試：產生的 zsh 腳本含 `--kind` 的可用值。
- 測試：`--kind agy` 這類別名仍可解析。
- 測試：socket 不存在時，`termade __complete sessions` 結束碼為 0、沒有輸出，且沒有產生 socket 或 daemon 處理程序。
- 測試：有 session 時輸出它的 label。
- 手動驗證：在 zsh 載入後，`termade <Tab>` 列出子命令，`termade prompt <Tab>` 列出 session。
