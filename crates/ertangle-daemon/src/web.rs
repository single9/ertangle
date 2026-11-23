//! 同步 HTTP 與 RFC 6455 WebSocket 端點 (Web 版支援 / Web Companion)。
//! 零外部 async/HTTP 框架相依性，直接以 `std::net::TcpListener` 與純 Rust 實作。

pub mod qr;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ertangle_core::base64_encode;
use ertangle_protocol::{decode_line, encode_line, Frame, Request};
use tr::tr;

use crate::client_channel;
use crate::server::{token_matches, AUTH_FAIL_DELAY};
use crate::Shared;

const INDEX_HTML: &str = include_str!("web/index.html");
const MANIFEST_JSON: &str = include_str!("web/manifest.json");

/// 計算 SHA-1 雜湊值 (RFC 3174)。
#[allow(clippy::chunks_exact_to_as_chunks, clippy::needless_range_loop)]
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x67452301;
    let mut h1: u32 = 0xEFCDAB89;
    let mut h2: u32 = 0x98BADCFE;
    let mut h3: u32 = 0x10325476;
    let mut h4: u32 = 0xC3D2E1F0;

    let ml = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;

        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

/// 計算 WebSocket Accept 金鑰。
pub fn compute_ws_accept(key: &str) -> String {
    let concat = format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let hash = sha1(concat.as_bytes());
    base64_encode(&hash)
}

/// 啟動 Web 版 HTTP/WebSocket 服務。
pub(crate) fn start_web_server(
    shared: Arc<Shared>,
    addr: &str,
    running: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    let listener = TcpListener::bind(addr)
        .with_context(|| tr!("綁定 Web 伴侶服務位址 {addr} 失敗", addr = addr))?;
    listener.set_nonblocking(true)?;

    let handle = std::thread::spawn(move || {
        while running.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let shared = shared.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = handle_http_or_ws(shared, stream) {
                            tracing::debug!("Web client error: {e}");
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
    });

    Ok(handle)
}

fn handle_http_or_ws(shared: Arc<Shared>, mut stream: TcpStream) -> Result<()> {
    // macOS (BSD) 上 accept() 會繼承 listener 的 non-blocking 屬性，需明確設回阻塞模式。
    stream.set_nonblocking(false)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first_line = String::new();
    if reader.read_line(&mut first_line)? == 0 {
        return Ok(());
    }

    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        bail!("無效的 HTTP 請求列");
    }
    let method = parts[0];
    let path = parts[1];

    let mut headers = Vec::new();
    let mut ws_key = None;
    let mut is_upgrade = false;

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("sec-websocket-key:") {
            if let Some(val) = line.split(':').nth(1) {
                ws_key = Some(val.trim().to_string());
            }
        }
        if lower.starts_with("upgrade:") && lower.contains("websocket") {
            is_upgrade = true;
        }
        headers.push(line);
    }

    // 1. CORS 預檢請求
    if method == "OPTIONS" {
        let resp = "HTTP/1.1 204 No Content\r\n\
                    Access-Control-Allow-Origin: *\r\n\
                    Access-Control-Allow-Methods: GET, OPTIONS\r\n\
                    Access-Control-Allow-Headers: Authorization, Content-Type\r\n\
                    Access-Control-Max-Age: 86400\r\n\r\n";
        stream.write_all(resp.as_bytes())?;
        stream.flush()?;
        return Ok(());
    }

    let (req_path, query_str) = match path.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path, None),
    };

    // 2. WebSocket 升級請求
    if method == "GET" && req_path == "/ws" && is_upgrade {
        let Some(key) = ws_key else {
            stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\nMissing Sec-WebSocket-Key")?;
            return Ok(());
        };
        let accept = compute_ws_accept(&key);
        let resp = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        stream.write_all(resp.as_bytes())?;
        stream.flush()?;

        return handle_ws_session(shared, stream, reader);
    }

    // 3. 檔案內容檢視端點 (文字與圖片等)
    if method == "GET" && (req_path == "/file" || req_path == "/api/file") {
        return serve_file(&shared, &mut stream, query_str, &headers);
    }

    // 4. 靜態資源處理
    match (method, req_path) {
        ("GET", "/") | ("GET", "/index.html") => {
            let resp = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\n\
                 Cache-Control: no-cache\r\n\
                 X-Content-Type-Options: nosniff\r\n\
                 \r\n{}",
                INDEX_HTML.len(),
                INDEX_HTML
            );
            stream.write_all(resp.as_bytes())?;
        }
        ("GET", "/manifest.json") => {
            let resp = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/manifest+json\r\n\
                 Content-Length: {}\r\n\
                 \r\n{}",
                MANIFEST_JSON.len(),
                MANIFEST_JSON
            );
            stream.write_all(resp.as_bytes())?;
        }
        _ => {
            stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nNot Found")?;
        }
    }
    stream.flush()?;
    Ok(())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn mime_type_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        // 圖片
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "avif" => "image/avif",

        // 程式碼與純文字
        "txt" | "log" => "text/plain; charset=utf-8",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "rs" => "text/plain; charset=utf-8",
        "toml" => "text/plain; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "yaml" | "yml" => "text/plain; charset=utf-8",
        "html" | "htm" => "text/plain; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" | "cjs" | "ts" => "text/plain; charset=utf-8",
        "py" => "text/plain; charset=utf-8",
        "sh" | "bash" | "zsh" => "text/plain; charset=utf-8",
        "c" | "cpp" | "cc" | "h" | "hpp" => "text/plain; charset=utf-8",
        "go" => "text/plain; charset=utf-8",
        "sql" => "text/plain; charset=utf-8",
        "diff" | "patch" => "text/plain; charset=utf-8",

        // 文件
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// 檢查 canonical 目標路徑是否位於任一被允許的根目錄下。
pub(crate) fn is_path_allowed(canonical_target: &Path, allowed_roots: &[PathBuf]) -> bool {
    allowed_roots
        .iter()
        .any(|root| canonical_target.starts_with(root))
}

