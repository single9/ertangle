use super::*;
use crate::git::*;
use ade_core::{AgentKind, Cost, Decision, Role};

// -----------------------------------------------------------------------
// T05：遠端與 socket 安全
// -----------------------------------------------------------------------

#[test]
fn token_compare_is_exact() {
    assert!(token_matches(Some("abc"), "abc"));
    assert!(!token_matches(Some("abd"), "abc"));
    assert!(!token_matches(Some("abcd"), "abc"));
    assert!(!token_matches(Some(""), "abc"));
    assert!(!token_matches(None, "abc"));
}

#[test]
fn listen_requires_token_and_loopback() {
    assert!(validate_listen("127.0.0.1:0", false, false).is_err());
    assert!(validate_listen("127.0.0.1:0", true, false).is_ok());
    assert!(validate_listen("0.0.0.0:7420", true, false).is_err());
    assert!(validate_listen("0.0.0.0:7420", true, true).is_ok());
}

#[test]
fn run_remote_refuses_listen_without_token() {
    std::env::remove_var("ADE_TOKEN");
    let err = run_remote(RemoteOptions {
        listen: Some("127.0.0.1:0".into()),
        ..Default::default()
    })
    .unwrap_err();
    assert!(err.to_string().contains("token"));
}

#[test]
fn token_file_must_be_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_cwd();
    let f = dir.join("tok");
    fs::write(&f, "secret\n").unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_token_file(&f).is_err());
    fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(read_token_file(&f).unwrap(), "secret");
}

#[test]
fn socket_dir_is_0700_and_loose_own_dir_is_tightened() {
    use std::os::unix::fs::PermissionsExt;
    let base = temp_cwd();
    let d = base.join("ade");
    ensure_private_dir(&d).unwrap();
    assert_eq!(
        fs::metadata(&d).unwrap().permissions().mode() & 0o777,
        0o700
    );
    // 已存在且 0700：通過
    ensure_private_dir(&d).unwrap();
    let loose = base.join("loose");
    fs::create_dir(&loose).unwrap();
    fs::set_permissions(&loose, fs::Permissions::from_mode(0o777)).unwrap();
    ensure_private_dir(&loose).unwrap();
    assert_eq!(
        fs::metadata(&loose).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn wrong_token_tcp_connection_is_closed() {
    use std::io::{BufRead, Write};
    let shared = Shared::restore(
        Path::new("/dev/null"),
        PolicyEngine::default(),
        std::env::temp_dir(),
        Some("right".into()),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let _ = handle_conn(shared, ConnStream::Tcp(s), true);
    });
    let mut c = TcpStream::connect(addr).unwrap();
    let hello = Request::Hello {
        client: "t".into(),
        since: None,
        token: Some("wrong".into()),
        epoch: None,
    };
    c.write_all(encode_line(&hello).unwrap().as_bytes())
        .unwrap();
    let mut r = BufReader::new(c.try_clone().unwrap());
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    assert!(line.contains("認證失敗"));
    // 之後的請求不被處理：連線已關閉，讀到 EOF
    let _ = c.write_all(encode_line(&Request::Snapshot).unwrap().as_bytes());
    line.clear();
    assert_eq!(r.read_line(&mut line).unwrap_or(0), 0);
}

#[test]
fn paths_are_under_expected_dirs() {
    assert!(socket_path().ends_with("daemon.sock"));
    assert!(log_path().ends_with("events.jsonl"));
}

// -----------------------------------------------------------------------
// 喚醒行為測試（廣播 / 直接訊息 / 任務指派）
// -----------------------------------------------------------------------

#[derive(Clone, Default)]
struct MockCtl {
    inputs: Arc<Mutex<Vec<String>>>,
    wakes: Arc<Mutex<usize>>,
}

impl MockCtl {
    fn as_ctl(&self) -> Arc<dyn AgentControl> {
        Arc::new(self.clone())
    }
    fn inputs(&self) -> Vec<String> {
        self.inputs.lock().unwrap().clone()
    }
    fn wake_count(&self) -> usize {
        *self.wakes.lock().unwrap()
    }
}

impl AgentControl for MockCtl {
    fn send_input(&self, text: &str) -> Result<()> {
        self.inputs.lock().unwrap().push(text.to_string());
        Ok(())
    }
    fn cancel(&self) -> Result<()> {
        Ok(())
    }
    fn kill(&self) -> Result<()> {
        Ok(())
    }
    fn wake(&self) -> Result<()> {
        *self.wakes.lock().unwrap() += 1;
        Ok(())
    }
    fn resolve_permission(&self, _req: &str, _decision: Decision) -> Result<()> {
        Ok(())
    }
    fn transport(&self) -> &'static str {
        "mock"
    }
}

