//! 檔案／圖片引用：`@path` 語法、路徑補全、貼上檔案路徑、剪貼簿圖片。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use termade_core::Attachment;

type FileCache = Mutex<HashMap<PathBuf, (Instant, Vec<String>)>>;
static FILE_CACHE: std::sync::OnceLock<FileCache> = std::sync::OnceLock::new();
const FILE_CACHE_TTL: Duration = Duration::from_secs(5);
const MAX_FILES: usize = 20_000;

/// 專案內的檔案清單（相對 `root` 的路徑）：優先用 `git ls-files`（含未追蹤、排除 .gitignore），
/// 不是 git repo 時退回走訪目錄（略過隱藏目錄、target、node_modules）。結果快取 5 秒。
pub(crate) fn project_files(root: &Path) -> Vec<String> {
    let cache = FILE_CACHE.get_or_init(Default::default);
    if let Some((t, v)) = cache.lock().unwrap().get(root) {
        if t.elapsed() < FILE_CACHE_TTL {
            return v.clone();
        }
    }
    let mut files: Vec<String> = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-co", "--exclude-standard"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if files.is_empty() {
        walk(root, root, 0, &mut files);
    }
    files.truncate(MAX_FILES);
    cache
        .lock()
        .unwrap()
        .insert(root.to_path_buf(), (Instant::now(), files.clone()));
    files
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 8 || out.len() >= MAX_FILES {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.filter_map(|e| e.ok()) {
        let name = e.file_name().to_string_lossy().to_string();
        let path = e.path();
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" && name != "node_modules" {
                walk(root, &path, depth + 1, out);
            }
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().to_string());
        }
    }
}

/// 模糊比對分數：`query` 的字元要依序出現在 `path` 中；檔名命中、連續命中、字首命中加分，路徑越短越好。
/// 不符合回傳 `None`。
pub(crate) fn fuzzy_score(query: &str, path: &str) -> Option<i64> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let p: Vec<char> = path.to_lowercase().chars().collect();
    let base_start = path.rfind('/').map_or(0, |i| path[..=i].chars().count());
    let mut score = 0i64;
    let mut qi = 0usize;
    let mut last: Option<usize> = None;
    for (i, c) in p.iter().enumerate() {
        if qi < q.len() && *c == q[qi] {
            score += 10;
            if last == Some(i.wrapping_sub(1)) {
                score += 15; // 連續
            }
            if i == base_start || (i > 0 && "/_-.".contains(p[i - 1])) {
                score += 20; // 詞首
            }
            if i >= base_start {
                score += 5; // 落在檔名
            }
            last = Some(i);
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    // 完整檔名／字首命中給大加分
    let base: String = p[base_start..].iter().collect();
    let ql: String = q.iter().collect();
    if base == ql {
        score += 200;
    } else if base.starts_with(&ql) {
        score += 100;
    } else if base.contains(&ql) {
        score += 50;
    }
    Some(score - p.len() as i64 / 4)
}

/// 依模糊分數排序的前 `limit` 個專案檔案。`query` 為空時回傳空清單。
pub(crate) fn suggestions(root: &Path, query: &str, limit: usize) -> Vec<String> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(i64, String)> = project_files(root)
        .into_iter()
        .filter_map(|f| fuzzy_score(query, &f).map(|s| (s, f)))
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.len().cmp(&b.1.len()))
            .then(a.1.cmp(&b.1))
    });
    scored.into_iter().take(limit).map(|(_, f)| f).collect()
}

/// 游標前正在輸入的 `@token`：回傳 (`@` 的字元索引, 不含 `@` 的內容)。引號內有空白的 token 不處理。
pub(crate) fn current_at_token(input: &str, cursor: usize) -> Option<(usize, String)> {
    let chars: Vec<char> = input.chars().collect();
    let cur = cursor.min(chars.len());
    let mut i = cur;
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    if i < cur && chars[i] == '@' {
        let tok: String = chars[i + 1..cur].iter().collect();
        return Some((i, tok));
    }
    if i == cur && cur > 0 {
        return None;
    }
    None
}

/// 搜尋根目錄：目前選取 session 的工作目錄，否則 TUI 的目前目錄。
pub(crate) fn search_root(session_cwd: Option<&str>) -> PathBuf {
    session_cwd
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 在專案內找 `@name` 對應的檔案：路徑直接存在，或唯一一個以 `/name` 結尾的檔案。
pub(crate) fn find_in_project(root: &Path, token: &str) -> Option<PathBuf> {
    let direct = root.join(token);
    if direct.is_file() {
        return Some(direct);
    }
    let suffix = format!("/{}", token.trim_start_matches("./"));
    let hits: Vec<String> = project_files(root)
        .into_iter()
        .filter(|f| *f == token || f.ends_with(&suffix))
        .collect();
    (hits.len() == 1).then(|| root.join(&hits[0]))
}

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
fn existing_file(token: &str, root: &Path) -> Option<(PathBuf, usize)> {
    let mut t = token;
    loop {
        if !t.is_empty() {
            let p = resolve(t);
            if p.is_file() {
                return Some((p, t.len()));
            }
            // 專案內搜尋：相對 session 工作目錄，或唯一以此結尾的檔案。
            if !t.starts_with('/') && !t.starts_with('~') {
                if let Some(found) = find_in_project(root, t) {
                    return Some((found, t.len()));
                }
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
pub(crate) fn leads_with_file(text: &str, root: &Path) -> bool {
    let t = text.trim_start();
    let Some(rest) = t.strip_prefix('@') else {
        return false;
    };
    let (tok, _) = token_len(rest);
    existing_file(&tok, root).is_some()
}

/// 把文字中的 `@檔案` 取出成附件，並把 token 換成檔名。不存在的 `@xxx` 原樣保留。
/// 附件讀取失敗（例如圖片太大）時回傳錯誤訊息。
pub(crate) fn extract_attachments(
    text: &str,
    root: &Path,
) -> Result<(String, Vec<Attachment>), String> {
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
                let p = if p.is_file() {
                    Some(p)
                } else {
                    find_in_project(root, &tok)
                };
                p.map(|p| (p, used))
            } else {
                existing_file(&tok, root)
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
    if let Some(first) = atts.first_mut() {
        first.raw_prompt = Some(text.to_string());
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
            let path = std::env::temp_dir().join(format!("termade-clip-{ts}.png"));
            std::fs::write(&path, &out.stdout).map_err(|e| format!("寫入暫存圖片失敗：{e}"))?;
            return Ok(path);
        }
    }
    Err("剪貼簿裡沒有圖片（需要 wl-paste 或 xclip）".into())
}
