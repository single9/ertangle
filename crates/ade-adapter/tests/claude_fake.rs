//! 用假的 claude 執行檔驗證 stream-json adapter 的 prompt 往返與結束事件。

use ade_adapter::{spawn, AgentEvent, SessionSpec};
use ade_core::{AgentKind, PermissionPolicy};
use ade_protocol::Transport;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn fake_bin() -> String {
    format!("{}/tests/fake_claude.sh", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn claude_roundtrip_output_cost_and_exit() {
    std::env::set_var("ADE_CLAUDE_BIN", fake_bin());
    let (tx, rx) = mpsc::channel();
    let spec = SessionSpec {
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
        initial_prompt: Some("ping".into()),
        cols: 80,
        rows: 24,
    };
    let ctl = spawn(spec, tx).expect("啟動假 claude");
    assert_eq!(ctl.transport(), "stream-json");

    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut out, mut cost, mut model) = (String::new(), None, None);
    while Instant::now() < deadline && (cost.is_none() || !out.contains("pong")) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(AgentEvent::Output(t)) => out.push_str(&t),
            Ok(AgentEvent::Cost(c)) => cost = Some(c),
            Ok(AgentEvent::Model(m)) => model = Some(m),
            _ => {}
        }
    }
    assert!(out.contains("pong from fake"), "output: {out:?}");
    assert_eq!(cost.expect("應收到 cost").output_tokens, 4);
    assert_eq!(model.as_deref(), Some("fake-model"));

    // kill 後必須收到 Exited。
    ctl.kill().unwrap();
    let mut exited = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !exited {
        if let Ok(AgentEvent::Exited(_)) = rx.recv_timeout(Duration::from_millis(200)) {
            exited = true;
        }
    }
    assert!(exited, "子程序結束時應發出 Exited");
}
