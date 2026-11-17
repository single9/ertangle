use super::*;
use crate::git::*;
use termade_core::{AgentKind, Cost, Decision, Role, TaskStatus};

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
fn listen_requires_token_and_loopback_or_tailscale() {
    // 必須有 token
    assert!(validate_listen("127.0.0.1:0", false, false).is_err());
    assert!(validate_listen("127.0.0.1:0", true, false).is_ok());

    // Tailscale IPv4 CGNAT (100.64.0.0/10) 不需 --insecure-listen-public
    assert!(validate_listen("100.64.0.1:7420", true, false).is_ok());
    assert!(validate_listen("100.127.255.255:7420", true, false).is_ok());

    // Tailscale IPv6 ULA (fd7a:115c:a1e0::/48) 不需 --insecure-listen-public
    assert!(validate_listen("[fd7a:115c:a1e0::1]:7420", true, false).is_ok());

    // 一般非 loopback / 非 Tailscale 位址需要 --insecure-listen-public
    assert!(validate_listen("0.0.0.0:7420", true, false).is_err());
    assert!(validate_listen("0.0.0.0:7420", true, true).is_ok());
    assert!(validate_listen("192.168.1.10:7420", true, false).is_err());
    assert!(validate_listen("192.168.1.10:7420", true, true).is_ok());
    assert!(validate_listen("100.128.0.1:7420", true, false).is_err());
}

#[test]
fn resolve_listen_addr_tailscale_and_standard() {
    std::env::set_var("TERMADE_TEST_TAILSCALE_IP", "100.88.99.100");
    let (addr, is_ts) = resolve_listen_addr("tailscale").unwrap();
    assert_eq!(addr, "100.88.99.100:7420");
    assert!(is_ts);

    let (addr, is_ts) = resolve_listen_addr("tailscale:8888").unwrap();
    assert_eq!(addr, "100.88.99.100:8888");
    assert!(is_ts);

    let (addr, is_ts) = resolve_listen_addr("127.0.0.1:7420").unwrap();
    assert_eq!(addr, "127.0.0.1:7420");
    assert!(!is_ts);

    let (addr, is_ts) = resolve_listen_addr("100.64.1.2:7420").unwrap();
    assert_eq!(addr, "100.64.1.2:7420");
    assert!(is_ts);
}

#[test]
fn run_remote_auto_creates_token_and_persists() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_cwd();
    std::env::set_var("XDG_STATE_HOME", &dir);
    std::env::remove_var("TERMADE_TOKEN");

    let opts = RemoteOptions {
        listen: Some("127.0.0.1:0".into()),
        ..Default::default()
    };
    let token = resolve_token(&opts, &crate::config::Config::default())
        .unwrap()
        .expect("應自動產生 token");
    let token_path = dir.join("termade").join("remote.token");
    assert!(token_path.exists(), "應建立 remote.token 檔案");
    let meta = fs::metadata(&token_path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o077, 0, "權限須為 0600");
    assert_eq!(read_token_file(&token_path).unwrap(), token);
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
    let d = base.join("termade");
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
// 喚醒行為測試 (廣播 / 直接訊息 / 任務指派)
// -----------------------------------------------------------------------

#[derive(Clone, Default)]
struct MockCtl {
    inputs: Arc<Mutex<Vec<String>>>,
    wakes: Arc<Mutex<usize>>,
    cancels: Arc<Mutex<usize>>,
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
    fn cancel_count(&self) -> usize {
        *self.cancels.lock().unwrap()
    }
}

impl AgentControl for MockCtl {
    fn send_input(&self, text: &str) -> Result<()> {
        self.inputs.lock().unwrap().push(text.to_string());
        Ok(())
    }
    fn cancel(&self) -> Result<()> {
        *self.cancels.lock().unwrap() += 1;
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
    fn pid(&self) -> Option<u32> {
        Some(std::process::id())
    }
}

/// 測試不能讀到使用者真實的 `~/.config/termade/config.toml`：把設定目錄指到空的暫存目錄。
fn isolate_global_config() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("termade-test-xdg-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        std::env::set_var("XDG_CONFIG_HOME", &dir);
    });
}

fn test_shared() -> Arc<Shared> {
    isolate_global_config();
    Shared::restore(
        Path::new("/dev/null"),
        PolicyEngine::default(),
        std::env::temp_dir(),
        None,
    )
    .expect("建立測試 Shared")
}

fn temp_cwd() -> PathBuf {
    let dir = std::env::temp_dir().join(new_id("termade-test"));
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
            context: None,
            native_id: None,
            permission: Default::default(),
            budget_usd: None,
            over_budget: false,
            handed_off_to: None,
            effort: None,
            slash_commands: Vec::new(),
            active_tools: Vec::new(),
            bg_procs: Vec::new(),
        },
    );
    inner.sessions.insert(
        id.to_string(),
        SessionEntry {
            ctl,
            run: new_id("run"),
        },
    );
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

fn mark_handed_off(shared: &Arc<Shared>, from: &str, to: &str) {
    shared.emit(Event::SessionExited {
        session: from.into(),
        code: None,
    });
    shared.emit(Event::SessionHandedOff {
        from: from.into(),
        to: to.into(),
    });
}

#[test]
fn direct_message_to_handed_off_session_goes_to_successor() {
    let shared = test_shared();
    let dir = temp_cwd();
    let old = MockCtl::default();
    let new = MockCtl::default();
    add_session(
        &shared,
        "ses_a",
        "t17",
        SessionStatus::Idle,
        old.as_ctl(),
        &dir,
    );
    add_session(
        &shared,
        "ses_b",
        "t17→claude",
        SessionStatus::Idle,
        new.as_ctl(),
        &dir,
    );
    mark_handed_off(&shared, "ses_a", "ses_b");

    let (_, notes) = shared
        .deliver_message_noted(
            Some("human".into()),
            Target::Direct("t17".into()),
            MsgKind::Chat,
            "提案".into(),
            None,
        )
        .unwrap();
    assert!(notes.iter().any(|n| n.contains("改投")), "{notes:?}");
    assert_eq!(new.inputs().len(), 1, "接手者應被喚醒");
    assert!(new.inputs()[0].contains("提案") && new.inputs()[0].contains("原收件者 t17"));
    assert!(old.inputs().is_empty());
    let inner = lock(&shared.inner);
    assert_eq!(inner.state.inbox_for("ses_b", None).len(), 1);
    assert!(
        inner.state.inbox_for("ses_a", None).is_empty(),
        "舊收件匣不該有新訊息"
    );
}

#[test]
fn direct_message_follows_handoff_chain() {
    let shared = test_shared();
    let dir = temp_cwd();
    let c = MockCtl::default();
    add_session(
        &shared,
        "ses_a",
        "t17",
        SessionStatus::Idle,
        MockCtl::default().as_ctl(),
        &dir,
    );
    add_session(
        &shared,
        "ses_b",
        "t17→x",
        SessionStatus::Idle,
        MockCtl::default().as_ctl(),
        &dir,
    );
    add_session(
        &shared,
        "ses_c",
        "t17→x→y",
        SessionStatus::Idle,
        c.as_ctl(),
        &dir,
    );
    mark_handed_off(&shared, "ses_a", "ses_b");
    mark_handed_off(&shared, "ses_b", "ses_c");

    shared
        .deliver_message(
            Some("human".into()),
            Target::Direct("ses_a".into()),
            MsgKind::Chat,
            "hi".into(),
            None,
        )
        .unwrap();
    assert_eq!(c.inputs().len(), 1);
}

#[test]
fn handoff_cycle_does_not_loop_and_warns_unreadable() {
    let shared = test_shared();
    let dir = temp_cwd();
    add_session(
        &shared,
        "ses_a",
        "a",
        SessionStatus::Idle,
        MockCtl::default().as_ctl(),
        &dir,
    );
    add_session(
        &shared,
        "ses_b",
        "b",
        SessionStatus::Idle,
        MockCtl::default().as_ctl(),
        &dir,
    );
    mark_handed_off(&shared, "ses_a", "ses_b");
    mark_handed_off(&shared, "ses_b", "ses_a");
    let (_, notes) = shared
        .deliver_message_noted(
            Some("human".into()),
            Target::Direct("ses_a".into()),
            MsgKind::Chat,
            "hi".into(),
            None,
        )
        .unwrap();
    assert!(notes.iter().any(|n| n.contains("不會被讀取")), "{notes:?}");
}

#[test]
fn message_to_ended_session_without_successor_is_logged_with_warning() {
    let shared = test_shared();
    let dir = temp_cwd();
    add_session(
        &shared,
        "ses_a",
        "a",
        SessionStatus::Idle,
        MockCtl::default().as_ctl(),
        &dir,
    );
    shared.emit(Event::SessionExited {
        session: "ses_a".into(),
        code: None,
    });
    let (id, notes) = shared
        .deliver_message_noted(
            Some("human".into()),
            Target::Direct("a".into()),
            MsgKind::Chat,
            "hi".into(),
            None,
        )
        .unwrap();
    assert!(notes.iter().any(|n| n.contains("不會被讀取")), "{notes:?}");
    let inner = lock(&shared.inner);
    assert!(inner.state.messages.iter().any(|m| m.id == id));
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

// ---- T18：任務相依性解鎖 ----

/// 建立任務並回傳 (回覆 frame, 任務 id)。任務 id 取狀態裡最新建立的那個。
fn create_task(
    shared: &Arc<Shared>,
    title: &str,
    assignee: Option<&str>,
    depends_on: &[&str],
) -> (Frame, Option<String>) {
    let before: HashSet<String> = lock(&shared.inner).state.tasks.keys().cloned().collect();
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::TaskCreate {
            title: title.into(),
            detail: String::new(),
            assignee: assignee.map(|a| a.to_string()),
            created_by: Some("human".into()),
            depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
        },
    );
    let frame = rx.rx.try_recv().expect("建立任務應有回覆");
    let id = lock(&shared.inner)
        .state
        .tasks
        .keys()
        .find(|k| !before.contains(*k))
        .cloned();
    (frame, id)
}

