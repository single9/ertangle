//! 用假的 ACP agent 驗證原生對話 id 的回報與 `session/load` 接續。

use ertangle_adapter::{spawn, AgentEvent, SessionSpec};
use ertangle_core::{AgentKind, PermissionPolicy};
use ertangle_protocol::Transport;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

fn fake_spec(resume: Option<&str>, prompt: Option<&str>, no_load: bool) -> SessionSpec {
    SessionSpec {
        id: "ses-acp".into(),
        kind: AgentKind::OpenCode,
        cwd: std::env::temp_dir(),
        transport: Transport::Structured,
        bin: Some(format!("{}/tests/fake_acp.sh", env!("CARGO_MANIFEST_DIR"))),
        args: vec![],
        model: None,
        effort: None,
        allowed_tools: vec![],
        disallowed_tools: vec![],
        read_only: false,
        sandbox: None,
        env: if no_load {
            vec![("FAKE_ACP_NO_LOAD".into(), "1".into())]
        } else {
            vec![]
        },
        permission: PermissionPolicy::default(),
        mcp_exe: None,
        system_append: None,
        initial_prompt: prompt.map(str::to_string),
        initial_attachments: vec![],
        resume: resume.map(str::to_string),
        cols: 80,
        rows: 24,
    }
}

/// 收事件直到 `done` 成立或逾時，回傳 (輸出, 原生 id)。
fn collect(
    rx: &Receiver<AgentEvent>,
    done: impl Fn(&str, &Option<String>) -> bool,
) -> (String, Option<String>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut out, mut native) = (String::new(), None);
    while Instant::now() < deadline && !done(&out, &native) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(AgentEvent::Output(t)) => out.push_str(&t),
            Ok(AgentEvent::NativeId(id)) => native = Some(id),
            _ => {}
        }
    }
    (out, native)
}

#[test]
fn new_session_reports_native_id_when_agent_can_load() {
    let (tx, rx) = mpsc::channel();
    let ctl = spawn(fake_spec(None, None, false), tx).expect("啟動假 ACP agent");
    let (_, native) = collect(&rx, |_, n| n.is_some());
    assert_eq!(native.as_deref(), Some("acp-new"));
    ctl.kill().unwrap();
}

#[test]
fn load_drops_replayed_history_and_keeps_new_output() {
    let (tx, rx) = mpsc::channel();
    let ctl = spawn(fake_spec(Some("acp-old"), Some("hi"), false), tx).expect("接續假 ACP agent");
    let (out, native) = collect(&rx, |o, n| n.is_some() && o.contains("fresh reply"));
    assert_eq!(native.as_deref(), Some("acp-old"), "接續後沿用原本的 id");
    assert!(out.contains("fresh reply"), "output: {out:?}");
    assert!(
        !out.contains("OLD HISTORY"),
        "載入時重播的歷史不該往上送：{out:?}"
    );
    ctl.kill().unwrap();
}

#[test]
fn agent_without_load_session_cannot_resume_and_reports_no_id() {
    let (tx, _rx) = mpsc::channel();
    let e = spawn(fake_spec(Some("acp-old"), None, true), tx)
        .err()
        .expect("沒有 loadSession 不能接續");
    assert!(e.to_string().contains("loadSession"), "{e}");

    // 開新對話可以，但不回報 id (之後接不回來，就不要讓上層以為可以)。
    let (tx, rx) = mpsc::channel();
    let ctl = spawn(fake_spec(None, Some("hi"), true), tx).expect("啟動假 ACP agent");
    let (out, native) = collect(&rx, |o, _| o.contains("fresh reply"));
    assert!(out.contains("fresh reply"), "output: {out:?}");
    assert_eq!(native, None);
    ctl.kill().unwrap();
}
