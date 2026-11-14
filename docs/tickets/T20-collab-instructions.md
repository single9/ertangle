# T20 — 把協作說明注入每個 agent

優先度：P2　範圍：`termade-daemon`、`termade-adapter`

## 問題

每個 agent 啟動時會被注入 `termade mcp`，拿到 14 個協作工具。但沒有任何地方告訴它**該怎麼用**：它是誰、是誰產生它的、做完要向誰回報、worktree 裡的成果要不要自己合併、什麼時候該更新任務狀態。agent 只能從各個工具的描述去猜，結果是有的做完就停住不回報，有的不更新任務。

目前唯一的行為指引，是指派任務時訊息裡寫死的那一句。

## 證據

- `crates/termade-daemon/src/mcp.rs:62-76`：`initialize` 的回應只有 `protocolVersion`、`capabilities`、`serverInfo`，沒有 `instructions`。
- `crates/termade-adapter/src/claude.rs:395-420`：組 Claude 命令列的地方沒有系統提示相關的參數。
- `crates/termade-daemon/src/handlers/task.rs:22-26`：指派訊息裡的「請先用 termade_task_update 把狀態改為 doing…」。
- `docs/DESIGN.md` 附錄 A：「`tasks.json`、`AGENTS.md` 注入：未實作」。

## 分析邏輯

1. 原設計是把說明寫成 worktree 裡的 `AGENTS.md`。不建議照做：合併前的自動 commit 會把它一起提交進使用者的分支，而且會蓋掉或干擾 repo 自己的 `AGENTS.md`。
2. 可用的注入管道，由省事到費事：
   - **MCP `initialize` 的 `instructions`**：MCP 規格定義的欄位，一處修改四家都吃得到。各家 client 會不會把它放進模型的 context，**沒有確認**；要逐家實測。
   - **Claude 的 `--append-system-prompt`**：`claude --help` 已確認有這個旗標。
   - **Codex**：`thread/start` 有沒有對應的欄位**沒有確認**。
   - **ACP**：協定沒有系統提示的欄位，只能接在第一則 prompt 前面。
3. 內容分兩種：對所有 session 都一樣的通則，適合放 MCP `instructions`；和這個 session 有關的 (label、角色、父 session、是否在 worktree)，`termade mcp` 啟動時有 `--session`，可以查 daemon 後組出來。

## 實作方式

1. **組說明的函式** (daemon，建議新檔 `collab.rs`)
   ```rust
   fn collab_instructions(me: &Session, parent: Option<&Session>, extra: &str) -> String
   ```
   內容保持在 30 行以內，依情況組合：
   - 身分：你的 label、角色，由誰產生。
   - 回報：進行中用 `termade_status` 更新一句話的進度；完成或卡住時用 `termade_send` 告訴父 session (人產生的 session 則直接在回覆裡說明)。
   - 任務：被指派任務時先改 `doing`，完成改 `done`。
   - 有 worktree 分支時：成果留在這個分支，不要自己合併，除非被明確要求。
   - 協調者角色：用 `termade_task_create` 與 `depends_on` 安排順序，用 `termade_wait` 等回覆，不要用輪詢。
   - 需要別人的答案時用 `termade_ask`。
2. **第一層：MCP `instructions`**
   - `termade mcp` 在 `initialize` 時向 daemon 查自己的 session (`termade_whoami` 已有相同的查詢)，把 `collab_instructions` 的結果放進回應的 `instructions`。查不到就放通則。
3. **第二層：各家補強** (第一層實測無效的 agent 才需要)
   - Claude：`SessionSpec` 加 `system_append: Option<String>`，有值時加 `--append-system-prompt`。
   - Codex：先查 `thread/start` 的參數定義；有對應欄位就用，沒有就走下一項。
   - ACP 與其他：把說明接在**第一則** prompt 前面送給 agent。逐字稿的 `UserPrompt` 事件仍只記使用者原本的文字，使用者不會看到這段前言。
   - PTY：不注入。
4. **設定** (`config.toml`)
   ```toml
   [collab]
   instructions = true          # 預設開啟
   extra = ""                   # 附加在通則後面

   [roles.reviewer]
   instructions = "只讀程式碼並提出意見，不要修改檔案。"
   ```
   repo 的 `.termade/instructions.md` 存在時，內容附加在最後。
5. **整理既有文字**：指派訊息 (`task.rs`)裡重複的行為指引可以縮短，只留任務本身的資訊。
6. **交接**：交接簡報 (`handoff.rs`)不重複這段說明，新 agent 啟動時會自己拿到。

## 驗收條件

- 測試：`collab_instructions` 對「人產生／agent 產生」「有／沒有 worktree」「協調者／worker」各組合的輸出包含對應段落，不包含不相關的段落。
- 測試：MCP `initialize` 的回應有 `instructions`，內容含該 session 的 label。
- 測試：`[collab] instructions = false` 時回應沒有 `instructions`，Claude 的參數也沒有 `--append-system-prompt`。
- 測試：ACP 走前置文字時，送給 agent 的第一則 prompt 含說明，`UserPrompt` 事件不含。
- 測試：`.termade/instructions.md` 的內容出現在輸出最後。
- 實機驗證：四家各產生一個子 agent，給一個小任務，確認它做完會主動回報給父 session。哪一層對哪一家有效，記進 `docs/VERIFICATION.md`。
- 同步 README 與 `docs/DESIGN.md` 附錄 A (註明沒有採用 `AGENTS.md` 的原因)。
