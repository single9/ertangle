//! `termade completions` 與 `termade __complete sessions` 的整合測試。

use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_termade")
}

fn cmd_with_env(root: &Path, sock: &Path) -> Command {
    let mut cmd = Command::new(bin());
    cmd.env("TERMADE_SOCKET", sock)
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env(
            "TERMADE_CLAUDE_BIN",
            format!(
                "{}/crates/termade-adapter/tests/fake_claude.sh",
                env!("CARGO_MANIFEST_DIR")
            ),
        )
        .env_remove("TERMADE_REMOTE")
        .env_remove("TERMADE_TOKEN");
    cmd
}

#[test]
fn cli_completions_and_complete_sessions_e2e() {
    // macOS 的 temp_dir 經過符號連結 (/var → /private/var)，先正規化讓路徑比較一致。
    let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let root = tmp.join(format!("termade-compl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let proj_a = root.join("proj-a");
    let proj_b = root.join("proj-b");
    std::fs::create_dir_all(&proj_a).unwrap();
    std::fs::create_dir_all(&proj_b).unwrap();

    // 1. `termade completions <zsh|bash|fish>` 輸出非空腳本並含主要子命令與旗標值。
    for shell in ["zsh", "bash", "fish"] {
        let out = Command::new(bin())
            .args(["completions", shell])
            .output()
            .unwrap();
        assert!(out.status.success(), "{shell} completions 應成功");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(!stdout.trim().is_empty(), "{shell} 輸出不應為空");
        assert!(stdout.contains("spawn") && stdout.contains("merge"));
        if shell == "zsh" {
            assert!(stdout.contains("(opencode claude codex antigravity)"));
        }
    }

    // 2. socket 不存在時，`__complete sessions` 結束碼為 0、無輸出、不啟動 daemon 或產生 socket。
    let missing_sock = root.join("missing.sock");
    let out = cmd_with_env(&root, &missing_sock)
        .args(["__complete", "sessions"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "不應有 stdout：{:?}", out.stdout);
    assert!(out.stderr.is_empty(), "不應有 stderr：{:?}", out.stderr);
    assert!(!missing_sock.exists(), "不應建立 socket");
    assert!(
        !root.join("state/termade/daemon.log").exists(),
        "不應啟動背景 daemon"
    );

    // 3. socket 存在但無回應時，逾時後靜默結束且結束碼為 0。
    let slow_sock = root.join("slow.sock");
    let _listener = UnixListener::bind(&slow_sock).unwrap();
    let t0 = Instant::now();
    let out = cmd_with_env(&root, &slow_sock)
        .env("TERMADE_COMPLETE_TIMEOUT_MS", "80")
        .args(["__complete", "sessions"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(t0.elapsed() < Duration::from_secs(2), "應在逾時後快速返回");

    // 4. daemon 有 session 時，輸出目前專案的 session label（不列其他專案）。
    let live_sock = root.join("d.sock");
    let mut daemon = cmd_with_env(&root, &live_sock)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    while !live_sock.exists() {
        assert!(Instant::now() < deadline, "daemon 未在時限內建立 socket");
        std::thread::sleep(Duration::from_millis(30));
    }

    let spawn_a = cmd_with_env(&root, &live_sock)
        .args([
            "spawn",
            "--kind",
            "claude-code",
            "--label",
            "worker/alpha",
            "--cwd",
            &proj_a.to_string_lossy(),
            "--transport",
            "structured",
        ])
        .output()
        .unwrap();
    assert!(
        spawn_a.status.success(),
        "spawn a 失敗：{}",
        String::from_utf8_lossy(&spawn_a.stderr)
    );

    let spawn_b = cmd_with_env(&root, &live_sock)
        .args([
            "spawn",
            "--kind",
            "claude",
            "--label",
            "worker/other",
            "--cwd",
            &proj_b.to_string_lossy(),
            "--transport",
            "structured",
        ])
        .output()
        .unwrap();
    assert!(spawn_b.status.success());

    let out_a = cmd_with_env(&root, &live_sock)
        .current_dir(&proj_a)
        .args(["__complete", "sessions"])
        .output()
        .unwrap();
    assert!(out_a.status.success());
    let lines_a: Vec<&str> = std::str::from_utf8(&out_a.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(lines_a, ["worker/alpha"]);

    let _ = cmd_with_env(&root, &live_sock).arg("shutdown").output();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&root);
}
