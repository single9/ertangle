# T25 — 收件者忙完後提醒未讀訊息；超限判斷看實際收件者

優先度：P2　範圍：`termade-daemon`(`bus.rs`、`lib.rs`)

## 問題

1. **忙碌時收到的訊息會被遺忘。** 直接訊息送達時，收件者若是 `Busy`，訊息只留在收件匣，不喚醒；之後它回到 `Idle`，也沒有任何機制提醒它去讀。除非 agent 自己主動呼叫 `termade_inbox`，訊息就這樣沉下去。
2. **超限提示看錯 session。** T24 之後，送給已交接 session 的訊息會改投給接手者，但 `handle_bus_send` 判斷「對方超過成本上限、不會被喚醒」時仍看原收件者。

## 證據

- 2026-11-13 T24 實機驗證：`termade send --to hx-a` 改投給 `hx-a→opencode` 時它正忙著處理交接簡報；回到 `idle` 後沒有回應那則訊息 (見 `docs/VERIFICATION.md` 的 T24 段)。
- `crates/termade-daemon/src/bus.rs:156` 起的註解與 `deliver_message`：只喚醒 `Idle`／`Sleeping` 的收件者，`Busy` 的「留在收件匣即可」。
- `crates/termade-daemon/src/lib.rs:603` 起：回到 `Idle` 時只呼叫 `flush_queue_if_idle`(T19 的 prompt 佇列)，不看收件匣。
- `crates/termade-daemon/src/bus.rs:122-135`：`over_budget_target` 依解析前的 `to` 判斷。

## 分析邏輯

1. 喚醒用的 nudge (`deliver_message` 裡組的「[termade 匯流排訊息 …]」prompt)已經存在，只是只在送達當下發一次。回到 `Idle` 時補發即可，不需要新協定。
2. 和 T19 佇列的優先順序：使用者排的 prompt 是明確指示，應該先送；佇列空了、回到 `Idle` 時才補發未讀訊息提醒。
3. 要避免來回打擾：一則訊息只補提醒一次；同一次 `Idle` 有多則未讀時合併成一則提醒。已透過 `termade_wait`／`termade_ask` 取走的、或 agent 已用 `termade_inbox` 讀過的 (`inbox_cursors` 之後沒有新訊息)不提醒。
4. 只處理會喚醒收件者的種類 (`kind_wakes_recipient`)，`Status`／`Artifact` 不提醒，與送達時的規則一致。超過成本上限 (`over_budget`)的 session 不提醒。
5. **沒有確認** `inbox_cursors` 在 agent 用 `termade_inbox` 讀過之後是否一定前進；動手前先查。

## 實作方式

1. `Inner` 記錄「送達時因為忙碌而沒有喚醒」的訊息：`pending_nudge: HashMap<SessionId, Vec<MsgId>>`，在 `deliver_message` 跳過 `Busy` 收件者時加入。
2. `ingest` 的回到 `Idle` 分支：先 `flush_queue_if_idle`；若沒有送出任何 prompt、session 不是 `over_budget`，取出該 session 的 `pending_nudge`，排除已被 `inbox_cursors` 涵蓋的，剩下的合併成一則提醒 prompt 送出，清空記錄。
3. session 結束或被移除時清掉它的 `pending_nudge`；交接時 (T24)把舊 session 的記錄併給接手者。
4. `handle_bus_send` 的 `over_budget_target` 改看 `successor_of` 之後的實際收件者。
5. 更新 `bus.rs` 的喚醒規則註解與 README 匯流排段落。

## 驗收條件

- 測試：收件者 `Busy` 時送一則直接訊息，不立刻送 prompt；回到 `Idle` 後送出一則含訊息 id 的提醒。
- 測試：`Busy` 期間收到兩則，回到 `Idle` 只送一則提醒，內含兩個 id。
- 測試：佇列 (T19)有 prompt 時，回到 `Idle` 先送佇列的；下一次 `Idle` 才補提醒。
- 測試：訊息已被 `termade_wait` 取走、或 inbox 已讀過時不提醒。
- 測試：`Status` 類訊息不提醒；`over_budget` 的 session 不提醒。
- 測試：送給已交接 session、接手者 `over_budget` 時，回覆註明不會喚醒。
