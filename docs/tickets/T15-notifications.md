# T15 — 回合結束、等待審批、額度用完時通知使用者

優先度：P1　範圍：`termade-tui`、`termade-daemon`（只有 `config.rs`）

## 問題

平行跑多個 agent 時，使用者不會一直看著 TUI。目前 agent 做完、卡在等審批、或額度用完，都只有畫面上的狀態符號會變，人不在這個終端分頁前就不會知道。等審批的情況最傷：agent 停在那裡，逾時（Claude 是 300 秒）就被當成拒絕。

## 證據

- `crates/termade-tui/src/lib.rs:232-268`：`apply_frame` 收到 `Frame::Record` 只做 `app.state.apply(&record)`，沒有比較前後狀態，也沒有任何輸出。
- 在 `crates/` 搜尋 `notify-send`、bell、`\x07`：只有 OSC 序列的結尾字元（`clipboard.rs:10`、`ansi.rs:88`），沒有通知用途。
- `crates/termade-daemon/src/config.rs:45-49`：`TuiCfg` 只有 `layout`。

## 分析邏輯

1. 通知該由誰發：daemon 可能跑在遠端或沒有桌面環境，TUI 才是在使用者面前的那一端，所以放在 TUI。代價是 TUI 沒開就沒有通知；這可接受，之後要補可以另外做 `termade watch`。
2. 終端機寫入跳脫序列已有先例：OSC 52 複製（`clipboard.rs:10`）就是在兩次繪製之間直接寫 stdout。
3. 連線時 daemon 會補送 `since` 之後的事件，這些是歷史，不能觸發通知。
4. 沒有做原型；各終端對 OSC 9／OSC 777 的支援程度沒有實測。

## 實作方式

1. **偵測**：新增純函式（建議放新檔 `crates/termade-tui/src/notify.rs`）
   ```rust
   fn notices_for(before: &State, record: &Record, ctx: &NotifyCtx) -> Vec<Notice>
   ```
   在 `apply_frame` 的 `Frame::Record` 分支，套用事件之前呼叫。要通知的情況：
   - `SessionStatus`：從忙碌變成 `Idle`（回合結束）。忙碌的判定以 `SessionStatus` 的實際變體為準，休眠／喚醒造成的變化不算。
   - `SessionStatus`：變成 `Failed`。
   - `PermissionRequested`（daemon 已自動決定的不會以這個事件出現，所以不必再過濾）。
   - `SessionLimited`。
2. **過濾**（都放在 `NotifyCtx`，方便測試）
   - 歷史事件：`record.ts` 比現在早超過 5 秒就略過。
   - 太短的回合：回合開始時間記在 `App`（`HashMap<SessionId, Instant>`，狀態變忙碌時寫入），不到 `min_turn_secs`（預設 10）的「做完了」不通知。審批與失敗不受這個限制。
   - 焦點：啟用 crossterm 的 focus 事件（`EnableFocusChange`，處理 `FocusGained`／`FocusLost`）。終端有焦點、且事件屬於目前選取的 session 時不通知。終端不回報焦點時當作沒有焦點。
   - 專案範圍：`scope_here` 開啟時，其他專案的 session 不通知。
   - 去抖：同一個 session 同一類通知 3 秒內只發一次。
3. **送出**：依設定逐一執行
   - `bell`：寫 `\x07`。
   - `osc9`：寫 `\x1b]9;<訊息>\x07`。
   - `command`：以 `sh -c` 執行使用者設定的命令，資訊用環境變數帶入（`TERMADE_NOTIFY_KIND`、`TERMADE_NOTIFY_SESSION`、`TERMADE_NOTIFY_LABEL`、`TERMADE_NOTIFY_TEXT`），不要把內容拼進命令字串。用 `spawn` 不等待，失敗只記到狀態列。
   - 訊息內容：`<label>：做完了`／`<label>：等待審批 <工具名>`／`<label>：額度用完`。寫進 OSC 前要去掉控制字元。
4. **設定**（`config.toml`）
   ```toml
   [tui]
   notify = ["bell"]                 # bell | osc9 | command；空陣列 = 關閉
   notify_command = "notify-send termade \"$TERMADE_NOTIFY_TEXT\""
   notify_min_turn_secs = 10
   ```
   預設 `["bell"]`。`TuiCfg` 加欄位後要補 `Config::layered` 的逐欄覆寫。
5. **執行期切換**：`:notify on|off`，狀態列顯示目前狀態。
6. **不在這張票**：TUI 沒開時的通知、手機推播。

## 驗收條件

- 測試 `notices_for`：忙碌 → `Idle` 產生一則；`Idle` → `Idle` 不產生；`PermissionRequested` 產生一則；`ts` 過舊的不產生。
- 測試：回合短於門檻不通知；審批不受門檻影響。
- 測試：有焦點且是選取中的 session 不通知；失去焦點後會通知。
- 測試：同一個 session 3 秒內的重複通知只有一則。
- 測試：設定解析與 repo 覆寫全域。
- 測試：訊息含控制字元時，輸出的 OSC 序列裡沒有它們。
- 手動驗證：在 tmux 內與直接在終端各試一次 bell 與 `notify_command`，結果記進 `docs/VERIFICATION.md`。
- 同步 `help.rs`、`COMMANDS` 表、README。