fn set_task_status(shared: &Arc<Shared>, id: &str, status: TaskStatus) {
    let (tx, _rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::TaskUpdate {
            id: id.into(),
            status: Some(status),
            assignee: None,
            detail: None,
        },
    );
}

fn task_status(shared: &Arc<Shared>, id: &str) -> TaskStatus {
    lock(&shared.inner).state.tasks[id].status
}

#[test]
fn task_with_unfinished_dependency_is_blocked_and_silent() {
    let shared = test_shared();
    let dir = temp_cwd();
    let worker = MockCtl::default();
    add_session(
        &shared,
        "ses_dep_w",
        "worker/dep",
        SessionStatus::Idle,
        worker.as_ctl(),
        &dir,
    );

    let (_, first) = create_task(&shared, "前置", None, &[]);
    let first = first.unwrap();
    let (frame, second) = create_task(&shared, "後續", Some("worker/dep"), &[&first]);
    let second = second.unwrap();

    assert_eq!(task_status(&shared, &second), TaskStatus::Blocked);
    assert!(worker.inputs().is_empty(), "相依性未完成時不該通知負責人");
    assert!(lock(&shared.inner)
        .state
        .inbox_for("ses_dep_w", None)
        .is_empty());
    let Frame::Ok { note: Some(note) } = frame else {
        panic!("應回 ok：{frame:?}");
    };
    assert!(note.contains(&first), "回覆要註明在等哪個任務：{note}");
}

#[test]
fn finishing_dependency_unblocks_and_wakes_assignee() {
    let shared = test_shared();
    let dir = temp_cwd();
    let first_owner = MockCtl::default();
    let worker = MockCtl::default();
    add_session(
        &shared,
        "ses_first",
        "worker/first",
        SessionStatus::Busy,
        first_owner.as_ctl(),
        &dir,
    );
    add_session(
        &shared,
        "ses_next",
        "worker/next",
        SessionStatus::Idle,
        worker.as_ctl(),
        &dir,
    );

    let (_, first) = create_task(&shared, "設計 API", Some("worker/first"), &[]);
    let first = first.unwrap();
    let (_, second) = create_task(&shared, "實作 client", Some("worker/next"), &[&first]);
    let second = second.unwrap();
    assert!(worker.inputs().is_empty());

    set_task_status(&shared, &first, TaskStatus::Doing);
    assert_eq!(task_status(&shared, &second), TaskStatus::Blocked);
    assert!(worker.inputs().is_empty(), "前置只是 doing，還不能解鎖");

    set_task_status(&shared, &first, TaskStatus::Done);
    assert_eq!(task_status(&shared, &second), TaskStatus::Todo);

    let inbox = lock(&shared.inner).state.inbox_for("ses_next", None);
    assert_eq!(inbox.len(), 1, "解鎖後剛好一則指派訊息");
    assert_eq!(inbox[0].kind, MsgKind::Handoff);
    assert!(inbox[0].body.contains("實作 client"));
    assert!(inbox[0].body.contains("設計 API"), "附上前置任務標題");
    assert!(
        inbox[0].body.contains("worker/first"),
        "附上前置負責人 label"
    );

    let inputs = worker.inputs();
    assert_eq!(inputs.len(), 1, "閒置中的負責人應被喚醒一次");
    assert!(inputs[0].contains("任務指派"));

    // 前置任務重複標 done 不會再通知一次。
    set_task_status(&shared, &first, TaskStatus::Done);
    assert_eq!(worker.inputs().len(), 1);
}

#[test]
fn two_dependencies_need_both_done() {
    let shared = test_shared();
    let dir = temp_cwd();
    let worker = MockCtl::default();
    add_session(
        &shared,
        "ses_two",
        "worker/two",
        SessionStatus::Idle,
        worker.as_ctl(),
        &dir,
    );
    let (_, a) = create_task(&shared, "A", None, &[]);
    let (_, b) = create_task(&shared, "B", None, &[]);
    let (a, b) = (a.unwrap(), b.unwrap());
    let (_, c) = create_task(&shared, "C", Some("worker/two"), &[&a, &b]);
    let c = c.unwrap();

    set_task_status(&shared, &a, TaskStatus::Done);
    assert_eq!(task_status(&shared, &c), TaskStatus::Blocked);
    assert!(worker.inputs().is_empty(), "只完成一個相依性不該解鎖");

    set_task_status(&shared, &b, TaskStatus::Done);
    assert_eq!(task_status(&shared, &c), TaskStatus::Todo);
    assert_eq!(worker.inputs().len(), 1);
}

#[test]
fn unknown_dependency_is_rejected() {
    let shared = test_shared();
    let (frame, id) = create_task(&shared, "孤兒", None, &["task_nope"]);
    assert!(
        matches!(&frame, Frame::Error { message, .. } if message.contains("task_nope")),
        "應回錯誤：{frame:?}"
    );
    assert!(id.is_none(), "不該建立任務");
}

#[test]
fn dependency_already_done_behaves_like_no_dependency() {
    let shared = test_shared();
    let dir = temp_cwd();
    let worker = MockCtl::default();
    add_session(
        &shared,
        "ses_ready",
        "worker/ready",
        SessionStatus::Idle,
        worker.as_ctl(),
        &dir,
    );
    let (_, a) = create_task(&shared, "A", None, &[]);
    let a = a.unwrap();
    set_task_status(&shared, &a, TaskStatus::Done);
    let (_, b) = create_task(&shared, "B", Some("worker/ready"), &[&a]);
    assert_eq!(task_status(&shared, &b.unwrap()), TaskStatus::Todo);
    assert_eq!(worker.inputs().len(), 1, "相依性都完成時立刻通知");
}

#[test]
fn reassigning_task_waiting_on_dependency_stays_silent() {
    let shared = test_shared();
    let dir = temp_cwd();
    let worker = MockCtl::default();
    add_session(
        &shared,
        "ses_late",
        "worker/late",
        SessionStatus::Idle,
        worker.as_ctl(),
        &dir,
    );
    let (_, a) = create_task(&shared, "A", None, &[]);
    let a = a.unwrap();
    let (_, b) = create_task(&shared, "B", None, &[&a]);
    let b = b.unwrap();

    let (tx, _rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::TaskUpdate {
            id: b.clone(),
            status: None,
            assignee: Some("worker/late".into()),
            detail: None,
        },
    );
    assert!(worker.inputs().is_empty(), "還在等相依性，改派也先不通知");

    set_task_status(&shared, &a, TaskStatus::Done);
    assert_eq!(worker.inputs().len(), 1, "解鎖時通知後來指派的負責人");
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

// ---- git 輔助函式 (T02／T03)----

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
    let d = std::env::temp_dir().join(format!("termade-git-{tag}-{}", new_id("t")));
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
        context: None,
        native_id: None,
        permission: Default::default(),
        budget_usd: None,
        over_budget: false,
        handed_off_to: None,
        effort: None,
        slash_commands: Vec::new(),
        active_tools: Vec::new(),
        bg_procs: Vec::new(),
    }
}

#[test]
fn merge_clean_succeeds_and_excludes_termade_dir() {
    let repo = temp_repo("m1");
    let s = wt_session(&repo, "s1");
    let wt = Path::new(&s.cwd);
    fs::write(wt.join("new.txt"), "x").unwrap();
    fs::create_dir_all(wt.join("sub.termade")).unwrap();
    fs::write(wt.join("sub.termade/f.txt"), "f").unwrap();
    fs::create_dir_all(wt.join(".termade/mailbox")).unwrap();
    fs::write(wt.join(".termade/mailbox/m"), "m").unwrap();
    merge_worktree(&repo, &s, false).unwrap();
    assert!(repo.join("new.txt").exists());
    assert!(repo.join("sub.termade/f.txt").exists());
    assert!(!repo.join(".termade/mailbox/m").exists());
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
fn auto_commit_worktree_rejects_unresolved_conflict() {
    let repo = temp_repo("uu1");
    let s = wt_session(&repo, "s_uu");
    let wt = Path::new(&s.cwd);
    // 製造衝突 (兩分支修改同一檔案同一行)
    fs::write(repo.join("file.txt"), "base\n").unwrap();
    git(&repo, &["add", "file.txt"]);
    git(&repo, &["commit", "-m", "base"]);
    git(&repo, &["branch", "feature_uu"]);
    git(&repo, &["checkout", "feature_uu"]);
    fs::write(repo.join("file.txt"), "from feature\n").unwrap();
    git(&repo, &["commit", "-am", "feature change"]);
    git(&repo, &["checkout", "main"]);
    fs::write(wt.join("file.txt"), "from worktree\n").unwrap();
    git(wt, &["add", "file.txt"]);
    git(wt, &["commit", "-m", "worktree change"]);
    // 在 worktree 嘗試 merge feature_uu 產生衝突標記
    let _ = Command::new("git")
        .current_dir(wt)
        .args(["merge", "--no-commit", "feature_uu"])
        .output();
    // 驗證工作區具有 UU 狀態
    let st = git_output(wt, &["status", "--porcelain"]).unwrap();
    let st_str = String::from_utf8_lossy(&st.stdout);
    assert!(st_str.contains("file.txt"));
    // auto_commit_worktree 應拒絕 commit
    let res = auto_commit_worktree(wt, "test_worker");
    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert!(err.contains("未解決的合併衝突標記"), "{err}");
}

#[test]
fn merge_resolve_loop_delegates_to_session() {
    let repo = temp_repo("resolve_loop");
    let sa = wt_session(&repo, "sa");
    let sb = wt_session(&repo, "sb");
    let wta = Path::new(&sa.cwd);
    let wtb = Path::new(&sb.cwd);

    fs::write(wta.join("conflict.txt"), "branch A change\n").unwrap();
    fs::write(wtb.join("conflict.txt"), "branch B change\n").unwrap();

    // 先合進 A
    merge_worktree(&repo, &sa, false).unwrap();
    assert!(repo.join("conflict.txt").exists());
    assert_eq!(
        fs::read_to_string(repo.join("conflict.txt")).unwrap(),
        "branch A change\n"
    );

    let shared = test_shared();
    let b_ctl = MockCtl::default();
    {
        let mut inner = shared.inner.lock().unwrap();
        inner.state.sessions.insert(sb.id.clone(), sb.clone());
        inner.sessions.insert(
            sb.id.clone(),
            SessionEntry {
                ctl: b_ctl.as_ctl(),
                run: new_id("run"),
            },
        );
    }

    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::WorktreeMerge {
            session: sb.id.clone(),
            dry_run: false,
            resolve: true,
        },
    );

    let frame = rx.rx.recv().unwrap();
    match frame {
        Frame::Ok { note: Some(n) } => {
            assert!(n.contains("委派") || n.contains("conflict.txt"), "{n}");
        }
        other => panic!("預期 Frame::Ok(note)，得到 {other:?}"),
    }

    // 驗證主工作區 (main) 保持乾淨無 MERGE_HEAD
    assert!(!repo.join(".git/MERGE_HEAD").exists());
    assert!(!has_uncommitted(&repo).unwrap());

    // 驗證 B 的 Worktree 內含有衝突檔案 (具有衝突標記)
    let content_b = fs::read_to_string(wtb.join("conflict.txt")).unwrap();
    assert!(
        content_b.contains("<<<<<<<"),
        "B 工作區應含衝突標記：{content_b}"
    );

    // 驗證 B Session 收到包含衝突檔案清單的解衝突 Prompt
    let inputs = b_ctl.inputs();
    assert!(!inputs.is_empty(), "B session 應收到 prompt");
    assert!(
        inputs.iter().any(|i| i.contains("conflict.txt")),
        "Prompt 應包含衝突檔案 conflict.txt：{:?}",
        inputs
    );
}

