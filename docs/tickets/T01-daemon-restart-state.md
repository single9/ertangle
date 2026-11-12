# T01 — daemon 重啟後還原狀態並維持 seq 連續

優先度：P0　範圍：`ade-daemon`、`ade-protocol`、`ade-tui`

## 問題

daemon 每次啟動都從空狀態、`seq = 0` 開始，卻把事件接著寫進同一個 `events.jsonl`。重啟後：

- 先前的 session、任務、匯流排訊息全部從 daemon 的狀態消失。
- log 內出現重複的 seq，`ade replay --since <seq>` 的語意不再明確。
- 還連著舊 `since` 的 client 可能拿到錯的增量。

## 證據

- `crates/ade-daemon/src/lib.rs:1698-1701`：log 以 `append(true)` 開啟。
- `crates/ade-daemon/src/lib.rs:1708-1711`：`state: State::new()`、`seq: 0`、`backlog: VecDeque::new()`。
- `crates/ade-daemon/src/lib.rs:106`：`replay_log` 存在，但唯一呼叫者是 `src/main.rs:856`（`ade replay`）。
- `crates/ade-daemon/src/lib.rs:571-577`：增量同步的條件是 `since + 1 >= backlog.front().seq || since == seq`。

## 分析邏輯

設計文件 2.1 節與 6 節的承諾是「事件流是 append-only log，可重播、可審計」以及「TUI 重連帶 `since`，畫面無縫重建」。我檢查這兩件事在 daemon 自己重啟時是否仍成立：

1. 找 log 的開檔方式與 seq 的初值，發現一個是續寫、一個歸零，兩者不一致。
2. 找 `replay_log` 的呼叫點，確認啟動流程沒有用它。
3. 推演增量同步：舊 daemon 的 client 記得 `since = 500`。新 daemon 跑到 `seq = 600` 且 backlog 從 1 開始時，`500 + 1 >= 1` 成立，於是只補送新 daemon 的 501–600，client 把它們疊在舊 daemon 的狀態上。這條是推論，沒有實機重現。

## 實作方式

1. **啟動時重播**：`run_with_options` 先呼叫 `replay_log(log_path())`，用結果初始化 `state`，`seq` 設為最後一筆的 seq，並把尾端最多 `BACKLOG_CAP` 筆放進 `backlog`。
2. **收斂孤兒 session**：重播後仍是 live 狀態的 session，其子程序已不受這個 daemon 管理。啟動時對每一個發出 `SessionExited`（或新增一個表示「daemon 重啟而遺失」的狀態），這樣狀態與 log 都一致。
3. **daemon 世代識別**：`Frame::Ready` 增加 `epoch`（啟動時產生的隨機 id）。client 記住 epoch，重連時發現不同就丟掉本地狀態、不帶 `since`。做了第 1 步後 seq 已連續，這一步是防 log 被刪除或換機器的情況。
4. **`replay_log` 不再靜默略過壞行**：計數並以 `tracing::warn!` 回報略過了幾行。
5. **收件匣游標**：`inbox_cursors` 目前只在記憶體。要嘛跟著事件落地（新增 `InboxAdvanced` 事件），要嘛明確記錄「重啟後未讀狀態重置」是已知行為。建議前者。

## 驗收條件

- 測試：寫入 N 筆事件 → 以同一個 log 建立新的 `Shared` → 新事件的 seq 為 N+1，`state` 含先前的 session 與任務。
- 測試：重播後原本 live 的 session 狀態為已結束。
- 測試：log 內 seq 嚴格遞增，跨一次重啟仍成立。
- 測試：client 帶舊 epoch 的 `since` 重連時拿到完整 snapshot。

## 備註

log 很大時啟動重播會變慢，這由 T08（輪替時寫入 snapshot）處理。
