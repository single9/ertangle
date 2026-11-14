# T24 — 交接後，發給舊 session 的訊息轉給接手的 session

優先度：P2　範圍：`termade-core`、`termade-daemon`(`bus.rs`、`handlers/session.rs`、`mcp.rs`)

## 問題

`termade handoff` 會開一個新 session 接手舊 session 的工作，但匯流排上的身分沒有跟著移交：

- 新 session 的 label 是 `<舊 label>→<agent>` (例如 `t17-budget→claude`)，其他 agent 仍照原本知道的 `t17-budget` 傳訊息。
- `resolve_session_id` 用 label 找 session 時，同名的只有舊的那個，所以訊息送到已結束的舊 session，沒人讀，也不會喚醒任何人。
- 傳送端拿到「已送出」，不知道對方其實收不到。

## 證據

2026-11-13 實際遇到：T19 的 worker (`ses_23a74e73f03d`，`t19-queue→claude`)把「超限時佇列怎麼處理」的提案送給 `t17-budget`。那時 T17 已交接給 `t17-budget→claude`(`ses_7bf5edbeb827`)，訊息落在已結束的 `ses_bf77f87272c8`，沒有回覆；兩者的交界最後由協調者在合併時自己決定。這次的 event log 裡 worker 之間共 6 則訊息，至少這一則白送。

- `crates/termade-daemon/src/handlers/session.rs:687` 起的 `handle_handoff`：新 session 的 label 用 `format!("{}→{}", src.label, kind.id())`，並以 `inherit_from` 沿用 cwd／分支。
- `crates/termade-daemon/src/handlers/session.rs:38-45`：`inherit_from` 只用來複製 cwd、branch、repo_root，沒有記在新的 `Session` 上。
- `crates/termade-core/src/lib.rs:1144-1160`：`resolve_session_id` 只比 id 與完全相同的 label；優先找 live 的同名 session，找不到就退回最新的同名 session (即使已結束)。
- `crates/termade-daemon/src/bus.rs:157` 起的 `deliver_message`：`Direct` 目標解析後直接投遞，不檢查收件者是否還活著，也不會改投他人。
- `crates/termade-core/src/lib.rs`：`Session` 沒有「被誰接手」或「接手自誰」的欄位。

## 分析邏輯

1. 訊息送錯的根源是「交接」在資料上沒有留下關聯，只有 label 字串看得出來，`resolve_session_id` 也不會解析 `→`。
2. 改 label 讓新 session 沿用舊 label 也能解決，但會讓清單上兩個同名 session 難以分辨，`rm`、`resume` 等以 label 操作的命令也會變得含糊。用明確的關聯欄位比較清楚。
3. 交接可以連續發生 (A → B → C)，轉送要沿著鏈走到最後一個；要防環。
4. 已經投遞到舊 session、但新 session 還沒看到的訊息也要處理：交接當下舊 session 收件匣裡未讀的訊息，新 session 應該看得到。**沒有確認**交接簡報 (`handoff.rs`)目前有沒有把未讀訊息帶進去。
5. 沒有確認 mailbox 檔案 (`append_mailbox` 寫進 worktree 的那份)在交接後的行為；新舊 session 共用 worktree，可能已經看得到同一份。

## 實作方式

1. `Session` 新增 `#[serde(default)] handed_off_to: Option<SessionId>`。`handle_handoff` 在新 session 啟動成功後 emit 新事件 `Event::SessionHandedOff { from, to }`，`State::apply` 設定舊 session 的欄位。舊 JSON 沒有這個欄位仍可解析。
2. `State` 新增 `fn successor_of(&self, id) -> SessionId`：沿 `handed_off_to` 走到最後一個；遇到環或超過 16 層就停在目前這個。
3. `deliver_message`：`Direct` 目標解析成 id 後，若該 session 已結束且有 `handed_off_to`，改投給 `successor_of` 的結果。訊息上記錄原本的收件者 (例如在 body 開頭加「(原收件者 t17-budget，已交接)」，或新增欄位，擇一，動手時決定)，回覆給傳送端的確認文字註明改投。
4. 收件者已結束、又沒有接手者時：仍然投遞 (保留現狀，留作紀錄)，但回覆給傳送端的確認文字要說明「對方已結束，訊息不會被讀取」。MCP `termade_send` 的回傳也要帶這段，讓 agent 知道。
5. 交接簡報：把舊 session 收件匣裡未讀的訊息附進去 (若第 4 點的查證發現已經有，就只補測試)。
6. 廣播與頻道訊息不改：它們的收件者本來就是動態決定的，已結束的 session 不會被喚醒。

## 驗收條件

- 測試：A 交接給 B 後，`send --to <A 的 label>` 的訊息由 B 收到並喚醒 B；A 的收件匣沒有新訊息。
- 測試：A → B → C 連續交接，送給 A 的訊息由 C 收到。
- 測試：`handed_off_to` 形成環時不會無限迴圈。
- 測試：送給已結束、沒有接手者的 session，訊息仍寫入 log，回覆含「不會被讀取」。
- 測試：舊的 `Session` JSON (沒有 `handed_off_to`)可解析。
- 測試：交接當下舊 session 未讀的訊息出現在新 session 的交接簡報。
- 同步 README 的交接段落與 MCP 工具說明。