#[test]
fn pre_merge_command_blocks_and_allows_merge() {
    let repo = temp_repo("pmc");
    let s = wt_session(&repo, "s_pmc");
    let wt = Path::new(&s.cwd);
    fs::write(wt.join("hello.txt"), "world\n").unwrap();

    // 1. 設定 pre_merge_command = "false"
    let err = merge_worktree_with(&repo, &s, false, Some("false"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("合併前驗證指令失敗") || err.contains("false"),
        "{err}"
    );
    assert!(
        !repo.join("hello.txt").exists(),
        "驗證失敗時主工作區不應被修改"
    );

    // 2. 設定 pre_merge_command = "true"
    let res = merge_worktree_with(&repo, &s, false, Some("true")).unwrap();
    assert!(repo.join("hello.txt").exists(), "驗證成功時應正常合併");
    assert!(res.contains("已將"), "{res}");
}

#[test]
fn task_review_status_notifies_and_wakes_reviewer() {
    let shared = test_shared();
    let dir = temp_cwd();
    let worker_ctl = MockCtl::default();
    let reviewer_ctl = MockCtl::default();

    add_session(
        &shared,
        "worker_ses",
        "worker-1",
        SessionStatus::Idle,
        worker_ctl.as_ctl(),
        &dir,
    );
    add_session(
        &shared,
        "reviewer_ses",
        "reviewer-1",
        SessionStatus::Sleeping,
        reviewer_ctl.as_ctl(),
        &dir,
    );

    // 設定角色與分支
    {
        let mut inner = shared.inner.lock().unwrap();
        if let Some(w) = inner.state.sessions.get_mut("worker_ses") {
            w.role = Role::Worker;
            w.branch = Some("termade/worker_ses".to_string());
        }
        if let Some(r) = inner.state.sessions.get_mut("reviewer_ses") {
            r.role = Role::Reviewer;
        }
    }

    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::TaskCreate {
            title: "實作重要功能".to_string(),
            detail: "詳細內容".to_string(),
            assignee: Some("worker_ses".to_string()),
            created_by: None,
            depends_on: Vec::new(),
        },
    );
    let _ = rx.rx.recv().unwrap();

    let task_id = {
        let inner = shared.inner.lock().unwrap();
        inner.state.tasks.values().next().unwrap().id.clone()
    };

    // 更新任務狀態為 Review
    shared.handle(
        &tx,
        Request::TaskUpdate {
            id: task_id.clone(),
            status: Some(TaskStatus::Review),
            assignee: None,
            detail: None,
        },
    );
    let _ = rx.rx.recv().unwrap();

    // 驗證 Reviewer 被喚醒或收到訊息
    let msgs = {
        let inner = shared.inner.lock().unwrap();
        inner.state.inbox_for("reviewer_ses", None)
    };
    assert!(!msgs.is_empty(), "Reviewer 應收到通知訊息");
    let msg = &msgs[0];
    assert_eq!(msg.kind, MsgKind::Handoff);
    assert!(
        msg.body.contains(&task_id),
        "通知應包含任務 ID：{}",
        msg.body
    );
    assert!(
        msg.body.contains("worker-1"),
        "通知應包含負責 Worker：{}",
        msg.body
    );
    assert!(
        msg.body.contains("review"),
        "通知應包含 review 狀態：{}",
        msg.body
    );
    assert!(reviewer_ctl.wake_count() > 0, "休眠中的 Reviewer 應被喚醒");
    let inputs = reviewer_ctl.inputs();
    assert!(
        inputs.iter().any(|i| i.contains(&task_id)),
        "Reviewer 應收到包含任務 ID 的 nudge 輸入：{:?}",
        inputs
    );
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
fn termade_status_line_matches_only_termade_dir() {
    assert!(is_termade_status_line("?? .termade/wt/x"));
    assert!(!is_termade_status_line("?? sub.termade/f"));
    assert!(!is_termade_status_line(" M src/.termade/f"));
}

// ---- 重啟還原 (T01)----

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
        context: None,
        native_id: None,
        permission: Default::default(),
        budget_usd: None,
        over_budget: false,
        handed_off_to: None,
        effort: None,
        slash_commands: Vec::new(),
        active_tools: Vec::new(),
        bg_procs: Vec::new(),
    }
}

#[test]
fn restart_restores_state_and_continues_seq() {
    let log = std::env::temp_dir().join(format!("{}.jsonl", new_id("termade-log")));
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
    // 原本 live 的 session 被收斂為已結束 (多一筆事件)。
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

// ---- 接續 (T14)----

#[test]
fn native_id_survives_flush_and_replay_and_restart_leaves_resume_hint() {
    let log = std::env::temp_dir().join(format!("{}.jsonl", new_id("termade-native")));
    let first = Shared::restore(&log, PolicyEngine::default(), std::env::temp_dir(), None).unwrap();
    for id in ["ses-with-id", "ses-no-id"] {
        first.emit(Event::SessionStarted {
            session: restart_session(id, SessionStatus::Idle),
        });
    }
    // 走 adapter 事件的路徑，確認 ingest 有對應。
    first.ingest(
        &"ses-with-id".to_string(),
        AgentEvent::NativeId("nat-1".into()),
    );
    assert_eq!(
        lock(&first.inner).state.sessions["ses-with-id"]
            .native_id
            .as_deref(),
        Some("nat-1")
    );
    first.flush_log();
    drop(first);

    let (state, _) = replay_log(&log).unwrap();
    assert_eq!(
        state.sessions["ses-with-id"].native_id.as_deref(),
        Some("nat-1")
    );

    // daemon 重啟：孤兒標成 Failed；有原生 id 的留下接續提示，不會自動接續。
    let second =
        Shared::restore(&log, PolicyEngine::default(), std::env::temp_dir(), None).unwrap();
    let inner = lock(&second.inner);
    let with = &inner.state.sessions["ses-with-id"];
    assert_eq!(with.status, SessionStatus::Failed);
    assert_eq!(with.native_id.as_deref(), Some("nat-1"));
    assert!(
        with.status_note
            .as_deref()
            .is_some_and(|n| n.contains(":resume")),
        "{:?}",
        with.status_note
    );
    let without = &inner.state.sessions["ses-no-id"];
    assert_eq!(without.status, SessionStatus::Failed);
    assert_eq!(without.status_note, None);
    assert!(inner.sessions.is_empty(), "不該自動啟動任何處理程序");
}

#[test]
fn resume_rejects_live_missing_and_idless_sessions_without_changing_state() {
    let shared = test_shared();
    let mut live = restart_session("ses-live", SessionStatus::Idle);
    live.native_id = Some("nat-live".into());
    shared.emit(Event::SessionStarted { session: live });
    shared.emit(Event::SessionStarted {
        session: restart_session("ses-noid", SessionStatus::Exited),
    });
    let before = lock(&shared.inner).state.sessions.clone();
    let seq = lock(&shared.inner).seq;

    let (tx, rx) = client_channel(None);
    for id in ["ses-live", "ses-noid", "ses-missing"] {
        shared.handle(
            &tx,
            Request::Resume {
                session: id.into(),
                prompt: Some("繼續".into()),
            },
        );
    }
    let errors: Vec<String> = rx
        .try_iter()
        .map(|f| match f {
            Frame::Error { message } => message,
            other => panic!("應回錯誤，得到 {other:?}"),
        })
        .collect();
    assert_eq!(errors.len(), 3);
    assert!(errors[0].contains("仍在執行"), "{}", errors[0]);
    assert!(errors[1].contains("沒有可接續"), "{}", errors[1]);
    assert!(errors[2].contains("找不到"), "{}", errors[2]);

    let inner = lock(&shared.inner);
    assert_eq!(inner.seq, seq, "不該送出任何事件");
    assert!(inner.sessions.is_empty());
    for (id, s) in &before {
        let now = &inner.state.sessions[id];
        assert_eq!(now.status, s.status);
        assert_eq!(now.native_id, s.native_id);
    }
}

#[test]
fn prompt_to_ended_session_points_to_resume() {
    let shared = test_shared();
    let mut done = restart_session("ses-done", SessionStatus::Exited);
    done.native_id = Some("nat".into());
    shared.emit(Event::SessionStarted { session: done });
    let (tx, rx) = client_channel(None);
    for id in ["ses-done", "ses-nope"] {
        shared.handle(
            &tx,
            Request::Prompt {
                session: id.into(),
                text: "hi".into(),
                attachments: vec![],
                queue: false,
            },
        );
    }
    let msgs: Vec<String> = rx
        .try_iter()
        .filter_map(|f| match f {
            Frame::Error { message } => Some(message),
            _ => None,
        })
        .collect();
    assert!(msgs[0].contains(":resume"), "{}", msgs[0]);
    assert!(msgs[1].contains("找不到"), "{}", msgs[1]);
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

// ---- 核准決策 (T04)----

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
    fs::create_dir_all(dir.join(".termade")).unwrap();
    fs::write(
        dir.join(".termade/policy.toml"),
        "deny = [\"curl\"]\nallow = [\"make\"]\n",
    )
    .unwrap();
    let merged = PolicyEngine::default().merged_with(&load_local_policy(&dir));
    assert!(merged.deny.iter().any(|d| d == "curl"));
    assert!(merged.deny.iter().any(|d| d == "git push --force"));
}

// ---- 併發與 log 輪替 (T07 / T08)----

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
    let log = std::env::temp_dir().join(format!("{}.jsonl", new_id("termade-log")));
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

    // seq 跨檔案連續 (排除 snapshot 紀錄本身，它沿用前一筆的 seq)。
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
        .join(new_id("termade-nodir"))
        .join("events.jsonl");
    let shared =
        Shared::restore(&log, PolicyEngine::default(), std::env::temp_dir(), None).unwrap();
    shared.emit(out_event(1));
    shared.flush_log();
    assert!(!shared.log_healthy());
    shared.emit(out_event(2));
    assert_eq!(shared.snapshot().seq, 2);
}

