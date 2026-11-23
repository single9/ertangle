# ertangle — 終端機原生的多 Agent 開發協作環境

[English](README_EN.md) | [繁體中文](README.md)

> **讓多個 Coding Agent 在同一個 Repo 內平行工作、自主溝通、互相協同開發的終端機環境**。

`ertangle` 是一個以終端機為核心的多 Agent 協同開發平台。支援 **OpenCode**、**Claude Code**、**Codex** 與 **Antigravity** 等現代 Coding Agent，讓它們各自在獨立的 Git Worktree 內平行寫程式，透過 MCP 訊息匯流排與任務看板互相溝通，並由開發者透過鍵盤驅動的 TUI 介面進行即時審查與安全合併。

---

## 為什麼選擇 ertangle？ (終端機原生 vs. 桌面 GUI IDE)

相較於肥重的桌面 GUI 或 Electron 應用程式，`ertangle` 採用終端機原生與 Daemon 分離架構：將多 Agent 排程、Worktree 隔離與即時審查帶入你最熟悉的命令列，不僅常駐極輕量 (`< 20 MB`)，更能無縫支援 SSH 遠端開發與行動端隨身查閱：

| 特性 | 本專案的做法 |
| --- | --- |
| **超輕量與極速** | Rust + Ratatui 開發，常駐記憶體成本 `< 20 MB`，瞬間啟動，SSH 遠端連線零卡頓 |
| **Daemon 常駐架構** | Daemon 負責真正持有 Agent 子處理程序、訊息匯流排與狀態；TUI 只是隨時可重開、重新連線的顯示端 |
| **平行作業不衝突** | 每個 Agent Session 自動配置獨立的 `git worktree` 與分支 (`ertangle/<session>`) |
| **零相依原生通訊** | 優先採用各 Agent 的結構化協定 (ACP / stream-json / app-server)，無需額外 Wrapper；未安裝時自動退回 PTY 直通 |
| **結構化協同機制** | 透過 Daemon 注入 MCP 伺服器，讓 Agent 能彼此通訊、委派任務、發布進度、排隊等待 |
| **全鍵盤與腳本驅動** | 完整 TUI 快速鍵、Helix 風格選單 (which-key)、命令列，以及功能對等的 `ertangle` CLI |

---

## 支援的 Agent 與連線協定

各家 Agent 均以其**官方原生、零額外相依**的結構化介面連接：

| Agent | 原生協定 | 預設啟動命令 | 傳輸標記 | 支援狀態與特性 |
| --- | --- | --- | --- | --- |
| **OpenCode** | ACP | `opencode acp` | `acp` | ✅ 完整雙向結構化 (串流、工具呼叫、權限核准轉發、MCP 注入) |
| **Antigravity** | ACP | `agy acp` | `acp` | ✅ 完整雙向結構化 (ACP 方言相容、MCP 注入) |
| **Claude Code** | stream-json | `claude -p --input-format stream-json --output-format stream-json --verbose` | `stream-json` | ✅ 逐字串流、工具呼叫、成本統計、5h/7d 額度回報、MCP 注入、逐項權限核准轉發 |
| **Codex** | app-server | `codex app-server` | `app-server` | ⚠️ 握手與 Thread 已驗證；執行回合需先於本機執行 `codex login` |
| **萬用後備** | PTY | 各 Agent 互動 CLI | `pty` | ✅ 找不到原生協定或未安裝時，`--transport auto` 自動退回 PTY 直通 |

> [!NOTE]
> - **Codex 授權**：`codex app-server` 的通訊協定完整支援，但實際發送 prompt 需要有效的 OpenAI 登入。未授權時 session 會變更為 `failed` 並提示執行 `codex login`。
> - **PTY 限制**：PTY 模式下全螢幕 TUI 輸出會去除 ANSI 控制碼作為盡力而為的文字記錄，建議優先使用結構化原生協定。
> - 完整驗證紀錄與實機測試版本請參閱 [`docs/VERIFICATION.md`](docs/VERIFICATION.md)；架構設計細節請見 [`docs/DESIGN.md`](docs/DESIGN.md)。

---

---

## 從 termade 遷移

本專案已由 `termade` 正式更名為 `ertangle`。升級時請留意以下相容層與遷移事項：

- **停止舊 Daemon**：首次執行 `ertangle` 前，請先停止舊的 `termade` daemon (`termade shutdown` 或關閉對應處理程序)。
- **設定與狀態目錄**：首次啟動時，若新目錄尚不存在，系統會自動將 `~/.config/termade` 與 `~/.local/state/termade` 搬移至 `~/.config/ertangle` 與 `~/.local/state/ertangle`。
- **專案設定檔**：Repo 內的 `.termade/config.toml` 在 v1.0 前仍會作為相容退路被讀取，但建議將目錄重新命名為 `.ertangle/`。
- **環境變數**：`TERMADE_*` 系列環境變數仍會作為退路支援至 v1.0，建議改用 `ERTANGLE_*`。
- **重新安裝 Skill**：請執行 `ertangle skill install` 重新安裝 agent skill (`ertangle skill uninstall` 也會一併清理舊的 `termade-ade`)。
- **Git 分支**：既有的 `termade/*` 分支維持原樣，不受影響。

## 快速開始

### 1. 安裝

