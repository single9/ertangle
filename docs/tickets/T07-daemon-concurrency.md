# T07 — daemon 併發：鎖內寫檔、無上限佇列、鎖中毒

優先度：P2　範圍：`termade-daemon`　相依：T06

## 問題

1. **鎖內做檔案 IO**：`emit` 持有全域 `inner` 鎖的期間做 JSON 序列化、秘密遮罩與寫檔。所有 session 的串流輸出都經過這裡，磁碟變慢時整個 daemon 一起卡住。
2. **client 佇列無上限**：每個連線是無上限的 `mpsc::channel`。client 停止讀取（終端機被暫停、網路卡住）時，`emit` 仍會成功送入，記憶體隨事件量無限成長。
3. **鎖中毒連鎖**：daemon 內 34 處 `unwrap()`／`expect()`，多數是 `lock().unwrap()`。任何一個持鎖的執行緒 panic，之後所有執行緒都會在 `lock().unwrap()` 上 panic，daemon 等同全毀，而它持有所有 agent 子處理程序。

## 證據

- `crates/termade-daemon/src/lib.rs:400-426`：`emit` 從頭到尾持有 `inner`，`410-415` 在鎖內寫 log。
- `crates/termade-daemon/src/lib.rs:1630`：`mpsc::channel::<Frame>()`；`425`：`inner.clients.retain(|c| c.send(...).is_ok())`。
- `grep -c 'unwrap()\|expect('`：`termade-daemon/src/lib.rs` 34 處。

## 分析邏輯

1. 找出最熱的路徑：每個 agent 的每段輸出都會呼叫 `ingest` → `emit`，所以 `emit` 的臨界區長度決定整個 daemon 的吞吐量。
2. 看臨界區內有什麼：狀態更新（必要）、序列化與寫檔（不必要在鎖內）、推送給 client（`send` 對無上限 channel 不會阻塞，所以不卡，但也因此沒有背壓）。
3. 無上限 channel 的寫入端只有在接收端被 drop 時才會失敗；連線的寫入執行緒卡在 `write_all` 時接收端還活著，所以佇列會一直長。
4. 這些都沒有實測到實際的卡頓或記憶體成長；在單人、少量 session 的使用下可能長期不會遇到。所以排在 P2。

## 實作方式

1. **log 寫入移出鎖**：新增一個 log 寫入執行緒與一個 channel。`emit` 在鎖內只把 `Record`（或已序列化的字串）送進 channel，順序由持鎖時送出保證。寫入執行緒負責遮罩與寫檔。`shutdown` 時關閉 channel 並等待寫入執行緒結束，確保最後的事件落地。
2. **client 佇列加上限**：改用 `mpsc::sync_channel(N)`（例如 4096）加 `try_send`。佇列滿時把該 client 移除並關閉連線；client 重連後以 `since` 補齊（backlog 有 20000 筆）。TUI 端確認斷線會自動重連。
3. **鎖中毒**：加一個小輔助函式統一取鎖，中毒時取回內部資料繼續（`unwrap_or_else(PoisonError::into_inner)`），並記一筆 `tracing::error!`。不引入 `parking_lot`，維持相依性精簡。
4. **連線執行緒不要帶走 daemon**：`handle_conn` 內對 `shared.handle` 包 `catch_unwind`，單一請求 panic 只斷該連線。

## 驗收條件

- 測試：一個不讀取的 client 連著時送出超過佇列上限的事件，該 client 被移除，其他 client 正常收到全部事件。
- 測試：在持有 `inner` 的執行緒中 panic 後，`snapshot()` 仍可正常呼叫。
- 測試：`shutdown` 後 log 檔含最後一筆事件。
- `emit` 內不再有檔案 IO。
