//! 同步 HTTP 與 RFC 6455 WebSocket 端點 (Mobile Companion 隨身審核器)。
//! 零外部 async/HTTP 框架相依性，直接以 `std::net::TcpListener` 與純 Rust 實作。

pub mod qr;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use termade_core::base64_encode;
use termade_protocol::{decode_line, encode_line, Frame, Request};
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

/// 啟動行動端 Web Companion 服務。
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

    // 1. WebSocket 升級請求
    if method == "GET" && path == "/ws" && is_upgrade {
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

    // 2. 靜態資源處理
    match (method, path) {
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
    let require_auth = shared.auth_token.is_some();
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
                                    .auth_token
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
        len = u64::from_be_bytes(ext) as usize;
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

    mod hex {
        pub fn encode(bytes: [u8; 20]) -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }
    }
}
