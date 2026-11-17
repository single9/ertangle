# T34 — 智慧合併衝突解決迴圈、合併前驗證與自動審查守門員

優先度：P2　範圍：`termade-daemon`、`termade-protocol`、`termade-tui`、`src/`　相依：T02, T18

Status: ready-for-agent

## 問題

在多 Agent 平行開發並透過 Worktree 隔離的工作流程中，從「Worker 改完程式碼」到「安全合併回主分支」仍有三個環節需要人工頻繁介入：

1. **合併衝突時僅中止退回，無法一鍵交給 Agent 解衝突**：
   - 當兩個 Worker 分支先後合併、第二個分支在 `termade merge` 發生 Git 衝突時，目前 `git.rs:295-315` 會安全執行 `git merge --abort` 並回報衝突訊息，但使用者必須自己切換目錄手動解衝突，或手動複製錯誤訊息貼給 Agent。
2. **合併前缺乏自動化測試／Lint 守門 (`pre_merge_command`)**：
   - 按下 `M` (`termade merge`) 時，`handlers/worktree.rs:78-98` 僅檢查 Git 狀態是否乾淨與是否無衝突，不會先在該 Worktree 驗證 `cargo test` 或專案指定的檢查指令，可能把編譯失敗的程式碼合進主分支。
3. **任務進入 `review` 狀態時缺乏自動審查觸發 (Auto-Review)**：
   - 當 Worker 呼叫 `termade_task_update(status="review")` 時，`handlers/task.rs:153-230` 目前只處理 `status == Done` 解鎖下游任務，不會自動通知或喚醒專案中的 `reviewer` 角色 Session 進行初步 Code Review。

## 證據

- `crates/termade-daemon/src/git.rs:199-325`：`merge_worktree` 在 `git merge` 失敗時於 `main_repo` 執行 `git merge --abort` 並回傳 `Err`，沒有提供「將主分支 merge 進 worktree 並發送解衝突 prompt 給該 Session」的路徑。
- `crates/termade-daemon/src/handlers/worktree.rs:78-98`：`handle_merge` 直接呼叫 `git::merge_worktree`，未讀取 `config.toml` 的前置驗證指令。
- `crates/termade-daemon/src/handlers/task.rs:153-230`：`handle_update` 僅在 `task.status == TaskStatus::Done` 時呼叫 `unblock_dependents`，對 `TaskStatus::Review` 無額外動作。

## UX 設計原則

1. **合併衝突時的無縫引導**：
   - 當使用者在 TUI 按 `M` 發生合併衝突時，狀態列清楚提示：「`合併衝突 (<檔案清單>)；輸入 :merge --resolve 交由 Agent 自動解衝突`」(或在 Which-key `Space g r` 一鍵觸發)。
   - 觸發 `--resolve` 後，主工作區 (`main`) 保持 100% 乾淨不留衝突標記；衝突標記只留在該 Session 自己的隔離 Worktree 中，並由該 Agent 自動接手修改、跑測試與重新 commit。
2. **合併前驗證不卡死 TUI**：
   - 執行 `pre_merge_command` 時在鎖外執行，期間 TUI 狀態列顯示進度提示；若驗證失敗，狀態列顯示失敗摘要，並可帶 `--fix` 將錯誤輸出直接轉成 Prompt 交給該 Session 修復。

## 分析邏輯

1. **智慧解衝突迴圈 (Conflict Resolution Loop)**：
   - 當 `merge_worktree` 偵測到 `session.branch` 合入 `main_repo` 的 `<target_branch>` 發生衝突時：
     1. 先在 `main_repo` 執行 `git merge --abort` 確保主工作區乾淨。
     2. 若 `resolve_on_conflict == true`：切換到該 Session 的 `wt_dir`，執行 `git merge <target_branch> --no-commit` (使衝突標記產生於隔離的 `wt_dir` 內)，並透過 `git diff --name-only --diff-filter=U` 取得衝突檔案清單。
     3. 自動向該 Session 送出結構化 Prompt (若該 Session 已 `Exited`/`Failed` 且有 `native_id`，自動先觸發 `resume`；若無法 `resume` 則在 `wt_dir` 中退回 `git merge --abort` 並報錯)。
