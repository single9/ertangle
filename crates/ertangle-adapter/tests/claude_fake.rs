//! 用假的 claude 執行檔驗證 stream-json adapter 的 prompt 往返與結束事件。

use ertangle_adapter::{spawn, AgentEvent, SessionSpec};
use ertangle_core::{AgentKind, PermissionPolicy};
use ertangle_protocol::Transport;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn fake_bin() -> String {
    format!("{}/tests/fake_claude.sh", env!("CARGO_MANIFEST_DIR"))
}

fn fake_spec(resume: Option<&str>) -> SessionSpec {
    SessionSpec {
        id: "ses-fake".into(),
        kind: AgentKind::ClaudeCode,
        cwd: std::env::temp_dir(),
        transport: Transport::Structured,
        bin: None,
        args: vec![],
        model: None,
        effort: None,
        allowed_tools: vec![],
        disallowed_tools: vec![],
        read_only: false,
        sandbox: None,
        env: vec![],
        permission: PermissionPolicy::default(),
        mcp_exe: None,
        system_append: None,
        initial_prompt: Some("ping".into()),
        initial_attachments: vec![],
        resume: resume.map(str::to_string),
        cols: 80,
        rows: 24,
    }
}

#[test]
fn claude_roundtrip_output_cost_and_exit() {
    std::env::set_var("ERTANGLE_CLAUDE_BIN", fake_bin());
    let (tx, rx) = mpsc::channel();
    let spec = fake_spec(None);
    let resume_spec = fake_spec(Some("native-42"));
    let ctl = spawn(spec, tx).expect("啟動假 claude");
    assert_eq!(ctl.transport(), "stream-json");

    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut out, mut cost, mut model, mut native) = (String::new(), None, None, None);
    while Instant::now() < deadline && (cost.is_none() || !out.contains("pong")) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(AgentEvent::Output(t)) => out.push_str(&t),
            Ok(AgentEvent::Cost(c)) => cost = Some(c),
            Ok(AgentEvent::Model(m)) => model = Some(m),
            Ok(AgentEvent::NativeId(id)) => native = Some(id),
            _ => {}
        }
    }
    assert!(out.contains("pong from fake"), "output: {out:?}");
    assert_eq!(cost.expect("應收到 cost").output_tokens, 4);
    assert_eq!(model.as_deref(), Some("fake-model"));
    assert_eq!(
        native.as_deref(),
        Some("fake-new"),
        "init 的 session_id 應往上送"
    );

    // kill 後必須收到 Exited。
    ctl.kill().unwrap();
    let mut exited = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !exited {
        if let Ok(AgentEvent::Exited(_)) = rx.recv_timeout(Duration::from_millis(200)) {
            exited = true;
        }
    }
    assert!(exited, "子處理程序結束時應發出 Exited");

    // 接續：`--resume <id>` 會傳給 agent，`init` 回報同一個 id。
    let (tx, rx) = mpsc::channel();
    let ctl = spawn(resume_spec, tx).expect("接續假 claude");
    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut out, mut native) = (String::new(), None);
    while Instant::now() < deadline && (native.is_none() || !out.contains("pong")) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(AgentEvent::Output(t)) => out.push_str(&t),
            Ok(AgentEvent::NativeId(id)) => native = Some(id),
            _ => {}
        }
    }
    assert!(out.contains("resumed native-42"), "output: {out:?}");
    assert_eq!(native.as_deref(), Some("native-42"));
    ctl.kill().unwrap();
}

#[test]
fn resume_is_rejected_for_pty_and_custom_agents() {
    let (tx, _rx) = mpsc::channel();
    let mut spec = fake_spec(Some("x"));
    spec.transport = Transport::Pty;
    let e = spawn(spec, tx.clone()).err().expect("PTY 不能接續");
    assert!(e.to_string().contains("PTY"), "{e}");
    let mut spec = fake_spec(Some("x"));
    spec.kind = AgentKind::Custom("mytool".into());
    spec.transport = Transport::Auto;
    assert!(spawn(spec, tx).is_err(), "自訂 agent 不能接續");
}
