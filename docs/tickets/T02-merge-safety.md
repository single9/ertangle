# T02 — merge 前置檢查、衝突還原與 auto-commit 範圍

優先度：P0　範圍：`termade-daemon`（git 輔助函式）

## 問題

`termade merge`（以及 MCP 的 `termade_merge`、TUI 的 `M`）直接在主工作區執行 `git merge`：

- 衝突時只回傳 stderr，主工作區留在合併中狀態，沒有還原。
- 不檢查主工作區是否有未 commit 的變更，也不檢查目前在哪個分支，agent 的成果可能併進使用者正在處理的任意分支。
- 合併前的自動 commit 用 `git add -A`，agent 留下的建置產物、暫存檔、憑證檔都會被 commit。
- 「主工作區」取自 daemon 啟動時的 cwd，而不是 session 所屬的 repo。daemon 設計上服務多個 repo，對其他 repo 的 session 會在錯的目錄執行 merge。

## 證據

- `crates/termade-daemon/src/lib.rs:1558-1577`：`merge_worktree` 只有 `git merge --no-edit <branch>`，失敗即 `bail!`。
- `crates/termade-daemon/src/lib.rs:1518-1555`：`auto_commit_worktree` 執行 `git add -A` 後 `git reset -q -- .termade`，且兩者的結果都被 `let _ =` 丟棄。
- `crates/termade-daemon/src/lib.rs:928-933`：`merge_worktree(&default_cwd, &s)`；`default_cwd` 在 `1703` 取自 `std::env::current_dir()`。
- `crates/termade-daemon/src/lib.rs:1527`、`1444`：以 `l.contains(".termade/")` 過濾狀態行，路徑中任何位置含這個字串的檔案都會被忽略。

## 分析邏輯

對會寫入使用者 repo 的函式，我逐一問「這個指令失敗時 repo 停在什麼狀態」與「它假設了什麼前提」：

1. `git merge` 衝突後不 abort，repo 就停在 `MERGING`，這是 git 的既定行為，不需重現。
2. 前提方面，函式沒有任何檢查，所以髒工作區與錯誤分支都會照做。
3. 追 `main_repo` 參數的來源，一路回到 daemon 啟動目錄，與「單一 daemon 管所有 repo」的設計前提衝突。多 repo 的錯誤行為是推論，沒有實機驗證。
4. `add -A` 的範圍問題取決於 repo 的 `.gitignore`，有妥善設定時影響較小。

## 實作方式

1. **記錄 session 的主 repo**：建立 worktree 時把 repo 根目錄存進 `Session`（例如 `repo_root`），merge／gc／pr 都用它，不再用 `default_cwd`。
2. **合併前檢查**（任一不成立就回錯誤、不動 repo）：
   - 主工作區 `git status --porcelain` 為空（忽略 `.termade/`）。
   - 主工作區不在 `MERGING`／`REBASING` 狀態。
   - 回覆訊息中明確寫出要併入的目標分支名稱。
3. **衝突還原**：`git merge` 失敗時執行 `git merge --abort`，回傳衝突檔案清單（`git diff --name-only --diff-filter=U`，在 abort 之前取）。
4. **auto-commit**：
   - `git add -A` 與後續指令的失敗要回報，不再丟棄。
   - 排除 `.termade` 改用 pathspec：`git add -A -- . ':(exclude).termade'`，取代先加再 reset。
   - 狀態行過濾改成比對路徑開頭（去掉前三個字元後 `starts_with(".termade/")`）。
5. **增加乾跑**：`Request::WorktreeMerge` 增加 `dry_run`，只回報會 commit 的檔案與是否可快轉，讓 TUI 能在 `M` 之前顯示確認。

## 驗收條件

以暫存 git repo 寫測試（目前這些函式完全沒有測試）：

- 無衝突：合併成功，主工作區乾淨。
- 有衝突：回傳錯誤與衝突檔案，主工作區回到合併前的 HEAD，且不在 `MERGING`。
- 主工作區有未 commit 變更：拒絕合併，repo 不變。
- worktree 內的 `.termade/` 內容不進 commit；名稱含 `.termade/` 子字串但不在 `.termade` 目錄下的檔案會被 commit。
- 兩個不同 repo 的 session 各自併回自己的 repo。
