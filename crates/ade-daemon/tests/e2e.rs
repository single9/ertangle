//! daemon 端到端：啟動 daemon → spawn（假 claude）→ prompt → 收到輸出 → shutdown。
//! 不依賴真實 agent 或網路。環境變數是行程全域的，所以整個流程放在單一測試裡。

use ade_core::{AgentKind, Event};
use ade_daemon::{replay_log, Client};
use ade_protocol::{Frame, Request, SpawnRequest, Transport};
use std::time::{Duration, Instant};

#[test]
fn spawn_prompt_output_shutdown() {
    let root = std::env::temp_dir().join(format!("ade-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("ADE_SOCKET", root.join("d.sock"));
    std::env::set_var("XDG_STATE_HOME", root.join("state"));
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::set_var(
        "ADE_CLAUDE_BIN",
        format!(
            "{}/../ade-adapter/tests/fake_claude.sh",
            env!("CARGO_MANIFEST_DIR")
        ),
    );
    std::env::remove_var("ADE_TOKEN");

    std::thread::spawn(|| {
        let _ = ade_daemon::run();
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ade_daemon::is_running() {
        assert!(Instant::now() < deadline, "daemon 沒有在時限內啟動");
        std::thread::sleep(Duration::from_millis(50));
    }

    let mut c = Client::connect().unwrap();
    c.send(&Request::Hello {
        client: "e2e".into(),
        since: None,
        token: None,
        epoch: None,
    })
    .unwrap();
    c.send(&Request::Spawn(SpawnRequest {
        kind: AgentKind::ClaudeCode,
        cwd: Some(root.to_string_lossy().to_string()),
        label: Some("e2e".into()),
        role: Default::default(),
        prompt: Some("ping".into()),
        worktree: false,
        transport: Transport::Structured,
        permission: None,
        parent: None,
        model: None,
        args: vec![],
    }))
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut got_output = false;
    while Instant::now() < deadline && !got_output {
        if let Frame::Record { record } = c.recv().unwrap() {
            if let Event::Output { text, .. } = &record.event {
                got_output = text.contains("pong from fake");
            }
        }
    }
    assert!(got_output, "應收到假 agent 的輸出");

    // T12：父 session 為 ask，帶 parent 的 bypass 請求會被調降；無 parent 的 bypass 則照給。
    let spawn_with = |parent: Option<String>, perm| {
        let mut cl = Client::connect().unwrap();
        cl.send(&Request::Spawn(SpawnRequest {
            kind: AgentKind::ClaudeCode,
            cwd: Some(root.to_string_lossy().to_string()),
            label: None,
            role: Default::default(),
            prompt: None,
            worktree: false,
            transport: Transport::Structured,
            permission: Some(perm),
            parent,
            model: None,
            args: vec![],
        }))
        .unwrap();
        loop {
            match cl.recv().unwrap() {
                Frame::Ok { note } => return note.unwrap_or_default(),
                Frame::Error { message } => panic!("spawn 失敗：{message}"),
                _ => {}
            }
        }
    };
    let parent_note = spawn_with(None, ade_core::PermissionPolicy::Ask);
    let parent_id = parent_note
        .split_whitespace()
        .find(|w| w.starts_with("ses_"))
        .unwrap()
        .to_string();
    let child_note = spawn_with(Some(parent_id), ade_core::PermissionPolicy::Bypass);
    assert!(child_note.contains("調降"), "{child_note}");
    let top_note = spawn_with(None, ade_core::PermissionPolicy::Bypass);
    assert!(!top_note.contains("調降"), "{top_note}");
    let mut snap = Client::connect().unwrap();
    snap.send(&Request::Snapshot).unwrap();
    let state = loop {
        if let Frame::Snapshot { state } = snap.recv().unwrap() {
            break state;
        }
    };
    let perms: Vec<_> = state.sessions.values().map(|s| s.permission).collect();
    assert!(perms.contains(&ade_core::PermissionPolicy::Bypass));
    assert!(perms.contains(&ade_core::PermissionPolicy::Ask));

    let mut s = Client::connect().unwrap();
    s.send(&Request::Shutdown).unwrap();
    // log 已落地且 seq 連續。
    let (_, recs) = replay_log(&ade_daemon::log_path()).unwrap();
    assert!(!recs.is_empty());
    assert!(recs.windows(2).all(|w| w[1].seq == w[0].seq + 1));
}
