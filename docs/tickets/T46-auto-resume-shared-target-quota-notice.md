# T46 — daemon 重啟自動接續、worktree 共用 cargo target、額度提早提醒

優先度：P2　範圍：`termade-core`、`termade-adapter`、`termade-daemon`、`termade-tui`　相依：T01, T14, T38

Status: done

## 問題

1. daemon 重啟後，所有執行中的 worker 都變成 `Failed`，只留「可用 :resume 接續」的說明；coordinator 或使用者必須逐一接續並重新交代脈絡，worker 也常重新探索整個專案，浪費 token。
2. 每個 worktree 各有自己的 `target/`，多個 worker 同時跑 `cargo test` 等於同時編譯好幾份，CPU、記憶體與磁碟都被吃滿。
3. Claude 在接近上限時會回報 `allowed_warning` (附 `surpassedThreshold`)，但 termade 要等到自己的 90% 門檻才提醒，額度常在提醒前就用完。

## 證據

- 修正前 `crates/termade-daemon/src/lib.rs` 的重啟還原只發出 `SessionStatus::Failed` 與說明，註解寫明「不自動做 (會在使用者沒注意時動用額度)」。
- 2026-11-17 實測：daemon 重啟後三個 worker 全數停止，需手動 `termade resume` 加提示訊息。
- 實測 `rate_limit_event`：`"status":"allowed_warning"`、`"surpassedThreshold":0.75`、7 天視窗 77%。

## 分析邏輯

- 自動接續只針對重啟當下「正在跑回合」(`Busy`／`WaitingApproval`／`Starting`) 且有原生對話 id 的 session：這些是使用者已授權、被打斷的工作，接續不算額外動用額度。閒置的 session 不接續。
- 避免崩潰迴圈：自動接續後在 session 留下固定說明；下次重啟時若說明還在 (代表接續後沒有任何進展，agent 沒更新過狀態)，就不再自動接續。額度已滿也不接續。
- cargo 對 target 目錄加檔案鎖，共用目錄會讓編譯自然排隊，同時共用已編好的相依套件。放在 `.termade/target` 而非主工作目錄的 `target/`，避免和使用者自己的編譯互相阻擋。
- `allowed_warning` 只發溫和的 `QUOTA_NOTICE` 給 coordinator，不動 spawn 檢查與其他 session；到門檻才發 `QUOTA_WARN`。
- 未確認：共用 target 時，不同分支輪流編譯造成的重新編譯量；若實測過多可改為每個專案一個、但依分支分子目錄。

## 實作方式

- `config.rs`：`[agent_spawn] auto_resume`、`shared_cargo_target` (皆預設 `true`，僅全域設定)；`:settings` 可切換。
- `lib.rs`：`auto_resume_eligible`、`AUTO_RESUME_PROMPT`；重啟還原時收集要接續的 session，背景執行緒等 1 秒後呼叫 `resume_session`。修正過程中發現在持有 `inner` 鎖時呼叫 `default_cwd()` 會死鎖，已改為先讀設定再上鎖。
- `handlers/session.rs`：`launch_session` 對 worktree session 設定 `CARGO_TARGET_DIR=<repo>/.termade/target` (`shared_cargo_target_dir`)，在 `resolved.env` 之前加入，設定檔可覆寫；已有 `CARGO_TARGET_DIR` 時不設。
- `termade-core::Quota` 新增 `warning`；`claude.rs` 解析 `allowed_warning`；`quota_watch.rs` 新增 `quota_notice_body` 與提醒分級；TUI 標題列轉黃、`:quota` 標示「⚠ 接近上限」。

## 驗收條件

- 單元測試：`auto_resume_eligible` 的各種條件、`shared_cargo_target_dir` 只在 Cargo 專案的 worktree 生效、`allowed_warning` 解析為 `warning` 而非 `reached`、`QUOTA_NOTICE` 格式。
- `cargo test`、`make pre-commit` 通過。
- 實機：daemon 重啟後執行中的 worker 自動接續並先檢查 `git status` (待驗證)。