// ---- MCP 核准轉發 (Claude --permission-prompt-tool)----

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
    // 等核准請求出現在狀態裡，再由「人」回覆
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
fn mcp_permission_auto_policy_answers_immediately_and_deny_wins() {
    let shared = test_shared();
    shared.emit(Event::SessionStarted {
        session: ask_sess("ses-auto-mcp", PermissionPolicy::Auto),
    });
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::McpPermission {
            session: "ses-auto-mcp".into(),
            title: "Bash".into(),
            detail: r#"{"command":"make"}"#.into(),
        },
    );
    let frames: Vec<Frame> = rx.try_iter().collect();
    assert!(
        matches!(
            &frames[0],
            Frame::PermissionDecision {
                decision: Decision::Allow
            }
        ),
        "Auto 政策一般指令應自動允許：{frames:?}"
    );

    shared.handle(
        &tx,
        Request::McpPermission {
            session: "ses-auto-mcp".into(),
            title: "Bash".into(),
            detail: r#"{"command":"git push --force"}"#.into(),
        },
    );
    let frames: Vec<Frame> = rx.try_iter().collect();
    assert!(
        matches!(
            &frames[0],
            Frame::PermissionDecision {
                decision: Decision::Deny
            }
        ),
        "命中 deny 規則應立即拒絕：{frames:?}"
    );

    let history: Vec<_> = lock(&shared.inner).backlog.iter().cloned().collect();
    assert!(
        history.iter().any(|r| matches!(
            r.event,
            Event::PermissionResolved {
                ref session,
                decision: Decision::Deny,
                ..
            } if session == "ses-auto-mcp"
        )),
        "應發出 PermissionResolved 拒絕事件"
    );
}

