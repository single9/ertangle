# T10 — 文件與實作同步、Makefile 可攜性

優先度：P3　範圍：`docs/DESIGN.md`、`README.md`、`Makefile`

## 問題

設計文件仍是動工前的版本加上逐里程碑追加的驗證紀錄，前半段描述的技術選型與介面和實作不同。新讀者（包含 agent）照文件理解會得到錯的模型。

## 證據

| 文件寫的 | 實際 |
| --- | --- |
| `DESIGN.md` 4.4：`#[async_trait] trait Adapter`，`async fn` | 同步的 `AgentControl` trait（`crates/ade-adapter/src/lib.rs:71`），std thread |
| `DESIGN.md` 第 11 節：`tokio`、`agent-client-protocol`、`rmcp`、`git2`、`notify`、`jiff`、`etcetera` | `Cargo.toml` 沒有這些依賴；ACP、MCP 手寫，git 以子程序呼叫 |
| `DESIGN.md` 第 12 節：`docs/PROTOCOL.md` | 檔案不存在 |
| `DESIGN.md` M2：12 個 `ade_*` 工具 | `README.md`：14 個 |
| `DESIGN.md` 驗證表：測試數 14／19／22／32 | 目前 58 |
| `DESIGN.md` 4.3 的 `Event` 變體名稱（`MessageDelta`、`ToolCallStarted`…） | 程式碼是 `Output`、`ToolCall`、`UserPrompt`、`SessionStatus`… |
| `DESIGN.md` 第 5 節機制 C：daemon 監看 `outbox/` | 只有寫入 `mailbox/`（`append_mailbox`），沒有 `notify` 也沒有 outbox 監看 |
| `DESIGN.md` 第 9 節：「MCP 工具回傳內容做敏感樣式過濾」 | `redact_secrets` 只用在寫 log（`lib.rs:411`）與 `ade_diff` 的回傳（`mcp.rs:684`），其他工具的回傳沒有過濾 |
| `README.md`：「M0–M5 全里程碑已完成」 | Codex 回合、ACP 逐項審批、Claude 審批都標示未驗證或未實作 |

- `Makefile:4`：`ZIG_DIR ?= $(HOME)/.local/lib/python3.10/site-packages/ziglang`，寫死 Python 版本與安裝位置。
- `README.md` 的安裝指令含 GitHub 網址，但本機 repo 沒有設定任何 remote。

## 分析邏輯

1. 把 `DESIGN.md` 第 4、11、12 節的具體名稱逐一到 `Cargo.toml` 與原始碼中找對應。
2. outbox 監看是用 grep 確認的：`outbox` 與 `notify` 在所有 `.rs` 中沒有任何結果。MCP 回傳過濾則是列出 `redact_secrets` 的全部呼叫點，只有 log 與 `ade_diff` 兩處；我沒有逐行讀完 `mcp.rs`。
3. 這些不影響執行，所以是 P3；但其中「規則引擎」「秘密過濾」的描述牽涉安全預期，T04、T05 完成時要同步改對應章節。

## 實作方式

1. **`DESIGN.md` 分成兩部分**：
   - 「現況」：照實作重寫第 4 節（核心型別與 `AgentControl`）、第 11 節（實際依賴與「為何手寫」的理由）、第 12 節（實際檔案結構）。
   - 「原始設計與未實作項目」：保留原構想，明確標示哪些沒做（outbox 監看、MCP 回傳過濾、每任務訊息預算、handoff 等）。
2. **里程碑驗證表**移到 `docs/CHANGELOG.md` 或 `docs/VERIFICATION.md`，移除會過期的測試數字。
3. **`PROTOCOL.md`**：若短期不寫，刪掉引用；要寫的話，從 `ade-protocol` 的 `Request`／`Frame` 型別整理，並列出 MCP 工具清單（以程式碼為準）。
4. **README 現況段**：把「全里程碑已完成」改成列出已驗證與未驗證的項目。
5. **Makefile**：`ZIG_DIR` 改為自動偵測（`python3 -c 'import ziglang, os; print(os.path.dirname(ziglang.__file__))'`，失敗時留空），並在 `macos-*` 目標開頭檢查 `cargo zigbuild` 是否存在、不存在時印出安裝提示。
6. **安裝網址**：確認 repo 的實際公開位置後更新，或在公開前先移除該行。

## 驗收條件

- `DESIGN.md`「現況」部分提到的每個 crate、型別、檔案在 repo 中都找得到。
- 文件中沒有指向不存在檔案的連結。
- 工具數量、按鍵表在 README 與 `DESIGN.md` 之間一致。
- 在沒有 `python3.10` 的機器上 `make build`、`make test` 正常，`make macos` 給出可理解的訊息。
