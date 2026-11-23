# T95 — Web UI: replace native `window.confirm` with a styled in-app confirm dialog

Priority: P2　Scope: `ertangle-daemon` (web UI only)　Depends on: —
Status: done

## Problem

The web UI (`crates/ertangle-daemon/src/web/index.html`) uses the browser's native `confirm()` dialog in 6 places. Native dialogs look out of place next to the dark sheet / toast design, cannot show a danger style, and on mobile Safari they show the page origin.

| Line (approx.) | Function | Message |
| --- | --- | --- |
| 3982 | `pruneSessions()` | 確定要清理已結束且已合併的 Session 嗎？ |
| 4545 | `confirmResumeBeforePrompt()` | Session「…」已停止 (…)，是否接續並送出 prompt？ |
| 4756 | `confirmMerge()` | 確定合併此 Session 的變更到主分支？ |
| 5048 | `killCurrentSession()` | 確定強制結束此 Agent 子處理程序 (Kill)？ |
| 5057 | `restartDaemon()` | 確定重啟共用的 daemon？… |
| 5066 | `removeCurrentSession()` | 確定從清單中移除此 Session？ |

`rg -n "confirm\(" crates/ertangle-daemon/src/web/index.html` lists them. There are no `alert(` / `prompt(` calls.

## Implementation

1. Add one reusable dialog in `index.html`, styled with the existing design tokens (`--border`, `--card-hover`, `--red`, `--cyan`, `.btn-action`, `.btn-secondary`, `.btn-danger-outline`, `.sheet-backdrop` blur):
   - Markup: a backdrop + centered card (title, message, cancel + confirm buttons). Centered card, not a bottom sheet, so it reads as a confirmation; full-width buttons on narrow screens.
   - JS: `function showConfirm({ title, message, confirmText = "確定", cancelText = "取消", danger = false }) -> Promise<boolean>`.
     - Resolves `true` on confirm, `false` on cancel, backdrop click, or `Escape`; `Enter` confirms.
     - Focus the confirm button on open; restore focus to the previously focused element on close.
     - Use `textContent` (never `innerHTML`) for title/message, since messages include session labels.
     - A second `showConfirm` while one is open resolves the first as `false` before showing the new one.
     - `z-index` above sheets (sheets use 50, toast uses 100): e.g. 90, so it can open from inside the session-manage sheet.
2. Convert the 6 call sites to `async` functions using `await showConfirm(...)`. Keep their existing behavior after confirmation exactly the same. Use `danger: true` for kill, remove, restart daemon, and prune. Suggested titles: 清理 Session / 接續 Session / 合併變更 / 強制結束 / 重啟 daemon / 移除 Session.
3. `confirmResumeBeforePrompt(text, attachments)` is called synchronously from `sendPrompt()` and returns `true` when it handled the prompt. Keep that contract: if the session is stopped and resumable, return `true` immediately and perform the dialog + send asynchronously (the draft must stay in the composer until the user confirms; clear it only after sending; on cancel keep the draft and toast 「已保留草稿並取消」). Capture `activeSession` before awaiting.
4. Update tests in `crates/ertangle-daemon/src/web.rs` (`mod tests`, string assertions on `INDEX_HTML`):
   - `prune_button_and_request` currently asserts `INDEX_HTML.contains("confirm(")`; change it to assert the `showConfirm(` usage.
   - `composer_confirms_resume_for_stopped_sessions` asserts current resume strings; keep it passing (adjust if wording moves into `showConfirm`).
   - Add `web_ui_uses_styled_confirm_dialog`: `INDEX_HTML` contains `function showConfirm(` and does not contain `window.confirm(` or ` confirm("` (native calls).

## Acceptance criteria

- No native `confirm(` / `window.confirm(` calls remain in `index.html`.
- All 6 actions show the styled dialog; cancel / Escape / backdrop do nothing (resume keeps the draft); confirm performs the original action.
- Dialog works on desktop and narrow mobile widths and above open sheets.
- `make pre-commit` and `cargo test -p ertangle-daemon` pass. If daemon git tests fail with gpg signing errors, rerun with `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=commit.gpgsign GIT_CONFIG_VALUE_0=false` (environment issue, unrelated).
- Extract the inline `<script>` and run `node --check` on it to catch syntax errors.
