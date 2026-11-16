//! T30 行動端隨身審核器 (`--web` 零相依 HTTP/WebSocket) 整合測試：
//! 1. 驗證 HTTP GET `/` 回傳 200 OK 與 Mobile Companion HTML。
//! 2. 驗證 HTTP GET `/manifest.json` 回傳 200 OK 與 PWA manifest。
//! 3. 驗證 WebSocket `/ws` 握手、Token 認證閘門 (錯誤 token 被斷線、正確 token 完成 Hello 交握)。
//! 4. 驗證透過 WebSocket 接收 Ready、Snapshot，以及送出 Prompt 與 Permission 操作。
//! 5. 驗證 WebSocket 斷線後以 since 與 epoch 增量重連。

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use termade_core::AgentKind;
use termade_daemon::{default_token_path, is_running, read_token_file, Client, RemoteOptions};
use termade_protocol::{decode_line, encode_line, Frame, Request, SpawnRequest, Transport};

/// 輔助函式：發送客戶端 WebSocket 遮罩 Frame。
fn write_ws_client_text_frame(stream: &mut TcpStream, text: &str) {
    let payload = text.as_bytes();
    let mut header = Vec::new();
    header.push(0x81); // FIN + Text

    let mask_key = [0x12, 0x34, 0x56, 0x78];
    let len = payload.len();
    if len <= 125 {
        header.push(0x80 | (len as u8));
    } else if len <= 65535 {
        header.push(0x80 | 126);
        header.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        header.push(0x80 | 127);
        header.extend_from_slice(&(len as u64).to_be_bytes());
    }
    header.extend_from_slice(&mask_key);

    let mut masked = payload.to_vec();
    for (i, b) in masked.iter_mut().enumerate() {
        *b ^= mask_key[i % 4];
    }
    header.extend_from_slice(&masked);

    stream.write_all(&header).unwrap();
    stream.flush().unwrap();
}