fn test_shared() -> Arc<Shared> {
    Shared::restore(
        Path::new("/dev/null"),
        PolicyEngine::default(),
        std::env::temp_dir(),
        None,
    )
    .expect("建立測試 Shared")
}

fn temp_cwd() -> PathBuf {
    let dir = std::env::temp_dir().join(new_id("ade-test"));
    fs::create_dir_all(&dir).expect("建立測試目錄");
    dir
}

fn add_session(
    shared: &Arc<Shared>,
    id: &str,
    label: &str,
    status: SessionStatus,
    ctl: Arc<dyn AgentControl>,
    cwd: &Path,
) {
    let mut inner = shared.inner.lock().unwrap();
    inner.state.sessions.insert(
        id.to_string(),
        Session {
            id: id.to_string(),
            kind: AgentKind::OpenCode,
            label: label.to_string(),
            cwd: cwd.to_string_lossy().to_string(),
            branch: None,
            role: Role::Worker,
            status,
            model: None,
            cost: Cost::default(),
            parent: None,
            created_ms: now_millis(),
            transport: "mock".to_string(),
            status_note: None,
            repo_root: None,
            permission: Default::default(),
        },
    );
    inner.sessions.insert(id.to_string(), SessionEntry { ctl });
}

#[test]
fn broadcast_wakes_idle_and_sleeping_recipients_only() {
    let shared = test_shared();
    let dir = temp_cwd();
    let sender = MockCtl::default();
    let idle = MockCtl::default();
    let sleeping = MockCtl::default();
    let busy = MockCtl::default();

    add_session(
        &shared,
        "ses_sender",
        "worker/sender",
        SessionStatus::Idle,
        sender.as_ctl(),
        &dir,
    );
    add_session(
        &shared,
        "ses_idle",
        "worker/idle",
        SessionStatus::Idle,
        idle.as_ctl(),
        &dir,
    );
    add_session(
        &shared,
        "ses_sleep",
        "worker/sleep",
        SessionStatus::Sleeping,
        sleeping.as_ctl(),
        &dir,
    );
    add_session(
        &shared,
        "ses_busy",
        "worker/busy",
        SessionStatus::Busy,
        busy.as_ctl(),
        &dir,
    );

    shared
        .deliver_message(
            Some("ses_sender".into()),
            Target::Broadcast,
            MsgKind::Chat,
            "請大家回報狀態".into(),
            None,
        )
        .expect("廣播應成功");

    assert!(sender.inputs().is_empty(), "發送者不該被自己喚醒");
    assert_eq!(idle.inputs().len(), 1, "idle 收件者應被喚醒");
    assert!(idle.inputs()[0].contains("請大家回報狀態"));
    assert_eq!(sleeping.inputs().len(), 1, "sleeping 收件者應被喚醒");
    assert_eq!(sleeping.wake_count(), 1, "sleeping 應先 SIGCONT 再送輸入");
    assert!(busy.inputs().is_empty(), "busy 收件者不該被打斷");
}

#[test]
fn artifact_broadcast_does_not_wake_anyone() {
    let shared = test_shared();
    let dir = temp_cwd();
    let idle = MockCtl::default();
    add_session(
        &shared,
        "ses_idle",
        "worker/idle",
        SessionStatus::Idle,
        idle.as_ctl(),
        &dir,
    );

    shared
        .deliver_message(
            Some("human".into()),
            Target::Broadcast,
            MsgKind::Artifact,
            "分享了一個檔案".into(),
            None,
        )
        .expect("artifact 廣播應成功");

    assert!(idle.inputs().is_empty(), "artifact 只進收件匣，不主動喚醒");
}