#[test]
fn mcp_permission_ask_waits_for_human_decision_allow_and_deny() {
    let shared = test_shared();
    shared.emit(Event::SessionStarted {
        session: ask_sess("ses-ask-mcp", PermissionPolicy::Ask),
    });
    let s2 = shared.clone();
    let h = std::thread::spawn(move || {
        let (tx, rx) = client_channel(None);
        s2.handle(
            &tx,
            Request::McpPermission {
                session: "ses-ask-mcp".into(),
                title: "Bash".into(),
                detail: r#"{"command":"cargo test"}"#.into(),
            },
        );
        let r = rx.try_iter().next();
        r
    });
    // 等核准請求出現在狀態裡，且確認有 PermissionRequested 事件
    let req = loop {
        if let Some(r) = lock(&shared.inner).state.pending_permissions.keys().next() {
            break r.clone();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let history: Vec<_> = lock(&shared.inner).backlog.iter().cloned().collect();
    assert!(
        history.iter().any(|r| matches!(
            r.event,
            Event::PermissionRequested { ref request } if request.session == "ses-ask-mcp" && request.req == req
        )),
        "應發出 PermissionRequested 事件"
    );

    let (tx, _rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::Permission {
            session: "ses-ask-mcp".into(),
            req: req.clone(),
            decision: Decision::Allow,
        },
    );
    let reply = h.join().unwrap();
    assert!(
        matches!(
            reply,
            Some(Frame::PermissionDecision {
                decision: Decision::Allow
            })
        ),
        "應收到 Decision::Allow 回覆：{reply:?}"
    );
    assert!(lock(&shared.inner).state.pending_permissions.is_empty());

    let history: Vec<_> = lock(&shared.inner).backlog.iter().cloned().collect();
    assert!(
        history.iter().any(|r| matches!(
            r.event,
            Event::PermissionResolved {
                ref session,
                ref req,
                decision: Decision::Allow,
            } if session == "ses-ask-mcp"
        )),
        "應發出 PermissionResolved 允許事件"
    );
}

#[test]
fn restore_prunes_old_ended_sessions_but_keeps_branches_and_recent() {
    let log = std::env::temp_dir().join(format!("{}.jsonl", new_id("termade-prune")));
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
        mk("ses-old-branch", 3 * day, Some("termade/x")),
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

// ---- session 移除與啟動失敗清理 ----

#[test]
fn failed_spawn_leaves_no_session() {
    let shared = test_shared();
    // 指向不存在的執行檔，讓啟動必定失敗 (與本機是否安裝 agent 無關)。
    std::env::set_var("TERMADE_CLAUDE_BIN", "/nonexistent/claude-for-test");
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::Spawn(termade_protocol::SpawnRequest {
            kind: AgentKind::ClaudeCode,
            cwd: Some(std::env::temp_dir().to_string_lossy().to_string()),
            label: None,
            role: Default::default(),
            prompt: None,
            worktree: false,
            transport: termade_protocol::Transport::Structured,
            permission: None,
            parent: None,
            model: None,
            effort: None,
            attachments: vec![],
            inherit_from: None,
            args: vec![],
            budget_usd: None,
        }),
    );
    let frames: Vec<Frame> = rx.try_iter().collect();
    assert!(frames.iter().any(|f| matches!(f, Frame::Error { .. })));
    std::env::remove_var("TERMADE_CLAUDE_BIN");
    assert!(lock(&shared.inner).state.sessions.is_empty());
}

#[test]
fn session_remove_only_ended() {
    let shared = test_shared();
    shared.emit(Event::SessionStarted {
        session: restart_session("ses-live", SessionStatus::Idle),
    });
    shared.emit(Event::SessionStarted {
        session: {
            let mut d = restart_session("ses-dead", SessionStatus::Failed);
            d.cwd = "/tmp/other-dead".into();
            d
        },
    });
    let (tx, rx) = client_channel(None);
    for id in ["ses-live", "ses-dead"] {
        shared.handle(
            &tx,
            Request::SessionRemove {
                session: id.into(),
                force: false,
            },
        );
    }
    let frames: Vec<Frame> = rx.try_iter().collect();
    assert!(matches!(frames[0], Frame::Error { .. }));
    assert!(matches!(frames[1], Frame::Ok { .. }));
    let inner = lock(&shared.inner);
    assert!(inner.state.sessions.contains_key("ses-live"));
    assert!(!inner.state.sessions.contains_key("ses-dead"));
}

#[test]
fn session_remove_branchless_allows_other_live_session_in_same_cwd() {
    let shared = test_shared();
    // 兩個一般 session 共用同一個 cwd (主 repo) 是常態：另一個還在執行時，
    // 仍然可以移除已結束的那一個 (不會刪到任何檔案)。
    shared.emit(Event::SessionStarted {
        session: restart_session("ses-live", SessionStatus::Idle),
    });
    shared.emit(Event::SessionStarted {
        session: restart_session("ses-dead", SessionStatus::Failed),
    });
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::SessionRemove {
            session: "ses-dead".into(),
            force: false,
        },
    );
    let frames: Vec<Frame> = rx.try_iter().collect();
    assert!(matches!(frames[0], Frame::Ok { .. }), "{frames:?}");
    let inner = lock(&shared.inner);
    assert!(inner.state.sessions.contains_key("ses-live"));
    assert!(!inner.state.sessions.contains_key("ses-dead"));
}

#[test]
fn high_quota_leaves_a_handoff_hint_once_per_crossing() {
    let shared = test_shared();
    shared.emit(Event::SessionStarted {
        session: restart_session("ses-c", SessionStatus::Idle),
    });
    let mk = |pct: f32| termade_core::Quota {
        agent: "claude_code".into(),
        windows: vec![termade_core::QuotaWindow {
            name: "5h".into(),
            used_percent: pct,
            resets_at_ms: None,
        }],
        reached: false,
        updated_ms: 0,
    };
    // restart_session 的 kind 是 ClaudeCode；agent id 要和 kind.id() 一致。
    let id = AgentKind::ClaudeCode.id().to_string();
    let mut high = mk(95.0);
    high.agent = id.clone();
    shared.react_to_quota(&Event::QuotaUpdated {
        quota: high.clone(),
    });
    let note = lock(&shared.inner).state.sessions["ses-c"]
        .status_note
        .clone()
        .unwrap();
    assert!(note.contains("handoff") && note.contains("95"), "{note}");
    // 再來一次不重複提醒 (先清掉備註確認)
    shared.emit(Event::SessionNote {
        session: "ses-c".into(),
        note: String::new(),
    });
    shared.react_to_quota(&Event::QuotaUpdated { quota: high });
    assert!(lock(&shared.inner).state.sessions["ses-c"]
        .status_note
        .is_none());
    // 降回門檻以下後，下次越線會再提醒
    let mut low = mk(10.0);
    low.agent = id.clone();
    shared.react_to_quota(&Event::QuotaUpdated { quota: low });
    let mut high2 = mk(92.0);
    high2.agent = id;
    shared.react_to_quota(&Event::QuotaUpdated { quota: high2 });
    assert!(lock(&shared.inner).state.sessions["ses-c"]
        .status_note
        .is_some());
}

// ---- T17：成本與子 agent 數量上限 ----

#[test]
fn spawn_enforces_max_depth_and_max_children_only_for_child_sessions() {
    let shared = test_shared();
    let cwd = temp_cwd();
    fs::create_dir_all(cwd.join(".termade")).unwrap();
    fs::write(
        cwd.join(".termade").join("config.toml"),
        "[limits]\nmax_depth = 2\nmax_children = 2\n",
    )
    .unwrap();

    let ctl = MockCtl::default();
    add_session(
        &shared,
        "root",
        "root",
        SessionStatus::Busy,
        ctl.as_ctl(),
        &cwd,
    );
    add_session(&shared, "c1", "c1", SessionStatus::Busy, ctl.as_ctl(), &cwd);
    add_session(&shared, "c2", "c2", SessionStatus::Idle, ctl.as_ctl(), &cwd);
    add_session(&shared, "g1", "g1", SessionStatus::Busy, ctl.as_ctl(), &cwd);
    {
        let mut inner = lock(&shared.inner);
        inner.state.sessions.get_mut("c1").unwrap().parent = Some("root".into());
        inner.state.sessions.get_mut("c2").unwrap().parent = Some("root".into());
        inner.state.sessions.get_mut("g1").unwrap().parent = Some("c1".into());
    }

    let spawn_req = |parent: Option<&str>| {
        Request::Spawn(termade_protocol::SpawnRequest {
            kind: AgentKind::ClaudeCode,
            cwd: Some(cwd.to_string_lossy().to_string()),
            label: None,
            role: Role::Worker,
            prompt: None,
            worktree: false,
            transport: termade_protocol::Transport::Structured,
            permission: None,
            parent: parent.map(String::from),
            model: None,
            effort: None,
            attachments: vec![],
            inherit_from: None,
            args: vec![],
            budget_usd: None,
        })
    };

    // g1 的深度已是 2 (root=0 -> c1=1 -> g1=2)，再往下一層深度為 3 > max_depth(2)
    let (tx, rx) = client_channel(None);
    shared.handle(&tx, spawn_req(Some("g1")));
    let frames: Vec<Frame> = rx.try_iter().collect();
    match &frames[0] {
        Frame::Error { message } => {
            assert!(
                message.contains("max_depth") && message.contains('2'),
                "應包含上限名稱與數值：{message}"
            );
        }
        other => panic!("預期錯誤，得到 {other:?}"),
    }

    // root 已有 2 個進行中的子 session (c1, c2)，再衍生第 3 個應被 max_children(2) 拒絕
    let (tx, rx) = client_channel(None);
    shared.handle(&tx, spawn_req(Some("root")));
    let frames: Vec<Frame> = rx.try_iter().collect();
    match &frames[0] {
        Frame::Error { message } => {
            assert!(
                message.contains("max_children") && message.contains('2'),
                "應包含上限名稱與數值：{message}"
            );
        }
        other => panic!("預期錯誤，得到 {other:?}"),
    }

    // 若 c2 已結束 (Exited)，進行中的子 session 只剩 1 個，不再被 max_children 擋下
    shared.emit(Event::SessionStatus {
        session: "c2".into(),
        status: SessionStatus::Exited,
    });
    assert_eq!(lock(&shared.inner).state.live_children_count("root"), 1);
}

#[test]
fn budget_warn_once_and_exceed_cancels_blocks_prompt_and_raises() {
    let log = std::env::temp_dir().join(format!("{}.jsonl", new_id("termade-budget-log")));
    let shared =
        Shared::restore(&log, PolicyEngine::default(), std::env::temp_dir(), None).unwrap();
    let cwd = temp_cwd();
    let ctl = MockCtl::default();
    add_session(
        &shared,
        "s1",
        "worker/a",
        SessionStatus::Busy,
        ctl.as_ctl(),
        &cwd,
    );
    let s1 = {
        let mut inner = lock(&shared.inner);
        let s = inner.state.sessions.get_mut("s1").unwrap();
        s.budget_usd = Some(5.0);
        s.clone()
    };
    shared.emit(Event::SessionStarted { session: s1 });

    // 82% ($4.10 / $5)：跨越 warn_percent (80%)，留一筆 SessionNote
    Shared::ingest(
        &shared,
        &"s1".into(),
        termade_adapter::AgentEvent::Cost(Cost {
            usd: 4.10,
            input_tokens: 1000,
            output_tokens: 500,
        }),
    );
    let note1 = lock(&shared.inner).state.sessions["s1"]
        .status_note
        .clone()
        .unwrap();
    assert!(note1.contains("82%") && note1.contains("$5"), "{note1}");
    assert_eq!(ctl.cancel_count(), 0);
    assert!(!lock(&shared.inner).state.sessions["s1"].over_budget);

    // 85% ($4.25 / $5)：同一次跨越不重複留 SessionNote
    shared.emit(Event::SessionNote {
        session: "s1".into(),
        note: String::new(),
    });
    Shared::ingest(
        &shared,
        &"s1".into(),
        termade_adapter::AgentEvent::Cost(Cost {
            usd: 4.25,
            input_tokens: 1100,
            output_tokens: 600,
        }),
    );
    assert!(lock(&shared.inner).state.sessions["s1"]
        .status_note
        .is_none());

    // 100% ($5.05 / $5)：超過上限，呼叫 ctl.cancel() 並標記 over_budget
    Shared::ingest(
        &shared,
        &"s1".into(),
        termade_adapter::AgentEvent::Cost(Cost {
            usd: 5.05,
            input_tokens: 1500,
            output_tokens: 800,
        }),
    );
    assert_eq!(ctl.cancel_count(), 1);
    assert!(lock(&shared.inner).state.sessions["s1"].over_budget);

    // 模擬回合被中斷後回到 Idle
    shared.emit(Event::SessionStatus {
        session: "s1".into(),
        status: SessionStatus::Idle,
    });

    // over_budget 時送 Request::Prompt 會被拒絕並回傳錯誤
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::Prompt {
            session: "s1".into(),
            text: "繼續做".into(),
            attachments: vec![],
            queue: false,
        },
    );
    let frames: Vec<Frame> = rx.try_iter().collect();
    match &frames[0] {
        Frame::Error { message } => assert!(message.contains("超過成本上限"), "{message}"),
        other => panic!("預期錯誤，得到 {other:?}"),
    }
    assert!(ctl.inputs().is_empty());

    // over_budget 時收到直接訊息也不會自動喚醒
    shared.handle(
        &tx,
        Request::BusSend {
            from: None,
            to: Target::Direct("s1".into()),
            kind: MsgKind::Chat,
            body: "ping".into(),
            reply_to: None,
        },
    );
    assert!(ctl.inputs().is_empty());

    // 重播事件日誌後 over_budget 仍保留
    shared.flush_log();
    let (replayed, _) = replay_log(&log).unwrap();
    assert!(replayed.sessions["s1"].over_budget);

    // 調高上限至 $10：解除 over_budget，之後可繼續送 prompt
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::SessionBudget {
            session: "s1".into(),
            budget_usd: 10.0,
        },
    );
    let frames: Vec<Frame> = rx.try_iter().collect();
    assert!(matches!(frames[0], Frame::Ok { .. }));
    assert!(!lock(&shared.inner).state.sessions["s1"].over_budget);
    assert_eq!(
        lock(&shared.inner).state.sessions["s1"].budget_usd,
        Some(10.0)
    );

    shared.handle(
        &tx,
        Request::Prompt {
            session: "s1".into(),
            text: "繼續做".into(),
            attachments: vec![],
            queue: false,
        },
    );
    assert_eq!(ctl.inputs().len(), 1);
}

#[test]
fn child_cost_counts_toward_parent_tree_usd_and_stops_tree() {
    let shared = test_shared();
    let cwd = temp_cwd();
    fs::create_dir_all(cwd.join(".termade")).unwrap();
    fs::write(
        cwd.join(".termade").join("config.toml"),
        "[limits]\ntree_usd = 10.0\n",
    )
    .unwrap();

    let parent_ctl = MockCtl::default();
    let child_ctl = MockCtl::default();
    add_session(
        &shared,
        "parent",
        "coord",
        SessionStatus::Idle,
        parent_ctl.as_ctl(),
        &cwd,
    );
    add_session(
        &shared,
        "child",
        "worker/1",
        SessionStatus::Busy,
        child_ctl.as_ctl(),
        &cwd,
    );
    {
        let mut inner = lock(&shared.inner);
        inner.state.sessions.get_mut("child").unwrap().parent = Some("parent".into());
    }

    // 父 session 已花 $6，子 session 再花 $4.5 → 整棵樹 $10.50 >= tree_usd ($10)
    Shared::ingest(
        &shared,
        &"parent".into(),
        termade_adapter::AgentEvent::Cost(Cost {
            usd: 6.0,
            input_tokens: 100,
            output_tokens: 100,
        }),
    );
    assert!(!lock(&shared.inner).state.sessions["parent"].over_budget);

    Shared::ingest(
        &shared,
        &"child".into(),
        termade_adapter::AgentEvent::Cost(Cost {
            usd: 4.5,
            input_tokens: 100,
            output_tokens: 100,
        }),
    );
    assert_eq!(lock(&shared.inner).state.tree_cost_usd("parent"), 10.5);
    assert!(lock(&shared.inner).state.sessions["parent"].over_budget);
    assert!(lock(&shared.inner).state.sessions["child"].over_budget);
    assert_eq!(parent_ctl.cancel_count(), 1);
    assert_eq!(child_ctl.cancel_count(), 1);
}

#[test]
fn acp_prepends_collab_instructions_to_first_prompt_only_and_hides_from_user_prompt_event() {
    use std::os::unix::fs::PermissionsExt;

    let shared = test_shared();
    let dir = temp_cwd();
    fs::create_dir_all(dir.join(".termade")).unwrap();
    let inbox_file = dir.join("acp_prompts.jsonl");
    let script = dir.join("fake_acp.sh");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":1,"agentCapabilities":{{}}}}}}\n' "$id"
      ;;
    *'"method":"session/new"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessionId":"acp-fake"}}}}\n' "$id"
      ;;
    *'"method":"session/prompt"'*)
      printf '%s\n' "$line" >> "{}"
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$id"
      ;;
  esac
