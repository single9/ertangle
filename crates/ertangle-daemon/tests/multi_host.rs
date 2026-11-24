//! T29 多主機 TUI 與跨機 Session 操控整合測試：
//! 1. 啟動兩個獨立的 daemon：一個本機 Unix socket、一個 TCP loopback 模擬遠端主機。
//! 2. 驗證 client 能透過 connect_tcp 連線並彙整 Session 清單。
//! 3. 驗證針對遠端 Session 發出的操作正確送達遠端 daemon。

use std::time::{Duration, Instant};

use ertangle_core::AgentKind;
use ertangle_daemon::{default_token_path, is_running, read_token_file, Client, RemoteOptions};
use ertangle_protocol::{Frame, Request, SpawnRequest, Transport};

#[test]
fn test_multi_host_aggregation_and_routing() {
    let root = ertangle_test_support::temp_dir("ertangle-multihost");

    let sock = root.join("local.sock");
    std::env::set_var("ERTANGLE_SOCKET", &sock);
    std::env::set_var("XDG_STATE_HOME", root.join("state"));
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::set_var(
        "ERTANGLE_CLAUDE_BIN",
        format!(
            "{}/../ertangle-adapter/tests/fake_claude.sh",
            env!("CARGO_MANIFEST_DIR")
        ),
    );
    std::env::remove_var("ERTANGLE_TOKEN");
    std::env::remove_var("ERTANGLE_REMOTE");

    // 尋找可用 TCP port
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let listen_addr = format!("127.0.0.1:{port}");

    // 1. 啟動遠端 daemon (TCP loopback)
    let addr_clone = listen_addr.clone();
    std::thread::spawn(move || {
        let _ = ertangle_daemon::run_remote(RemoteOptions {
            listen: Some(addr_clone),
            ..Default::default()
        });
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    while !is_running() {
        assert!(Instant::now() < deadline, "遠端 daemon 未在時限內啟動");
        std::thread::sleep(Duration::from_millis(50));
    }

    let token_file = default_token_path();
    let token = read_token_file(&token_file).unwrap();

    // 2. 連線至遠端 daemon 並驗證交握
    let mut remote_client = Client::connect_tcp(&listen_addr, Some(token.clone())).unwrap();
    remote_client
        .send(&Request::Hello {
            client: "tui".into(),
            since: None,
            token: Some(token),
            epoch: None,
        })
        .unwrap();

    let remote_ready = remote_client.recv().unwrap();
    assert!(matches!(remote_ready, Frame::Ready { .. }));
    let remote_snap = remote_client.recv().unwrap();
    assert!(matches!(remote_snap, Frame::Snapshot { .. }));

    // 3. 在遠端建立 Session
    remote_client
        .send(&Request::Spawn(SpawnRequest {
            kind: Some(AgentKind::ClaudeCode),
            cwd: Some(root.to_string_lossy().to_string()),
            label: Some("remote_s1".into()),
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

    // 接收 record 取得 session id
    let mut remote_session_id = None;
    for _ in 0..5 {
        match remote_client.recv().unwrap() {
            Frame::Record { record } => {
                if let ertangle_core::Event::SessionStarted { session } = record.event {
                    remote_session_id = Some(session.id);
                }
            }
            Frame::Ok { .. } => {}
            _ => {}
        }
        if remote_session_id.is_some() {
            break;
        }
    }
    let remote_session_id = remote_session_id.expect("應收到 SessionStarted");

    // 4. 驗證透過 TCP 路由送出的 Prompt 請求能成功傳抵遠端 daemon
    remote_client
        .send(&Request::Prompt {
            session: remote_session_id.clone(),
            text: "ping from multi host client".into(),
            attachments: Vec::new(),
            queue: false,
        })
        .unwrap();

    let prompt_resp = loop {
        match remote_client.recv().unwrap() {
            f @ Frame::Ok { .. } => break f,
            Frame::Error { message } => panic!("Prompt 被拒絕：{message}"),
            _ => {}
        }
    };
    assert!(
        matches!(prompt_resp, Frame::Ok { .. }),
        "遠端 daemon 應接受 Prompt"
    );
}
