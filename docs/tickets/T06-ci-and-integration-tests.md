# T06 — CI 與 adapter 整合測試

優先度：P1　範圍：全 workspace、新增 `.github/workflows/`

## 問題

測試集中在容易測的地方，風險高的路徑沒有覆蓋，也沒有任何自動化檢查在把關。

## 證據

`cargo test` 於 `9575aa7` 的結果，共 58 個：

| 位置 | 測試數 | 備註 |
| --- | --- | --- |
| `termade-tui` | 31 | 其中 `markdown.rs` 16 |
| `termade-adapter` | 12 | `ansi` 4、`stream` 4、`claude` 2、`codex` 1、`lib` 1；`acp.rs` 與 `pty.rs` 為 0 |
| `termade-daemon` | 7 | 喚醒邏輯與 `BusGuard`；git 函式、`handle_conn`、認證為 0 |
| `termade-core` | 5 | |
| `termade-protocol` | 3 | |
| `src/main.rs` | 0 | 927 行 |

- repo 沒有 `.github/` 目錄，也沒有其他 CI 設定。
- `docs/DESIGN.md` 的驗證表都是「實機」手動驗證。

## 分析邏輯

1. 以 `grep '#\[test\]'` 逐檔計數，對照各檔行數與它負責的事情。
2. 把「出錯代價」與「測試數」放在一起看：git 操作（會動使用者 repo）、認證（安全）、ACP 協定處理（四家中兩家相依於它）都是零。
3. 設計文件的驗證靠手動實機，這表示每次改動後沒有辦法便宜地重跑；T07 與 T09 的重構需要這個安全網。
4. 我沒有量測行覆蓋率，以上是用測試分布做的粗估。

## 實作方式

1. **CI**（GitHub Actions，`ubuntu-latest` 與 `macos-latest`）：
   - `cargo fmt --check`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test`
   - 快取 `~/.cargo` 與 `target`。
2. **假 agent 執行檔**：在 `crates/termade-adapter/tests/` 放一個測試用的小 binary（或以 `[[bin]]` 加 `required-features = ["test-agent"]`），依參數扮演：
   - ACP agent：回應 `initialize`、`session/new`、`session/prompt`，串流幾段輸出，發一次 `session/request_permission`。
   - Claude stream-json：吐出固定的 JSON 行。
   - Codex app-server：回應握手與一個回合。

   adapter 以可設定的執行檔路徑啟動它（若目前寫死 `opencode`／`claude`，加一個測試用的覆寫，例如環境變數）。
3. **adapter 整合測試**：對每個 adapter 驗證 prompt 往返、輸出事件順序、核准往返、`cancel`、子處理程序結束時發出 `Exited`。
4. **daemon 端到端測試**：以暫存目錄設定 `TERMADE_SOCKET` 與 `XDG_STATE_HOME`，在測試執行緒內啟動 daemon，用 `Client` 走 spawn（假 agent）→ prompt → 收到輸出 → shutdown。
5. **git 函式測試**：隨 T02、T03 一起寫，不在這張票。

## 驗收條件

- push 與 PR 會觸發 CI，三項檢查都跑。
- `acp.rs` 至少涵蓋：正常回合、核准轉發與回覆、agent 中途結束。
- 有一個 daemon 端到端測試在 CI 上穩定通過（不需要任何真實 agent 或網路）。
- 測試全部在 30 秒內跑完。