#[test]
fn direct_chat_wakes_idle_target() {
    let shared = test_shared();
    let dir = temp_cwd();
    let target = MockCtl::default();
    add_session(
        &shared,
        "ses_target",
        "worker/target",
        SessionStatus::Idle,
        target.as_ctl(),
        &dir,
    );

    shared
        .deliver_message(
            Some("human".into()),
            Target::Direct("worker/target".into()),
            MsgKind::Chat,
            "請看一下這則訊息".into(),
            None,
        )
        .expect("直接訊息應成功");

    assert_eq!(target.inputs().len(), 1, "直接訊息應喚醒 idle 對象");
}

#[test]
fn task_assignment_wakes_idle_assignee() {
    let shared = test_shared();
    let dir = temp_cwd();
    let assignee = MockCtl::default();
    add_session(
        &shared,
        "ses_assignee",
        "worker/assignee",
        SessionStatus::Idle,
        assignee.as_ctl(),
        &dir,
    );

    let (tx, _rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::TaskCreate {
            title: "跑測試".into(),
            detail: "跑完整測試並回報".into(),
            assignee: Some("worker/assignee".into()),
            created_by: Some("human".into()),
            depends_on: vec![],
        },
    );

    let inputs = assignee.inputs();
    assert_eq!(inputs.len(), 1, "指派任務應喚醒負責人");
    assert!(inputs[0].contains("任務指派"));
    assert!(inputs[0].contains("跑測試"));

    let inbox = shared
        .inner
        .lock()
        .unwrap()
        .state
        .inbox_for("ses_assignee", None);
    assert_eq!(inbox.len(), 1, "任務通知應進負責人的收件匣");
    assert_eq!(inbox[0].kind, MsgKind::Handoff);
}

#[test]
fn bus_guard_detects_loop_and_rate_limit() {
    let mut guard = BusGuard::default();
    let target = Target::Direct("ses_b".into());
    for i in 0..LOOP_REPEAT_CAP {
        assert!(
            guard
                .check("ses_a", &target, "ping", 1000 + i as u64)
                .is_ok(),
            "repeat {i} should pass"
        );
    }
    let err = guard
        .check("ses_a", &target, "ping", 2000)
        .unwrap_err()
        .to_string();
    assert!(err.contains("迴圈偵測"));

    let mut guard2 = BusGuard::default();
    for i in 0..PAIR_RATE_CAP_PER_MIN {
        assert!(guard2
            .check("ses_a", &target, &format!("msg-{i}"), 1000 + i as u64)
            .is_ok());
    }
    let err2 = guard2
        .check("ses_a", &target, "overflow", 2000)
        .unwrap_err()
        .to_string();
    assert!(err2.contains("速率限制"));
}

// ---- git 輔助函式（T02／T03）----

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn temp_repo(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ade-git-{tag}-{}", new_id("t")));
    fs::create_dir_all(&d).unwrap();
    git(&d, &["init", "-q", "-b", "main"]);
    fs::write(d.join("a.txt"), "base\n").unwrap();
    git(&d, &["add", "."]);
    git(&d, &["commit", "-qm", "init"]);
    d
}

fn wt_session(repo: &Path, id: &str) -> Session {
    let (dir, branch) = make_worktree(repo, id).unwrap();
    Session {
        id: id.into(),
        kind: AgentKind::ClaudeCode,
        label: format!("w/{id}"),
        cwd: dir.to_string_lossy().to_string(),
        branch: Some(branch),
        role: Role::Worker,
        status: SessionStatus::Exited,
        model: None,
        cost: Default::default(),
        parent: None,
        created_ms: now_millis(),
        transport: String::new(),
        status_note: None,
        repo_root: Some(repo.to_string_lossy().to_string()),
        permission: Default::default(),
    }
}

