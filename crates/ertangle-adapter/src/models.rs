//! 各 agent 可選的 model 清單：Claude 內建，其他 agent 向各自 CLI 查詢。TUI 與 daemon (網頁版) 共用。

use ertangle_core::AgentKind;

/// Claude Code 可用的 model。`claude` CLI 沒有列出 model 的子命令，只能內建：
/// 前段是別名 (永遠指向該系列最新版)，後段是完整 id (固定版本)。新模型推出時要更新這裡，
/// 網頁版透過 daemon 的 `list_models` 取得同一份清單。
pub const CLAUDE_MODELS: &[(&str, &str)] = &[
    ("fable", ertangle_core::tr_noop!("最新 Fable")),
    ("opus", ertangle_core::tr_noop!("最新 Opus")),
    ("sonnet", ertangle_core::tr_noop!("最新 Sonnet")),
    ("haiku", ertangle_core::tr_noop!("最新 Haiku")),
    ("claude-fable-5-2", "Claude Fable 5.2"),
    ("claude-fable-5-1", "Claude Fable 5.1"),
    ("claude-fable-5", "Claude Fable 5"),
    ("claude-opus-5-5", "Claude Opus 5.5"),
    ("claude-opus-5", "Claude Opus 5"),
    ("claude-opus-4-8", "Claude Opus 4.8"),
    ("claude-opus-4-7", "Claude Opus 4.7"),
    ("claude-opus-4-6", "Claude Opus 4.6"),
    ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
    ("claude-sonnet-5", "Claude Sonnet 5"),
    ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
    ("claude-haiku-5-5", "Claude Haiku 5.5"),
    ("claude-haiku-4-5", "Claude Haiku 4.5"),
];

/// 在 `timeout` 內執行指令並取回 stdout，逾時或啟動失敗回 `None`。
/// 必須一邊等行程一邊把 stdout 讀走：`codex debug models` 的輸出可達數百 KB，
/// 若等行程結束才讀，pipe 被填滿後行程會卡住，直到逾時被殺掉而得到空清單。
fn capture_stdout(bin: &str, args: &[&str], timeout: std::time::Duration) -> Option<String> {
    use std::process::Stdio;

    let mut child = std::process::Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let reader = child.stdout.take().map(|so| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut so = so;
            let mut bytes = Vec::new();
            let ok = so.read_to_end(&mut bytes).is_ok();
            let _ = tx.send(if ok {
                String::from_utf8_lossy(&bytes).into_owned()
            } else {
                String::new()
            });
        });
        rx
    });
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
    reader.and_then(|rx| rx.recv_timeout(std::time::Duration::from_secs(1)).ok())
}

/// 向 agent 的 CLI 查詢原始可用 model 清單 (`opencode models`、`agy models`、`codex debug models`)，最多等 8 秒。
/// 不會合併 `antigravity` 的 effort 變體，供需要依變體建立 effort 選項的呼叫端使用。
pub fn query_raw_models(kind: &AgentKind) -> Vec<(String, String)> {
    let (bin, args): (&str, &[&str]) = match kind {
        AgentKind::OpenCode => ("opencode", &["models"]),
        AgentKind::Antigravity => ("agy", &["models"]),
        AgentKind::Codex => ("codex", &["debug", "models"]),
        _ => return Vec::new(),
    };
    let Some(out) = capture_stdout(bin, args, std::time::Duration::from_secs(8)) else {
        return Vec::new();
    };
    match kind {
        AgentKind::Codex => parse_codex_models(&out),
        _ => parse_models(&out),
    }
}

/// 向 agent 的 CLI 查詢可用 model (`opencode models`、`agy models`)，最多等 8 秒。
/// 輸出每行第一欄是 model id，後面 (以 tab 分隔)是顯示名稱。
pub fn query_models(kind: &AgentKind) -> Vec<(String, String)> {
    let raw = query_raw_models(kind);
    match kind {
        // agy 的清單把 effort 寫進 model id (`xxx-high`)，但 ACP 的 model 與 effort 是分開的選項。
        AgentKind::Antigravity => split_effort_variants(raw),
        _ => raw,
    }
}

/// 把 `base-low／-medium／-high／-xhigh／-max` 這類「model + effort 合在一起」的 id 合併成 `base`
/// (effort 另外選)。只有同一個 base 有兩個以上變體才合併，避免把 `gpt-oss-120b-medium`
/// 這種單一 id 誤拆。輸入是 ` (顯示文字, id)`，保留原本的順序。
pub fn split_effort_variants(models: Vec<(String, String)>) -> Vec<(String, String)> {
    let base_of = |id: &str| -> Option<String> {
        let (base, last) = id.rsplit_once('-')?;
        crate::EFFORT_LEVELS
            .contains(&last)
            .then(|| base.to_string())
    };
    let mut counts: std::collections::HashMap<String, usize> = Default::default();
    for (_, id) in &models {
        if let Some(b) = base_of(id) {
            *counts.entry(b).or_default() += 1;
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (label, id) in models {
        match base_of(&id).filter(|b| counts.get(b).copied().unwrap_or(0) >= 2) {
            Some(b) => {
                if seen.insert(b.clone()) {
                    // 顯示名稱 "Gemini 4 Argon (Medium)" → "Gemini 4 Argon"
                    let name = label
                        .split_once("  (")
                        .map(|(_, n)| n.trim_end_matches(')'))
                        .map(|n| match n.rsplit_once(" (") {
                            Some((head, _)) => head.to_string(),
                            None => n.to_string(),
                        });
                    let label = match name {
                        Some(n) => format!("{b}  ({n})"),
                        None => b.clone(),
                    };
                    out.push((label, b));
                }
            }
            None => out.push((label, id)),
        }
    }
    out
}

/// `codex debug models` 輸出 `{"models":[{slug,display_name,visibility}]}`；只取 `visibility == list`。
pub fn parse_codex_models(out: &str) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(out) else {
        return Vec::new();
    };
    v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"] == "list")
        .filter_map(|m| {
            let slug = m["slug"].as_str()?;
            let label = match m["display_name"].as_str() {
                Some(n) if !n.is_empty() => format!("{slug}  ({n})"),
                _ => slug.to_string(),
            };
            Some((label, slug.to_string()))
        })
        .collect()
}

/// 解析 `models` 子命令的輸出：忽略空行與沒有 id 樣子的提示行。
pub fn parse_models(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let id = cols.next()?.trim();
            if id.is_empty() || id.contains(char::is_whitespace) || id.ends_with("...") {
                return None;
            }
            let label = match cols.next().map(str::trim) {
                Some(name) if !name.is_empty() => format!("{id}  ({name})"),
                _ => id.to_string(),
            };
            Some((label, id.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `codex debug models` 的輸出遠大於 pipe buffer；若等行程結束才讀，
    /// 行程會卡在寫入而永遠不結束，最後回傳空清單。
    #[cfg(unix)]
    #[test]
    fn capture_stdout_drains_large_output() {
        let out = capture_stdout(
            "sh",
            &["-c", "yes x | head -c 200000"],
            std::time::Duration::from_secs(8),
        )
        .expect("命令應成功執行");
        assert!(out.len() >= 200_000, "只讀到 {} bytes", out.len());
    }
}
