# T47 — 降低 Agent 的 token 用量 (長對話提醒、省 token 提示、注入內容瘦身)

優先度：P2　範圍：`termade-daemon`、`termade-tui`、`termade-core`　相依：T38, T42

Status: in-progress

## 問題

原本的需求是「內建快速工具取代 Agent 自己寫的 Python 腳本」。分析後發現 Python 腳本既不是等待時間的主因，也不是 token 的主要來源，因此方向改為降低整體 token 用量。

## 證據

資料來源與限制：

- Gemini (antigravity)：`~/.gemini/antigravity-cli/brain/*/.system_generated/logs/transcript.jsonl`，169 個對話、25686 個回合。
- Claude：`~/.claude/projects/**/*.jsonl`，19 個對話檔 (17 個有用量資料)、995 則助理訊息 (依 `message.id` 去重)，涵蓋 2026-09-17 到 2026-11-18，樣本小。
- 兩邊都以 Petition 專案為主；termade 自身專案樣本少。
- 指令耗時可能含等待核准的時間 (見下方「耗時」)，所以不用於結論。

### token 結構

| 項目 | Gemini | Claude |
|---|---|---|
| 快取讀取 | 9.49 億 | 1.25 億 |
| 未快取輸入 | 1.17 億 | 約 0.2 萬 (另有快取建立 237 萬) |
| 輸出 | 730 萬 | 65 萬 |
| 平均上下文 | 約 4 萬 | 約 12.8 萬 |
| 對話第一則訊息的上下文 (中位數) | 未量測 | 40625 (範圍 23776 到 56036) |

### 成本集中在長對話

- Gemini：最長的 17 個對話 (10%) 占 74% 的快取讀取；回合數中位數 51、p90 468、最長 1504。
- Claude：17 個對話中最長的 3 個占 83%；最長一個 483 回合。

### 一個回合只做一件事

- Gemini：22620 個含工具呼叫的回合中，單一呼叫占約 96%。
- Claude：774 則含工具呼叫的訊息中，單一呼叫占 726 則 (94%)。
- Gemini 的小型探查指令 (`git status`、`ls`、`git log`、`head`、`echo` 等) 共 2294 次，約占回合數的 9%。

### 臨時 Python 腳本影響很小

- Gemini：1246 次 Python 呼叫的輸出共約 54 萬 token，占輸出的 7%。
- Claude：含 Python 的訊息輸出約 9.1 萬 token，占輸出的 14%；Bash 446 次中 154 次含 Python，但多數是執行專案本身的程式 (`test_*.py`、`backtest.py`、`pip install`)，臨時的 `json.load`、`openpyxl` 只有約 24 次。
- 其餘細節：Gemini 重複讀取同一檔案同一範圍只有 332 次 (約 6.5%)。

### 大型工具結果

- Claude：848 筆工具結果中，15 筆超過 1 萬字元，占總字元的 30%；這些結果在後續每個回合都會被重讀。
- Gemini 的結果在紀錄中被截在約 4120 字元，無法比較。

### termade 自己注入的量 (實測 `termade mcp`)

- `initialize` 的 `instructions`：1201 字元。
- `tools/list`：15 個工具、共 7724 字元；最大的是 `termade_spawn` (1717)、`termade_send` (913)、`termade_task_create` (858)。
- 粗估約 2000 到 2500 token，約為 Claude 基底上下文 (中位數 4 萬) 的 5%。若 `inject_skill` 開啟會再增加。

### 耗時 (供參考，不作為依據)

Gemini 指令耗時扣除 ≥60 秒的離群值後：ssh 20%、git 20%、python 9%，平均都在 1 到 2 秒。51 筆 (0.6%) 離群值占總耗時 54%，推測多為等待核准、休眠，核准事件沒有直接記錄。

## 分析邏輯

1. 成本主體是「回合數 × 每回合重讀的上下文」，與單一回合做什麼無關。
2. 因此兩個槓桿：(a) 少開長對話或在長對話中途交接；(b) 減少回合數。
3. 工具化取代臨時 Python 的收益太小，不做。
4. termade 注入量約占基底的 5%，可小幅瘦身，但不是主因。
5. 預期效果沒有基線可驗證。需先量測每個 session 的回合數與 token。

## 實作方式

分三步，每步獨立可交付。

1. **量測與顯示** (已完成)：context 用量原本就有 (`Session.context`，TUI 顯示 `ctx` 並在 80%、92% 變色)；新增 `Session.tool_calls` (只累計工具呼叫的開始事件，作為回合數近似值，因為約 95% 的回合只有單一工具呼叫)，顯示在 TUI session 次要資訊列的 `N calls`。
2. **長對話提醒**：新增設定 `[budget] long_session_turns` 與 `long_session_context` (預設關閉)，超過時在 TUI 顯示提示，建議 `:handoff` 或開新 session；沿用 T15 的通知機制與 T17 的預算設定。
3. **省 token 提示**：新增 `[collab] token_hints` (預設關閉)，開啟時在協作說明附加數條：獨立的工具呼叫放在同一回合、唯讀探查合併成單一 shell 指令、輸出以 `head`、`--stat` 等限制長度、不重複讀取已讀過的檔案。沿用 `inject_skill` 的模式 (`config.rs`、`collab.rs`)，TUI `:settings` 增加開關 (同 T42)。
4. **注入內容瘦身 (可選)**：縮短 `termade_spawn` 等工具的描述；以 step 1 的數據確認有效再做。

## 待確認

- 是否接受先做步驟 1，等有基線再決定 2 與 3 的門檻與提示內容。
- 省 token 提示對 Claude 的效果未知 (Claude 本身已偶爾平行呼叫)，需以 A/B 比較兩種設定下的回合數。

## 驗收條件

- 步驟 1：TUI 顯示每個 session 的回合數與上下文大小；單元測試涵蓋累計邏輯。
- 步驟 2：超過門檻時只提醒一次，關閉時無任何提示。
- 步驟 3：設定關閉時，協作說明不含提示；開啟時新建立的 session 才會包含。
- 新增使用者可見字串都有英文翻譯；`make pre-commit` 通過。
