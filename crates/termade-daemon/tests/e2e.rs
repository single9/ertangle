//! daemon 端到端：啟動 daemon → spawn（假 claude）→ prompt → 收到輸出 → shutdown。
//! 不依賴真實 agent 或網路。環境變數是行程全域的，所以整個流程放在單一測試裡。

use std::time::{Duration, Instant};
use termade_core::{AgentKind, Event};
use termade_daemon::{replay_log, Client};
use termade_protocol::{Frame, Request, SpawnRequest, Transport};

#[test]
fn spawn_prompt_output_shutdown() {
    let root = std::env::temp_dir().join(format!("termade-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("TERMADE_SOCKET", root.join("d.sock"));
    std::env::set_var("XDG_STATE_HOME", root.join("state"));
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::set_var(
        "TERMADE_CLAUDE_BIN",
        format!(
            "{}/../termade-adapter/tests/fake_claude.sh",
            env!("CARGO_MANIFEST_DIR")
        ),
    );
    std::env::remove_var("TERMADE_TOKEN");

    std::thread::spawn(|| {
        let _ = termade_daemon::run();
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !termade_daemon::is_running() {
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
        effort: None,
        attachments: vec![],
        inherit_from: None,
        args: vec![],
        budget_usd: None,
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
            effort: None,
            attachments: vec![],
            inherit_from: None,
            args: vec![],
            budget_usd: None,
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
    let parent_note = spawn_with(None, termade_core::PermissionPolicy::Ask);
    let parent_id = parent_note
        .split_whitespace()
        .find(|w| w.starts_with("ses_"))
        .unwrap()
        .to_string();
    let child_note = spawn_with(Some(parent_id), termade_core::PermissionPolicy::Bypass);
    assert!(child_note.contains("調降"), "{child_note}");
    let top_note = spawn_with(None, termade_core::PermissionPolicy::Bypass);
    assert!(!top_note.contains("調降"), "{top_note}");
    let mut snap = Client::connect().unwrap();
    snap.send(&Request::Snapshot).unwrap();
    let state = loop {
        if let Frame::Snapshot { state } = snap.recv().unwrap() {
            break state;
        }
    };
    let perms: Vec<_> = state.sessions.values().map(|s| s.permission).collect();
    assert!(perms.contains(&termade_core::PermissionPolicy::Bypass));
    assert!(perms.contains(&termade_core::PermissionPolicy::Ask));

    resume_keeps_session_transcript_branch_and_cost(&root);

    // 交接：舊 session 停止，新 session 沿用同一個工作目錄，簡報含原始任務。
    let mut h = Client::connect().unwrap();
    let old_id = {
        let mut snap = Client::connect().unwrap();
        snap.send(&Request::Snapshot).unwrap();
        let state = loop {
            if let Frame::Snapshot { state } = snap.recv().unwrap() {
                break state;
            }
        };
        state
            .sessions
            .values()
            .find(|s| s.label == "e2e")
            .unwrap()
            .id
            .clone()
    };
    h.send(&Request::Handoff {
        session: old_id.clone(),
        kind: AgentKind::ClaudeCode,
        model: None,
        effort: None,
        note: Some("先看測試".into()),
    })
    .unwrap();
    loop {
        match h.recv().unwrap() {
            Frame::Ok { .. } => break,
            Frame::Error { message } => panic!("交接失敗：{message}"),
            _ => {}
        }
    }
    let mut snap = Client::connect().unwrap();
    snap.send(&Request::Snapshot).unwrap();
    let state = loop {
        if let Frame::Snapshot { state } = snap.recv().unwrap() {
            break state;
        }
    };
    let old = &state.sessions[&old_id];
    assert!(!old.status.is_live(), "舊 session 應已停止");
    let new = state
        .sessions
        .values()
        .find(|s| s.label == "e2e→claude_code" || s.label.starts_with("e2e→"))
        .expect("應有接手的新 session");
    assert_eq!(new.cwd, old.cwd);
    let echoed = state.transcripts[&new.id]
        .iter()
        .any(|l| l.contains("接手"));
    assert!(echoed, "新 session 的第一個 prompt 應是交接簡報");

    let mut s = Client::connect().unwrap();
    s.send(&Request::Shutdown).unwrap();
    // log 已落地且 seq 連續。
    let (_, recs) = replay_log(&termade_daemon::log_path()).unwrap();
    assert!(!recs.is_empty());
    assert!(recs.windows(2).all(|w| w[1].seq == w[0].seq + 1));
}

fn snapshot() -> termade_core::State {
    let mut c = Client::connect().unwrap();
    c.send(&Request::Snapshot).unwrap();
    loop {
        if let Frame::Snapshot { state } = c.recv().unwrap() {
            return *state;
        }
    }
}

/// 送一個請求並等到 `Ok`；`Error` 直接失敗。
fn request_ok(req: &Request) -> String {
    let mut c = Client::connect().unwrap();
    c.send(req).unwrap();
    loop {
        match c.recv().unwrap() {
            Frame::Ok { note } => return note.unwrap_or_default(),
            Frame::Error { message } => panic!("{req:?} 失敗：{message}"),
            _ => {}
        }
    }
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// T14：spawn → kill → resume 後，session id 不變、回到 Idle、原本的逐字稿還在、
/// worktree 分支沒被動到，成本是舊值加新值。
fn resume_keeps_session_transcript_branch_and_cost(root: &std::path::Path) {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "a").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    request_ok(&Request::Spawn(SpawnRequest {
        kind: AgentKind::ClaudeCode,
        cwd: Some(repo.to_string_lossy().to_string()),
        label: Some("res".into()),
        role: Default::default(),
        prompt: Some("first-prompt".into()),
        worktree: true,
        transport: Transport::Structured,
        permission: None,
        parent: None,
        model: None,
        effort: None,
        attachments: vec![],
        inherit_from: None,
        args: vec![],
        budget_usd: None,
    }));
    let find = || {
        let st = snapshot();
        let s = st.sessions.values().find(|s| s.label == "res").cloned();
        s.map(|s| {
            let lines = st.transcripts.get(&s.id).cloned().unwrap_or_default();
            (s, lines)
        })
    };
    let wait_cost = |usd: f64| {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let (s, lines) = find().expect("session 應存在");
            if (s.cost.usd - usd).abs() < 1e-9 && s.status == termade_core::SessionStatus::Idle {
                return (s, lines);
            }
            assert!(
                Instant::now() < deadline,
                "等不到成本 {usd}：{:?} {:?}",
                s.cost,
                s.status
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let (before, _) = wait_cost(0.25);
    assert_eq!(before.native_id.as_deref(), Some("fake-new"));
    let branch = before.branch.clone().expect("應有 worktree 分支");
    let head = git(&repo, &["rev-parse", &branch]);

    // 沒結束的 session 不能接續。
    let mut c = Client::connect().unwrap();
    c.send(&Request::Resume {
        session: "res".into(),
        prompt: None,
    })
    .unwrap();
    assert!(matches!(c.recv().unwrap(), Frame::Error { .. }));

    request_ok(&Request::Kill {
        session: before.id.clone(),
    });
    assert!(!find().unwrap().0.status.is_live());
    // 緊接著接續：舊程序的 Exited 這時多半還沒到，不能把接回來的 session 又標成結束。
    let note = request_ok(&Request::Resume {
        session: "res".into(),
        prompt: Some("second-prompt".into()),
    });
    assert!(note.contains("已接續"), "{note}");

    let (after, lines) = wait_cost(0.5);
    assert_eq!(after.id, before.id, "session id 不變");
    assert_eq!(after.cost.output_tokens, 8, "token 也是舊值加新值");
    assert_eq!(after.native_id.as_deref(), Some("fake-new"));
    assert_eq!(after.branch.as_deref(), Some(branch.as_str()));
    assert_eq!(after.cwd, before.cwd);
    assert!(std::path::Path::new(&after.cwd).is_dir());
    assert_eq!(git(&repo, &["rev-parse", &branch]), head, "分支沒被動到");
    let text = lines.into_iter().collect::<Vec<_>>().join("\n");
    assert!(text.contains("first-prompt"), "原本的逐字稿應還在：{text}");
    assert!(text.contains("second-prompt"), "{text}");
    assert!(
        text.contains("resumed fake-new"),
        "agent 應以 --resume <原生 id> 啟動：{text}"
    );
    assert_eq!(
        snapshot()
            .sessions
            .values()
            .filter(|s| s.label == "res")
            .count(),
        1,
        "接續不會多出一個 session"
    );
    // 等舊程序的結束事件確定到過之後，接回來的 session 仍然活著、仍可送 prompt。
    std::thread::sleep(Duration::from_millis(400));
    assert!(find().unwrap().0.status.is_live());
    request_ok(&Request::Prompt {
        session: before.id.clone(),
        text: "third-prompt".into(),
        attachments: vec![],
    });
    request_ok(&Request::Kill { session: after.id });
}
