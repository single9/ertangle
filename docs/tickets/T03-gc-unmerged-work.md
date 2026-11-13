# T03 — gc 不得刪除未合併的成果

優先度：P0　範圍：`termade-daemon`、`termade-protocol`、`src/main.rs`、`termade-tui`

## 問題

`termade gc`（TUI 的 `G`）會對所有已結束的 session 強制移除 worktree 並刪除分支，不檢查成果是否已合併、worktree 內是否還有未 commit 的變更。agent 做完工作結束後，使用者按一次 `G` 就可能失去全部成果。

## 證據

- `crates/termade-daemon/src/lib.rs:969-995`：候選條件只有 `branch.is_some()` 且 `!status.is_live()`（或指定的 session）。
- `crates/termade-daemon/src/lib.rs:1603-1621`：`git worktree remove --force` 加 `git branch -D`，兩者的結果都被丟棄，函式永遠回 `Ok(())`。
- 因為永遠回 `Ok`，`已清理 N 個 worktree` 的數字是候選數，不是實際移除數。

## 分析邏輯

1. 設計文件第 8 節寫「session 結束後 worktree 預設保留供檢視，可 `/gc` 清除」，所以 gc 的對象正是「結束但可能還沒看過」的 worktree。
2. 看實作用的旗標：`--force` 會丟掉未 commit 變更，`-D` 會刪掉未合併的分支。兩者合起來沒有任何保護。
3. commit 過的內容在 reflog 過期前還能用 `git fsck` 找回；未 commit 的變更則無法復原。

## 實作方式

1. **分類候選**：對每個候選判斷
   - 是否有未 commit 變更（`git -C <wt> status --porcelain`，忽略 `.termade/`）。
   - 分支是否已併入主 repo 目前的 HEAD（`git merge-base --is-ancestor <branch> HEAD`）。
2. **預設只清安全的**：乾淨且已合併的才移除，改用不帶 `--force` 的 `git worktree remove` 與 `git branch -d`。
3. **其餘列出不刪**：回覆中列出被略過的 session 與原因（未合併／有未 commit 變更）。
4. **明確的強制選項**：`Request::WorktreeGc` 增加 `force: bool`，CLI 為 `termade gc --force`。TUI 的 `G` 走預設安全模式；強制只能從命令面板下，並顯示會失去的 session 清單後再確認。
5. **回報真實結果**：`remove_worktree` 回傳實際錯誤，計數只算成功的。
6. 主 repo 路徑改用 T02 引入的 `Session.repo_root`（若 T02 尚未完成，先沿用 `default_cwd`）。

## 驗收條件

以暫存 git repo 寫測試：

- 已合併且乾淨的 worktree：被移除，分支被刪。
- 有未合併 commit 的 worktree：預設保留，回覆列出原因。
- 有未 commit 變更的 worktree：預設保留。
- `force` 時三種都移除。
- 移除失敗時計數不增加。
