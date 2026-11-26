//! 各 agent 可選的 model 清單：Claude 內建，其他 agent 向各自 CLI 查詢。TUI 與 daemon (網頁版) 共用。

use ertangle_core::AgentKind;
use tr::tr;

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

/// Antigravity (agy) 可用的預設 model 清單。
pub const ANTIGRAVITY_MODELS: &[(&str, &str)] = &[
    ("gemini-4-neon", "Gemini 4 Neon"),
    ("gemini-4-carbon", "Gemini 4 Carbon"),
    ("gemini-4-argon", "Gemini 4 Argon"),
    ("gemini-4-hydrogen", "Gemini 4 Hydrogen"),
    ("gemini-3.8-flash", "Gemini 3.8 Flash"),
    ("gemini-pro-orchestrator-neon", "Orchestrator (Neon)"),
    ("gemini-pro-orchestrator", "Orchestrator (Carbon)"),
    ("gemini-pro-orchestrator-argon", "Orchestrator (Argon)"),
    ("claude-opus-5-5", "Claude Opus 5.5"),
    ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
    ("gpt-oss-120b-medium", "GPT-OSS 120B (Medium)"),
];

/// 查詢 model 清單所需的時間上限。
const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// 子處理程序執行完畢後收集到的輸出。
#[derive(Debug, Default)]
struct Captured {
    stdout: String,
    stderr: String,
    /// 結束碼；被訊號終止時為 `None`。
    code: Option<i32>,
}

/// 執行失敗的原因。
#[derive(Debug)]
enum CaptureError {
    Spawn(std::io::Error),
    Timeout,
}

/// 背景把整條 pipe 讀完，避免輸出塞滿 pipe 讓子處理程序卡住。
fn drain<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            // 盡可能讀取管線剩餘資料，忽略讀取與傳送錯誤
            let _ = pipe.read_to_end(&mut bytes);
            let _ = tx.send(String::from_utf8_lossy(&bytes).into_owned());
        });
    }
    rx
}

/// 在 `timeout` 內執行指令並取回 stdout、stderr 與結束碼。
/// 必須一邊等處理程序一邊把輸出讀走：`codex debug models` 的輸出可達數百 KB，
/// 若等處理程序結束才讀，pipe 被填滿後處理程序會卡住，直到逾時被殺掉而得到空清單。
/// `remove` 列出的環境變數不會傳給子處理程序。
fn capture(
    bin: &str,
    args: &[&str],
    remove: &[&str],
    timeout: std::time::Duration,
) -> Result<Captured, CaptureError> {
    use std::process::Stdio;

    let mut command = std::process::Command::new(bin);
    for k in remove {
        command.env_remove(k);
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(CaptureError::Spawn)?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                // 執行逾時強制終止子處理程序並回收，忽略錯誤
                let _ = child.kill();
                let _ = child.wait();
                return Err(CaptureError::Timeout);
            }
        }
    };
    let grace = std::time::Duration::from_secs(1);
    Ok(Captured {
        stdout: stdout.recv_timeout(grace).unwrap_or_default(),
        stderr: stderr.recv_timeout(grace).unwrap_or_default(),
        code: status.code(),
    })
}

/// 從 stderr 挑出最能說明問題的一行：跳過 glog 的 INFO 行 (`I1123 …`)、進度動畫與空行，
/// 優先取含有錯誤字樣的行，否則取最後一行。
fn stderr_summary(stderr: &str) -> Option<String> {
    let lines: Vec<&str> = stderr
        .split(['\n', '\r'])
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\x1b'))
        .map(|l| l.strip_prefix("[K").unwrap_or(l).trim())
        .filter(|l| !l.is_empty())
        .filter(|l| !is_glog_info(l))
        .filter(|l| !l.contains("Fetching available models"))
        .collect();
    let pick = lines
        .iter()
        .rev()
        .find(|l| looks_like_error(l))
        .or_else(|| lines.last())?;
    let short: String = pick.chars().take(200).collect();
    Some(short)
}

/// glog 的 INFO 行，例如 `I1123 14:35:36.832595 155 http_helpers.go:760] …`。
fn is_glog_info(line: &str) -> bool {
    let mut chars = line.chars();
    chars.next() == Some('I') && chars.take(4).all(|c| c.is_ascii_digit())
}

/// 看起來像錯誤或登入問題的行。
fn looks_like_error(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let glog_error = matches!(line.chars().next(), Some('E' | 'F'))
        && line.chars().skip(1).take(4).all(|c| c.is_ascii_digit());
    glog_error
        || [
            "error",
            "fail",
            "auth",
            "login",
            "log in",
            "sign in",
            "credential",
            "token",
            "permission denied",
            "401",
            "403",
        ]
        .iter()
        .any(|k| lower.contains(k))
}