#[test]
fn merge_clean_succeeds_and_excludes_ade_dir() {
    let repo = temp_repo("m1");
    let s = wt_session(&repo, "s1");
    let wt = Path::new(&s.cwd);
    fs::write(wt.join("new.txt"), "x").unwrap();
    fs::create_dir_all(wt.join("sub.ade")).unwrap();
    fs::write(wt.join("sub.ade/f.txt"), "f").unwrap();
    fs::create_dir_all(wt.join(".ade/mailbox")).unwrap();
    fs::write(wt.join(".ade/mailbox/m"), "m").unwrap();
    merge_worktree(&repo, &s, false).unwrap();
    assert!(repo.join("new.txt").exists());
    assert!(repo.join("sub.ade/f.txt").exists());
    assert!(!repo.join(".ade/mailbox/m").exists());
    assert!(!has_uncommitted(&repo).unwrap());
}

#[test]
fn merge_dry_run_reports_files_and_preserves_workspace() {
    let repo = temp_repo("m_dry");
    let s = wt_session(&repo, "s_dry");
    let wt = Path::new(&s.cwd);
    fs::write(wt.join("dry_file.txt"), "content").unwrap();
    let res = merge_worktree(&repo, &s, true).unwrap();
    assert!(res.contains("[dry-run]"), "{res}");
    assert!(res.contains("dry_file.txt"), "{res}");
    assert!(res.contains("可快轉"), "{res}");
    // 主工作區不應有變更
    assert!(!repo.join("dry_file.txt").exists());
    // worktree 內檔案尚未被 commit
    assert!(has_uncommitted(wt).unwrap());
}

