# Rust 程式寫作慣例

本文件是 `ertangle` 所有 Rust 程式碼必須遵守的定義。`AGENTS.md` 是入口，細則以此文件為準；程式碼審查與提交前檢查都依此進行。

## 1. 錯誤處理與 Panic

- production 程式碼不得對可能失敗的來源直接 `unwrap()` 或 `expect()`：外部輸入、設定檔、JSON、IPC 訊框、子行程輸出、檔案 I/O、網路、共用狀態與 `Mutex` lock 都包含在內。
- 改用 `Result` 搭配 `?`、`match`、`if let`、`let-else`、`ok_or_else`、`unwrap_or` 或 `unwrap_or_default`。需要保留上下文時用 `anyhow::Context` 的 `.with_context()`。
- 唯一允許 `unwrap()` / `expect()` 的情境：
  - `#[cfg(test)]` 模組與 `tests/` 整合測試；
  - 編譯期常數或明顯不可能失敗的不變數，且必須在同行或上一行寫明原因，例如 `// 內建常數，parse 必定成功`。
- `Mutex` 中毒時不要 panic：用 `lock().unwrap_or_else(|e| e.into_inner())` 取回資料並繼續，或改成 `try_lock()` 搭配明確的錯誤回報。
- 不要用 `panic!`、`unreachable!`、`todo!`、`assert!` 處理執行期錯誤；`assert!` 只出現在測試。不要用索引直接存取外部資料的 `Vec` / `slice`，改用 `.get()`。
- 每個新的分支都要有明確的降級行為：回傳 `Err`、使用安全預設值，或送出錯誤訊息，而不是讓程式崩潰。

## 2. Unsafe

- 預設不使用 `unsafe`。能透過安全函式庫 (例如 `nix`) 或重構達成相同功能時，一律選安全做法。
- 真的無法避免時必須：
  - 把 `unsafe` 區塊縮到最小範圍，不要包住整個函式；
  - 在 `unsafe` 正上方寫 `// SAFETY:` 註解，說明所有必要條件為什麼成立；
  - 在 commit 說明中列出原因，並經另一位 agent 審查。
- 禁止非必要的 `unsafe impl`、raw pointer 算術與手動記憶體管理；若為 FFI 必要，需在此文件補上個案說明。

## 3. Clean Code

- 函式只做一件事；超過約 40 行或三層以上巢狀時，抽出有意義的純函式。
- 用 `let-else` 與提前 `return` 減少縮排；避免深層 `match` 巢狀。
- 命名要說清楚意圖：`session_id` 而不是 `s`；布林值用 `is_` / `has_` / `can_` 開頭。
- 不為了方便亂用 `clone()`；需要所有權時才 clone，優先 borrowing、`Cow` 或 `Arc`。
- 註解解釋「為什麼」，不重述程式碼；public API 要有 Rustdoc。
- 不新增相依性，除非它明顯降低複雜度或消除 `unsafe`；新增時要在 commit 說明原因。

## 4. 測試

- 任何行為修改都要補單元測試；純邏輯優先抽成可獨立測試的函式。
- 單元測試放在同一檔案的 `#[cfg(test)] mod tests`；跨 crate 行為放 `tests/` 整合測試。
- 測試中的 `unwrap()` / `expect()` 是可接受的，但要用有意義的訊息，例如 `.expect("無效 JSON 應被拒絕")`。
- 每個錯誤路徑至少一個測試：空值、格式錯誤、I/O 失敗、鎖中毒、權限不足。
- 提交前執行 `make pre-commit` 與受影響 crate 的 `cargo test -p <crate>`；兩者都要成功。

## 5. 提交前檢查

- `cargo fmt`、`cargo clippy --all-targets -- -D warnings`、`make i18n-check` 全部成功。
- commit message 使用英文 Conventional Commits。
- 修改本文件時要同步更新 `AGENTS.md` 的摘要。
