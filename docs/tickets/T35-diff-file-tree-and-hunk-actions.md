# T35 — Diff 視圖檔案清單導覽、單檔摺疊與局部 Hunk 退回

優先度：P2　範圍：`termade-tui`、`termade-daemon`、`termade-protocol`　相依：—

Status: ready-for-agent

## 問題

當 Agent 在一個回合內修改了多個檔案 (例如同時改動 10 個以上的原始碼與測試檔) 時，目前 TUI 的 Diff 視圖 (`CenterView::Diff`) 操作體驗有以下限制：

1. **缺乏檔案級導覽與單檔摺疊**：
   - 目前 Diff 視圖 (`crates/termade-tui/src/diff.rs`) 只支援 `j` / `k` 逐行移動與 `{` / `}` 跳轉 Hunk (`@@`)。當某個檔案有數百行變更時，要跳到下一個檔案必須連按多次 `}`，也無法將已審閱完畢的檔案摺疊收起。
2. **缺乏快速捨棄單一 Hunk 或單一檔案變更的能力**：
   - 在審查過程中，若發現 Agent 多改了某個不相關的 Hunk (例如多餘的 debug 印出或誤改格式)，目前只能按 `c` 留註解花費額外一輪 Token 讓 Agent 改回來，無法直接在 Diff 視圖按鍵退回 (`discard` / `revert`) 該 Hunk 或檔案。

## 證據

- `crates/termade-tui/src/diff.rs:1-92`：僅提供 `jump_hunk`、`parse_hunk`、`build_gutter` 與 `diff_stat`，未解析檔案邊界 (`diff --git a/... b/...`)，也沒有檔案摺疊狀態。
- `crates/termade-tui/src/draw.rs:1310-1415`：`draw_diff` 直接逐行渲染 `app.diff_lines`，標題列只顯示總 `+N -M`，未顯示目前游標所在的檔案名稱與檔案進度 (`file 2/5`)。
- `crates/termade-tui/src/input.rs:230-310`：Diff 視圖按鍵僅綁定 `j`/`k`、`{`/`}`、`c`、`M`、`G`。

## UX 設計原則

1. **直覺的階層導覽對稱性**：
   - `{` / `}` 跳上／下一個 Hunk (`@@`)；對稱新增 **`(` / `)`** (或 `<` / `>`) 跳上／下一個檔案 (`diff --git`)。
   - 在 Diff 視圖按下 **`Enter` (或 `za` / `Space` 外的摺疊鍵 `v`)**，即可將目前游標所在的檔案「摺疊／展開」切換。摺疊時該檔案濃縮為單行高亮摘要：`▸ [已摺疊] src/foo.rs (+42 -10) — 按 Enter 展開`。
2. **標題列即時方位感**：
   - Diff 視圖上方標題列自動顯示目前游標所在的檔案與進度：`Diff: worker-a (@termade/ses_1) · [2/6] src/foo.rs · +48 -12`。
3. **安全的局部退回 (`d` / `:discard`)**：
   - 在 Diff 視圖移到某個 Hunk (或已摺疊的整個檔案) 按 **`X`** (或輸入 `:discard`)，狀態列先提示確認 (或帶 `:discard!` 直接執行)，將該 Hunk 從 Worktree 中安全退回並立即重整 Diff 畫面。

## 分析邏輯

1. **檔案邊界解析與摺疊映射 (`diff.rs`)**：
   - 在收到 `Frame::Diff` 時，掃描原始 `raw_diff_lines` 建立 `Vec<DiffFile>`：記錄每個檔案的 `path`、`start_line`、`end_line`、`added`、`removed`、`is_untracked` (`new file mode` + `--- /dev/null`)。
   - 在 `App` 新增 `diff_collapsed: BTreeSet<String>` (記錄被摺疊的檔案路徑)。
   - 當 `diff_collapsed` 改變時，由原始 diff 重建可見的 `diff_lines` 與 `diff_gutter`：被摺疊的檔案只輸出一行 `▸ [已摺疊] <path> (+<a> -<r>)`，這樣既有的游標移動 (`j`/`k`)、行號邊欄 (`diff_gutter`) 與搜尋 (`search.rs`) 都能直接無縫運作，不需在 `draw_diff` 內做複雜的座標換算。
