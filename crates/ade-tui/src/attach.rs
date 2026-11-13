//! 檔案／圖片引用：`@path` 語法、路徑補全、貼上檔案路徑、剪貼簿圖片。

use ade_core::Attachment;
use std::path::PathBuf;

/// 展開 `~` 並以目前目錄為基準。
fn resolve(token: &str) -> PathBuf {
    if let Some(rest) = token.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(token)
}

/// 從一個 `@` token（不含 `@`）找出實際存在的檔案；會嘗試去掉結尾的標點。
fn existing_file(token: &str) -> Option<(PathBuf, usize)> {
    let mut t = token;
    loop {
        if !t.is_empty() {
            let p = resolve(t);
            if p.is_file() {
                return Some((p, t.len()));
            }
        }
        match t.chars().last() {
            Some(c) if ",.;:)]}!?\"'".contains(c) => t = &t[..t.len() - c.len_utf8()],
            _ => return None,
        }
    }
}

/// 切出游標之前以空白分隔的最後一個 token 的起點（支援 `@"含 空白 的路徑"`）。
fn token_len(s: &str) -> (String, usize) {
    if let Some(rest) = s.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (rest[..end].to_string(), end + 2);
        }
    }
    // 空白與全形標點都結束 token（中文句子裡 `@a.png，然後…` 很常見）。
    let end = s
        .find(|c: char| c.is_whitespace() || "，。、；：！？（）「」『』【】《》".contains(c))
        .unwrap_or(s.len());
    (s[..end].to_string(), end)
}

/// prompt 的第一個 token 是不是 `@存在的檔案`（用來和 `@peer` 訊息區分）。
pub(crate) fn leads_with_file(text: &str) -> bool {
    let t = text.trim_start();
    let Some(rest) = t.strip_prefix('@') else {
        return false;
    };
    let (tok, _) = token_len(rest);
    existing_file(&tok).is_some()
}

/// 把文字中的 `@檔案` 取出成附件，並把 token 換成檔名。不存在的 `@xxx` 原樣保留。
/// 附件讀取失敗（例如圖片太大）時回傳錯誤訊息。
pub(crate) fn extract_attachments(text: &str) -> Result<(String, Vec<Attachment>), String> {
    let mut out = String::with_capacity(text.len());
    let mut atts: Vec<Attachment> = Vec::new();
    let mut rest = text;
    let mut at_token_start = true;
    while let Some(c) = rest.chars().next() {
        if c == '@' && at_token_start {
            let (tok, used) = token_len(&rest[1..]);
            let quoted = rest[1..].starts_with('"');
            let found = if quoted {
                let p = resolve(&tok);
                p.is_file().then_some((p, used))
            } else {
                existing_file(&tok)
            };
            if let Some((path, consumed)) = found {
                let att = Attachment::load(&path)?;
                out.push_str(&att.name);
                if !atts.iter().any(|a| a.path == att.path) {
                    atts.push(att);
                }
                rest = &rest[1 + consumed..];
                at_token_start = false;
                continue;
            }
        }
        at_token_start = c.is_whitespace();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    Ok((out, atts))
}

/// 路徑補全：回傳符合的完整路徑字串（目錄結尾加 `/`）。
pub(crate) fn complete_path(prefix: &str) -> Vec<String> {
    let (dir_part, name_part) = match prefix.rfind('/') {
        Some(i) => (&prefix[..=i], &prefix[i + 1..]),
        None => ("", prefix),
    };
    let dir = if dir_part.is_empty() {
        PathBuf::from(".")
    } else {
        resolve(dir_part)
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with(name_part)
                || (name.starts_with('.') && !name_part.starts_with('.'))
            {
                return None;
            }
            let is_dir = e.path().is_dir();
            Some(format!("{dir_part}{name}{}", if is_dir { "/" } else { "" }))
        })
        .collect();
    out.sort();
    out
}

/// 所有候選的最長共同前綴。
pub(crate) fn common_prefix(items: &[String]) -> String {
    let Some(first) = items.first() else {
        return String::new();
    };
    let mut len = first.len();
    for s in items {
        len = len.min(
            first
                .bytes()
                .zip(s.bytes())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    while !first.is_char_boundary(len) {
        len -= 1;
    }
    first[..len].to_string()
}

/// 貼上的文字若就是一個存在的檔案路徑（拖曳檔案進終端機常見：可能帶引號或 `file://`），
/// 回傳對應的 `@path` 寫法。
pub(crate) fn pasted_file_ref(pasted: &str) -> Option<String> {
    let mut t = pasted.trim().trim_matches(['\'', '"']).to_string();
    if let Some(p) = t.strip_prefix("file://") {
        t = percent_decode(p);
    }
    t = t.replace("\\ ", " ");
    if t.is_empty() || t.contains('\n') || !resolve(&t).is_file() {
        return None;
    }
    Some(if t.contains(char::is_whitespace) {
        format!("@\"{t}\" ")
    } else {
        format!("@{t} ")
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
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

/// 從系統剪貼簿取圖片（Linux：wl-paste 或 xclip），存成暫存 PNG 並回傳路徑。
pub(crate) fn clipboard_image() -> Result<PathBuf, String> {
    let attempts: [(&str, &[&str]); 2] = [
        ("wl-paste", &["--type", "image/png"]),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        ),
    ];
    for (bin, args) in attempts {
        let Ok(out) = std::process::Command::new(bin)
            .args(args)
            .stderr(std::process::Stdio::null())
            .output()
        else {
            continue;
        };
        if out.status.success() && out.stdout.starts_with(&[0x89, b'P', b'N', b'G']) {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!("ade-clip-{ts}.png"));
            std::fs::write(&path, &out.stdout).map_err(|e| format!("寫入暫存圖片失敗：{e}"))?;
            return Ok(path);
        }
    }
    Err("剪貼簿裡沒有圖片（需要 wl-paste 或 xclip）".into())
}