#[test]
fn merge_conflict_is_aborted() {
    let repo = temp_repo("m2");
    let s = wt_session(&repo, "s2");
    fs::write(Path::new(&s.cwd).join("a.txt"), "branch\n").unwrap();
    fs::write(repo.join("a.txt"), "main\n").unwrap();
    git(&repo, &["commit", "-qam", "main change"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let err = merge_worktree(&repo, &s, false).unwrap_err().to_string();
    assert!(err.contains("a.txt"), "{err}");
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
    assert!(!repo.join(".git/MERGE_HEAD").exists());
    assert!(!has_uncommitted(&repo).unwrap());
}

#[test]
fn merge_refuses_dirty_main() {
    let repo = temp_repo("m3");
    let s = wt_session(&repo, "s3");
    fs::write(Path::new(&s.cwd).join("n.txt"), "n").unwrap();
    fs::write(repo.join("a.txt"), "dirty\n").unwrap();
    assert!(merge_worktree(&repo, &s, false).is_err());
    assert_eq!(fs::read_to_string(repo.join("a.txt")).unwrap(), "dirty\n");
    assert!(!repo.join("n.txt").exists());
}

#[test]
fn merge_targets_each_sessions_own_repo() {
    let r1 = temp_repo("m4a");
    let r2 = temp_repo("m4b");
    let s1 = wt_session(&r1, "s4a");
    let s2 = wt_session(&r2, "s4b");
    fs::write(Path::new(&s1.cwd).join("one.txt"), "1").unwrap();
    fs::write(Path::new(&s2.cwd).join("two.txt"), "2").unwrap();
    merge_worktree(&session_repo(&r1, &s1), &s1, false).unwrap();
    merge_worktree(&session_repo(&r1, &s2), &s2, false).unwrap();
    assert!(r1.join("one.txt").exists() && !r1.join("two.txt").exists());
    assert!(r2.join("two.txt").exists() && !r2.join("one.txt").exists());
}

#[test]
fn gc_protects_unmerged_and_dirty() {
    let repo = temp_repo("g1");
    // 已合併且乾淨
    let merged = wt_session(&repo, "gm");
    assert!(gc_block_reason(&repo, &merged).is_none());
    remove_worktree(&repo, &merged, false).unwrap();
    assert!(!Path::new(&merged.cwd).exists());
    // 未合併 commit
    let unmerged = wt_session(&repo, "gu");
    let wt = Path::new(&unmerged.cwd);
    fs::write(wt.join("u.txt"), "u").unwrap();
    git(wt, &["add", "u.txt"]);
    git(wt, &["commit", "-qm", "u"]);
    assert!(gc_block_reason(&repo, &unmerged)
        .unwrap()
        .contains("未合併"));
    assert!(remove_worktree(&repo, &unmerged, false).is_err());
    // 未 commit
    let dirty = wt_session(&repo, "gd");
    fs::write(Path::new(&dirty.cwd).join("d.txt"), "d").unwrap();
    assert!(gc_block_reason(&repo, &dirty)
        .unwrap()
        .contains("未 commit"));
    // force 全部移除
    remove_worktree(&repo, &unmerged, true).unwrap();
    remove_worktree(&repo, &dirty, true).unwrap();
    assert!(!Path::new(&unmerged.cwd).exists() && !Path::new(&dirty.cwd).exists());
}

#[test]
fn ade_status_line_matches_only_ade_dir() {
    assert!(is_ade_status_line("?? .ade/wt/x"));
    assert!(!is_ade_status_line("?? sub.ade/f"));
    assert!(!is_ade_status_line(" M src/.ade/f"));
}

// ---- 重啟還原（T01）----

fn restart_session(id: &str, status: SessionStatus) -> Session {
    Session {
        id: id.into(),
        kind: AgentKind::ClaudeCode,
        label: id.into(),
        cwd: "/tmp".into(),
        branch: None,
        role: Role::Worker,
        status,
        model: None,
        cost: Default::default(),
        parent: None,
        created_ms: now_millis(),
        transport: String::new(),
        status_note: None,
        repo_root: None,
        permission: Default::default(),
    }
}

#[test]
fn restart_restores_state_and_continues_seq() {
    let log = std::env::temp_dir().join(format!("{}.jsonl", new_id("ade-log")));
    let first = Shared::restore(&log, PolicyEngine::default(), std::env::temp_dir(), None).unwrap();
    first.emit(Event::SessionStarted {
        session: restart_session("ses-live", SessionStatus::Idle),
    });
    first.emit(Event::SessionStarted {
        session: restart_session("ses-done", SessionStatus::Exited),
    });
    let n = first.inner.lock().unwrap().seq;
    first.flush_log();
    drop(first);

    let second =
        Shared::restore(&log, PolicyEngine::default(), std::env::temp_dir(), None).unwrap();
    let inner = second.inner.lock().unwrap();
    assert!(inner.state.sessions.contains_key("ses-live"));
    assert!(inner.state.sessions.contains_key("ses-done"));
    // 原本 live 的 session 被收斂為已結束（多一筆事件）。
    assert!(!inner.state.sessions["ses-live"].status.is_live());
    assert_eq!(inner.seq, n + 1);
    drop(inner);
    second.emit(Event::SessionStatus {
        session: "ses-done".into(),
        status: SessionStatus::Exited,
    });
    second.flush_log();
    drop(second);

    let (_, recs) = replay_log(&log).unwrap();
    assert!(recs.windows(2).all(|w| w[1].seq == w[0].seq + 1));
    assert_eq!(recs.len() as u64, n + 2);
}

#[test]
fn hello_with_wrong_epoch_gets_snapshot() {
    let shared = test_shared();
    shared.emit(Event::SessionStarted {
        session: restart_session("ses-a", SessionStatus::Idle),
    });
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::Hello {
            client: "t".into(),
            since: Some(1),
            token: None,
            epoch: Some("old".into()),
        },
    );
    let frames: Vec<Frame> = rx.try_iter().collect();
    assert!(frames.iter().any(|f| matches!(f, Frame::Snapshot { .. })));
    let (tx2, rx2) = client_channel(None);
    shared.handle(
        &tx2,
        Request::Hello {
            client: "t".into(),
            since: Some(1),
            token: None,
            epoch: Some(shared.epoch.clone()),
        },
    );
    assert!(!rx2.try_iter().any(|f| matches!(f, Frame::Snapshot { .. })));
}

// ---- 審批決策（T04）----

#[test]
fn permission_decision_order() {
    let p = PolicyEngine::default();
    let deny = r#"{"command":"git push --force origin x"}"#;
    // auto 也要被 deny 擋下
    assert_eq!(
        decide_permission(&p, PermissionPolicy::Auto, "bash", deny),
        Some(Decision::Deny)
    );
    // auto 未命中任何規則 → 自動允許
    assert_eq!(
        decide_permission(&p, PermissionPolicy::Auto, "bash", r#"{"command":"make"}"#),
        Some(Decision::Allow)
    );
    // ask 且 `cat README.md` 不會被 `Read` 放行 → 交給人
    assert_eq!(
        decide_permission(
            &p,
            PermissionPolicy::Ask,
            "cat README.md",
            r#"{"command":"cat README.md"}"#
        ),
        None
    );
}

#[test]
fn repo_policy_adds_deny_but_cannot_remove() {
    let dir = temp_cwd();
    fs::create_dir_all(dir.join(".ade")).unwrap();
    fs::write(
        dir.join(".ade/policy.toml"),
        "deny = [\"curl\"]\nallow = [\"make\"]\n",
    )
    .unwrap();
    let merged = PolicyEngine::default().merged_with(&load_local_policy(&dir));
    assert!(merged.deny.iter().any(|d| d == "curl"));
    assert!(merged.deny.iter().any(|d| d == "git push --force"));
}

// ---- 併發與 log 輪替（T07 / T08）----

fn out_event(i: usize) -> Event {
    Event::Output {
        session: "ses-a".into(),
        text: format!("line {i} {}", "x".repeat(60)),
    }
}

#[test]
fn stalled_client_is_dropped_others_unaffected() {
    let shared = test_shared();
    let (slow_tx, _slow_rx) = client_channel(None);
    let (good_tx, good_rx) = client_channel(None);
    {
        let mut inner = lock(&shared.inner);
        inner.clients.push(slow_tx);
        inner.clients.push(good_tx);
    }
    let total = CLIENT_QUEUE_CAP + 50;
    let mut got = 0;
    for i in 0..total {
        shared.emit(out_event(i));
        got += good_rx.try_iter().count();
    }
    assert_eq!(got, total);
    assert_eq!(lock(&shared.inner).clients.len(), 1);
}

#[test]
fn snapshot_works_after_lock_poisoned() {
    let shared = test_shared();
    let s2 = shared.clone();
    let r = std::thread::spawn(move || {
        let _g = s2.inner.lock().unwrap();
        panic!("故意中毒");
    })
    .join();
    assert!(r.is_err());
    let _ = shared.snapshot();
    shared.emit(out_event(0));
}

#[test]
fn flush_persists_last_event() {
    let log = std::env::temp_dir().join(format!("{}.jsonl", new_id("ade-log")));
    let shared =
        Shared::restore(&log, PolicyEngine::default(), std::env::temp_dir(), None).unwrap();
    shared.emit(out_event(1));
    shared.emit(out_event(2));
    shared.flush_log();
    let (_, recs) = replay_log(&log).unwrap();
    assert_eq!(recs.len(), 2);
    assert!(matches!(&recs[1].event, Event::Output { text, .. } if text.starts_with("line 2")));
}

#[test]
fn log_rotates_with_snapshot_and_continuous_seq() {
    let dir = temp_cwd();
    let log = dir.join("events.jsonl");
    let cfg = LogConfig {
        max_bytes: 2000,
        keep: 2,
    };
    let shared = Shared::restore_with(
        &log,
        PolicyEngine::default(),
        std::env::temp_dir(),
        None,
        cfg,
    )
    .unwrap();
    shared.emit(Event::SessionStarted {
        session: restart_session("ses-a", SessionStatus::Idle),
    });
    for i in 0..60 {
        shared.emit(out_event(i));
    }
    shared.flush_log();
    assert!(rotated_log_path(&log, 1).is_file());
    let first_line = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    let first: Record = serde_json::from_str(&first_line).unwrap();
    assert!(matches!(first.event, Event::Snapshot { .. }));

    // 只用目前這份重播，狀態與記憶體中的一致。
    let (state, _) = replay_log(&log).unwrap();
    let live = shared.snapshot();
    assert_eq!(
        serde_json::to_value(&state).unwrap(),
        serde_json::to_value(&live).unwrap()
    );

    // seq 跨檔案連續（排除 snapshot 紀錄本身，它沿用前一筆的 seq）。
    let (_, recs) = replay_log_all(&log).unwrap();
    let real: Vec<u64> = recs
        .iter()
        .filter(|r| !matches!(r.event, Event::Snapshot { .. }))
        .map(|r| r.seq)
        .collect();
    assert!(real.windows(2).all(|w| w[1] == w[0] + 1));
    assert_eq!(*real.last().unwrap(), live.seq);
}

#[test]
fn unwritable_log_does_not_stop_daemon() {
    let log = std::env::temp_dir()
        .join(new_id("ade-nodir"))
        .join("events.jsonl");
    let shared =
        Shared::restore(&log, PolicyEngine::default(), std::env::temp_dir(), None).unwrap();
    shared.emit(out_event(1));
    shared.flush_log();
    assert!(!shared.log_healthy());
    shared.emit(out_event(2));
    assert_eq!(shared.snapshot().seq, 2);
}

// ---- MCP 審批轉發（Claude --permission-prompt-tool）----

fn ask_sess(id: &str, perm: PermissionPolicy) -> Session {
    let mut s = restart_session(id, SessionStatus::Idle);
    s.permission = perm;
    s
}

#[test]
fn approval_ask_auto_policy_answers_immediately_and_deny_wins() {
    let shared = test_shared();
    shared.emit(Event::SessionStarted {
        session: ask_sess("ses-auto", PermissionPolicy::Auto),
    });
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::ApprovalAsk {
            session: "ses-auto".into(),
            tool: "Bash".into(),
            input: r#"{"command":"make"}"#.into(),
        },
    );
    assert!(matches!(rx.try_iter().next(), Some(Frame::Ok { note: Some(n) }) if n == "allow"));
    shared.handle(
        &tx,
        Request::ApprovalAsk {
            session: "ses-auto".into(),
            tool: "Bash".into(),
            input: r#"{"command":"git push --force"}"#.into(),
        },
    );
    assert!(matches!(rx.try_iter().next(), Some(Frame::Ok { note: Some(n) }) if n == "deny"));
}

#[test]
fn approval_ask_waits_for_human_decision() {
    let shared = test_shared();
    shared.emit(Event::SessionStarted {
        session: ask_sess("ses-ask", PermissionPolicy::Ask),
    });
    let s2 = shared.clone();
    let h = std::thread::spawn(move || {
        let (tx, rx) = client_channel(None);
        s2.handle(
            &tx,
            Request::ApprovalAsk {
                session: "ses-ask".into(),
                tool: "Bash".into(),
                input: r#"{"command":"make"}"#.into(),
            },
        );
        let r = rx.try_iter().next();
        r
    });
    // 等審批請求出現在狀態裡，再由「人」回覆
    let req = loop {
        if let Some(r) = lock(&shared.inner).state.pending_permissions.keys().next() {
            break r.clone();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let (tx, _rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::Permission {
            session: "ses-ask".into(),
            req,
            decision: Decision::Deny,
        },
    );
    let reply = h.join().unwrap();
    assert!(matches!(reply, Some(Frame::Ok { note: Some(n) }) if n == "deny"));
    assert!(lock(&shared.inner).state.pending_permissions.is_empty());
}

#[test]
fn restore_prunes_old_ended_sessions_but_keeps_branches_and_recent() {
    let log = std::env::temp_dir().join(format!("{}.jsonl", new_id("ade-prune")));
    let first = Shared::restore(&log, PolicyEngine::default(), std::env::temp_dir(), None).unwrap();
    let now = now_millis();
    let day = 24 * 3600 * 1000;
    let mk = |id: &str, age: u64, branch: Option<&str>| {
        let mut s = restart_session(id, SessionStatus::Exited);
        s.created_ms = now - age;
        s.branch = branch.map(str::to_string);
        s
    };
    for s in [
        mk("ses-old", 3 * day, None),
        mk("ses-old-branch", 3 * day, Some("ade/x")),
        mk("ses-recent", 1000, None),
    ] {
        first.emit(Event::SessionStarted { session: s });
    }
    first.flush_log();
    drop(first);
    let second =
        Shared::restore(&log, PolicyEngine::default(), std::env::temp_dir(), None).unwrap();
    let inner = lock(&second.inner);
    assert!(!inner.state.sessions.contains_key("ses-old"));
    assert!(inner.state.sessions.contains_key("ses-old-branch"));
    assert!(inner.state.sessions.contains_key("ses-recent"));
}
