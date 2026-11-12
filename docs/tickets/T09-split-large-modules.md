# T09 — 拆分 daemon 與 TUI 的大檔

優先度：P2　範圍：`ade-daemon`、`ade-tui`　相依：T06

## 問題

兩個檔案承擔了過多不相干的職責，改一處要在兩千多行裡找位置，也讓單元測試難以針對單一部分撰寫。

## 證據

| 檔案 | 行數 | 內容 |
| --- | --- | --- |
| `crates/ade-tui/src/lib.rs` | 2394 | 狀態、終端設定、鍵盤、滑鼠選取、命令面板、七個繪製函式、剪貼簿、diff 解析 |
| `crates/ade-daemon/src/lib.rs` | 2064 | 路徑、client 連線、`BusGuard`、請求分派、匯流排投遞、spawn、git 操作、伺服器迴圈 |
| `src/main.rs` | 927 | CLI 定義與所有子命令的實作 |

- `Shared::handle`（`crates/ade-daemon/src/lib.rs:549-1041`）是單一函式約 490 行，一個 `match` 處理 23 種請求。
- `handle_key`（`crates/ade-tui/src/lib.rs:373-618`）約 245 行。

## 分析邏輯

1. 以 `wc -l` 排序，前兩名佔全專案 43%。
2. 列出每個檔案的頂層項目，看能否用一句話描述它的職責；兩者都不行。
3. git 輔助函式（`lib.rs:1414-1621`）不依賴 `Shared`，是最容易先抽出來的一塊，而且抽出後 T02、T03 的測試可以直接對它寫。
4. 這是可維護性問題，不影響行為，所以排在測試（T06）之後，重構時才有東西確認沒改壞。

## 實作方式

純搬移，不改行為。每一步獨立成一個 commit，每個 commit 後 `cargo test` 與 `clippy` 都要通過。

**`ade-daemon`**

| 新模組 | 內容 |
| --- | --- |
| `paths.rs` | `runtime_dir`、`state_dir`、`config_dir`、`socket_path`、`log_path` |
| `client.rs` | `ConnStream`、`Client`、`is_running`、`ensure_daemon` |
| `bus.rs` | `BusGuard`、`Waiter`、`handle_bus_send`、`deliver_message`、`handle_bus_wait`、`append_mailbox` |
| `git.rs` | worktree 建立、狀態、diff、auto-commit、merge、PR、移除 |
| `server.rs` | `handle_conn`、`run_with_options` |
| `handlers/` | `Shared::handle` 只留分派，各請求類別（session、task、worktree、review）各一個檔案 |

`lib.rs` 以 `pub use` 維持現有的公開路徑，`src/main.rs` 與 `ade-tui` 不需要改 import。

**`ade-tui`**

| 新模組 | 內容 |
| --- | --- |
| `app.rs` | `App`、`Mode`、`CenterView`、`MdCache` |
| `input.rs` | `handle_key`、`handle_input_mode`、輸入框編輯函式、`handle_paste` |
| `mouse.rs` | `handle_mouse` 與選取相關函式 |
| `commands.rs` | `execute_command`、`execute_jump`、`submit_spawn`、`submit_prompt_or_route` |
| `draw/` | `header`、`sessions`、`transcript`、`diff`、`worktrees`、`bus`、`input` |
| `clipboard.rs` | `write_clipboard`、`base64_encode`、`copy_main_pane` |
| `diff.rs` | `parse_hunk`、`build_gutter`、`diff_stat`、`jump_hunk` |

`handle_key` 另外依 `CenterView` 拆成小函式。

**`src/main.rs`**：CLI 定義留著，子命令實作移到 `src/cmd/`。

## 驗收條件

- 沒有單一 `.rs` 檔超過約 800 行，沒有單一函式超過約 150 行。
- 公開 API 不變（`ade-tui`、`src/main.rs` 的 import 不需修改，或只因模組重新匯出而調整）。
- 測試數量與結果在重構前後相同。
- diff 中只有搬移與必要的 `use`／可見度調整，沒有邏輯變更。
