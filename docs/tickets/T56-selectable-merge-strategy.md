# T56 — 讓使用者選擇 worktree 合併策略 (merge / squash / rebase)

優先度：P2　範圍：CLI、TUI、MCP、daemon Git helper　相依：—

Status: doing

## 問題

`termade merge` 目前固定執行 `git merge --no-edit`，無法 squash 或 rebase。使用者若想把 worker 分支整理成單一提交，或維持線性歷史，只能事後手動處理。

## 證據

- `src/main.rs:351` 的 `Cmd::Merge` 只有 `--dry-run` 與 `--resolve`。
- `crates/termade-daemon/src/git.rs:435` 固定呼叫 `git merge --no-edit <branch>`。
- `termade merge --help` 沒有策略選項，`--dry-run` 只回報是否可快轉。

## 分析邏輯

- 協定先加入 `MergeStrategy` 與 `Request::WorktreeMerge.strategy`，並用 `#[serde(default)]` 保持舊 client 相容；預設 `merge` 不改現有行為。
- CLI 用 clap 限制輸入值，TUI `:merge` 與 MCP `termade_merge` 接受策略並在無效值時安全降級為 `merge`。
- daemon 共用一個合併實作，依策略執行 `git merge`、`git merge --squash` 加單一 commit，或在 worktree 先 `git rebase` 再 `--ff-only` 合併。
- 三種策略都沿用 `MergeConflict` 錯誤，讓 `--resolve` 的自動解衝突流程仍可運作；rebase 衝突起時不呼叫會觸發 `git merge --no-commit` 的 helper。

## 實作方式

- `66e7eb3` 加入協定型別與欄位。
- worker 分別處理使用者介面 (`src/main.rs`、`src/cmd/worktree.rs`、`crates/termade-tui/src/{input,commands}.rs`、`crates/termade-daemon/src/mcp.rs`) 與 daemon 實作 (`crates/termade-daemon/src/{git.rs,handlers/worktree.rs,handlers/mod.rs,tests.rs}`)。
- coordinator 審查、合併、更新多國語系目錄並執行完整驗證。

## 驗收條件

- `termade merge <session> --strategy squash` 與 `--strategy rebase` 可正確運作，`--strategy merge` 與省略參數行為不變。
- TUI `:merge --squash` / `--rebase` 與 MCP `strategy` 參數皆可選擇策略。
- 三種策略的衝突都可被 `--resolve` 接手，且 rebase 衝突不會殘留 rebase 狀態。
- `make pre-commit`、`make i18n-check` 與完整 `make test` 成功。