done
"#,
            inbox_file.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        dir.join(".termade/config.toml"),
        format!(
            "[collab]\ninstructions = true\n[agents.opencode]\nbin = {:?}\n",
            script.to_string_lossy()
        ),
    )
    .unwrap();

    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::Spawn(termade_protocol::SpawnRequest {
            kind: AgentKind::OpenCode,
            cwd: Some(dir.to_string_lossy().to_string()),
            label: Some("worker/acp-t20".into()),
            role: Role::Worker,
            prompt: Some("只做第一件事".into()),
            worktree: false,
            transport: termade_protocol::Transport::Structured,
            permission: None,
            parent: None,
            model: None,
            effort: None,
            attachments: vec![],
            inherit_from: None,
            args: vec![],
            budget_usd: None,
        }),
    );

    // 等待 fake_acp.sh 收到第一則 session/prompt。
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if inbox_file.is_file()
            && fs::read_to_string(&inbox_file)
                .unwrap_or_default()
                .lines()
                .count()
                >= 1
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // 再送第二則 prompt，確認前置說明不會重複附上。
    shared.handle(
        &tx,
        Request::Prompt {
            session: "worker/acp-t20".into(),
            text: "只做第二件事".into(),
            attachments: vec![],
            queue: false,
        },
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if fs::read_to_string(&inbox_file)
            .unwrap_or_default()
            .lines()
            .count()
            >= 2
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let raw = fs::read_to_string(&inbox_file).expect("應收到兩則 session/prompt");
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2, "{raw}");
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    let first_text = first["params"]["prompt"][0]["text"].as_str().unwrap();
    assert!(
        first_text.contains("[termade collaboration instructions]")
            && first_text.contains("worker/acp-t20"),
        "{first_text}"
    );
    assert!(first_text.ends_with("只做第一件事"), "{first_text}");

    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    let second_text = second["params"]["prompt"][0]["text"].as_str().unwrap();
    assert_eq!(second_text, "只做第二件事");

    // 檢查 UserPrompt 事件與 transcript：只含使用者原本的文字，不含協作說明。
    drop(rx);
    let inner = lock(&shared.inner);
    let sid = inner.state.resolve_session_id("worker/acp-t20").unwrap();
    let transcript: Vec<String> = inner.state.transcripts[&sid].iter().cloned().collect();
    let joined = transcript.join("\n");
    assert!(joined.contains("❯ 只做第一件事"), "{joined}");
    assert!(joined.contains("❯ 只做第二件事"), "{joined}");
    assert!(!joined.contains("[termade 協作說明]"), "{joined}");
    drop(inner);

    let (tx_k, _rx_k) = client_channel(None);
    shared.handle(
        &tx_k,
        Request::Kill {
            session: "worker/acp-t20".into(),
        },
    );
}

// ---- T19：Prompt 排隊移至 daemon ----

#[test]
fn queue_flushes_one_at_a_time_when_idle_even_without_clients() {
    let shared = test_shared();
    let dir = temp_cwd();
    let ctl = MockCtl::default();
    add_session(
        &shared,
        "ses_q",
        "worker/q",
        SessionStatus::Busy,
        ctl.as_ctl(),
        &dir,
    );

    let (tx, rx) = client_channel(None);
    for text in ["first prompt", "second prompt"] {
        shared.handle(
            &tx,
            Request::Prompt {
                session: "ses_q".into(),
                text: text.into(),
                attachments: vec![],
                queue: true,
            },
        );
    }
    drop(tx);
    drop(rx);
    // 確認沒有任何 client 連線
    assert!(lock(&shared.inner).clients.is_empty());
    assert!(ctl.inputs().is_empty(), "Busy 時不應立刻送出");
    assert_eq!(
        lock(&shared.inner).state.queues.get("ses_q"),
        Some(&vec![
            "first prompt".to_string(),
            "second prompt".to_string()
        ])
    );

    // 第一次回到 Idle：只送出第一則，狀態轉為 Busy，佇列剩第二則
    shared.ingest(
        &"ses_q".to_string(),
        AgentEvent::Status(SessionStatus::Idle),
    );
    assert_eq!(ctl.inputs(), vec!["first prompt".to_string()]);
    assert_eq!(
        lock(&shared.inner).state.queues.get("ses_q"),
        Some(&vec!["second prompt".to_string()])
    );
    assert_eq!(
        lock(&shared.inner).state.sessions["ses_q"].status,
        SessionStatus::Busy
    );

    // 第二次回到 Idle：送出第二則，佇列清空並從 State.queues 移除 key
    shared.ingest(
        &"ses_q".to_string(),
        AgentEvent::Status(SessionStatus::Idle),
    );
    assert_eq!(
        ctl.inputs(),
        vec!["first prompt".to_string(), "second prompt".to_string()]
    );
    assert!(!lock(&shared.inner).state.queues.contains_key("ses_q"));
}

#[test]
fn queue_true_on_idle_session_with_empty_queue_sends_immediately() {
    let shared = test_shared();
    let dir = temp_cwd();
    let ctl = MockCtl::default();
    add_session(
        &shared,
        "ses_idle",
        "worker/idle",
        SessionStatus::Idle,
        ctl.as_ctl(),
        &dir,
    );

    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::Prompt {
            session: "ses_idle".into(),
            text: "run now".into(),
            attachments: vec![],
            queue: true,
        },
    );
    let reply = rx.try_iter().next().unwrap();
    assert!(matches!(reply, Frame::Ok { note: None }));
    assert_eq!(ctl.inputs(), vec!["run now".to_string()]);
    assert!(!lock(&shared.inner).state.queues.contains_key("ses_idle"));
}

#[test]
fn queue_clears_when_session_fails_or_exits() {
    let shared = test_shared();
    let dir = temp_cwd();
    let ctl = MockCtl::default();
    add_session(
        &shared,
        "ses_fail",
        "worker/fail",
        SessionStatus::Busy,
        ctl.as_ctl(),
        &dir,
    );

    let (tx, _rx) = client_channel(None);
    for text in ["p1", "p2"] {
        shared.handle(
            &tx,
            Request::Prompt {
                session: "ses_fail".into(),
                text: text.into(),
                attachments: vec![],
                queue: true,
            },
        );
    }
    assert_eq!(lock(&shared.inner).state.queues["ses_fail"].len(), 2);

    // session 失敗結束：佇列清空、State.queues 移除該 key，並留下 SessionNote
    shared.ingest(
        &"ses_fail".to_string(),
        AgentEvent::Status(SessionStatus::Failed),
    );
    let inner = lock(&shared.inner);
    assert!(!inner.queues.contains_key("ses_fail"));
    assert!(!inner.state.queues.contains_key("ses_fail"));
    let note = inner.state.sessions["ses_fail"]
        .status_note
        .as_deref()
        .unwrap_or("");
    assert!(note.contains("丟棄 2 則排隊 prompt"), "note={note}");
}

#[test]
fn queue_pop_clear_and_cap_20() {
    let shared = test_shared();
    let dir = temp_cwd();
    let ctl = MockCtl::default();
    add_session(
        &shared,
        "ses_cap",
        "worker/cap",
        SessionStatus::Busy,
        ctl.as_ctl(),
        &dir,
    );

    let (tx, rx) = client_channel(None);
    for i in 1..=20 {
        shared.handle(
            &tx,
            Request::Prompt {
                session: "ses_cap".into(),
                text: format!("item-{i}"),
                attachments: vec![],
                queue: true,
            },
        );
        assert!(matches!(rx.try_iter().next(), Some(Frame::Ok { .. })));
    }
    // 第 21 則應回傳錯誤
    shared.handle(
        &tx,
        Request::Prompt {
            session: "ses_cap".into(),
            text: "overflow".into(),
            attachments: vec![],
            queue: true,
        },
    );
    match rx.try_iter().next() {
        Some(Frame::Error { message }) => assert!(message.contains("20"), "{message}"),
        other => panic!("expected error, got {other:?}"),
    }
    assert_eq!(lock(&shared.inner).state.queues["ses_cap"].len(), 20);

    // QueuePop 取回最後一則 (item-20)
    shared.handle(
        &tx,
        Request::QueuePop {
            session: "ses_cap".into(),
        },
    );
    assert!(matches!(rx.try_iter().next(), Some(Frame::Ok { note: Some(n) }) if n == "item-20"));
    assert_eq!(lock(&shared.inner).state.queues["ses_cap"].len(), 19);

    // QueueClear 清空全部
    shared.handle(
        &tx,
        Request::QueueClear {
            session: "ses_cap".into(),
        },
    );
    match rx.try_iter().next() {
        Some(Frame::Ok { note: Some(n) }) => assert!(n.contains("19"), "{n}"),
        other => panic!("unexpected reply: {other:?}"),
    }
    assert!(!lock(&shared.inner).state.queues.contains_key("ses_cap"));

    // 佇列空時 QueuePop 回傳 note: None
    shared.handle(
        &tx,
        Request::QueuePop {
            session: "ses_cap".into(),
        },
    );
    assert!(matches!(
        rx.try_iter().next(),
        Some(Frame::Ok { note: None })
    ));
}