/// 查詢 model 清單的結果：`problem` 有值時清單可能不完整 (例如尚未登入或登入過期)，應提示使用者。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ModelQuery {
    /// ` (顯示文字, id)`。
    pub models: Vec<(String, String)>,
    /// 給使用者看的問題說明。
    pub problem: Option<String>,
}

/// 依查詢結果組出問題說明；沒有問題時回 `None`。
fn diagnose(
    bin: &str,
    cmd: &str,
    result: &Result<Captured, CaptureError>,
    parsed: usize,
) -> Option<String> {
    let login_hint = tr!(
        "若尚未登入或登入已過期，請先在終端機執行 `{bin}` 重新登入",
        bin = bin
    );
    match result {
        Err(CaptureError::Spawn(e)) => Some(tr!("無法執行 `{cmd}`：{err}", cmd = cmd, err = e)),
        Err(CaptureError::Timeout) => Some(tr!(
            "`{cmd}` 超過 {secs} 秒沒有回應；{hint}",
            cmd = cmd,
            secs = QUERY_TIMEOUT.as_secs(),
            hint = login_hint
        )),
        Ok(c) => {
            let detail = stderr_summary(&c.stderr);
            let failed = c.code != Some(0);
            let suspicious = detail.as_deref().is_some_and(looks_like_error);
            if !failed && parsed > 0 && !suspicious {
                return None;
            }
            let head = if failed {
                match c.code {
                    Some(code) => tr!("`{cmd}` 失敗 (結束碼 {code})", cmd = cmd, code = code),
                    None => tr!("`{cmd}` 被中斷", cmd = cmd),
                }
            } else if parsed == 0 {
                tr!("`{cmd}` 沒有列出任何 model", cmd = cmd)
            } else {
                tr!("`{cmd}` 回報錯誤，清單可能不完整", cmd = cmd)
            };
            Some(match detail {
                Some(d) => tr!(
                    "{head}：{detail}；{hint}",
                    head = head,
                    detail = d,
                    hint = login_hint
                ),
                None => tr!("{head}；{hint}", head = head, hint = login_hint),
            })
        }
    }
}

/// 向 agent 的 CLI 查詢原始可用 model 清單 (`opencode models`、`agy models`、`codex debug models`)，最多等 8 秒。
/// 不會合併 `antigravity` 的 effort 變體；查詢失敗或 CLI 回報錯誤時附上說明。
pub fn query_raw_models_detailed(kind: &AgentKind) -> ModelQuery {
    let (bin, args): (&str, &[&str]) = match kind {
        AgentKind::OpenCode => ("opencode", &["models"]),
        AgentKind::Antigravity => ("agy", &["models"]),
        AgentKind::Codex => ("codex", &["debug", "models"]),
        _ => return ModelQuery::default(),
    };
    let cmd = std::iter::once(bin)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    let result = capture(bin, args, crate::env_removals(kind), QUERY_TIMEOUT);
    let models = match (&result, kind) {
        (Ok(c), AgentKind::Codex) => parse_codex_models(&c.stdout),
        (Ok(c), _) => parse_models(&c.stdout),
        (Err(_), _) => Vec::new(),
    };
    let problem = diagnose(bin, &cmd, &result, models.len());
    ModelQuery { models, problem }
}

/// 同 [`query_raw_models_detailed`]，只取清單。
pub fn query_raw_models(kind: &AgentKind) -> Vec<(String, String)> {
    query_raw_models_detailed(kind).models
}

/// 同 [`query_models`]，但附上查詢問題說明。
pub fn query_models_detailed(kind: &AgentKind) -> ModelQuery {
    let q = query_raw_models_detailed(kind);
    match kind {
        AgentKind::Antigravity => ModelQuery {
            models: split_effort_variants(q.models),
            problem: q.problem,
        },
        _ => q,
    }
}