**方式一：下載預編譯執行檔** (從 [Releases](https://github.com/single9/ertangle/releases/latest) 取得，支援 Linux x86_64 / arm64 與 macOS arm64 / x86_64 / universal)

```bash
# 以 Linux x86_64 為例，其他平台請替換檔名 (ertangle-linux-arm64、ertangle-macos-arm64 等)
curl -fsSL https://github.com/single9/ertangle/releases/latest/download/ertangle-linux-x86_64.tar.gz | tar -xz
install -m 755 ertangle-linux-x86_64 ~/.local/bin/ertangle
```

**方式二：直接從 git 安裝** (需要 Rust 工具鏈)

```bash
cargo install --git https://github.com/single9/ertangle

# 指定版本
cargo install --git https://github.com/single9/ertangle --tag v1.0.0
```

**方式三：從原始碼建置**

```bash
git clone https://github.com/single9/ertangle && cd ertangle
cargo install --path .        # 安裝至 ~/.cargo/bin
# 或使用 Makefile 安裝至 ~/.local/bin/ertangle
make install
```

`make install` 會一併建立短別名 `ert` (指向 `ertangle` 的符號連結)。用 `cargo install` 安裝時，可自行設定：`ln -s ~/.cargo/bin/ertangle ~/.cargo/bin/ert` 或在 shell 加上 `alias ert=ertangle`。

**Shell 自動補齊** (支援 bash / zsh / fish，並能動態補齊 session label)

```bash
make install-completions      # 於原始碼目錄執行，依目前 SHELL 自動安裝
# 或手動輸出：ertangle completions <bash|zsh|fish>
```

### 2. 啟動與第一個任務

在任何 Git 專案目錄下執行：

```bash
ertangle
```

- 若 Daemon 尚未執行，`ertangle` 會**自動在背景啟動 Daemon** 並進入 TUI。
- 在 TUI 中按下 **`n`**，依序選擇 Agent (如 `opencode`)、Model (如預設或建議模型)，並輸入提示：
  ```
  幫我把測試修好，並修正任何編譯錯誤
  ```
- Agent 會在專屬 Worktree (`ertangle/session-xxx`) 開始工作。你可以在中央視圖即時觀察 Markdown 串流逐字稿、切換到 Diff 視圖審查程式碼變更，並一鍵完成合併。

---

## 實戰案例：Coordinator + Worker 多 Agent 協同開發

當遇到跨模組的中大型需求時，你不需要自己手動開好幾個終端機視窗分別複製貼上。在 `ertangle` 中，你可以啟動一位 **Coordinator (協調者)**，讓它自動拆解任務、建立相依性看板、派生多個各自擁有獨立 Git Worktree 的 **Worker (實作者)** 平行開發，最後由你在 TUI 統一審查與合併。

### 情境範例：同時開發後端 API 認證、前端登入表單與整合測試

#### 第一步：啟動 Coordinator (三種方式擇一)

**方式 A：在 TUI 互動啟動 (最常用)**
1. 在 TUI 按下 **`n`**，選擇主力模型 (例如 `claude` 或 `opencode`)。
2. 在輸入提示詞畫面，按 **`Ctrl-R`** 將角色切換為 **`coordinator`** (按 `Ctrl-W` 保持關閉 Worktree，讓 Coordinator 留在主目錄綜觀全局)。
3. 輸入需求提示詞：
   ```text
   請擔任 Coordinator 完成「JWT 登入認證功能」：
   1. 先用 ertangle_task_create 建立三個任務：
      - T1：後端實作 /api/login 與 JWT middleware
      - T2：前端實作登入表單與錯誤提示
      - T3：撰寫端到端整合測試 (設定 depends_on 等待 T1 與 T2 完成)
   2. 用 ertangle_spawn 派生兩個開啟 worktree 的 worker (worker-backend 與 worker-frontend) 平行執行 T1 與 T2。
   3. 用 ertangle_wait 等待它們回報 TASK_DONE，檢查無誤後再派或喚醒 worker 執行 T3。
   ```

**方式 B：使用 TUI 命令列單行啟動**
```text
:spawn --role coordinator claude:sonnet "請拆解 JWT 登入認證功能，建立任務看板並派生兩個 worktree worker 分頭實作前後端，完成後彙整向我回報"
```

**方式 C：使用內建 Playbook 團隊範本一鍵啟動**
```text
:playbook coord-review 實作 JWT 登入認證與前後端整合
```
(或按 `Space s b` 從選單挑選 `coord-review`、`tdd-pair`、`dual-review` 等範本)。

---

#### 第二步：觀察 Coordinator 與 Worker 的自動協作過程

一旦送出提示詞，`ertangle` 會在幕後自動完成以下協同流程，你只需在 TUI 旁觀或隨時介入：

```
[Coordinator: coord] (主分支，負責規劃與調度)
   ├── 1. 呼叫 ertangle_task_create 建立任務看板 (T1, T2 為 todo；T3 因 depends_on 自動設為 blocked)
   ├── 2. 呼叫 ertangle_spawn 派生子 Agent
   │
   ├─► ↳ [worker-backend]  (獨立分支 @ertangle/ses_a1 · .ertangle/wt/ses_a1)
   │      ├── 呼叫 ertangle_task_update(T1, status="doing")
   │      ├── 修改 src/auth.rs、執行單元測試
   │      ├── 呼叫 ertangle_task_update(T1, status="done")
   │      └── 呼叫 ertangle_send(to="coord", "TASK_DONE task_1 Implemented /api/login and JWT middleware") 後立即結束回合
   │
   └─► ↳ [worker-frontend] (獨立分支 @ertangle/ses_b2 · .ertangle/wt/ses_b2)
          ├── 呼叫 ertangle_ask(peer="worker-backend", "QUERY: What is the exact JSON schema for 401 errors?")
          ├── 修改 web/login.ts、完成表單驗證
          ├── 呼叫 ertangle_task_update(T2, status="done")
          └── 呼叫 ertangle_send(to="coord", "TASK_DONE task_2 Added login form and error handling") 後立即結束回合
```

在 TUI 畫面上你會看到：
- **左側 Sessions 欄**：自動呈現父子縮排階層 (`↳ worker-backend [worker]`)、各自所在的分支名稱，以及目前正在執行的工具摘要。
- **中央 Tasks 視圖 (按 `Tab` 切換)**：即時看到 T1、T2 進入 `doing`；當 T1 與 T2 都標記為 `done` 的瞬間，原本處於 `blocked` (`⧗`) 的 T3 會**自動解鎖為 `todo` 並主動發信喚醒負責人**開工。
- **零寒暄協定 (Zero Pleasantries)**：Agent 之間強制使用精簡英文結構化格式 (`TASK_DONE`、`TASK_BLOCKED`、`QUERY:`、`ANSWER:`) 溝通，Worker 送出 `TASK_DONE` 後立即終止回合，絕不互相客套說「Thank you」而消耗額外 Token。

---

#### 第三步：審查 Diff、處理衝突與安全合併

當 Worker 們完成實作後，你可以逐一檢視成果並合入主分支：

1. **檢視變更**：用 `j` / `k` 選取 `worker-backend`，按 **`Tab`** 切到 **Diff 視圖**。
   - 按 `(` / `)` 在不同修改檔案間跳轉，按 `Enter` 將檢查完的檔案摺疊收起。
   - 若某一行需要微調，直接將游標移到該行按 **`c`** 輸入審查意見 (如「Token 過期時間請改為由設定檔讀取」)，Worker 會自動被喚醒並針對該行修正。
   - 若發現某個多餘的 debug 區塊，直接按 **`X`** (`:discard`) 將該 hunk 退回，不必再花一輪 Token。
2. **安全合併回主分支**：
   - 確認 `worker-backend` 的 Diff 無誤後，按 **`M`** (`:merge`) 將其自動 commit 並合併回 `main`。
   - 接著選取 `worker-frontend` 按 **`M`**。若兩個 Worker 剛好改到同一個共用設定檔而發生 Git 衝突，只需執行 **`:merge --resolve`** (或按 `Space g r`)：`main` 主分支會保持乾淨，衝突標記會留在 `worker-frontend` 自己的 Worktree 中，並自動喚醒該 Worker 解完衝突與重跑測試。
3. **清理與匯出報告**：
   - 按 **`G`** (`:gc`) 一鍵清除已結束且已合併的 Worktree 分支。
   - 在 Coordinator Session 上輸入 **`:export`**，即可將整棵父子協作樹的對話重點、各 Worker 花費與任務清單匯出為 Markdown 報告並自動複製到剪貼簿，直接貼上 GitHub PR 描述。

---

## 核心功能與工作流程

### 1. 工作區隔離與審查迴圈 (Review Loop)

- **Worktree 獨立分支**：每個 session 使用專屬 `git worktree` 與 `ertangle/<session>` 分支，互不干擾。
- **即時彩色 Diff**：在 TUI 按 `Tab` 切換至 Diff 視圖，按 `{` / `}` 跳轉 hunk，`(` / `)` 跳轉上／下一個檔案，`Enter` (或 `v`) 摺疊／展開單一檔案，按 `X` (或 `:discard`) 安全退回選取的 hunk 或整個檔案變更。
- **行內註解審查 (Review Comment)**：在 Diff 視圖按 `c`，直接對該行留下修改建議 (例如：「請補上邊界條件檢查」)，系統會自動將註解送回該 Agent 要求修正。
- **安全合併與衝突解決 (Merge & Conflict Resolution)**：按 `M` 或執行 `ertangle merge` 合併回主分支，策略可選 `merge` / `squash` / `rebase`。衝突時可用 `:merge --resolve` 委派 Agent 處理。
- **清理 (GC)**：按 `G` 或執行 `ertangle gc`，自動清除已結束且已合併的 Worktree，絕不誤刪未合併成果。

### 2. 跨 Agent 協作匯流排與任務看板 (MCP & Bus)

Daemon 會在啟動 session 時自動注入包含 **16 個協作工具**的 stdio MCP 伺服器：

| 類別 | 工具名稱 | 功能 |
| --- | --- | --- |
| **自我與環境** | `ertangle_whoami`, `ertangle_peers` | 查詢自己的 session ID、工作區分支，以及其他同伴資訊 |
| **訊息傳遞** | `ertangle_send`, `ertangle_inbox`, `ertangle_wait`, `ertangle_ask` | 發送訊息、非同步讀取收件匣、阻塞等待新訊息、同步問答 |
| **任務看板** | `ertangle_task_list`, `ertangle_task_create`, `ertangle_task_update` | 建立協作任務、指定負責人、支援 `depends_on` 前置相依性 |
| **衍生與產出** | `ertangle_spawn`, `ertangle_artifact_share`, `ertangle_status` | 派生子 Agent、分享產出物、廣播進度狀態 |
| **互動與核准** | `ertangle_question`, `ertangle_approve` | 提出互動式問題 (單選／複選／文字自填)、工具使用權限核准轉發 |
| **Git 整合** | `ertangle_diff`, `ertangle_merge` | 查詢自身分支與主分支 diff、請求合併 |

- **空閒提醒 (Unread Nudge on Idle)**：
  - 當 Agent 正忙於執行回合時，發來的直接訊息 (或廣播) 只會暫存在收件匣，**不打斷運算**。
  - 當 Agent 結束回合回到 `idle` 且佇列中的指令處理完畢後，系統會將未讀訊息合併為一則提醒，每則訊息**僅提醒一次**。
  - 已透過 `ertangle_wait` / `ertangle_ask` 取走或超過成本預算的 session 則不予打擾。
- **任務相依性 (Task Dependencies)**：
  - 建立任務時可帶 `--depends-on <id>`，在前置任務全數 `done` 之前，任務維持 `blocked` 且不打擾負責人。
  - 前置任務完成後自動轉為 `todo`，並主動發信喚醒指派的負責人。
- **信箱後備**：所有訊息同步寫入專案目錄下的 `.ertangle/mailbox/<session>.jsonl`。

### 3. 額度監控、智慧交接 (Handoff) 與接續 (Resume)

- **額度監視**：
  - 即時解析 Agent 回報的額度狀態 (Claude 的 5 小時／7 天視窗與重置倒數；Codex 的 rateLimits)。
  - 標題列即時標示最吃緊視窗 (`≥ 70%` 黃色、`≥ 90%` 紅色)；輸入 `:quota`、按 `Space s q` 或點擊標題列的額度摘要，會彈出額度面板，以 progress bar 列出本機已安裝 agent 的各視窗用量與重置倒數。
- **智慧交接 (Handoff)**：
  - 當 Agent 回報額度用完、被限流，或使用者手動要求 (`:handoff <agent> [備註]`)：
  - 舊 Agent 停止執行，工作區未 commit 變更自動 commit；
  - Daemon 彙整任務背景、指派任務、最後輸出與完整 Diff 生成交接簡報；
  - 新 Agent **直接沿用同一個工作目錄與分支**接手；
  - 之後發往舊 session 的直接訊息會**自動改投給新接手者**。
- **對話接續 (Resume)**：
  - 若 Agent 處理程序異常中斷或 Daemon 重啟，按下 `Space s r` 或執行 `ertangle resume <session> [prompt]`，即可喚回**原本的對話**。
  - 利用各家原生的對話回復機制 (Claude `--resume`、Codex `thread/resume`、ACP `session/load`)，保持同一個 session ID、逐字稿與分支。

### 4. 資源控管與休眠 (Budget & Limits)

- **成本上限設定**：
  - 支援單一 session 預算 (`session_usd`)、子樹總額上限 (`tree_usd`)、Token 上限 (`session_tokens`)，以及最大子 Agent 數量與層級深度。
  - 達到警戒線 (預設 80%) 黃色警告，超過上限立即阻斷執行並暫停排隊的 prompt，使用 `:budget <金額>` 調高後自動恢復。
- **閒置休眠**：
  - 按 `z` 或執行 `ertangle sleep <session>`，發送 `SIGSTOP` 暫停 Agent 處理程序節省系統資源；按 `z` 或收到訊息時發送 `SIGCONT` 自動喚醒。

### 5. 權限政策與安全防護 (Policy Engine)

- 提供跨 Agent 的統一控制旋鈕 `--permission <ask|auto|bypass>`：
  - `ask`：所有工具呼叫轉交由使用者在 TUI 逐一按 `a` (允許) / `A` (一律允許) / `d` (拒絕) 核准。
  - `auto` (預設)：安全工具自動核准。
  - `bypass`：跳過權限確認。
- **規則引擎**：支援全域 `~/.config/ertangle/policy.toml` 與專案 `.ertangle/policy.toml`，以 `deny` (黑名單) → `allow` (白名單) → session 政策順序過濾指令。
- **Claude 逐項核准轉發 (T33)**：非 `bypass` 模式下為 Claude 掛載 `--permission-prompt-tool mcp__ertangle__ertangle_approve --permission-prompts host`，使 Claude 的單一工具呼叫能受到 `policy.toml` 攔截並在 `ask` 模式下由 TUI 互動核准。
- **全面秘密遮罩 (Secret Redaction, T33)**：所有 MCP 工具輸出 (包含 `ertangle_inbox`、`ertangle_wait`、`ertangle_ask`、`ertangle_diff` 等) 與事件紀錄在送出前均自動過濾常見 API Key 與 Token (`sk-...`、`ghp_...`、`AKIA...` 等)，遮罩為 `<REDACTED>`。

### 6. 遠端連線、Tailscale 整合與斷線自動重連 (T28)

- **自動偵測 Tailscale 位址**：
  - 支援 `ertangle daemon --listen tailscale`，自動呼叫本機 `tailscale ip -4` 解析點對點安全通道 IP，無需手動查詢輸入。
  - 自動生成並持久化安全認證 Token 於 `~/.local/state/ertangle/remote.token` (檔案權限嚴格限制為 `0600`)。
- **終端機連線資訊速查**：
  - 執行 `ertangle daemon info` 即可一鍵印出目前本機與 Tailscale 遠端連線位址、Token 與連線指令。
- **無縫斷線重連 (Auto-Reconnect)**：
  - 當筆電休眠喚醒、跨 Wi-Fi 切換網路或短暫斷線時，TUI 會在背景以指數退避演算法自動重試。
  - 連線恢復時自動發送帶有最後已知序列號的 `Request::Hello { since, epoch, token }`，無縫還原狀態並補齊離線期間遺漏的事件記錄。

### 7. TUI 多主機聚合視圖 (Multi-Host Aggregation, T29)

- **跨機統一儀表板**：
  - 在 `config.toml` 中配置 `[remotes]` 或使用命令列參數 `ertangle --remote workstation:7420 --remote gpu-box:7420`。
  - 單一 TUI 畫面即可聚合呈現本機與所有遠端伺服器上的 Agent Sessions。
- **智慧路由與篩選**：
  - Sessions 清單依主機顯示彩色來源標記 (`[workstation]`、`[gpu-box]`)。
  - 發送 Prompt、審核權限或檢視 Diff 時，指令會自動透明路由至所屬的遠端主機。
  - 按 `:` 輸入 `:host <名稱>` (或 `:host all`) 可隨時切換聚焦單一主機或全域視圖。

### 8. Web 版支援與行動介面 (Web Interface & Mobile PWA, T30)

- **隨身攜帶、買咖啡不中斷**：
  - 離開座位時，無需在手機狹窄螢幕透過 SSH 艱苦操作終端機。可在 TUI 設定面板 (`:settings` 或 `Space ,`) 中直接切換啟用，或輸入 `:web on [位址]` (亦可從 CLI 啟動 `ertangle daemon --web tailscale`)。
  - 終端機內建 **Unicode QR Code**，在 TUI 輸入 `:web qr` 或按 `Space w`，手機打開相機一掃即可在瀏覽器開啟專屬 PWA 介面。
- **零相依原生 HTTP / WebSocket**：
  - Daemon 內部以純 Rust 同步網路實作，不引入 Node.js 或額外 async 框架，維持單一執行檔與極低記憶體開銷。
- **App 級大拇指原生設計**：
  - **待審核懸浮橫幅與互動問答**：Agent 遇到工具執行或危險指令等待核准時，頂部即時彈出醒目核准卡片，單鍵 Allow / Always / Deny；支援互動式問題直接在卡片上點選或輸入文字回覆。
  - **對話回合 (Turn)**：將逐字稿自動重構為結構化對話回合，收攏正文、摺疊 Agent 內部思考過程 (`⋯`) 與工具執行記錄。
  - **即時思考動畫**：思考中的對話區塊具備動態呼吸指示器 (`⋯ 思考中`) 與最新思考片段預覽，完全無 emoji。
  - **斜線命令面板與 Shell 執行**：輸入 `/` 即可喚出命令選單；輸入 `!<命令>` 或 `/sh <命令>` 可直接在該 session 工作目錄執行指令。
  - **已結束 Session 收合與封存**：自動將已結束的對話摺疊至側欄底部，提供「清理已結束」按鈕與抽屜式封存列表隨時調閱。
  - **對話 Markdown 與行內圖片預覽**：完整渲染 `h1`–`h6` 標題、保留編號的有序清單、縮排項目符號與引言，程式碼區塊附帶一鍵「複製」按鈕，並支援 `![alt](url)` 行內圖片預覽與嚴格來源 / 路徑安全驗證。
  - **文字與圖片檔案即時預覽**：點擊對話中的檔案膠囊，直接在面板中即時預覽文字與程式碼內容 (含行號跳轉與高亮)，或直接檢視渲染圖片。
  - **即時 Diff 審查與一鍵 Merge**：在手機端審視 Git 變更，確認無誤後單鍵完成合併。

---

## TUI 終端機介面操作指南

### 1. 版面與檢視

- **版面模式**：寬螢幕 (`>= 130` 欄) 下預設自適應展開為「三欄」 (左側 `Projects & Sessions` + 中央 + 右側上下分割的 `Flow` 節點拓撲圖與 `Bus / Tasks`)；按 `L` 可手動鎖定並循環切換「兩欄」、「分頁」 (頂部 Tabs + 中央) 與「三欄」，輸入 `:layout auto` 可恢復自適應，按 `b` 可切換右側面板於 `Flow + Bus` 與 `Project` 之間。
- **中央視圖**：按 `Tab` (或按 `F` 直達 Flow) 依序切換主要視圖：
  1. **Overview**：即時串流逐字稿，支援 Markdown 渲染 (標題、清單、表格、程式碼區塊等)。
  2. **Diff**：即時彩色 Git Diff 視圖，支援行級審查註解。
  3. **Worktrees**：檢視目前所有由 ertangle 建立的 Git Worktree 狀態與磁碟路徑。
  4. **Tasks**：協作看板視圖，任務依狀態 (doing、review、todo、blocked、done) 分組，顯示前置任務相依性 (`⧗`)。按 `Enter` 直達負責人 Session。
  5. **Flow**：90 度正交節點拓撲圖與封包動畫，即時呈現 Coordinator 與 Worker 間的協作拓撲與訊息流向。
- **版本識別**：`ert --version` 與 TUI 狀態列皆會顯示 `v<semver>+<short_hash>`，方便開發時辨識目前建置版本。

### 2. 快速鍵速查表

#### 導航、選單與核心動作

| 按鍵 | 動作 |
| --- | --- |
| `Space` | 開啟 **Which-key 選單** (`s` session、`g` git、`v` 檢視、`m` 訊息、`?` 說明) |
| `:` 或 `/` | 開啟**命令列** (支援 `Tab` 自動補齊) |
| `!` | 進入 **Shell 命令模式** (在選取 Session 的工作目錄直接執行命令 `!<命令>`) |
| `n` | 建立新 Session (依序選擇 Agent、Model 與輸入 Prompt；支援 `Ctrl-R` 切換角色、`Ctrl-W` 切換 Worktree、`Ctrl-F` 切換 Effort) |
| `i` / `Enter` | 聚焦輸入框，對選取的 Session 發送 prompt (亦支援 `@peer` 或 `#channel` 傳訊) |
| `j` / `k` 或 `↑` / `↓` | 在左側 `Projects & Sessions` 移動選取 Session 與摺疊列；在 Diff 視圖移動行游標；在 Tasks 視圖移動任務游標 |
| `l` / `→` / `h` / `←` / `Space` | 在左側欄展開 (`l` / `→`) 或收合 (`h` / `←`) 已結束 (`▸ 已結束 (N)`) 與其他專案摺疊列 (`Space` 切換) |
| `[` / `]` | 快速跳至上一個 / 下一個 Session |
| `Ctrl-K` | **Jump-Anywhere**：依關鍵字模糊搜尋並跳轉 Session |
| `Space ,` (或 `:settings`) | 開啟**設定面板** (即時切換預設 Agent、Model、Effort、Worktree、Web 版支援等) |
| `Space w` (或 `:web qr`) | 彈出**Web 版網址與 QR Code** 彈窗 (手機掃描即開即用) |
| `q` / `Q` | `q` 關閉 TUI (Daemon 與 Agent 續跑)；`Q` 關閉 Daemon (等同 `:shutdown`) |

#### 逐字稿、搜尋與複製

| 按鍵 | 動作 |
| --- | --- |
| `t` | 切換逐字稿顯示模式：Markdown 渲染 ↔ 原始文字 |
| `T` (或 `:thoughts`) | 顯示 / 隱藏 Agent 的思考行 (⋯，預設摺疊為摘要以加速渲染並呈現動態動畫) |
| `O` (或 `:tools`) | 展開 / 摺疊工具呼叫紀錄 (連續成功呼叫預設摺疊) |
| `f` (或 `:find <文字>`) | **即時搜尋**逐字稿或 Diff (邊打邊找，`Enter` / `↓` 下一個、`↑` 上一個、`Esc` 退出) |
| 滑鼠拖曳 | 在逐字稿中央區塊拖曳選取，放開即透過 **OSC 52** 複製至系統剪貼簿 |
| `Y` (或 `/copy`) | 複製目前中央區塊的完整文字 (不含側欄與邊框) |
| `F2` | 切換滑鼠擷取模式 (關閉後可使用終端機原生選取，或使用 `Shift`+拖曳) |
| `PgUp` / `PgDn` | 向上 / 向下翻頁捲動逐字稿 (捲動時暫停自動跟隨) |
| `Home` / `End` | 跳至頂部 / 回到底部並恢復跟隨最新輸出 (`▼ follow`) |
| `P` | 切換專案範圍：只看本專案 (`here`) ↔ 顯示全部專案 Session (`all`) |

#### Prompt 輸入與排隊

| 按鍵 / 語法 | 功能說明 |
| --- | --- |
| `!<命令>` | **在 Session 工作目錄執行 Shell 命令** (不經過 agent，例如 `!cargo test`；亦可在 Normal mode 按 `!` 直達) |
| `@檔名` | **檔案附件模糊搜尋**：輸入 `@` 片段，自動在專案內搜尋檔案並補齊，送出時自動內嵌文字 (上限 256 KB) 或圖片 (上限 5 MB) |
| `Ctrl-V` | 貼上系統剪貼簿內的圖片 (Linux 需安裝 `wl-paste` 或 `xclip`) |
| 拖曳檔案 | 直接將檔案從檔案總管拖進終端機，自動轉換為 `@路徑` 語法 |
| `Shift-Enter` / `Ctrl-Enter` / `Ctrl-J` | 輸入框原生換行 (透過 Kitty 鍵盤增強協定支援，並保留 `Ctrl-J` 相容；輸入框隨行數自動伸展，最多 8 行) |
| `Option` / `Cmd` 編輯快速鍵 | 支援 macOS / readline 游標與刪除操作 (`Option+←/→`、`Option+Backspace` / `Ctrl-W`、`Option+Delete`、`Cmd+←/→/Backspace`、`Ctrl-K`) |
| 停止 Session 提示確認 | 對已結束、失敗或休眠中的 Session 送出提示詞時，彈出確認視窗後自動喚醒或恢復執行 (取消則保留草稿) |
| `Alt-Enter` | **將 Prompt 排入佇列** (Session 忙碌時排入 Daemon 佇列，閒置時依序自動送出) |
| `↑` / `↓` | 在空輸入框時切換歷史輸入指令 (持久化儲存於本機) |

#### 建立 Session (Spawn 模式快速鍵)

| 按鍵 | 功能說明 |
| --- | --- |
| `Ctrl-R` | 循環切換角色：`worker` ↔ `coordinator` ↔ `reviewer` (依角色自動調整協作提示詞) |
| `Ctrl-W` | 切換是否啟用獨立 Git Worktree 分支 |
| `Ctrl-E` | 循環切換權限政策：預設 (config) ↔ `ask` ↔ `auto` ↔ `bypass` |
| `Ctrl-F` | 循環切換推論強度 Effort：預設 ↔ `low` ↔ `medium` ↔ `high` ↔ `max` |

#### 審查、Git 與生命週期控制

| 按鍵 | 動作 |
| --- | --- |
| `{` / `}` | (Diff 視圖) 跳至上一個 / 下一個 hunk (`@@`) |
| `(` / `)` (或 `<` / `>`) | (Diff 視圖) 跳至上一個 / 下一個檔案 (`diff --git`) |
| `Enter` / `v` | (Diff 視圖) 摺疊 / 展開目前游標所在的檔案 |
| `X` (或 `:discard`) | (Diff 視圖) 退回游標所在的 hunk 或整個檔案變更 (再按一次 `X` 或 `:discard!` 確認) |
| `c` | (Diff 視圖) 針對當前選取的行留下 Review 審查註解並送回 Agent 修改 |
| `M` | 將選取 Session 的 Worktree 分支安全合併回主分支 (可先用 `:merge --dry-run` 預覽、`:merge --squash` 壓成單一提交，或 `:merge --resolve` 委派解衝突) |
| `G` | 清理 (GC) 已結束且已合併的 Worktree |
| `z` | 切換暫停休眠 (`SIGSTOP`) 與喚醒 (`SIGCONT`) |
| `x` | 中斷 Agent 當前回合運算 (Cancel) |
| `K` | 強制結束選取的 Agent 子處理程序 (Kill) |
| `a` / `A` / `d` | 審核請求：允許一次 / 本回合一律允許 / 拒絕 |

### 3. TUI 命令列 (`:command`) 速查

按 `:` 進入命令列，支援 `Tab` 自動補齊：

- `:spawn [--role <角色>] <agent>[:model] <prompt>` — 啟動新 Agent Session (如 `:spawn --role coordinator claude:opus "規劃架構"`)
- `:playbook [<名稱> <目標需求>]` — 啟動多 Agent 團隊範本 (如 `:playbook tdd-pair 完成登入功能`)；不帶參數開選擇器 (也可按 `Space s b`)
- `:export [路徑]` — 匯出目前 Session (含子 Session 樹)為 Markdown 至 `.ertangle/exports/<label>-<timestamp>.md`，並透過 OSC 52 複製到剪貼簿
- `:exec <命令>` (或 `:sh`) — 在選取 Session 的工作目錄直接執行 shell 命令 (不經過 agent，亦可輸入 `!<命令>`)
- `:handoff <agent> [備註]` — 將當前 Session 交接給另一位 Agent 接手
- `:resume [prompt]` — 接續已結束或中斷的 Session 對話
- `:restart` — 重新執行 Daemon 並自動接續正在運算中的 Session
- `:merge [--dry-run] [--resolve] [--strategy merge|squash|rebase]` — 合併選取 Session 的 Worktree 回主分支 (亦可用 `--squash` / `--rebase` 簡寫，支援自動委派解衝突)
- `:discard` / `:discard!` — (Diff 視圖) 退回游標所在的 hunk 或整個檔案變更
- `:queue` / `:unqueue` / `:queue clear` — 檢視、取出或清空待送佇列
- `:rm` / `:clean` — 移除已結束的 Session / 清除所有已結束項目
- `:prune` — 封存所有已合併且已結束的 Worker Session，並清理其 Git Worktree
- `:budget <金額>` — 查看或調高當前 Session 的美元預算上限
- `:quota` — 彈出額度面板：已安裝 agent 的各視窗用量 (progress bar) 與重置倒數
- `:task <標題>` / `:tasks` — 建立新任務 / 切換到任務看板視圖
- `:task-status <todo|doing|blocked|review|done>` — 修改選取任務的狀態
- `:host <目標|all>` — 切換多主機視圖篩選 (T29)
- `:remote` — 顯示目前主機的遠端連線資訊、Tailscale IP 與行動端 Web 網址
- `:notify on|off|test` — 開關桌面通知或測試發送
- `:shutdown` — 關閉共用 Daemon 與所有專案的 Agent (安全保護：需輸入 `:shutdown!` 確認)

---

## CLI 命令參考

`ertangle` 提供與 TUI 對等的完整 CLI 工具：

```bash
# Session 管理
ertangle sessions [--json]                         # 列出所有 session (別名 ertangle peers)
ertangle whoami [--session <id>] [--json]          # 查看當前 session 資訊
ertangle spawn --kind <agent> [--label <label>] [--worktree] [--prompt <prompt>]
ertangle resume <session-or-label> ["接續 prompt"] # 接續已中斷的 session
ertangle restart                                   # 重啟 Daemon 並自動接續執行中的 session
ertangle exec <session> <command...>               # 在指定 session 工作目錄執行命令 (別名 ertangle sh)
ertangle handoff <session-or-label> --to <agent> [--note "交接補充"]
ertangle sleep <session> ; ertangle wake <session>  # 暫停 / 喚醒 Agent 處理程序
ertangle cancel <session> ; ertangle kill <session> # 中斷回合 / 強制中止處理程序
ertangle rm <session> [--force]                    # 移除已結束的 session 紀錄
ertangle prune [--project <path>]                  # 封存已合併且已結束的 worker 並清理 worktree

# 多 Agent 團隊範本與協作報告
ertangle playbook <name> "<goal>"                  # 展開內建或自訂 playbook，一次啟動整個團隊
ertangle export <session> [--tree] [--diff] [-o <path>] # 匯出 Markdown 報告 (-o 省略則印到 stdout)

# 提示詞與佇列
ertangle prompt [--queue] <session-or-label> "提示詞"
ertangle queue <session-or-label> [--clear]        # 查看或清空待送佇列

# Worktree 與程式碼審查
ertangle worktrees                                 # 列出所有由 ertangle 管理的 worktree
ertangle diff <session-or-label>                   # 檢視指定 session 的 git diff
ertangle review <session-or-label> --location <file>:<line> "審查意見"
ertangle merge <session-or-label> [--dry-run] [--resolve] [--strategy merge|squash|rebase] # 合併分支回主工作區
ertangle pr <session-or-label> --title "PR 標題"   # 透過 gh 建立 Pull Request
ertangle gc [--session <id>]                       # 清理已結束且已合併的 worktrees

# 跨 Agent 訊息與任務看板
ertangle send <session-or-label> "訊息內容"         # 發送訊息給指定 agent
ertangle inbox [--session <id>]                    # 檢視收件匣
ertangle tasks                                     # 列出所有任務與相依性狀態
ertangle task "任務標題" [--assignee <label>] [--depends-on <id>]
ertangle task-update <id> --status <status>        # 更新任務狀態 (todo/doing/blocked/review/done)

# 成本、額度與審核記錄
ertangle quota [--json]                            # 查詢帳號額度視窗與重置倒數
ertangle cost [--json]                             # 統計各 session 與整體 token / 美元花費
ertangle budget <session-or-label> <金額>          # 設定或調高預算上限
ertangle replay [--since <seq>] [--json]           # 重播與審核 events.jsonl 事件記錄

# 遠端 Daemon、多主機與 Web 版支援
ertangle daemon --listen tailscale                 # 啟動 Daemon 並自動綁定 Tailscale 安全通道
ertangle daemon --listen 127.0.0.1:7420            # 啟動 Daemon 並監聽 TCP (自動產生 token)
ertangle daemon --web tailscale                    # 啟動 Web 版並印出終端機 QR Code (預設 port 7421)
ertangle daemon --web tailscale --web-port 8080    # 啟動 Web 版並指定自訂 port (8080)
ertangle daemon info                               # 查詢目前主機的 Tailscale 位址、Token 與連線指令
ertangle --remote tailscale                        # 連線至本機 Tailscale Daemon
ertangle --remote workstation:7420 --remote gpu:7420 # 同時連線多台主機並於單一 TUI 聚合呈現
ertangle completions <bash|zsh|fish>               # 產生 Shell 自動補齊腳本
```

---

### 安裝 ertangle-ade skill

內嵌的 `ertangle-ade` skill 說明通訊協定、省 token 規則與工作流程：

```bash
ertangle skill install [--target claude|agents|codex] [--global|--project]
ertangle skill uninstall [--target ...] [--global|--project]
ertangle skill show
```

在 TUI 裡也可以用 `:skill install [claude|agents|codex] [global]` 安裝 (`:skill show` 查看狀態)。

不想安裝時，可在 `:settings` 開啟「把 ertangle-ade skill 注入協作說明」，或在設定檔加入 `[collab] inject_skill = true`，把 skill 內文附加到每個 agent 的協作說明。同樣地，`:settings` 的「把省 token 提示注入協作說明」(`[collab] token_hints = true`，預設關閉) 會附加幾條節省 token 的工具使用建議 (獨立呼叫放在同一回合、合併唯讀探查、限制輸出長度)；session 次要資訊列的 `N calls` 可用來比較開關前後的工具呼叫數。額度達到 `[handoff] warn_percent` 時，agent 自行 spawn 會改用 `fallback` 或被拒絕，coordinator 也會收到 `QUOTA_WARN` 訊息；`[agent_spawn] max_live_children` (預設 3) 限制同時執行的子 session 數。Claude 回報 `allowed_warning` (例如 7 天視窗超過 75%) 時，coordinator 會先收到較溫和的 `QUOTA_NOTICE`，標題列與 `:quota` 也會標示「⚠ 接近上限」。

### 中斷復原與編譯資源

- `[agent_spawn] auto_resume` (預設 `true`)：daemon 重啟時，自動接續當下正在執行回合的 session，並提示 agent 先用 `git status`／`git diff` 找回進度，不要重新探索整個專案。額度已滿、或上次自動接續後沒有任何進展的 session 不會再自動接續。
- `[auto_compact] enabled` (預設 `true`)、`threshold_percent` (預設 `80`，限制在 10 到 95)：session 回到閒置且上下文用量達到門檻時，daemon 自動代送 `/compact`；僅對宣告支援 `compact` 斜線指令的 agent (如 Claude) 生效，只讀全域設定檔。也可在設定面板的「上下文」區調整。
- `[agent_spawn] shared_cargo_target` (預設 `true`)：Cargo 專案裡，所有 worktree 的 agent 共用 `<repo>/.ertangle/target` 當 `CARGO_TARGET_DIR`。不用每個 worktree 各編一份，cargo 的目錄鎖也讓多個 worker 的編譯與測試自動排隊。已自行設定 `CARGO_TARGET_DIR` 時不覆寫。
- 兩者都可在 `:settings` 切換。

## 設定檔與環境變數

### 設定檔：`config.toml`

`ertangle` 依序讀取全域設定 (`~/.config/ertangle/config.toml`) 並由專案設定 (`<repo>/.ertangle/config.toml`) 覆寫：

```toml
[tui]
language = "auto"                 # 介面語言："auto" (依 LANG) | "zh-TW" | "en"；只認全域設定
layout = "two"                    # 預設版面："two" | "three" | "tabs"
notify = ["bell", "command"]      # 通知方式："bell" | "osc9" | "command" (可複選)
notify_command = "notify-send ertangle \"$ERTANGLE_NOTIFY_TEXT\""
notify_min_turn_secs = 10         # 短於此秒數的回合完成不觸發通知

[web]
# Web 版支援設定 (只認全域設定；亦可在 TUI :settings 中隨時切換)
enabled = true                    # 是否啟用
port = 7421                       # 自訂連接埠：預設 7421 (亦可直接於 addr 指定，例如 "127.0.0.1:8080")
addr = "tailscale"                # 監聽位址："tailscale" (自動偵測 IP) | "127.0.0.1" | "0.0.0.0"

[remotes]
# 跨主機聚合設定 (T29)
workstation = { addr = "100.64.0.5:7420", token_file = "~/.config/ertangle/workstation.token" }
gpu-box = { addr = "100.64.0.8:7420" }

[limits]
session_usd = 5.0                 # 單一 Session 成本上限 (美元)
tree_usd = 15.0                   # 父子 Agent 樹累計成本上限
warn_percent = 80                 # 成本與額度警告門檻百分比
max_children = 5                  # 單一 Session 允許派生的最大子 Agent 數
max_depth = 3                     # 子 Agent 派生深度上限

[handoff]
warn_percent = 90                 # 額度消耗達 90% 時提示建議 :handoff
auto = false                      # 額度用盡時是否自動交接 (預設 false 避免非預期費用)
fallback = ["codex", "claude:sonnet"]

[collab]
instructions = true               # 是否自動向 Agent 注入 ertangle 協作工具使用規範
extra = "請優先遵循專案的 CONTRIBUTING.md 規範"

[playbooks.tdd]
# 內建範本：coord-review、tdd-pair、dual-review；同名設定會覆寫內建範本，enabled = false 可停用
description = "TDD 協作團隊 (Coordinator + Test Worker + Impl Worker)"
enabled = true
[[playbooks.tdd.sessions]]
label = "coord"
kind = "claude"
role = "coordinator"
prompt = "請協調團隊完成以下目標：{{goal}}"
[[playbooks.tdd.sessions]]
label = "reviewer"
kind = "opencode"
role = "reviewer"
worktree = true

[git]
# Git 與 worktree 設定 (安全起見只認全域設定，專案層不可覆寫)
pre_merge_command = "cargo test"       # 合併前驗證指令；非 0 退出則中止合併
merge_strategy = "squash"              # 合併策略：merge / squash / rebase
```

`merge_strategy` 也可在 TUI 設定面板 (`:settings`) 切換；明確指定優先。

> [!NOTE]
> 介面語言也可以在設定面板 (`:settings`) 切換，立即生效。daemon 回傳的訊息跟 daemon 啟動時的語言走，切換後要重啟 daemon 才會一致。想協助翻譯請見 [翻譯指南](docs/TRANSLATING.md)。

> [!TIP]
> 專案內可建立 `.ertangle/instructions.md` (上限 8 KB)，內容會在啟動時自動附加至 Agent 的 System Prompt 底部。

### 支援的環境變數

| 環境變數 | 預設值 | 說明 |
| --- | --- | --- |
| `XDG_RUNTIME_DIR` | `/tmp/ertangle-$USER/` | Daemon Unix domain socket 存放目錄 |
| `XDG_STATE_HOME` | `~/.local/state/` | 事件持久化日誌路徑 (`$XDG_STATE_HOME/ertangle/events.jsonl`) |
| `ERTANGLE_LOG` | `warn` | Tracing 日誌過濾等級 (例如 `ERTANGLE_LOG=debug`) |
| `ERTANGLE_TOKEN` | — | 遠端連線認證 Token (建議搭配 SSH 轉發避免明文傳輸) |
| `ERTANGLE_REMOTE` | — | 預設遠端 Daemon 位址 (等同 `--remote` 參數) |

---

## 開發與貢獻前檢查

為確保本地提交能穩定通過 CI 檢查，提交程式碼前可安裝 pre-commit hook 或執行一鍵驗證：

```bash
make setup-hooks   # 自動安裝 .git/hooks/pre-commit
make pre-commit    # 一鍵自動排版 (fmt)、跑 clippy 與檢查多國語系 (i18n)
```

---

## 進階架構與相關文件

- [`docs/DESIGN.md`](docs/DESIGN.md) — 系統架構、事件同步協定、狀態機與 MCP 規格定義。
- [`docs/VERIFICATION.md`](docs/VERIFICATION.md) — 實機驗證成果、各家 Agent 相容性測試紀錄。
- [`docs/MAINTAINING.md`](docs/MAINTAINING.md) — 開發者維護指南與修改同步檢查清單。
- [`docs/tickets/`](docs/tickets/) — 系統重構、功能擴充與改善計畫票紀錄。

---

## 授權條款

本專案依 [MIT 授權條款](LICENSE) 開源。

---

## 致謝

`ertangle` 的發想來自 [Orca](https://github.com/stablyai/orca) (MIT 授權)，其 full handoff 與 review 迴圈的概念啟發了本專案的設計。本專案為獨立實作，未複製其程式碼。

---

## 命名由來

專案名稱 `ertangle` (念作 er-tangle) 由 ER (愛因斯坦-羅森橋) 與 entangle (糾纏) 組成，靈感源自 Maldacena 與 Susskind 提出的 ER=EPR 猜想 (量子糾纏等於愛因斯坦-羅森橋)。在 `ertangle` 中，每個 Agent 各自生活在獨立的宇宙 (Git Worktree)，並透過量子糾纏 (MCP 訊息匯流排) 相互連通與協同工作。本專案為 [erbridge](https://github.com/single9/erbridge) 的姊妹專案。
