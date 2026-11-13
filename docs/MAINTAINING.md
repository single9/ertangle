# 維護備忘：改了 A，記得同步 B

這些地方是**手動同步**的，沒有編譯器或測試會幫你擋。改動左欄的東西時，順手檢查右欄。

## TUI 按鍵、命令與選單

| 你改了 | 要同步 |
| --- | --- |
| 新增／改動 Normal 模式按鍵（`crates/ade-tui/src/input.rs` 的 `handle_normal_key`） | 說明面板 `help.rs` 的 `HELP`；README「TUI 操作」；底部狀態列提示（`draw.rs` 的 `draw_status`）只放最常用的，不必每個都加 |
| 新增／改名 `:` 命令（`commands.rs` 的 `execute_command`） | `commands.rs` 的 `COMMANDS` 表（決定 Tab 補全與提示浮層，漏了就不會被提示）；`help.rs` 的「命令」一節 |
| 新增 Space 選單項目（`menu.rs` 的 `menu_items`） | `help.rs`（有對應的獨立按鍵或命令時）；快捷鍵衝突要檢查同一層的 `key` 不重複 |
| 新增 spawn 輸入模式的 Ctrl 組合鍵 | `help.rs` 的「prompt 輸入」；`draw.rs` 裡 Spawn 模式的輸入框標題 |
| 新增畫面上的標記（顏色、`ctx`、額度、`!bypass` 等） | `help.rs` 的「畫面上的標記」 |

說明面板的內容是寫死在 `help.rs` 的清單，**沒有測試比對它和實際按鍵是否一致**。

## 協定與資料結構

| 你改了 | 要同步 |
| --- | --- |
| `SpawnRequest` 加欄位 | 所有用 struct literal 建立它的地方（目前有 `src/cmd/session.rs`、`crates/ade-tui/src/commands.rs`、`crates/ade-daemon/src/mcp.rs`、`handlers/session.rs` 的交接、以及測試）；新欄位一律 `#[serde(default)]`，舊 client 與舊 log 才能解析；MCP `ade_spawn` 的 schema 與說明 |
| `Session` 加欄位 | 所有 `Session { … }` literal（core、daemon、tui 的測試與 `handoff.rs` 測試）；`#[serde(default)]`，否則舊 `events.jsonl` 無法重播 |
| `Request`／`Frame` 加變體 | `handlers/mod.rs` 的 `match`（沒有萬用分支，漏了編譯不過）；CLI 子命令（`src/main.rs` 與 `src/cmd/`）；需要時加 `COMMANDS`／說明面板 |
| `Event` 加變體 | `State::apply`（`crates/ade-core/src/lib.rs`）要處理，否則重播不會還原該狀態；`ingest` 的 `AgentEvent` 對應；需要 snapshot 的欄位要 `#[serde(default)]` |
| `AgentEvent` 加變體 | 四個 adapter 是否要送；`ingest`（`crates/ade-daemon/src/lib.rs`）的 `match` |
| `SessionSpec` 加欄位 | 所有 literal：`handlers/session.rs`、`claude.rs`／`codex.rs` 測試、`tests/claude_fake.rs` |

## 設定檔

| 你改了 | 要同步 |
| --- | --- |
| `config.toml` 新增欄位（`crates/ade-daemon/src/config.rs`） | `Config::layered`（全域被 repo 逐欄覆寫，漏了 repo 設定會被忽略）；`resolve` 的優先序（請求 > 角色 > agent）；README 對應段落；加解析與覆寫的測試 |
| 新增 agent 支援的選項（model、effort、sandbox、工具限制…） | 各 adapter 套用的位置，以及 `crates/ade-adapter/src/lib.rs` `spawn()` 開頭的「不支援就明確報錯」檢查；不要靜默忽略 |

## Agent 相關

| 你改了 | 要同步 |
| --- | --- |
| 新增或調整某個 agent 的 model／effort 清單來源 | TUI 選擇器（`menu.rs` 的 `model_choices`／`query_models`）；Antigravity 的 id 尾巴會拆成 model + effort（`split_effort_suffix`） |
| 新增額度或 context 的資料來源 | 對應 adapter 發 `AgentEvent::Quota`／`Context`；`README` 的說明；沒有來源的 agent 要在文件寫明「不顯示」 |
| 額度用完的偵測關鍵字（`ade_core::looks_like_limit`） | 補測試，同時確認不會誤判一般錯誤（例如檔名含 `limit`） |

## 文件

| 你改了 | 要同步 |
| --- | --- |
| 使用者看得到的行為 | `README.md`（只放目前實際行為與限制，不寫還沒做的事） |
| 架構、事件流程、設計取捨 | `docs/DESIGN.md`；已知缺口記在 DESIGN 的「未實作項目」附錄 |
| 手動實機驗證了什麼 | `docs/VERIFICATION.md`（記錄實際跑過的 agent 與版本，沒跑過的不要寫成已驗證） |
| 完成 `docs/tickets/` 的票 | 票本身不會自動更新；`docs/tickets/README.md` 的清單要自己標記 |

## 測試與建置

- 提交前跑：`cargo fmt`、`cargo clippy --all-targets -- -D warnings`、`cargo test`（CI 也跑這三項）。
- 單元測試不要依賴本機有沒有安裝某個 agent；需要「啟動失敗」時，把 `ADE_CLAUDE_BIN` 指向不存在的路徑。
- 需要假 agent 時用 `crates/ade-adapter/tests/fake_claude.sh`（git 裡必須保持可執行權限）。
- 測試裡要重新載入 event log 前，先呼叫 `flush_log()`，log 是非同步寫入的。
- 這個專案放在不支援 `chmod` 的磁碟上時，`sed -i` 會印出權限警告但仍然會改檔；用 `git diff` 確認結果。
