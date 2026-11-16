# T29 — TUI 多主機聚合視圖 (`[remotes]` 設定與跨機 Session 操控)

優先度：P2　範圍：`termade-daemon` (`config.rs`)、`termade-tui`、`src/`　相依：T28

Status: ready-for-agent

## 問題

當開發者透過 Tailscale 同時在多台機器 (例如：本機筆電 `local`、家中工作站 `workstation`、雲端機器 `gpu-box`) 執行 `termade daemon` 時，目前必須開多個終端機視窗分別連上不同主機的 TUI。單一 `termade-tui` 只能連一個 daemon，無法在同一個畫面上同時監看所有機器的 Session 狀態、接收跨機的 `WaitingApproval` 通知，或直接切換過去下達 Prompt 與審查 Diff。

## 證據

- `crates/termade-daemon/src/config.rs:290-330`：`Config` 結構目前有 `defaults`、`tui`、`limits`、`collab`、`agent_spawn`、`agents`、`roles`，尚無定義遠端主機清單的 `[remotes]` 區塊。
- `crates/termade-tui/src/lib.rs:90-110`、`164-168`：`run()` 與 `event_loop()` 僅接受單一 `writer: &mut ConnStream` 與單一 `rx: Receiver<Frame>`，所有收到的 `Frame` 直接寫入單一 `app.state`。
- `crates/termade-tui/src/app.rs:106-160`：`App` 只持有一份 `State`，Session 清單 (`visible_sessions`) 沒有主機來源 (`host`) 的概念。
- `crates/termade-tui/src/input.rs`、`commands.rs`：發送 `Request::Prompt`、`Request::Permission`、`Request::WorktreeDiff`、`Request::WorktreeMerge` 等操作時，一律寫入唯一的 `writer`。

## 分析邏輯

1. **為什麼選擇「UI 控制面多機聚合」而非「跨機 MCP Bus 互聯」**：
   - `termade` 的核心操作 (`WorktreeMerge`、`Handoff` 共用工作目錄、`ArtifactShared` 檔案路徑) 皆以單機 Git Repo 與磁碟為邊界。
   - 在 TUI (Client 層) 同時維護多條 `ConnStream`，讓每台主機的 `State` 各自獨立演進、在 UI 呈現時合併列表並依選取的 Session 路由 `Request`，既能達到「單一面板管控所有機器」的目標，又完全不會污染單機 Daemon 的 Git Worktree 與 Event `seq` 假設。
2. **Session ID 碰撞與路由**：
   - 不同主機的 `SessionId` 雖然都是隨機 ID (`ses_...`)，但 `State` 內部的 `seq`、`tasks`、`messages` 是各主機獨立的。因此 `App` 應以 `HostId` (例如 `"local"` 或 `[remotes.<name>]` 的 `<name>`) 為鍵，持有 `BTreeMap<HostId, HostState>` (每個 `HostState` 內含自己的 `State`、`writer`、連線狀態 `Connected / Reconnecting / Offline` 與 `epoch`)。
   - 當目前選取的 Session 屬於某台 `host` 時，中央視圖 (逐字稿、Diff、Worktrees、Tasks) 與右側 Bus 視圖就顯示該 `host` 的對應資料，所有針對該 Session 發出的 `Request` 也自動送往該 `host` 的 `writer`。
3. **未實機確認的部分**：同時連上 3 台以上遠端主機時，左側欄寬度顯示 `[host] label` 的截斷視覺效果，需在 80 欄與 140 欄終端機下實際檢視。

## 實作方式

1. **設定檔擴充 (`crates/termade-daemon/src/config.rs`)**：
   - 在 `Config` 新增 `pub remotes: BTreeMap<String, RemoteHostCfg>` (只認全域 `~/.config/termade/config.toml`，避免不受信任的 repo 設定檔誘使 TUI 連向外部位址)：
     ```toml
     [remotes.workstation]
     addr = "100.64.0.12:7420"
     token_file = "~/.config/termade/workstation.token" # 或 token_env = "WORKSTATION_TOKEN"
     enabled = true
     ```
2. **多主機連線池 (`crates/termade-tui/src/hosts.rs`)**：
   - 定義 `HostConn`：封裝每台主機的名稱 (`local` + 設定檔中 `enabled` 的各遠端主機)、`State`、連線狀態 (`Connected`、`Reconnecting(String)`、`Disconnected`)、`daemon_epoch` 與寫入通道。
   - 每個 `HostConn` 啟動獨立的讀取與自動重連執行緒 (重用 T28 的重連邏輯)，收到的 `Frame` 帶上 `(host_name, frame)` 送入 TUI 的統一 `mpsc::Sender<(String, Frame)>`。
3. **TUI 狀態與畫面整合 (`crates/termade-tui/src/app.rs`、`draw.rs`、`input.rs`、`commands.rs`)**：
   - **主機篩選與切換**：新增 `app.host_filter: Option<String>` (`None` 表示顯示所有主機的 Session)。按 `H` (或 `:host [名稱|all]`) 可在「全部主機」與個別主機之間循環切換。
   - **左側 Session 清單**：當啟用多主機 (設定檔有至少一個遠端主機) 時，Session 標籤前顯示主機標記 (如 `[ws] worker/api`)，並在標題列顯示各主機連線燈號 (例如 `● local  ● ws  ◌ gpu`)。
   - **請求路由**：將 `send(writer, req)` 改為依目標 Session (或目前聚焦的主機) 查找對應的 `HostConn` 並送出；在建立新 Session (`n` / `:spawn`) 時，若處於「全部主機」視圖，可選擇要在哪一台主機上建立 (預設為目前選取 Session 所在的主機)。
   - **跨機通知整合 (`notify.rs`)**：任何一台已連線主機上的 Session 進入 `WaitingApproval`、回合完成 (`Idle`) 或額度告警時，同樣觸發 TUI 通知並在標題列標示。

## 驗收條件

- 未設定 `[remotes]` 時，TUI 介面與行為與原本單機模式 100% 一致 (不顯示多餘的 `[local]` 前綴)。
- 在測試中啟動兩個獨立的 `termade daemon` (一個 Unix socket、一個本地 TCP Loopback 模擬遠端主機)，TUI 載入設定後能同時顯示兩邊的 Session。
- 選取遠端主機的 Session 時，送出 `Prompt`、核准 `Permission`、檢視 `WorktreeDiff` 皆正確路由至該遠端 daemon。
- 當其中一台遠端 daemon 斷線或重啟時，本機與其他遠端主機的操作完全不受影響，且斷線的主機能獨立完成自動重連。
- `cargo test`、`cargo clippy --all-targets -- -D warnings` 全部通過。
