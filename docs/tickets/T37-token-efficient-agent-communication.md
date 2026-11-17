# T37 — 跨 Agent 極簡英文通訊協定與禁止客套往返迴圈 (Token-Efficient Inter-Agent Communication Protocol & Zero Pleasantries Policy)

優先度：P2　範圍：`termade-daemon`、`AGENTS.md`、`docs/`　相依：T20, T31

Status: ready-for-agent

## 問題

目前 Agent 之間的協作通訊存在顯著的 Token 浪費與無效喚醒問題：

1. **過度禮貌性往返 (Courtesy Ping-Pong)**：
   - 當 Worker 完成任務回報後，Coordinator 常習慣性回送「收到了，辛苦了！...」，Worker 收到後又回送「收到，感謝確認！隨時待命...」，雙方不斷禮貌道別（「再見，合作愉快」、「祝一切順利」、「謝謝，再會」），形成無窮對話迴圈。
   - 每次 Agent 被喚醒一個 Turn，都會重複注入整個對話逐字稿與系統提示，每回合消耗 1 萬至 5 萬 Tokens，造成巨額且無實質產出的成本浪費。
2. **通訊語言未標準化**：
   - 繁體中文在多數 LLM Tokenizer (如 Claude BPE、Gemini SentencePiece) 下的編碼密度偏低，單個中文字約消耗 2~3 Tokens，而英文單字僅約 1 Token。跨 Session 的內部協作訊息使用中文長句會徒增 Token 消耗。
3. **缺乏終止狀態原則 (Terminal State Policy)**：
   - 協作說明 (`collab_instructions`) 僅規定「完成或卡住時要回報」，未明確規定「回報後即為終止狀態，不得再發送客套感謝或道別訊息」。

## 分析邏輯

1. **精簡英文通訊協定 (Concise English Communication Protocol)**：
   - 跨 Agent 匯流排訊息 (`termade send` / `termade_send`) 一律採用結構化、極簡英文格式：
     - 完成回報：`TASK_DONE <task_id> [<git_commit_or_short_summary>]`
     - 遇到卡點：`TASK_BLOCKED <task_id> <reason>`
     - 提問諮詢：`QUERY: <question>`
     - 回答提問：`ANSWER: <direct answer>`
     - 簽收確認 (選填，非必要不回傳)：`ACK`
2. **零客套終止規則 (Zero Pleasantries & Terminal State Rule)**：
   - 明確規定：嚴禁發送任何寒暄、禮貌性感謝或道別字句 ("Thank you", "You're welcome", "Goodbye", "祝一切順利", "辛苦了", "合作愉快")。
   - Worker 回報 `TASK_DONE` 後直接停止 Turn；Coordinator 確認後直接進行 Git 合併或看板更新，不回覆感謝訊息。若 Coordinator 發送 `TERMINATE`，Worker 必須保持安靜停止。
3. **協作說明注入優化 (`crates/termade-daemon/src/collab.rs`)**：
   - 在 `general_rules` 與 `collab_instructions` 中注入上述通訊協定與禁止客套規範，使所有被 `termade spawn` 的 Agent 自然遵守。

## 實作方式

1. **協作指引更新 (`crates/termade-daemon/src/collab.rs`)**：
   - 更新 `general_rules`、`collab_instructions` 的 Reporting 與 Coordination 說明，加入 Communication Protocol 與 Zero Pleasantries Policy。
   - 更新 `collab.rs` 單元測試。
2. **專案規範更新 (`AGENTS.md`、`docs/DESIGN.md`)**：
   - 在 `AGENTS.md` 新增 Inter-Agent Communication Protocol 專節。
3. **驗證與檢查**：
   - 執行 `cargo test` 與 `make pre-commit` 確保符合所有標準。

## 驗收條件

- `collab.rs` 生成的所有 Session 協作說明均包含英語結構化通訊協定與禁止客套往返規定。
- `AGENTS.md` 包含明確的跨 Agent 英文通訊與零客套規則。
- `make pre-commit` 與 `cargo test` 全數通過。