/// 向 agent 的 CLI 查詢可用 model (`opencode models`、`agy models`)，最多等 8 秒。
/// 輸出每行第一欄是 model id，後面 (以 tab 分隔)是顯示名稱。
/// agy 的清單把 effort 寫進 model id (`xxx-high`)，但 ACP 的 model 與 effort 是分開的選項，這裡會合併。
pub fn query_models(kind: &AgentKind) -> Vec<(String, String)> {
    query_models_detailed(kind).models
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
    v.get("models")
        .and_then(|m| m.as_array())
        .into_iter()
        .flatten()
        .filter(|m| m.get("visibility").and_then(|x| x.as_str()) == Some("list"))
        .filter_map(|m| {
            let slug = m.get("slug")?.as_str()?;
            let label = match m.get("display_name").and_then(|x| x.as_str()) {
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
            let (id, name) = if let Some((id, rest)) = line.split_once('\t') {
                (id.trim(), Some(rest.trim()))
            } else if let Some(idx) = line.find("  ") {
                let id = line[..idx].trim();
                let rest = line[idx..].trim();
                (id, if rest.is_empty() { None } else { Some(rest) })
            } else {
                (line.trim(), None)
            };
            if id.is_empty() || id.contains(char::is_whitespace) || id.ends_with("...") {
                return None;
            }
            let label = match name {
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

    /// `codex debug models` 的輸出遠大於 pipe buffer；若等處理程序結束才讀，
    /// 處理程序會卡在寫入而永遠不結束，最後回傳空清單。
    #[cfg(unix)]
    #[test]
    fn capture_drains_large_output() {
        let out = capture(
            "sh",
            &["-c", "yes x | head -c 200000; yes y | head -c 200000 >&2"],
            &[],
            std::time::Duration::from_secs(8),
        )
        .expect("命令應成功執行");
        assert!(
            out.stdout.len() >= 200_000,
            "只讀到 {} bytes",
            out.stdout.len()
        );
        assert!(
            out.stderr.len() >= 200_000,
            "只讀到 {} bytes",
            out.stderr.len()
        );
        assert_eq!(out.code, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn capture_reports_timeout() {
        let r = capture("sleep", &["5"], &[], std::time::Duration::from_millis(200));
        assert!(matches!(r, Err(CaptureError::Timeout)));
    }

    #[test]
    fn capture_reports_missing_binary() {
        let r = capture(
            "ertangle-no-such-binary",
            &[],
            &[],
            std::time::Duration::from_secs(1),
        );
        assert!(matches!(r, Err(CaptureError::Spawn(_))));
    }

    #[test]
    fn capture_removes_listed_env_vars() {
        let out = capture(
            "sh",
            &["-c", "echo ${HOME-unset}"],
            &["HOME"],
            std::time::Duration::from_secs(8),
        )
        .expect("命令應成功執行");
        assert_eq!(out.stdout.trim(), "unset");
    }

    fn ok(stdout: &str, stderr: &str, code: i32) -> Result<Captured, CaptureError> {
        Ok(Captured {
            stdout: stdout.into(),
            stderr: stderr.into(),
            code: Some(code),
        })
    }

    #[test]
    fn stderr_summary_skips_info_and_spinner() {
        let err = "\r⠦ Fetching available models...\rI1123 14:35:36.832595 155 http_helpers.go:760] URL: x\n\r\x1b[K";
        assert_eq!(stderr_summary(err), None);
        let err = "I1123 a] ok\nE1123 14:35:36 auth.go:12] token expired\nI1123 b] bye\n";
        assert_eq!(
            stderr_summary(err).as_deref(),
            Some("E1123 14:35:36 auth.go:12] token expired")
        );
    }

    #[test]
    fn diagnose_accepts_clean_success() {
        let r = ok(
            "m1\tModel 1\n",
            "\r⠦ Fetching available models...\r\x1b[K",
            0,
        );
        assert_eq!(diagnose("agy", "agy models", &r, 1), None);
    }

    #[test]
    fn diagnose_flags_failure_and_empty_and_auth_errors() {
        let failed = diagnose("agy", "agy models", &ok("", "Error: not logged in\n", 1), 0)
            .expect("非零結束碼應回報");
        assert!(failed.contains("not logged in"), "{failed}");
        assert!(failed.contains("`agy`"), "{failed}");

        let empty = diagnose("agy", "agy models", &ok("", "", 0), 0).expect("空清單應回報");
        assert!(empty.contains("agy models"), "{empty}");

        let partial = diagnose(
            "agy",
            "agy models",
            &ok(
                "m1\tModel 1\n",
                "E1123 x] unauthenticated: please sign in\n",
                0,
            ),
            1,
        )
        .expect("有錯誤訊息時即使有清單也應回報");
        assert!(partial.contains("please sign in"), "{partial}");

        let timeout =
            diagnose("agy", "agy models", &Err(CaptureError::Timeout), 0).expect("逾時應回報");
        assert!(timeout.contains("agy models"), "{timeout}");
    }

    #[test]
    fn parse_models_handles_space_separated_columns() {
        let out = "gemini-4-neon-high    Gemini 4 Neon (High)\nplain-model\n\n";
        let parsed = parse_models(out);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].1, "gemini-4-neon-high");
        assert_eq!(parsed[0].0, "gemini-4-neon-high  (Gemini 4 Neon (High))");
        assert_eq!(parsed[1].1, "plain-model");
        assert_eq!(parsed[1].0, "plain-model");
    }

    #[test]
    fn antigravity_builtin_models_available() {
        assert!(!ANTIGRAVITY_MODELS.is_empty());
        assert!(ANTIGRAVITY_MODELS
            .iter()
            .any(|(id, _)| *id == "gemini-4-neon"));
        assert!(ANTIGRAVITY_MODELS
            .iter()
            .any(|(id, _)| *id == "gemini-3.8-flash"));
    }
}
