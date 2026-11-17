# T39 — `kill -SIG -<pgid>` 被 procps-ng 誤解析成 `kill -1`，結束 session 時整個桌面被登出

優先度：P0　範圍：`termade-adapter`　相依：T32

Status: done

## 問題

在 TUI 或 MCP 結束、休眠、喚醒 session (以及跑含 `Request::Kill` 的 e2e 測試) 時，使用者的整個桌面工作階段被登出，daemon 與所有 agent 一起消失，重新登入後所有 session 都顯示「daemon 重啟」。

## 證據

- `journalctl`：`user@1000.service: Main process exited, code=killed, status=9/KILL` 緊接 `Session NN logged out`，發生在 2026-11-17 20:54:27 與 20:56:18，兩次都與 worker 執行整個 workspace 的 `cargo test` 同一秒。記憶體充足 (125 GB 只用 9 GB)，不是 OOM。
- 修正前的 `crates/termade-adapter/src/lib.rs` `signal_pgid` 執行 `kill <sig> -<pgid>`。
- 以無害訊號實測 (`procps-ng 4.0.4`)：`strace -e trace=kill /usr/bin/kill -CONT -<pid>` 送出的是 `kill(-1, SIGCONT)`；`kill -0 -999999` 送出 `kill(-9, 0)`。也就是負號目標被拆成選項解析。
- `kill -s CONT -- -<pid>` 則正確送出 `kill(-<pid>, SIGCONT)`。

## 分析邏輯

1. `kill(-1, SIGKILL)` 會送給呼叫者有權限的所有處理程序，包含 `systemd --user`，所以整個桌面被登出。
2. `signal_pgid` 用在所有 adapter 的 `kill`／`sleep`／`wake` (`claude.rs`、`codex.rs`、`acp.rs`、`pty.rs`)，因此平常操作也會觸發，不只測試。
3. 未確認：其他發行版的 `kill` (util-linux、busybox) 是否有相同行為；改用 `-s NAME --` 形式在三者都合法，所以不影響修正。

## 實作方式

- `kill_args` 一律組成 `kill -s <NAME> -- <target>`，`signal_pid`／`signal_pgid` 共用 (`crates/termade-adapter/src/lib.rs:168-201`)。
- 拒絕對 PID／PGID 0 與 1 送訊號，避免打到自己的群組或所有處理程序。
- 單元測試：參數形式、0／1 被拒、對 `process_group(0)` 的 `sleep` 送 `-KILL` 只結束該群組。

## 驗收條件

- `strace -f -e trace=kill` 執行 `signal_tests` 只看到 `kill(-<pgid>, SIGKILL)`，沒有 `kill(-1, …)`。
- 整個 workspace 的 `cargo test` (含 e2e `Request::Kill`) 執行完桌面未被登出。
- 修正 commit：`79d9029`。