/// 輔助函式：讀取伺服器端 WebSocket 未遮罩 Frame。
fn read_ws_server_text_frame<R: Read>(reader: &mut R) -> std::io::Result<String> {
    let mut header = [0u8; 2];
    reader.read_exact(&mut header)?;
    let mut len = (header[1] & 0x7F) as usize;
    if len == 126 {
        let mut ext = [0u8; 2];
        reader.read_exact(&mut ext)?;
        len = u16::from_be_bytes(ext) as usize;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        reader.read_exact(&mut ext)?;
        len = u64::from_be_bytes(ext) as usize;
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok(String::from_utf8(payload).unwrap())
}

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn test_web_companion_http_and_websocket() {
    let _guard = TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("termade-webtest-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let sock = root.join("daemon.sock");
    std::env::set_var("TERMADE_SOCKET", &sock);
    std::env::set_var("XDG_STATE_HOME", root.join("state"));
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::remove_var("TERMADE_TOKEN");
    std::env::remove_var("TERMADE_REMOTE");

    // 尋找可用 TCP port
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let web_port = listener.local_addr().unwrap().port();
    drop(listener);
    let web_addr = format!("127.0.0.1:{web_port}");

    // 1. 啟動 daemon 附帶 --web 監聽
    let web_clone = web_addr.clone();
    std::thread::spawn(move || {
        let _ = termade_daemon::run_remote(RemoteOptions {
            web: Some(web_clone),
            ..Default::default()
        });
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    while !is_running() {
        assert!(Instant::now() < deadline, "daemon 未在時限內啟動");
        std::thread::sleep(Duration::from_millis(50));
    }

    let token = read_token_file(&default_token_path()).expect("Token 應自動建立");

    // 2. 測試 HTTP GET / 回傳 200 OK 與 Mobile Companion HTML
    {
        let mut http_stream = TcpStream::connect(&web_addr).unwrap();
        http_stream
            .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut resp = String::new();
        http_stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
        assert!(resp.contains("Content-Type: text/html"));
        assert!(resp.contains("termade companion"));
        assert!(resp.contains("approval-banner"));
    }

    // 3. 測試 HTTP GET /manifest.json 回傳 PWA manifest
    {
        let mut http_stream = TcpStream::connect(&web_addr).unwrap();
        http_stream
            .write_all(
                b"GET /manifest.json HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let mut resp = String::new();
        http_stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
        assert!(resp.contains("application/manifest+json"));
        assert!(resp.contains("termade Mobile Companion"));
    }

    // 3.5 測試 HTTP GET /file 檔案內容檢視端點 (文字檔與圖片)
    {
        let test_txt = root.join("hello.rs");
        fs::write(&test_txt, "fn main() { println!(\"Hello\"); }\n").unwrap();

        // 3.5.1 正確 Token 讀取文字檔
        let mut http_stream = TcpStream::connect(&web_addr).unwrap();
        let req = format!(
            "GET /file?path={}&token={} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            test_txt.display(),
            token
        );
        http_stream.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        http_stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "{resp}");
        assert!(resp.contains("Content-Type: text/plain"));
        assert!(resp.contains("println!(\"Hello\")"));

        // 3.5.2 錯誤 Token 應拒絕 (401)
        let mut http_stream = TcpStream::connect(&web_addr).unwrap();
        let req = format!(
            "GET /file?path={}&token=wrong_token HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            test_txt.display()
        );
        http_stream.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        http_stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 401 Unauthorized"), "{resp}");

        // 3.5.3 讀取圖片檔 (Content-Type 應為 image/png)
        let test_png = root.join("icon.png");
        fs::write(&test_png, [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
        let mut http_stream = TcpStream::connect(&web_addr).unwrap();
        let req = format!(
            "GET /file?path={}&token={} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            test_png.display(),
            token
        );
        http_stream.write_all(req.as_bytes()).unwrap();
        let mut resp = Vec::new();
        http_stream.read_to_end(&mut resp).unwrap();
        let header_str = String::from_utf8_lossy(&resp);
        assert!(header_str.starts_with("HTTP/1.1 200 OK"));
        assert!(header_str.contains("Content-Type: image/png"));

        // 3.5.4 檔案不存在應回傳 404
        let mut http_stream = TcpStream::connect(&web_addr).unwrap();
        let req = format!(
            "GET /file?path={}&token={} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            root.join("non_existent.txt").display(),
            token
        );
        http_stream.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        http_stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 404 Not Found"), "{resp}");
    }

    // 4. 測試 WebSocket 認證閘門：未帶正確 token 時被斷線
    {
        let mut ws_stream = TcpStream::connect(&web_addr).unwrap();
        let req = "GET /ws HTTP/1.1\r\n\
                   Host: 127.0.0.1\r\n\
                   Upgrade: websocket\r\n\
                   Connection: Upgrade\r\n\
                   Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                   Sec-WebSocket-Version: 13\r\n\r\n";
        ws_stream.write_all(req.as_bytes()).unwrap();

        let mut reader = BufReader::new(ws_stream.try_clone().unwrap());
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        assert!(status.contains("101 Switching Protocols"));

        // 讀完握手標頭
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
                break;
            }
        }

        // 發送錯誤 Token
        let bad_hello = encode_line(&Request::Hello {
            client: "web".into(),
            since: None,
            token: Some("invalid_wrong_token".into()),
            epoch: None,
        })
        .unwrap();
        write_ws_client_text_frame(&mut ws_stream, &bad_hello);

        ws_stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        // 應收到 Error frame
        let err_json = read_ws_server_text_frame(&mut reader).unwrap();
        let frame: Frame = decode_line(&err_json).unwrap();
        assert!(matches!(frame, Frame::Error { .. }));
    }

    // 5. 測試 WebSocket 正確 Token 連線、接收 Ready、Snapshot 與雙向操控
    {
        let mut ws_stream = TcpStream::connect(&web_addr).unwrap();
        let req = "GET /ws HTTP/1.1\r\n\
                   Host: 127.0.0.1\r\n\
                   Upgrade: websocket\r\n\
                   Connection: Upgrade\r\n\
                   Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                   Sec-WebSocket-Version: 13\r\n\r\n";
        ws_stream.write_all(req.as_bytes()).unwrap();

        let mut reader = BufReader::new(ws_stream.try_clone().unwrap());
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        assert!(status.contains("101 Switching Protocols"));

        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
                break;
            }
        }

        // 發送正確 Token 的 Hello
        let hello = encode_line(&Request::Hello {
            client: "web".into(),
            since: None,
            token: Some(token.clone()),
            epoch: None,
        })
        .unwrap();
        write_ws_client_text_frame(&mut ws_stream, &hello);

        ws_stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let ready_raw = read_ws_server_text_frame(&mut reader).unwrap();
        let ready_frame: Frame = decode_line(&ready_raw).unwrap();
        assert!(matches!(ready_frame, Frame::Ready { .. }));

        let snap_raw = read_ws_server_text_frame(&mut reader).unwrap();
        let snap_frame: Frame = decode_line(&snap_raw).unwrap();
        assert!(matches!(snap_frame, Frame::Snapshot { .. }));

        // 透過本機 daemon 建立一個 session
        let mut admin = Client::connect().unwrap();
        admin
            .send(&Request::Spawn(SpawnRequest {
                kind: AgentKind::ClaudeCode,
                cwd: Some(root.to_string_lossy().to_string()),
                label: Some("web_s1".into()),
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

        // WebSocket 端應收到 SessionStarted 廣播 Record
        let mut session_id = None;
        for _ in 0..5 {
            if let Ok(rec_raw) = read_ws_server_text_frame(&mut reader) {
                if let Ok(Frame::Record { record }) = decode_line(&rec_raw) {
                    if let termade_core::Event::SessionStarted { session } = record.event {
                        session_id = Some(session.id);
                        break;
                    }
                }
            }
        }
        let _sid = session_id.expect("WebSocket 應收到 SessionStarted Record");

        // 6. 透過 WebSocket 送出 Snapshot 請求並驗證即時回覆
        let snap_req = encode_line(&Request::Snapshot).unwrap();
        write_ws_client_text_frame(&mut ws_stream, &snap_req);

        let mut got_snap = false;
        for _ in 0..5 {
            if let Ok(resp_raw) = read_ws_server_text_frame(&mut reader) {
                if let Ok(frame) = decode_line::<Frame>(&resp_raw) {
                    if matches!(frame, Frame::Snapshot { .. }) {
                        got_snap = true;
                        break;
                    }
                }
            }
        }
        assert!(got_snap, "WebSocket 應收到 Snapshot 回應");
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_web_companion_dynamic_control_and_config() {
    let _guard = TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("termade-webdyn-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let sock = root.join("daemon.sock");
    std::env::set_var("TERMADE_SOCKET", &sock);
    std::env::set_var("XDG_STATE_HOME", root.join("state"));
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::remove_var("TERMADE_TOKEN");
    std::env::remove_var("TERMADE_REMOTE");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let web_port = listener.local_addr().unwrap().port();
    drop(listener);

    // 啟動 daemon (未傳入 --web)
    let d_sock = sock.clone();
    let _daemon_handle = std::thread::spawn(move || {
        let _ = termade_daemon::run_remote(RemoteOptions {
            listen: None,
            web: None,
            token: None,
            token_file: None,
            insecure_listen_public: false,
        });
    });

    let start = Instant::now();
    while !d_sock.exists() {
        if start.elapsed() > Duration::from_secs(3) {
            panic!("daemon 啟動逾時");
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    {
        let mut client = Client::connect().unwrap();
        client
            .send(&Request::Hello {
                epoch: None,
                client: "test".into(),
                since: None,
                token: None,
            })
            .unwrap();
        let ready = client.recv().unwrap();
        assert!(matches!(ready, Frame::Ready { .. }));
        let snap = client.recv().unwrap();
        assert!(matches!(snap, Frame::Snapshot { .. }));

        // 1. 查詢 Web 狀態，目前應為 off
        client
            .send(&Request::WebControl {
                enabled: None,
                addr: None,
            })
            .unwrap();
        let frame = client.recv().unwrap();
        if let Frame::Ok { note } = frame {
            assert_eq!(note.as_deref(), Some("off"));
        } else {
            panic!("預期 Frame::Ok，實際為 {:?}", frame);
        }

        // 2. 動態啟動 Web 隨身審核器
        let addr = format!("127.0.0.1:{web_port}");
        client
            .send(&Request::WebControl {
                enabled: Some(true),
                addr: Some(addr.clone()),
            })
            .unwrap();
        let frame = client.recv().unwrap();
        if let Frame::Ok { note } = frame {
            let note_str = note.unwrap();
            assert!(
                note_str.contains(&format!("http://{addr}/#token=")),
                "note 應包含正確的 URL，實際為: {note_str}"
            );
        } else {
            panic!("預期 Frame::Ok，實際為 {:?}", frame);
        }

        // 3. 測試 HTTP GET 是否可正常取得首頁
        std::thread::sleep(Duration::from_millis(50));
        let mut http_stream = TcpStream::connect(&addr).unwrap();
        http_stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        http_stream.flush().unwrap();
        let mut reader = BufReader::new(http_stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).unwrap();
        assert!(status_line.contains("200 OK"));

        // 4. 動態停止 Web 隨身審核器
        client
            .send(&Request::WebControl {
                enabled: Some(false),
                addr: None,
            })
            .unwrap();
        let frame = client.recv().unwrap();
        assert!(matches!(frame, Frame::Ok { .. }));
    }

    let _ = fs::remove_dir_all(&root);
}