2. **局部 Hunk / 檔案退回 (`git.rs`)**：
   - **針對已追蹤檔案的單一 Hunk**：從原始 diff 擷取該檔案的標頭 (`diff --git ...`、`--- a/...`、`+++ b/...`) 加上當前游標所在的 `@@ ... @@` Hunk 區塊，組合成合法的 unified patch，透過 `git -C <wt_dir> apply -R --unidiff-zero -` 反向套用退回。
   - **針對整個檔案** (當游標在 `diff --git` 標頭行或已摺疊行)：
     - 若為已追蹤檔案：執行 `git -C <wt_dir> checkout HEAD -- <path>`。
     - 若為未追蹤新檔 (`git ls-files --others`)：安全檢查確認 `<path>` 為相對路徑且不含 `..`、不位於 `.termade/` 後，刪除該未追蹤新檔 (`fs::remove_file`)。
3. **未實機確認的部分**：
   - `compute_worktree_diff` (`git.rs:97-101`) 對超過 200 行的未追蹤新檔會截斷並加上 `+... (truncated)`，因此對未追蹤新檔不能用 `git apply -R`，必須直接按整檔退回 (`checkout` / 刪除未追蹤檔) 處理。

## 實作方式

1. **Diff 檔案索引、跳轉與摺疊 (`crates/termade-tui/src/diff.rs`、`app.rs`、`draw.rs`、`input.rs`)**：
   - `App` 新增 `diff_raw_lines: Vec<String>`、`diff_files: Vec<DiffFile>`、`diff_collapsed: BTreeSet<String>`。
   - `diff.rs` 新增 `parse_files`、`rebuild_visible_diff(app: &mut App)`、`jump_file(app: &mut App, forward: bool)`、`toggle_collapse_current_file(app: &mut App)`、`extract_current_hunk_patch(app: &App) -> Option<(String, Option<String>)>` (回傳 `(file_path, optional_hunk_patch)`)。
   - `input.rs` 在 `CenterView::Diff` 綁定 `(` / `)` 跳檔案、`Enter` 摺疊／展開目前檔案。
   - `draw_diff` 在標題列顯示 `[i/total] <current_file>`。
2. **局部 Hunk 與單檔退回 (`crates/termade-protocol/src/lib.rs`、`crates/termade-daemon/src/git.rs`、`handlers/worktree.rs`)**：
   - `Request` 新增 `WorktreeDiscard { session: SessionId, file: String, patch: Option<String> }`。
   - `git.rs` 新增 `discard_worktree_change(wt_dir: &Path, file: &str, patch: Option<&str>) -> Result<String>`：有 `patch` 時以 stdin 餵給 `git apply -R`；無 `patch` 時對整個檔案還原。
   - 完成後 Daemon 回傳更新後的 `Frame::Diff`，TUI 自動重繪。

## 驗收條件

- 測試 (`diff.rs`)：解析含 3 個檔案的 unified diff，驗證 `jump_file` 能正確在 3 個檔案標頭間跳轉；驗證摺疊第 2 個檔案後，`diff_lines` 將該檔案濃縮為單行且第 3 個檔案的行號邊欄 (`diff_gutter`) 依然正確。
- 測試 (`git.rs`)：在暫存 Git Worktree 修改同一個檔案的兩個不同區塊 (產生 2 個 Hunk)，對第 1 個 Hunk 呼叫 `discard_worktree_change`，驗證第 1 個 Hunk 被還原且第 2 個 Hunk 完整保留。
- 同步更新 `help.rs`、`COMMANDS`、`locales/`、`README.md`、`README_EN.md`。
- `make pre-commit` 與 `cargo test` 全數通過。