2. **合併前驗證 (`pre_merge_command`)**：
   - 在全域 `~/.config/termade/config.toml` 的 `[git]` 區段支援 `pre_merge_command = "cargo test"` (基於安全考量，專案層 `.termade/config.toml` 不得覆寫執行任意 shell 指令)。
   - 在 `auto_commit_worktree` 之後、實際合入 `main_repo` 之前，於 `wt_dir` 執行 `sh -c <pre_merge_command>` (設 timeout 300 秒)；非 0 退出則中止合併並回傳最後 20 行輸出。
3. **自動審查守門員 (`Auto-Review`)**：
   - 在 `handlers/task.rs:handle_update` 中，當任務狀態由非 `Review` 變為 `TaskStatus::Review` 時，搜尋同專案 (`project_root()`) 下活著且 `role == Role::Reviewer` 的 Session；若找到，自動投遞一則 `MsgKind::Handoff` 審查通知 (附上任務 ID、標題、負責 Worker 的 Label 與分支名稱)，利用既有的閒置喚醒機制讓 Reviewer 自動呼叫 `termade_diff` 開始審查。
4. **未實機確認的部分**：
   - 當 Worktree 處於有衝突 (`Unmerged` paths) 的狀態下，若使用者還沒等 Agent 修完就再次按 `M`，`auto_commit_worktree` 的 `git add -A` 會把還沒修完的 `<<<<<<<` 標記直接 commit 進去！因此 `auto_commit_worktree` 必須先檢查 `git status --porcelain` 是否含有 `UU`/`AA`/`DD` 等未解決衝突狀態，或檔案內是否仍含衝突標記，若有則拒絕自動 commit。

## 實作方式

1. **防呆與衝突委派 (`crates/termade-daemon/src/git.rs`、`handlers/worktree.rs`、`crates/termade-protocol/src/lib.rs`)**：
   - `auto_commit_worktree` 新增檢查：若 `git diff --name-only --diff-filter=U` 非空，回傳錯誤「工作區仍有未解決的合併衝突標記」。
   - `Request::WorktreeMerge` 新增 `#[serde(default)] resolve: bool`。
   - 新增 `prepare_conflict_in_worktree(wt_dir: &Path, target_branch: &str) -> Result<Vec<String>>`：在 `wt_dir` 執行 `git merge <target_branch>` 並回傳衝突檔案列表。
   - `handle_merge` 在衝突且 `resolve == true` 時，呼叫 `prepare_conflict_in_worktree` 並將解衝突 Prompt 送給該 Session。
2. **合併前檢查指令 (`crates/termade-daemon/src/config.rs`、`git.rs`)**：
   - `Config` 新增 `[git]` 區段 (`pre_merge_command: Option<String>`)，僅從全域設定讀取。
   - `merge_worktree` (非 `dry_run`) 在合併前於 `wt_dir` 執行 `pre_merge_command`。
3. **`Review` 狀態自動喚醒 Reviewer (`crates/termade-daemon/src/handlers/task.rs`)**：
   - 在 `handle_update` 新增 `notify_reviewer_on_review(shared, &task)`，找出同專案的 `Role::Reviewer` Session 並發送審查請求訊息。
4. **TUI 與 CLI 入口 (`crates/termade-tui/src/commands.rs`、`menu.rs`、`src/main.rs`)**：
   - `:merge` 支援 `--resolve` 旗標；Which-key `Space g` 新增 `r` (`合併並委派解衝突`)。
   - CLI `termade merge <session>` 新增 `--resolve` 旗標。

## 驗收條件

- 測試：製造兩個修改同一行的 Worktree 分支 A 與 B，先合併 A，再對 B 執行 `merge --resolve`：
  - 驗證主工作區 (`main`) 保持乾淨無 `MERGE_HEAD`。
  - 驗證 B 的 Worktree 內含有衝突檔案，且 B Session 收到包含衝突檔案清單的解衝突 Prompt。
- 測試：當 Worktree 內有未解決的 `UU` 衝突狀態時，`auto_commit_worktree` 會拒絕自動 commit，防止把 `<<<<<<<` 標記合入分支。
- 測試：設定 `pre_merge_command = "false"` 時，`merge` 會被阻擋並回傳錯誤訊息；設為 `"true"` 時正常合併。
- 測試：當任務狀態更新為 `review` 且同專案存在閒置的 `Role::Reviewer` Session 時，Reviewer 會收到訊息並被喚醒。
- 同步更新 `help.rs`、`COMMANDS`、`locales/`、`README.md`、`README_EN.md`。
- `make pre-commit` 與 `cargo test` 全數通過。