#[test]
fn queue_changed_updates_state_and_old_snapshot_parses_and_hides_attachment_data_from_log() {
    let dir = temp_cwd();
    let log = dir.join("events.jsonl");
    let shared = Shared::restore(&log, PolicyEngine::default(), dir.clone(), None).unwrap();
    let ctl = MockCtl::default();
    add_session(
        &shared,
        "ses_att",
        "worker/att",
        SessionStatus::Busy,
        ctl.as_ctl(),
        &dir,
    );

    let secret_text = "SUPER_SECRET_ATTACHMENT_BODY_12345";
    let secret_data = "U1VQRVJfU0VDUkVUX0JBU0U2NF9EQVRB";
    let att = termade_core::Attachment {
        name: "foo.rs".into(),
        mime: "text/plain".into(),
        path: Some("/tmp/foo.rs".into()),
        text: Some(secret_text.into()),
        data: Some(secret_data.into()),
        size: secret_text.len() as u64,
        raw_prompt: Some("請看 @foo.rs".into()),
    };

    let (tx, _rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::Prompt {
            session: "ses_att".into(),
            text: "請看 foo.rs".into(),
            attachments: vec![att],
            queue: true,
        },
    );
    shared.flush_log();

    // State.queues 儲存的是原始輸入文字 (含 @foo.rs)
    assert_eq!(
        shared.snapshot().queues.get("ses_att"),
        Some(&vec!["請看 @foo.rs".to_string()])
    );

    // event log 不得包含附件的 text 或 data 內容
    let raw_log = fs::read_to_string(&log).unwrap();
    assert!(
        !raw_log.contains(secret_text),
        "attachment text leaked into log: {raw_log}"
    );
    assert!(
        !raw_log.contains(secret_data),
        "attachment data leaked into log: {raw_log}"
    );
    assert!(raw_log.contains("請看 @foo.rs"));

    // 舊格式快照 (不含 queues 欄位)仍可正常反序列化
    let old_state_json = r#"{
        "seq": 5,
        "sessions": {},
        "tasks": {},
        "messages": [],
        "pending_permissions": {},
        "transcripts": {}
    }"#;
    let parsed: State = serde_json::from_str(old_state_json).expect("舊快照應可解析");
    assert_eq!(parsed.seq, 5);
    assert!(parsed.queues.is_empty());
}

// ---- T17 × T19：超過成本上限時的排隊 ----

#[test]
fn over_budget_holds_queue_rejects_new_prompts_and_resumes_after_raise() {
    let shared = test_shared();
    let dir = temp_cwd();
    let ctl = MockCtl::default();
    add_session(
        &shared,
        "ses_ob",
        "worker/ob",
        SessionStatus::Busy,
        ctl.as_ctl(),
        &dir,
    );
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::Prompt {
            session: "ses_ob".into(),
            text: "queued before limit".into(),
            attachments: vec![],
            queue: true,
        },
    );
    while rx.rx.try_recv().is_ok() {}

    // 超過上限：回到 Idle 也不送出排隊的 prompt。
    shared.emit(Event::SessionOverBudget {
        session: "ses_ob".into(),
        spent: 1.0,
        limit: 1.0,
    });
    shared.ingest(
        &"ses_ob".to_string(),
        AgentEvent::Status(SessionStatus::Idle),
    );
    assert!(ctl.inputs().is_empty(), "超限時不應送出排隊的 prompt");
    assert!(lock(&shared.inner).state.queues.contains_key("ses_ob"));

    // 超限期間新的 prompt (立即或排隊)都被拒絕，佇列不變。
    for queue in [false, true] {
        shared.handle(
            &tx,
            Request::Prompt {
                session: "ses_ob".into(),
                text: "new".into(),
                attachments: vec![],
                queue,
            },
        );
    }
    let mut errors = 0;
    while let Ok(frame) = rx.rx.try_recv() {
        if format!("{frame:?}").contains("over_budget") {
            errors += 1;
        }
    }
    assert_eq!(errors, 2);
    assert_eq!(
        lock(&shared.inner).state.queues.get("ses_ob").map(Vec::len),
        Some(1)
    );

    // 調高上限：保留的 prompt 恢復送出。
    shared.handle(
        &tx,
        Request::SessionBudget {
            session: "ses_ob".into(),
            budget_usd: 5.0,
        },
    );
    assert_eq!(ctl.inputs(), vec!["queued before limit".to_string()]);
    assert!(!lock(&shared.inner).state.queues.contains_key("ses_ob"));
}

// ---- T22：共用 worktree 的移除 ----

fn rm(shared: &Arc<Shared>, id: &str, force: bool) -> Frame {
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::SessionRemove {
            session: id.into(),
            force,
        },
    );
    rx.rx.try_recv().expect("rm 應有回覆")
}

/// A 與交接後的 B 共用 A 的 worktree 與分支，兩者都已結束、分支已合併。
fn shared_pair(tag: &str) -> (Arc<Shared>, PathBuf, Session) {
    let repo = temp_repo(tag);
    let shared = Shared::restore(
        Path::new("/dev/null"),
        PolicyEngine::default(),
        repo.clone(),
        None,
    )
    .unwrap();
    let a = wt_session(&repo, "ses_a");
    let mut b = a.clone();
    b.id = "ses_b".into();
    b.label = "w/ses_a→claude".into();
    shared.emit(Event::SessionStarted { session: a.clone() });
    shared.emit(Event::SessionStarted { session: b });
    (shared, repo, a)
}

#[test]
fn removing_shared_worktree_keeps_it_until_last_session() {
    let (shared, repo, a) = shared_pair("t22a");
    let branch = a.branch.clone().unwrap();

    let f = rm(&shared, "ses_a", false);
    assert!(matches!(f, Frame::Ok { .. }), "{f:?}");
    assert!(format!("{f:?}").contains("保留"), "{f:?}");
    assert!(Path::new(&a.cwd).exists(), "還有 B 參照，worktree 要保留");
    assert!(crate::git::branch_exists(&repo, &branch));

    let f = rm(&shared, "ses_b", false);
    assert!(matches!(f, Frame::Ok { .. }), "{f:?}");
    assert!(!Path::new(&a.cwd).exists());
    assert!(!crate::git::branch_exists(&repo, &branch));
    let inner = lock(&shared.inner);
    assert!(!inner.state.sessions.contains_key("ses_a"));
    assert!(!inner.state.sessions.contains_key("ses_b"));
}

#[test]
fn removing_session_whose_branch_and_worktree_are_gone_succeeds() {
    let (shared, repo, a) = shared_pair("t22b");
    // 舊版行為留下的狀態：worktree 與分支已被刪掉，session 紀錄還在。
    remove_worktree(&repo, &a, false).unwrap();
    assert!(gc_block_reason(&repo, &a).is_none());
    // 再刪一次也不算錯 (分支不存在時跳過)。
    remove_worktree(&repo, &a, false).unwrap();
    for id in ["ses_a", "ses_b"] {
        let f = rm(&shared, id, false);
        assert!(matches!(f, Frame::Ok { .. }), "{id}: {f:?}");
    }
    assert!(lock(&shared.inner).state.sessions.is_empty());
}

#[test]
fn missing_branch_with_worktree_dir_still_blocks_without_force() {
    let repo = temp_repo("t22c");
    let s = wt_session(&repo, "ses_c");
    // 只拿掉分支，目錄還在。
    git(&repo, &["worktree", "remove", &s.cwd]);
    fs::create_dir_all(&s.cwd).unwrap();
    git(&repo, &["branch", "-D", s.branch.as_deref().unwrap()]);
    let reason = gc_block_reason(&repo, &s).expect("目錄還在時要擋下");
    assert!(reason.contains("分支不存在"), "{reason}");
}

#[test]
fn gc_cleans_shared_worktree_once() {
    let (shared, repo, a) = shared_pair("t22d");
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::WorktreeGc {
            session: None,
            force: false,
        },
    );
    let note = format!("{:?}", rx.rx.try_recv().unwrap());
    assert!(note.contains("已清理 1 個"), "{note}");
    assert!(!note.contains("略過"), "{note}");
    assert!(!Path::new(&a.cwd).exists());
    assert!(!crate::git::branch_exists(
        &repo,
        a.branch.as_deref().unwrap()
    ));

    // 再跑一次：沒有東西可清，也不報錯。
    shared.handle(
        &tx,
        Request::WorktreeGc {
            session: None,
            force: false,
        },
    );
    let note = format!("{:?}", rx.rx.try_recv().unwrap());
    assert!(
        note.contains("已清理 0 個") && !note.contains("略過"),
        "{note}"
    );
    // gc 之後兩個 session 都能一般 rm。
    for id in ["ses_a", "ses_b"] {
        assert!(matches!(rm(&shared, id, false), Frame::Ok { .. }));
    }
}

// -----------------------------------------------------------------------
// T25：忙完後提醒未讀訊息；超限判斷看實際收件者
// -----------------------------------------------------------------------

fn nudge_setup(status: SessionStatus) -> (Arc<Shared>, MockCtl) {
    let shared = test_shared();
    let dir = temp_cwd();
    let ctl = MockCtl::default();
    add_session(&shared, "ses_n", "worker/n", status, ctl.as_ctl(), &dir);
    (shared, ctl)
}

fn send_to_n(shared: &Arc<Shared>, kind: MsgKind, body: &str) -> String {
    shared
        .deliver_message(
            Some("human".into()),
            Target::Direct("ses_n".into()),
            kind,
            body.into(),
            None,
        )
        .expect("送出應成功")
}

fn go_idle(shared: &Arc<Shared>) {
    shared.ingest(
        &"ses_n".to_string(),
        AgentEvent::Status(SessionStatus::Idle),
    );
}

#[test]
fn busy_recipient_gets_one_nudge_after_returning_to_idle() {
    let (shared, ctl) = nudge_setup(SessionStatus::Busy);
    let id = send_to_n(&shared, MsgKind::Chat, "ping");
    assert!(ctl.inputs().is_empty(), "Busy 時不應送 prompt");
    go_idle(&shared);
    let inputs = ctl.inputs();
    assert_eq!(inputs.len(), 1);
    assert!(inputs[0].contains(&id), "{}", inputs[0]);
    // 一則訊息只提醒一次
    go_idle(&shared);
    assert_eq!(ctl.inputs().len(), 1);
}

#[test]
fn two_unread_messages_merge_into_one_nudge() {
    let (shared, ctl) = nudge_setup(SessionStatus::Busy);
    let a = send_to_n(&shared, MsgKind::Chat, "one");
    let b = send_to_n(&shared, MsgKind::Question, "two");
    go_idle(&shared);
    let inputs = ctl.inputs();
    assert_eq!(inputs.len(), 1);
    assert!(inputs[0].contains(&a) && inputs[0].contains(&b));
}

#[test]
fn queued_prompt_goes_before_unread_nudge() {
    let (shared, ctl) = nudge_setup(SessionStatus::Busy);
    let (tx, _rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::Prompt {
            session: "ses_n".into(),
            text: "queued".into(),
            attachments: vec![],
            queue: true,
        },
    );
    let id = send_to_n(&shared, MsgKind::Chat, "ping");
    go_idle(&shared);
    assert_eq!(ctl.inputs(), vec!["queued".to_string()]);
    go_idle(&shared);
    let inputs = ctl.inputs();
    assert_eq!(inputs.len(), 2);
    assert!(inputs[1].contains(&id));
}

