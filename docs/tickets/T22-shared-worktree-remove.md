# T22 — 共用 worktree 的 session 在分支刪掉後無法移除

優先度：P2　範圍：`termade-daemon`(`handlers/session.rs`、`handlers/worktree.rs`、`git.rs`、`lib.rs`)

## 問題

`termade handoff` 讓新舊 session 共用同一個 worktree 與分支。兩個都結束後，先 `rm` 其中一個會把 worktree 和分支一起刪掉；剩下那個就再也移除不了：

- `termade rm <s>`：回「尚未合併：請先 merge，或用 force 強制移除」，但分支其實已合併、已刪除。
- `termade rm --force <s>`：回「移除 worktree 失敗：error: branch 'termade/ses_…' not found」。

結果是 session 永遠卡在清單和 event log 裡。

## 證據

2026-11-13 實際遇到：T14–T21 的 8 個 handoff session (`t14-resume→claude` 等)在原 session 被 `rm` 之後，兩種 rm 都失敗，最後只能留著。

- `crates/termade-daemon/src/lib.rs:435-442`：`cwd_in_use_by_other` 只看 `is_live()` 的 session。已結束、但還沒移除、共用同一個 cwd 的 session 不算「使用中」，所以第一個 `rm` 會放行並刪掉 worktree。
- `crates/termade-daemon/src/git.rs:307-324`：`gc_block_reason` 對 `merge-base --is-ancestor <branch> HEAD` 的任何非成功結果都回「尚未合併」。分支不存在時 git 也回非零，於是誤判。
- `crates/termade-daemon/src/git.rs:327-350`：`remove_worktree` 在目錄不存在時會跳過，但分支不存在時 `git branch -D` 失敗就整個 bail，連 `--force` 都過不了。
- `handle_gc`(`handlers/worktree.rs:117` 起)用同一組函式，推測有同樣的問題。**沒有實測 gc。**

## 分析邏輯

1. 根本原因是「worktree 屬於誰」沒有定義：兩個 session 記著同一個 `cwd` 與 `branch`，誰先 rm 誰就把東西刪了。
2. 只修第 2、3 點 (容忍分支不存在)就能讓卡住的 session 移得掉，風險低。
3. 第 1 點決定第一個 rm 該不該刪 worktree。保守做法：只要還有其他「沒被移除」的 session 參照同一個 cwd，就只移除 session 紀錄、不動 worktree 與分支；最後一個被移除時才真的刪。

## 實作方式

1. `gc_block_reason`：分支不存在 (`git rev-parse --verify --quiet refs/heads/<branch>` 失敗)且 worktree 目錄也不存在時，回 `None` (沒有東西要保護)。分支不存在但目錄還在時維持保守，回「分支不存在，worktree 仍在」。
2. `remove_worktree`：刪分支前先確認分支存在，不存在就跳過，不當成錯誤。
3. 新增 `cwd_shared_with(&s) -> Vec<label>`：其他尚未移除、`cwd` 相同的 session (不論狀態)。`handle_remove` 與 `handle_gc`：
   - 有 live 的共用者：維持現狀，拒絕。
   - 只有已結束的共用者：只 emit `SessionRemoved`，不刪 worktree／分支，回覆註明「worktree 仍由 <label> 參照，保留」。
   - 沒有共用者：照現在的流程。
4. 已經卡住的舊資料靠第 1、2 步就能用一般的 `rm` 清掉，不需要遷移。

## 驗收條件

- 測試：A、B 共用 worktree，都結束、分支已合併。`rm A` 後 worktree 與分支仍在；`rm B` 後兩者都刪除，A、B 都不在清單。
- 測試：分支與目錄都已不存在的 session，`rm` (不加 force)成功。
- 測試：分支不存在但目錄還在時，`rm` 不加 force 會拒絕並說明原因。
- 測試：`gc` 在同樣的共用情境下行為與 `rm` 一致。
- README 的 handoff 段落補一句：共用 worktree 會保留到最後一個 session 被移除。