fn serve_file(
    shared: &Shared,
    stream: &mut TcpStream,
    query_str: Option<&str>,
    headers: &[String],
) -> Result<()> {
    let mut target_path = None;
    let mut session_id = None;
    let mut query_token = None;

    if let Some(qs) = query_str {
        for pair in qs.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                match k {
                    "path" => target_path = Some(percent_decode(v)),
                    "session" => session_id = Some(percent_decode(v)),
                    "token" => query_token = Some(percent_decode(v)),
                    _ => {}
                }
            }
        }
    }

    let mut auth_token = query_token;
    if auth_token.is_none() {
        for h in headers {
            let lower = h.to_ascii_lowercase();
            if lower.starts_with("authorization:") {
                if let Some(val) = h.split(':').nth(1) {
                    let val = val.trim();
                    if let Some(tok) = val.strip_prefix("Bearer ") {
                        auth_token = Some(tok.trim().to_string());
                    }
                }
            }
        }
    }

    let expected_token = shared.auth_token();
    if let Some(expected) = expected_token.as_deref() {
        if !token_matches(auth_token.as_deref(), expected) {
            let resp = "HTTP/1.1 401 Unauthorized\r\n\
                        Content-Type: text/plain; charset=utf-8\r\n\
                        Access-Control-Allow-Origin: *\r\n\
                        \r\n未認證或 Token 無效";
            stream.write_all(resp.as_bytes())?;
            stream.flush()?;
            return Ok(());
        }
    }

    let Some(mut raw_path) = target_path else {
        let resp = "HTTP/1.1 400 Bad Request\r\n\
                    Content-Type: text/plain; charset=utf-8\r\n\
                    Access-Control-Allow-Origin: *\r\n\
                    \r\n缺少 path 參數";
        stream.write_all(resp.as_bytes())?;
        stream.flush()?;
        return Ok(());
    };

    if let Some(rest) = raw_path.strip_prefix("file://") {
        raw_path = rest.to_string();
    }
    if let Some(rest) = raw_path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            raw_path = format!("{home}/{rest}");
        }
    }
    if let Some((clean, _)) = raw_path.split_once('#') {
        raw_path = clean.to_string();
    }

    let mut resolved_path = PathBuf::from(&raw_path);
    let mut allowed_roots = Vec::new();
    if let Ok(inner) = shared.inner.lock() {
        if let Ok(c) = inner.default_cwd.canonicalize() {
            allowed_roots.push(c);
        }
        for s in inner.state.sessions.values() {
            if !s.cwd.is_empty() {
                if let Ok(c) = Path::new(&s.cwd).canonicalize() {
                    allowed_roots.push(c);
                }
            }
            if let Some(repo) = &s.repo_root {
                if let Ok(c) = Path::new(repo).canonicalize() {
                    allowed_roots.push(c);
                }
            }
        }
        if resolved_path.is_relative() {
            let mut found = false;
            if let Some(sid) = session_id.as_deref() {
                if let Some(s) = inner.state.sessions.get(sid) {
                    if !s.cwd.is_empty() {
                        let candidate = Path::new(&s.cwd).join(&resolved_path);
                        if candidate.is_file() {
                            resolved_path = candidate;
                            found = true;
                        }
                    }
                    if !found {
                        if let Some(repo) = &s.repo_root {
                            let candidate = Path::new(repo).join(&resolved_path);
                            if candidate.is_file() {
                                resolved_path = candidate;
                                found = true;
                            }
                        }
                    }
                }
            }
            if !found {
                let candidate = inner.default_cwd.join(&resolved_path);
                if candidate.is_file() {
                    resolved_path = candidate;
                }
            }
        }
    }

    // T83: Also allow known agent artifact/state directories under $HOME
    if let Ok(home) = std::env::var("HOME") {
        let home_path = Path::new(&home);
        let agent_subdirs = [
            ".gemini/antigravity-cli/brain",
            ".ertangle",
            ".claude",
            ".codex",
        ];
        for rel in &agent_subdirs {
            let dir = home_path.join(rel);
            if dir.is_dir() {
                if let Ok(c) = dir.canonicalize() {
                    allowed_roots.push(c);
                }
            }
        }
    }

    let resolved_path = match resolved_path.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            let body = format!("找不到檔案：{}", resolved_path.display());
            let resp = format!(
                "HTTP/1.1 404 Not Found\r\n\
                 Content-Type: text/plain; charset=utf-8\r\n\
                 Content-Length: {}\r\n\
                 Access-Control-Allow-Origin: *\r\n\
                 \r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes())?;
            stream.flush()?;
            return Ok(());
        }
    };

    if !resolved_path.is_file() {
        let body = format!("找不到檔案：{}", resolved_path.display());
        let resp = format!(
            "HTTP/1.1 404 Not Found\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             Content-Length: {}\r\n\
             Access-Control-Allow-Origin: *\r\n\
             \r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes())?;
        stream.flush()?;
        return Ok(());
    }

    if !is_path_allowed(&resolved_path, &allowed_roots) {
        let body = "拒絕存取：路徑不在工作區範圍內";
        let resp = format!(
            "HTTP/1.1 403 Forbidden\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             Content-Length: {}\r\n\
             Access-Control-Allow-Origin: *\r\n\
             \r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes())?;
        stream.flush()?;
        return Ok(());
    }

    let meta = match std::fs::metadata(&resolved_path) {
        Ok(m) => m,
        Err(e) => {
            let body = format!("讀取檔案屬性失敗：{e}");
            let resp = format!(
                "HTTP/1.1 500 Internal Server Error\r\n\
                 Content-Type: text/plain; charset=utf-8\r\n\
                 Content-Length: {}\r\n\
                 Access-Control-Allow-Origin: *\r\n\
                 \r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes())?;
            stream.flush()?;
            return Ok(());
        }
    };

    if meta.len() > 30 * 1024 * 1024 {
        let body = "檔案過大 (超過 30MB 限制)";
        let resp = format!(
            "HTTP/1.1 413 Payload Too Large\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             Content-Length: {}\r\n\
             Access-Control-Allow-Origin: *\r\n\
             \r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes())?;
        stream.flush()?;
        return Ok(());
    }

    let bytes = match std::fs::read(&resolved_path) {
        Ok(b) => b,
        Err(e) => {
            let body = format!("讀取檔案失敗：{e}");
            let resp = format!(
                "HTTP/1.1 500 Internal Server Error\r\n\
                 Content-Type: text/plain; charset=utf-8\r\n\
                 Content-Length: {}\r\n\
                 Access-Control-Allow-Origin: *\r\n\
                 \r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes())?;
            stream.flush()?;
            return Ok(());
        }
    };

    let mime = mime_type_for_path(&resolved_path);
    let header = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: {mime}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-cache\r\n\
         Access-Control-Allow-Origin: *\r\n\
         X-Content-Type-Options: nosniff\r\n\
         \r\n",
        bytes.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

fn handle_ws_session(
    shared: Arc<Shared>,
    mut write_stream: TcpStream,
    mut reader: BufReader<TcpStream>,
) -> Result<()> {
    let (tx, rx) = client_channel(write_stream.try_clone().ok().map(crate::ConnStream::Tcp));

    // 發送通道 (Server -> WebSocket Client: 封裝為 Text Frame)
    let mut ws_writer = write_stream.try_clone()?;
    std::thread::spawn(move || {
        for frame in rx {
            match encode_line(&frame) {
                Ok(line) => {
                    if send_ws_text_frame(&mut ws_writer, line.trim_end()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // 接收迴圈 (WebSocket Client -> Server)
    // 遠端連線必須經過 Token 認證
    let require_auth = shared.auth_token().is_some();
    let mut authenticated = !require_auth;

    loop {
        let (opcode, payload) = match read_ws_frame(&mut reader) {
            Ok(res) => res,
            Err(_) => break,
        };

        match opcode {
            // Text frame
            1 => {
                let text = match String::from_utf8(payload) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                match decode_line::<Request>(text.trim()) {
                    Ok(req) => {
                        if !authenticated {
                            if let Request::Hello { ref token, .. } = req {
                                let ok = shared
                                    .auth_token()
                                    .as_deref()
                                    .is_some_and(|exp| token_matches(token.as_deref(), exp));
                                if ok {
                                    authenticated = true;
                                } else {
                                    std::thread::sleep(AUTH_FAIL_DELAY);
                                    let _ = tx.send(Frame::error(tr!(
                                        "遠端認證失敗：請提供正確的 Token"
                                    )));
                                    break;
                                }
                            } else {
                                std::thread::sleep(AUTH_FAIL_DELAY);
                                let _ = tx.send(Frame::error(tr!(
                                    "未認證的連線：請先發送 Hello 帶 token"
                                )));
                                break;
                            }
                        }
                        shared.handle(&tx, req);
                    }
                    Err(e) => {
                        let _ = tx.send(Frame::error(tr!("無法解析 JSON：{e}", e = e)));
                    }
                }
            }
            // Ping -> 回應 Pong
            9 => {
                let _ = send_ws_frame(&mut write_stream, 10, &payload);
            }
            // Close frame
            8 => {
                let _ = send_ws_frame(&mut write_stream, 8, &[]);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

pub(crate) const MAX_WS_PAYLOAD_LEN: usize = 30 * 1024 * 1024;

/// 讀取單一 RFC 6455 WebSocket Frame。
fn read_ws_frame<R: Read>(reader: &mut R) -> Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 2];
    reader.read_exact(&mut header)?;

    let opcode = header[0] & 0x0F;
    let masked = (header[1] & 0x80) != 0;
    let mut len = (header[1] & 0x7F) as usize;

    if len == 126 {
        let mut ext = [0u8; 2];
        reader.read_exact(&mut ext)?;
        len = u16::from_be_bytes(ext) as usize;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        reader.read_exact(&mut ext)?;
        let raw_len = u64::from_be_bytes(ext);
        if raw_len > MAX_WS_PAYLOAD_LEN as u64 {
            bail!("WebSocket 承載過大 (超過 30MB 限制)");
        }
        len = raw_len as usize;
    }

    if len > MAX_WS_PAYLOAD_LEN {
        bail!("WebSocket 承載過大 (超過 30MB 限制)");
    }

    let mask_key = if masked {
        let mut key = [0u8; 4];
        reader.read_exact(&mut key)?;
        Some(key)
    } else {
        None
    };

    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;

    if let Some(mask) = mask_key {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
    }

    Ok((opcode, payload))
}

/// 發送 RFC 6455 WebSocket Text Frame (未遮罩，符合伺服器端規範)。
fn send_ws_text_frame<W: Write>(writer: &mut W, text: &str) -> Result<()> {
    send_ws_frame(writer, 1, text.as_bytes())
}

/// 發送 RFC 6455 Frame。
fn send_ws_frame<W: Write>(writer: &mut W, opcode: u8, data: &[u8]) -> Result<()> {
    let mut header = Vec::with_capacity(10);
    // FIN (0x80) | Opcode
    header.push(0x80 | (opcode & 0x0F));

    let len = data.len();
    if len <= 125 {
        header.push(len as u8);
    } else if len <= 65535 {
        header.push(126);
        header.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        header.push(127);
        header.extend_from_slice(&(len as u64).to_be_bytes());
    }

    writer.write_all(&header)?;
    writer.write_all(data)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha1_known_vectors() {
        assert_eq!(
            hex::encode(sha1(b"")),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            hex::encode(sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }

    #[test]
    fn test_ws_accept_known_key() {
        // RFC 6455 範例金鑰
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let accept = compute_ws_accept(key);
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("foo+bar%2Fbaz"), "foo bar/baz");
        assert_eq!(percent_decode("%E6%B8%AC%E8%A9%A6"), "測試");
    }

    #[test]
    fn test_mime_type_for_path() {
        assert_eq!(mime_type_for_path(Path::new("photo.png")), "image/png");
        assert_eq!(mime_type_for_path(Path::new("pic.jpg")), "image/jpeg");
        assert_eq!(
            mime_type_for_path(Path::new("src/main.rs")),
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            mime_type_for_path(Path::new("Cargo.toml")),
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            mime_type_for_path(Path::new("data.json")),
            "application/json; charset=utf-8"
        );
        assert_eq!(
            mime_type_for_path(Path::new("bin/app")),
            "application/octet-stream"
        );
    }

    #[test]
    fn test_read_ws_frame_exceeds_max_payload() {
        let mut data = Vec::new();
        data.push(0x81); // opcode 1
        data.push(127); // 64-bit len
        let oversized = (MAX_WS_PAYLOAD_LEN as u64) + 1;
        data.extend_from_slice(&oversized.to_be_bytes());
        let mut cursor = std::io::Cursor::new(data);
        let res = read_ws_frame(&mut cursor);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("超過 30MB 限制"));
    }

    #[test]
    fn test_read_ws_frame_valid_frames() {
        // Unmasked frame
        let unmasked = vec![0x81, 0x05, b'h', b'e', b'l', b'l', b'o'];
        let (opcode, payload) = read_ws_frame(&mut std::io::Cursor::new(unmasked)).unwrap();
        assert_eq!(opcode, 1);
        assert_eq!(payload, b"hello");

        // Masked frame
        let mask = [0x12, 0x34, 0x56, 0x78];
        let original = b"world";
        let mut masked_payload = Vec::new();
        for (i, &b) in original.iter().enumerate() {
            masked_payload.push(b ^ mask[i % 4]);
        }
        let mut frame = vec![0x81, 0x80 | 0x05];
        frame.extend_from_slice(&mask);
        frame.extend_from_slice(&masked_payload);

        let (opcode, payload) = read_ws_frame(&mut std::io::Cursor::new(frame)).unwrap();
        assert_eq!(opcode, 1);
        assert_eq!(payload, b"world");
    }

    #[test]
    fn test_is_path_allowed() {
        let tmp = std::env::temp_dir().canonicalize().unwrap();
        let sub = tmp.join("ertangle_test_allowed");
        let allowed_roots = vec![sub.clone()];

        let child = sub.join("file.txt");
        assert!(is_path_allowed(&child, &allowed_roots));
        assert!(is_path_allowed(&sub, &allowed_roots));

        // Outside path: /etc/passwd
        assert!(!is_path_allowed(Path::new("/etc/passwd"), &allowed_roots));

        // Sibling directory or parent
        let sibling = tmp.join("ertangle_test_sibling");
        assert!(!is_path_allowed(&sibling, &allowed_roots));
        assert!(!is_path_allowed(&tmp, &allowed_roots));
    }

    #[test]
    fn question_banner_is_outside_tab_views() {
        let main_start = INDEX_HTML.find("<main>").expect("main element");
        let banner = INDEX_HTML
            .find("id=\"urgent-approval-banner\"")
            .expect("question banner");
        let first_tab = INDEX_HTML.find("class=\"tab-view").expect("tab view");
        assert!(main_start < banner && banner < first_tab);

        let css_start = INDEX_HTML.find(".urgent-banner {").expect("banner CSS");
        let css_end = INDEX_HTML[css_start..].find('}').expect("banner CSS end") + css_start;
        let css = &INDEX_HTML[css_start..css_end];
        assert!(css.contains("position: sticky;"));
        assert!(css.contains("top: 0;"));
    }

    #[test]
    fn question_card_is_vertical_and_survives_rerender() {
        // Option rows are vertical full-width labels, not crammed into .btn-row.
        assert!(INDEX_HTML.contains(".q-opt {"));
        assert!(INDEX_HTML.contains("class=\"q-opt-desc\""));
        assert!(INDEX_HTML.contains(".q-submit-row { display:flex; justify-content:flex-end;"));
        // Minimal list style: options sit in a divided list inside an accent-bar panel.
        assert!(
            INDEX_HTML.contains("<div class=\"q-list${q.multi_select ? \" q-multi\" : \"\"}\">")
        );
        assert!(INDEX_HTML.contains("q-group q-panel"));
        // Options show a 1-9 keycap and a right-side mark (✓ / checkbox) instead of native dots.
        assert!(INDEX_HTML.contains("<span class=\"q-key\">"));
        assert!(INDEX_HTML.contains("<span class=\"q-mark\"></span>"));
        assert!(
            INDEX_HTML.contains("document.addEventListener(\"keydown\", handleQuestionDigitKey);")
        );
        // iOS Safari zooms on inputs below 16px.
        assert!(INDEX_HTML.contains("border:none; outline:none; font-size:16px;"));
        assert!(INDEX_HTML.contains("min-height:44px"));
        // Re-render is gated by a signature so selections and custom text persist.
        assert!(INDEX_HTML.contains("function setCardHtml("));
        assert!(INDEX_HTML.contains("el.dataset.sig === sig"));
        assert!(!INDEX_HTML.contains("urgentActionRow.innerHTML = questionAnswerHtml"));
        // Grouped questions share one submit that sends one op per question.
        assert!(INDEX_HTML.contains("function questionGroupOf("));
        assert!(INDEX_HTML.contains("function submitQuestionGroup("));
        assert!(INDEX_HTML.contains("payloads.every(p => !p.answers.length)"));
        assert!(INDEX_HTML.contains("op: \"answer_question\""));
        // Enter in the custom input submits.
        assert!(INDEX_HTML.contains("event.key==='Enter'"));
    }

    #[test]
    fn chat_question_renders_inline_above_composer() {
        let slot = INDEX_HTML
            .find("id=\"chat-question-slot\"")
            .expect("inline question slot");
        let transcript = INDEX_HTML
            .find("id=\"chat-transcript\"")
            .expect("transcript");
        let composer = INDEX_HTML.find("id=\"chat-composer\"").expect("composer");
        assert!(transcript < slot && slot < composer);
        let css_start = INDEX_HTML.find("#chat-question-slot {").expect("slot CSS");
        let css_end = INDEX_HTML[css_start..].find('}').expect("end") + css_start;
        let css = &INDEX_HTML[css_start..css_end];
        assert!(css.contains("max-height:50vh"));
        assert!(css.contains("overflow-y:auto"));
        assert!(INDEX_HTML.contains("function answerQuestionInChat("));
    }

    #[test]
    fn composer_supports_image_attachments() {
        assert!(INDEX_HTML.contains("id=\"attach-input\" type=\"file\" accept=\"image/png,image/jpeg,image/gif,image/webp\" multiple"));
        assert!(INDEX_HTML.contains("function addImageFiles("));
        assert!(INDEX_HTML.contains("promptTextarea.addEventListener(\"paste\""));
        assert!(INDEX_HTML.contains("chatView.addEventListener(\"drop\""));
        assert!(INDEX_HTML.contains("MAX_ATTACH_BYTES = 5 * 1024 * 1024"));
        assert!(INDEX_HTML.contains("MAX_ATTACH_SIDE = 2000"));
        assert!(INDEX_HTML.contains("canvas.toBlob("));
        // base64 payload has the data: prefix stripped and images may be sent without text.
        assert!(INDEX_HTML.contains(".replace(/^data:[^;]*;base64,/, \"\")"));
        assert!(INDEX_HTML.contains("(!text && !pendingImages.length)"));
        assert!(INDEX_HTML.contains("attachments: pendingImages.map("));
        assert!(!INDEX_HTML.contains("attachments: [],\n        queue: queueMode"));
    }

    /// T71: `updateAllViews` throws a swallowed ReferenceError when an identifier it uses is
    /// undefined, leaving the Review slot stale. Every `pendingKeys` use needs a declaration.
    #[test]
    fn index_html_declares_pending_keys_before_use() {
        let decl = INDEX_HTML.find("const pendingKeys = ");
        let first_use = INDEX_HTML.find("pendingKeys.length");
        assert!(decl.is_some(), "pendingKeys must be declared");
        assert!(decl < first_use, "pendingKeys must be declared before use");
    }

    /// T73c: Ended sessions are folded into a collapsed group across session lists.
    #[test]
    fn ended_sessions_folded_into_collapsed_group() {
        assert!(INDEX_HTML.contains("已結束 ("));
        assert!(INDEX_HTML.contains("ertangle_sessions_ended_open"));
        assert!(INDEX_HTML.contains("ertangle_ov_ended_open"));
        assert!(INDEX_HTML.contains("ertangle_picker_ended_open"));
    }

    /// T73c: Sessions tab has a prune button with confirm prompt sending `{op: "prune"}`.
    #[test]
    fn prune_button_and_request() {
        assert!(INDEX_HTML.contains("清理已結束"));
        assert!(INDEX_HTML.contains("pruneSessions()"));
        assert!(INDEX_HTML.contains("op: \"prune\""));
        assert!(INDEX_HTML.contains("confirm("));
    }

    /// T73c: Sessions tab has a collapsed archived list with tap-to-transcript and resume.
    #[test]
    fn archived_sessions_list_and_transcript_drawer() {
        assert!(INDEX_HTML.contains("已封存 ("));
        assert!(INDEX_HTML.contains("ertangle_archived_sessions_open"));
        assert!(INDEX_HTML.contains("openArchivedTranscript("));
        assert!(INDEX_HTML.contains("id=\"sheet-archived-transcript\""));
        assert!(INDEX_HTML.contains("archive_pruned"));
    }

    /// T74b: composer command palette combines built-in web commands with native agent slash
    /// commands, and `!cmd` / `/exec cmd` are sent as `{op: "exec"}` requests.
    #[test]
    fn web_supports_command_palette_and_shell_exec() {
        assert!(INDEX_HTML.contains("id=\"cmd-palette\""));
        assert!(INDEX_HTML.contains("id=\"btn-cmd-palette\""));
        assert!(INDEX_HTML.contains("id=\"btn-shell-prefix\""));
        assert!(INDEX_HTML.contains("const WEB_COMMANDS = ["));
        assert!(INDEX_HTML.contains("function toggleCmdPalette()"));
        assert!(INDEX_HTML.contains("function prefillShellCmd()"));
        assert!(INDEX_HTML.contains("function syncCmdPalette("));
        assert!(INDEX_HTML.contains("function selectCmdPaletteItem("));
        assert!(INDEX_HTML.contains("function handlePromptKey("));
        assert!(INDEX_HTML.contains("function combinedWebCommands("));
        assert!(INDEX_HTML.contains("ArrowDown"));
        assert!(INDEX_HTML.contains("ArrowUp"));
        assert!(INDEX_HTML.contains("Agent 原生指令"));
        assert!(INDEX_HTML.contains("kind === \"session_commands\""));
        assert!(INDEX_HTML.contains("slash_commands = event.commands || []"));
        assert!(INDEX_HTML.contains("placeholder=\"輸入 Prompt、/命令 或 !Shell 指令…\""));
        assert!(INDEX_HTML.contains("const command = text.slice(1).trim()"));
        assert!(INDEX_HTML.contains("op: \"exec\", session: activeSession, command"));
        assert!(INDEX_HTML.contains("verb === \"exec\" || verb === \"sh\""));
        assert!(INDEX_HTML.contains("已送出 Shell 命令"));
        assert!(INDEX_HTML.contains("text.startsWith(\"//\")"));
        assert!(INDEX_HTML.contains("webCommandByVerb(verb)"));
    }

    /// T75: thought lines are emoji-free, animated, and show a live busy indicator.
    #[test]
    fn thinking_display_is_animated_and_emoji_free() {
        // Both the new `⋯ ` prefix and legacy `💭 ` logs are recognized.
        assert!(INDEX_HTML.contains("line.startsWith(\"⋯ \") || line.startsWith(\"💭 \")"));
        assert!(INDEX_HTML.contains("replace(/^(?:⋯|💭)\\s*/, \"\")"));
        // The collapsed summary shows an animated symbol, busy label, count and preview.
        assert!(INDEX_HTML
            .contains("<span class=\"think-ind ${thinkActive ? \"active\" : \"\"}\">⋯</span>"));
        assert!(INDEX_HTML.contains("const summaryLabel = thinkActive ? \"思考中\" : \"思考過程\""));
        assert!(INDEX_HTML.contains("class=\"think-preview\""));
        assert!(INDEX_HTML.contains("@keyframes think-pulse"));
        // A live bottom row is present while busy without an active tool.
        assert!(INDEX_HTML.contains("function thinkingLiveRow()"));
        assert!(INDEX_HTML.contains("chat-thinking-live"));
        assert!(INDEX_HTML.contains("@keyframes think-dot"));
        assert!(INDEX_HTML.contains("const showThinkingLive = isSessionBusy && noActiveTool"));
        // No emoji in the thinking render block itself (the legacy recognizer is elsewhere).
        let render_start = INDEX_HTML
            .find("function renderChatTranscriptNow()")
            .unwrap();
        let render_end = INDEX_HTML.find("function toolCallParts(").unwrap();
        assert!(!INDEX_HTML[render_start..render_end].contains("💭"));
    }

    /// T83: Web UI Markdown image preview syntax and allowed roots for agent artifact dirs.
    #[test]
    fn web_markdown_image_syntax_and_artifact_preview() {
        assert!(INDEX_HTML.contains("/!\\[([^\\]]*)\\]\\(([^)]+)\\)/g"));
        assert!(INDEX_HTML.contains("openFileActionSheet"));
        assert!(INDEX_HTML.contains("chat-file-pill"));
        assert!(INDEX_HTML.contains("圖片載入失敗"));

        // Verify path validation for artifact dirs
        let tmp = std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from("/tmp"));
        let artifact_dir = tmp.join("ertangle_test_brain");
        let _ = std::fs::create_dir_all(&artifact_dir);
        let canonical_artifact = artifact_dir.canonicalize().unwrap();

        let allowed = vec![canonical_artifact.clone()];
        let inside = canonical_artifact.join("shot.png");
        assert!(is_path_allowed(&inside, &allowed));

        // System paths and traversal outside allowed roots are blocked
        assert!(!is_path_allowed(Path::new("/etc/passwd"), &allowed));
        assert!(!is_path_allowed(&tmp, &allowed));
        let _ = std::fs::remove_dir_all(&artifact_dir);
    }

    mod hex {
        pub fn encode(bytes: [u8; 20]) -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }
    }
}
