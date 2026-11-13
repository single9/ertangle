//! 用假的 codex app-server 驗證錯誤會被送到逐字稿與 status note（T23）。

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};
use termade_adapter::{spawn, AgentEvent, SessionSpec};
use termade_core::{AgentKind, PermissionPolicy, SessionStatus};
use termade_protocol::Transport;

fn spec(mode: &str) -> SessionSpec {
    SessionSpec {
        id: "ses-codex-fake".into(),
        kind: AgentKind::Codex,
        cwd: std::env::temp_dir(),
        transport: Transport::Structured,
        bin: Some(format!(
            "{}/tests/fake_codex.sh",
            env!("CARGO_MANIFEST_DIR")
        )),
        args: vec![],
        model: None,
        effort: None,
        allowed_tools: vec![],
        disallowed_tools: vec![],
        read_only: false,
        sandbox: None,
        env: vec![("FAKE_MODE".into(), mode.into())],
        permission: PermissionPolicy::default(),
        mcp_exe: None,
        system_append: None,
        initial_prompt: Some("hi".into()),
        initial_attachments: vec![],
        resume: None,
        cols: 80,
        rows: 24,
    }
}

#[derive(Default)]
struct Seen {
    out: String,
    notes: Vec<String>,
    failed: bool,
    exited: Option<Option<i32>>,
}

/// 收事件直到 `Exited` 或逾時。
fn collect(rx: &Receiver<AgentEvent>, until_exit: bool) -> Seen {
    let mut s = Seen::default();
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(AgentEvent::Output(t)) => s.out.push_str(&t),
            Ok(AgentEvent::Note(n)) => s.notes.push(n),
            Ok(AgentEvent::Status(SessionStatus::Failed)) => {
                s.failed = true;
                if !until_exit {
                    break;
                }
            }
            Ok(AgentEvent::Exited(c)) => {
                s.exited = Some(c);
                break;
            }
            _ => {}
        }
    }
    s
}

#[test]
fn auth_error_is_reported_once_with_login_hint() {
    let (tx, rx) = mpsc::channel();
    let _ctl = spawn(spec("auth"), tx).expect("啟動假 codex");
    let s = collect(&rx, false);
    // Failed 之後若還有重複的錯誤事件，會在這段時間內到達。
    std::thread::sleep(Duration::from_millis(500));
    let mut tail = Seen::default();
    for ev in rx.try_iter() {
        match ev {
            AgentEvent::Output(t) => tail.out.push_str(&t),
            AgentEvent::Note(n) => tail.notes.push(n),
            _ => {}
        }
    }
    assert!(s.failed);
    let all = format!("{}{}", s.out, tail.out);
    assert_eq!(all.matches("refresh token was revoked").count(), 1, "{all}");
    assert_eq!(s.notes.len() + tail.notes.len(), 1);
    assert!(s.notes[0].contains("codex login"), "{:?}", s.notes);
}

#[test]
fn plain_error_has_no_login_hint() {
    let (tx, rx) = mpsc::channel();
    let _ctl = spawn(spec("plain"), tx).expect("啟動假 codex");
    let s = collect(&rx, false);
    assert!(s.failed);
    assert!(s.out.contains("model exploded"), "{}", s.out);
    assert!(!s.notes[0].contains("codex login"));
}

#[test]
fn stderr_tail_shown_on_nonzero_exit() {
    let (tx, rx) = mpsc::channel();
    // 握手前就掛掉：spawn 回 Err，但事件仍會送出。
    assert!(spawn(spec("die"), tx).is_err());
    let s = collect(&rx, true);
    assert_eq!(s.exited, Some(Some(3)));
    assert!(s.out.contains("final failure"), "{}", s.out);
    assert!(s.out.contains("boom line 29"));
    assert!(!s.out.contains("boom line 5\n"), "只應保留最後幾行");
    assert!(!s.out.contains('\u{1b}'), "應已剝除 ANSI");
}

#[test]
fn heavy_stderr_does_not_block_agent() {
    let (tx, rx) = mpsc::channel();
    let _ctl = spawn(spec("flood"), tx).expect("灌 stderr 時握手仍應完成");
    let s = collect(&rx, true);
    assert_eq!(s.exited, Some(Some(1)));
    assert!(s.out.contains("noise noise"), "{}", s.out);
    assert!(s.out.lines().count() <= 25);
}
