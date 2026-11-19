//! T28 遠端斷線重連與增量補齊整合測試：
//! 1. 驗證 --listen 在未指定 token 時自動產生 0600 的 remote.token 並啟動。
//! 2. 模擬 client 連線後收到事件、強制斷線、daemon 產生新事件後，以 Hello { since, epoch } 重連。
//! 3. 驗證只收到增量 Record (無 Snapshot) 且本地 State 完整銜接。

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use termade_core::{AgentKind, State};
use termade_daemon::{default_token_path, is_running, read_token_file, Client, RemoteOptions};
use termade_protocol::{Frame, Request, SpawnRequest, Transport};

#[test]
fn test_remote_auto_token_and_incremental_reconnect() {
    let root = std::env::temp_dir().join(format!("termade-reconn-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let sock = root.join("daemon.sock");
    std::env::set_var("TERMADE_SOCKET", &sock);
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
    std::env::remove_var("TERMADE_REMOTE");

    // 1. 啟動 daemon --listen 127.0.0.1:0 (未指定 token)
    std::thread::spawn(|| {
        let _ = termade_daemon::run_remote(RemoteOptions {
            listen: Some("127.0.0.1:0".into()),
            ..Default::default()
        });
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    while !is_running() {
        assert!(Instant::now() < deadline, "daemon 未在時限內啟動");
        std::thread::sleep(Duration::from_millis(50));
    }

    // 驗收條件 1：驗證 remote.token 已自動建立且權限為 0600
    let token_file = default_token_path();
    assert!(token_file.exists(), "remote.token 應自動建立");
    let meta = fs::metadata(&token_file).unwrap();
    assert_eq!(
        meta.permissions().mode() & 0o077,
        0,
        "remote.token 權限應為 0600"
    );
    let auto_token = read_token_file(&token_file).unwrap();
    assert!(!auto_token.is_empty(), "Token 不得為空");

    // 2. Client 1 初次連線 (模擬 TUI 啟動)
    let mut c1 = Client::connect().unwrap();
    c1.send(&Request::Hello {
        client: "tui".into(),
        since: None,
        token: Some(auto_token.clone()),
        epoch: None,
    })
    .unwrap();

    let (pid, version, epoch) = loop {
        if let Frame::Ready {
            pid,
            version,
            epoch,
        } = c1.recv().unwrap()
        {
            break (pid, version, epoch);
        }
    };
    assert!(pid > 0);
    assert!(!version.is_empty());
    assert!(!epoch.is_empty());

    let mut client_state: State = loop {
        if let Frame::Snapshot { state } = c1.recv().unwrap() {
            break *state;
        }
    };

    // 3. 在 daemon 端產生第一批事件 (例如 Spawn 假 session)
    let mut admin = Client::connect().unwrap();
    admin
        .send(&Request::Spawn(SpawnRequest {
            kind: Some(AgentKind::ClaudeCode),
            cwd: Some(root.to_string_lossy().to_string()),
            label: Some("s1".into()),
            role: Default::default(),
            prompt: None,
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

    // Client 1 收到 Spawn 相關的 Record
    let deadline = Instant::now() + Duration::from_secs(5);
    while client_state.sessions.is_empty() {
        assert!(Instant::now() < deadline, "Client 1 未收到 Spawn 事件");
        if let Frame::Record { record } = c1.recv().unwrap() {
            client_state.apply(&record);
        }
    }
    // 等待 s1 狀態事件到達，讓 Client 1 消化完畢
    std::thread::sleep(Duration::from_millis(50));
    while let Ok(frame) = c1.recv() {
        if let Frame::Record { record } = frame {
            client_state.apply(&record);
            break;
        }
    }
    let seq_at_disconnect = client_state.seq;
    assert!(
        seq_at_disconnect >= 2,
        "應該已經消化完成 s1 的啟動與狀態事件"
    );

    // 4. 強制中斷 Client 1 連線
    drop(c1);

    // 5. 斷線期間，daemon 產生第二批事件 (Spawn 第二個 session)
    admin
        .send(&Request::Spawn(SpawnRequest {
            kind: Some(AgentKind::ClaudeCode),
            cwd: Some(root.to_string_lossy().to_string()),
            label: Some("s2".into()),
            role: Default::default(),
            prompt: None,
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

    // 等待 daemon 處理完成第二個 session
    std::thread::sleep(Duration::from_millis(150));

    // 取得目前 daemon 最新 seq
    let mut snap_c = Client::connect().unwrap();
    snap_c.send(&Request::Snapshot).unwrap();
    let daemon_state = loop {
        if let Frame::Snapshot { state } = snap_c.recv().unwrap() {
            break *state;
        }
    };
    let target_seq = daemon_state.seq;
    assert!(target_seq > seq_at_disconnect, "daemon seq 應已推進");

    // 6. Client 2 模擬重連：帶 since = seq_at_disconnect 與相同的 epoch
    let mut c2 = Client::connect().unwrap();
    c2.send(&Request::Hello {
        client: "tui".into(),
        since: Some(seq_at_disconnect),
        token: Some(auto_token),
        epoch: Some(epoch.clone()),
    })
    .unwrap();

    // 第一個 frame 必須是 Ready (相同 epoch)
    let reconnected_epoch = loop {
        if let Frame::Ready { epoch: ep, .. } = c2.recv().unwrap() {
            break ep;
        }
    };
    assert_eq!(reconnected_epoch, epoch, "重連後 epoch 應一致");

    // 7. 驗收條件 3：後續收到的 frame 必須只有增量 Record，絕對不可收到完整 Snapshot！
    let mut got_diff_records = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while client_state.seq < target_seq {
        assert!(
            Instant::now() < deadline,
            "Client 2 未收到重連增量事件至 target_seq"
        );
        match c2.recv().unwrap() {
            Frame::Record { record } => {
                assert!(
                    record.seq > seq_at_disconnect,
                    "增量 record.seq ({}) 必須大於斷線前的 seq ({})",
                    record.seq,
                    seq_at_disconnect
                );
                client_state.apply(&record);
                got_diff_records = true;
            }
            Frame::Snapshot { .. } => {
                panic!("重連時 since 在 backlog 內，不應收到全量 Snapshot！");
            }
            _ => {}
        }
    }
    assert!(got_diff_records, "應成功收到增量 Record");

    // 8. 與 daemon 最新快照比對，確認 State 完全一致且銜接無誤
    assert_eq!(
        client_state.seq, daemon_state.seq,
        "客戶端與 daemon 的 seq 必須一致"
    );
    assert_eq!(
        client_state.sessions.len(),
        daemon_state.sessions.len(),
        "客戶端與 daemon 的 session 數量必須一致"
    );

    // 9. 清理關閉 daemon
    snap_c.send(&Request::Shutdown).unwrap();
    let _ = fs::remove_dir_all(&root);
}
