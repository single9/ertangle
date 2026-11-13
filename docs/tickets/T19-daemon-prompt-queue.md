# T19 — 把 prompt 排隊移到 daemon

優先度：P2　範圍：`termade-core`、`termade-protocol`、`termade-daemon`、`termade-tui`、`src/cmd/session.rs`

## 問題

排隊的 prompt（`Alt-Enter`、`:queue`）只存在 TUI 的記憶體裡，由 TUI 的事件迴圈在 session 閒下來時送出。所以：

- 關掉 TUI，排隊的內容就消失，agent 做完手上的事就停住。
- 開兩個 TUI 時各有各的佇列，互相看不到。
- CLI 與其他 agent 沒辦法排隊。

這和專案的核心設計相衝突：daemon 持有狀態，TUI 是可以隨時關掉的顯示器。排好一串工作後關掉 TUI 離開，正是這個設計該支援的用法。

## 證據

- `crates/termade-tui/src/history.rs:107-183`：`Queued` 與 `PromptQueues` 定義在 TUI。
- `crates/termade-tui/src/commands.rs:78-147`：`queue_prompt` 與 `flush_queues`。
- `crates/termade-tui/src/lib.rs:188`：`flush_queues` 每輪事件迴圈呼叫一次。
- `README.md` 按鍵表：「排隊只存在 TUI 內，關掉 TUI 就沒了」。
- `crates/termade-protocol/src/lib.rs:97-104`：`Request::Prompt` 沒有排隊的選項。

## 分析邏輯

1. TUI 的做法需要一個 `inflight` 計時（`history.rs:119`）來處理「剛送出、還沒看到 session 變忙碌」的空窗。daemon 是送出 prompt 的那一方，可以在同一把鎖內決定順序，不需要這個計時。
2. 觸發時機：session 狀態變成 `Idle` 的那個事件。送出一則之後，要等這個回合結束才會再有下一個 `Idle` 事件，所以自然是一個回合送一則。
3. 佇列要不要撐過 daemon 重啟：目前重啟後 agent 處理程序本來就不在了（T14 完成前也接不回來），所以先只放記憶體。做完 T14 後再評估要不要從 log 還原。
4. 沒有確認匯流排的自動喚醒（`deliver_message`）是不是也用同一條送輸入的路徑；如果是，兩者可能在同一個 `Idle` 時點搶著送。動手時要讀 `bus.rs:141` 之後的實作。

## 實作方式

1. **協定**
   - `Request::Prompt` 加 `#[serde(default)] queue: bool`。為真且 session 不是閒置時排隊；閒置且佇列為空時直接送出（和現在 TUI 的 `queue_prompt` 相同）。
   - 新增 `Request::QueueClear { session }` 與 `Request::QueuePop { session }`（取消最後一則）。
2. **狀態**
   - daemon 的 `Inner` 加 `queues: HashMap<SessionId, VecDeque<QueuedPrompt>>`，`QueuedPrompt` 含送給 agent 的文字、附件、以及使用者原本輸入的文字（含 `@檔名`，取消時放回輸入框用）。
   - 讓 client 看得到：新增 `Event::QueueChanged { session, items: Vec<String> }`（`items` 是原始輸入文字），`State` 加 `#[serde(default)] queues: BTreeMap<SessionId, Vec<String>>`。附件內容不進事件與 log。
   - 上限：每個 session 最多 20 則，超過回錯誤。附件已有大小上限（圖片 5 MB）。
3. **送出**
   - 把 `handle_prompt`（`handlers/session.rs:228-263`）裡「喚醒 → 送 `UserPrompt` 事件 → `send_input_with`」抽成 `send_prompt(shared, sid, text, attachments) -> Result<()>`。
   - `ingest` 送出 `SessionStatus::Idle` 事件之後，若該 session 有排隊，取出最前面一則呼叫 `send_prompt`，再送 `QueueChanged`。送失敗就把那一則放回去，並留一則 `SessionNote`。
   - session 變成 `Exited`／`Failed` 或被移除：清空它的佇列並送 `QueueChanged`，留備註說明丟掉幾則。
   - 被 T17 的預算擋住的 session：不送出，佇列保留。
4. **TUI**
   - 移除 `PromptQueues`、`flush_queues` 與事件迴圈裡的呼叫。
   - `Alt-Enter`／`:queue <文字>`：送 `Request::Prompt { queue: true, … }`。
   - `:queue`（列出）：讀 `app.state.queues`。
   - `:queue clear` → `QueueClear`；`:unqueue` → 先從 `app.state.queues` 取最後一則放回輸入框，再送 `QueuePop`。
   - 輸入框標題的排隊數（`draw.rs:1162`）改讀 `app.state.queues`。
5. **CLI**：`termade prompt --queue <session> "…"`，以及 `termade queue <session> [--clear]` 列出或清空。
6. **MCP**：先不開放。agent 之間已經有訊息匯流排。
7. **文件**：README 的排隊說明改掉「只存在 TUI 內」，寫明 daemon 重啟後不保留。

## 驗收條件

- 測試：對忙碌中的 session 排兩則，session 每次回到 `Idle` 送出一則，順序正確。
- 測試：對閒置且佇列為空的 session 用 `queue: true`，立即送出。
- 測試：沒有任何 client 連線時，排隊的 prompt 仍會送出。
- 測試：session 失敗後佇列被清空，`State.queues` 沒有它。
- 測試：`QueuePop` 移除最後一則；`QueueClear` 清空；超過 20 則回錯誤。
- 測試：`QueueChanged` 寫入 `State`；舊的 snapshot（沒有 `queues` 欄位）仍可解析。
- 測試：log 裡沒有附件的內容。
- TUI 測試：`:unqueue` 後輸入框的內容是使用者原本輸入的文字。
- 既有與排隊相關的 TUI 測試改寫或移除。
- 依 `docs/MAINTAINING.md` 同步 `handlers/mod.rs`、`State::apply`、`help.rs`、README。