#[test]
fn read_messages_are_not_nudged() {
    // termade_wait 取走：以 inbox 已讀 (cursor 前進)模擬同一個判斷
    let (shared, ctl) = nudge_setup(SessionStatus::Busy);
    send_to_n(&shared, MsgKind::Chat, "one");
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::BusInbox {
            session: "ses_n".into(),
            unread_only: true,
        },
    );
    assert!(format!("{:?}", rx.try_iter().collect::<Vec<_>>()).contains("one"));
    go_idle(&shared);
    assert!(ctl.inputs().is_empty(), "inbox 已讀不應提醒");

    // 讀過之後又來一則新的：只提醒新的
    let (shared, ctl) = nudge_setup(SessionStatus::Busy);
    let old = send_to_n(&shared, MsgKind::Chat, "old");
    shared.handle(
        &tx,
        Request::BusInbox {
            session: "ses_n".into(),
            unread_only: true,
        },
    );
    let new = send_to_n(&shared, MsgKind::Chat, "new");
    go_idle(&shared);
    let inputs = ctl.inputs();
    assert_eq!(inputs.len(), 1);
    assert!(inputs[0].contains(&new) && !inputs[0].contains(&old));
}

#[test]
fn message_taken_by_waiter_is_not_nudged() {
    let (shared, ctl) = nudge_setup(SessionStatus::Busy);
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::BusWait {
            session: "ses_n".into(),
            from: None,
            reply_to: None,
            timeout_ms: 5_000,
        },
    );
    send_to_n(&shared, MsgKind::Chat, "answer");
    let got = rx
        .rx
        .recv_timeout(Duration::from_secs(2))
        .expect("waiter 應收到");
    assert!(format!("{got:?}").contains("answer"));
    go_idle(&shared);
    assert!(ctl.inputs().is_empty());
}

#[test]
fn status_messages_and_over_budget_sessions_are_not_nudged() {
    let (shared, ctl) = nudge_setup(SessionStatus::Busy);
    send_to_n(&shared, MsgKind::Status, "fyi");
    go_idle(&shared);
    assert!(ctl.inputs().is_empty(), "Status 不提醒");

    let (shared, ctl) = nudge_setup(SessionStatus::Busy);
    lock(&shared.inner)
        .state
        .sessions
        .get_mut("ses_n")
        .unwrap()
        .over_budget = true;
    send_to_n(&shared, MsgKind::Chat, "ping");
    go_idle(&shared);
    assert!(ctl.inputs().is_empty(), "over_budget 不提醒");
}

#[test]
fn ended_session_drops_pending_nudge() {
    let (shared, _ctl) = nudge_setup(SessionStatus::Busy);
    send_to_n(&shared, MsgKind::Chat, "ping");
    assert!(lock(&shared.inner).pending_nudge.contains_key("ses_n"));
    shared.ingest(&"ses_n".to_string(), AgentEvent::Exited(Some(0)));
    assert!(!lock(&shared.inner).pending_nudge.contains_key("ses_n"));
}

#[test]
fn over_budget_note_uses_successor_of_handed_off_session() {
    let shared = test_shared();
    let dir = temp_cwd();
    let old = MockCtl::default();
    let new = MockCtl::default();
    add_session(
        &shared,
        "ses_a",
        "t25",
        SessionStatus::Idle,
        old.as_ctl(),
        &dir,
    );
    add_session(
        &shared,
        "ses_b",
        "t25→claude",
        SessionStatus::Idle,
        new.as_ctl(),
        &dir,
    );
    mark_handed_off(&shared, "ses_a", "ses_b");
    lock(&shared.inner)
        .state
        .sessions
        .get_mut("ses_b")
        .unwrap()
        .over_budget = true;
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::BusSend {
            from: None,
            to: Target::Direct("t25".into()),
            kind: MsgKind::Chat,
            body: "ping".into(),
            reply_to: None,
        },
    );
    let frames = format!("{:?}", rx.try_iter().collect::<Vec<_>>());
    assert!(frames.contains("over_budget"), "{frames}");
    assert!(new.inputs().is_empty());
}

#[test]
fn raising_budget_sends_nudge_held_while_over_budget() {
    let (shared, ctl) = nudge_setup(SessionStatus::Busy);
    let id = send_to_n(&shared, MsgKind::Chat, "ping");
    shared.emit(Event::SessionOverBudget {
        session: "ses_n".into(),
        spent: 1.0,
        limit: 1.0,
    });
    go_idle(&shared);
    assert!(ctl.inputs().is_empty(), "超限時不提醒");

    let (tx, _rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::SessionBudget {
            session: "ses_n".into(),
            budget_usd: 5.0,
        },
    );
    let inputs = ctl.inputs();
    assert_eq!(inputs.len(), 1, "調高上限後補提醒");
    assert!(inputs[0].contains(&id), "{}", inputs[0]);
}

/// 記錄 `set_config` 呼叫的控制把手。
type ConfigCall = (Option<String>, Option<String>);

#[derive(Clone, Default)]
struct ConfigCtl {
    calls: Arc<Mutex<Vec<ConfigCall>>>,
    fail: bool,
}

impl AgentControl for ConfigCtl {
    fn send_input(&self, _text: &str) -> Result<()> {
        Ok(())
    }
    fn cancel(&self) -> Result<()> {
        Ok(())
    }
    fn kill(&self) -> Result<()> {
        Ok(())
    }
    fn resolve_permission(&self, _req: &str, _decision: Decision) -> Result<()> {
        Ok(())
    }
    fn transport(&self) -> &'static str {
        "mock"
    }
    fn set_config(&self, model: Option<&str>, effort: Option<&str>) -> Result<Option<String>> {
        if self.fail {
            anyhow::bail!("不支援");
        }
        self.calls
            .lock()
            .unwrap()
            .push((model.map(str::to_string), effort.map(str::to_string)));
        Ok(model.map(str::to_string))
    }
}

#[test]
fn session_config_switches_model_and_effort_and_records_them() {
    let shared = test_shared();
    let cwd = temp_cwd();
    let ctl = ConfigCtl::default();
    add_session(
        &shared,
        "s1",
        "w1",
        SessionStatus::Idle,
        Arc::new(ctl.clone()),
        &cwd,
    );

    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::SessionConfig {
            session: "w1".into(),
            model: Some("opus".into()),
            effort: Some("high".into()),
        },
    );
    let frames: Vec<Frame> = rx.try_iter().collect();
    assert!(matches!(frames[0], Frame::Ok { .. }), "{frames:?}");
    let s = lock(&shared.inner).state.sessions["s1"].clone();
    assert_eq!(s.model.as_deref(), Some("opus"));
    assert_eq!(s.effort.as_deref(), Some("high"));
    assert_eq!(
        ctl.calls.lock().unwrap()[0],
        (Some("opus".into()), Some("high".into()))
    );

    // 兩個都沒給 → 錯誤，不呼叫 adapter
    shared.handle(
        &tx,
        Request::SessionConfig {
            session: "s1".into(),
            model: None,
            effort: Some("  ".into()),
        },
    );
    let frames: Vec<Frame> = rx.try_iter().collect();
    assert!(matches!(frames[0], Frame::Error { .. }));
    assert_eq!(ctl.calls.lock().unwrap().len(), 1);
}

#[test]
fn session_config_error_leaves_state_untouched() {
    let shared = test_shared();
    let cwd = temp_cwd();
    let ctl = ConfigCtl {
        fail: true,
        ..Default::default()
    };
    add_session(
        &shared,
        "s1",
        "w1",
        SessionStatus::Idle,
        Arc::new(ctl),
        &cwd,
    );
    let (tx, rx) = client_channel(None);
    shared.handle(
        &tx,
        Request::SessionConfig {
            session: "s1".into(),
            model: Some("x".into()),
            effort: None,
        },
    );
    let frames: Vec<Frame> = rx.try_iter().collect();
    assert!(matches!(frames[0], Frame::Error { .. }));
    assert!(lock(&shared.inner).state.sessions["s1"].model.is_none());
}

#[test]
fn kill_bg_proc_refuses_non_descendant_and_terminates_real_child() {
    let shared = test_shared();
    let cwd = temp_cwd();
    let ctl = MockCtl {
        inputs: Arc::new(Mutex::new(Vec::new())),
        wakes: Arc::new(Mutex::new(0)),
        cancels: Arc::new(Mutex::new(0)),
    };
    add_session(&shared, "s1", "w1", SessionStatus::Idle, ctl.as_ctl(), &cwd);

    // 啟動一個真實的背景子處理程序 (sleep 30)
    let mut child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep failed");
    let child_pid = child.id();

    let (tx, rx) = client_channel(None);

    // 1. 嘗試終止一個不屬於該 session 後代的隨機 PID，應被拒絕
    shared.handle(
        &tx,
        Request::KillBgProc {
            session: "s1".into(),
            pid: Some(999_999),
            force: false,
        },
    );
    let frames: Vec<Frame> = rx.try_iter().collect();
    assert!(
        matches!(&frames[0], Frame::Error { message } if message.contains("999999")),
        "應拒絕殺死非後代處理程序：{frames:?}"
    );

    // 2. 探測後代處理程序，應該能檢測到真實的 child_pid
    shared.scan_bg_procs_for_session(&"s1".into());
    let procs = lock(&shared.inner).state.sessions["s1"].bg_procs.clone();
    assert!(
        procs.iter().any(|p| p.pid == child_pid),
        "應在 session 的 bg_procs 中探測到子處理程序 PID {child_pid}"
    );

    // 3. 呼叫 KillBgProc 終止該真實子處理程序
    shared.handle(
        &tx,
        Request::KillBgProc {
            session: "s1".into(),
            pid: Some(child_pid),
            force: false,
        },
    );
    let frames: Vec<Frame> = rx.try_iter().collect();
    assert!(matches!(&frames[0], Frame::Ok { .. }));

    // 等待子處理程序結束並確認被訊號終止
    let status = child.wait().expect("wait child failed");
    assert!(!status.success());
}
