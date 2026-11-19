# T55 — 建立 Rust 程式寫作慣例並減少 production unwrap 與 unsafe

優先度：P2　範圍：全專案 Rust 程式碼　相依：—

Status: done

## 問題

專案沒有成文的 Rust 錯誤處理與 `unsafe` 規範。production 程式碼有大量 `unwrap()` / `expect()`，外部輸入、JSON、IPC 訊框或鎖中毒時可能直接 panic；唯一一處 `unsafe` 雖然已有 `SAFETY` 註解，但缺少統一的審查標準。

## 證據

- 全專案約 872 處 `.unwrap()` 與 62 處 `.expect()`，多數在測試，但仍約百處位於 production 路徑。
- `crates/termade-adapter/src/acp.rs:405` 對外部行程送來的 JSON 直接 `msg.get("id").unwrap()`。
- `crates/termade-core/src/lib.rs:2151` 在 `redact_secrets` 以 `chars().next().unwrap()` 推進索引。
- `crates/termade-daemon/src/server.rs:423` 使用 `unsafe { libc_geteuid() }`；`Cargo.lock` 已有 `nix`，可改用安全 API。
- `AGENTS.md` 先前只有 commit、通訊協定與提交前檢查，沒有 Rust 寫作規範。

## 分析邏輯

- 已在 `AGENTS.md` 與 `docs/agents/rust-conventions.md` 定義 production 禁用 panic 式 unwrap、unsafe 最低限度與 SAFETY 註解、單元測試要求。
- 已確認多數 unwrap 位於 `#[cfg(test)]` 與 `tests/`，測試中的 unwrap 是可接受且不算在整改範圍。
- 未確認：所有 production unwrap 是否都能在不改變公開 API 的前提下消除；由各 crate worker 逐一判讀。

## 實作方式

- 由 coordinator 編寫慣例文件與 ticket，worker 分頭處理 `termade-adapter`、`termade-core` / `termade-protocol` / `termade-tui`、`termade-daemon`。
- 以 `match` / `if let` / `let-else` / `ok_or_else` / 安全預設值取代可能 panic 的取值；`Mutex` 中毒採 `unwrap_or_else(|e| e.into_inner())`。
- `server.rs` 的 `geteuid` 優先改用 `nix::unistd::geteuid()` 消除 `unsafe`。
- 每條錯誤路徑補單元測試；純邏輯抽成可獨立測試的函式。
- 所有 worker 提交後由 coordinator 審查 diff、合併並執行 `make pre-commit` 與完整測試。

## 驗收條件

- `AGENTS.md` 與 `docs/agents/rust-conventions.md` 包含並連結完整的 Rust 寫作慣例。
- production 程式碼不再對外部輸入或 I/O 直接 `unwrap()` / `expect()`；`unsafe` 要嘛消除，要嘛有 `// SAFETY:` 註解與 commit 說明。
- `make pre-commit` 與 `make test` 成功；已知的 gpg 簽章環境問題不視為程式碼失敗。
- 新增或修改的錯誤路徑都有對應測試。

## 完成紀錄

- `5aa21f1` docs(agents): 新增 `docs/agents/rust-conventions.md`、`AGENTS.md` 規範摘要與本 ticket。
- `c12b913` fix(daemon): 移除 production unwrap；`server.rs` 改用 `nix::unistd::geteuid()`，刪除唯一的 `unsafe` 與 FFI 宣告。
- `f4a25ed` refactor(core,tui): `redact_secrets` 與 TUI 繪製路徑改用 `let-else` / `if let` / `match`，並補 `redact_secrets` 測試。
- `80e053f` refactor(adapter): ACP/Codex/Claude/PTY/stderr 的鎖與 JSON 取值改為安全降級；新增 `extract_request_id` 純函式與測試。
- 三個 worker 分支已合併回 `main`；`make pre-commit` 與完整 `make test` 均成功。
