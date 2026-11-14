//! Shell 補全：`completions <shell>` 產生腳本；隱藏的 `__complete sessions` 供腳本在按 Tab 時查 session。

use std::ffi::OsStr;
use std::time::Duration;

use anyhow::Result;
use clap::builder::{PossibleValue, PossibleValuesParser, TypedValueParser};
use clap::{Arg, Command, CommandFactory};
use clap_complete::Shell;
use termade_core::{AgentKind, State};
use termade_protocol::{Frame, Request};
use tr::tr;

// ---------------------------------------------------------------------------
// 旗標的可用值（補全腳本會列出這些；別名仍可解析但不列出）
// ---------------------------------------------------------------------------

/// `--effort` 的可用值；與 `termade_adapter::EFFORT_LEVELS` 相同（有測試比對）。
pub(crate) const EFFORT_VALUES: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// `--kind`／`handoff --to`：接受 `kind` 或 `kind:model`，kind 的部分交給 [`AgentKind::parse`]（含別名）。
#[derive(Clone)]
pub(crate) struct KindParser;

impl TypedValueParser for KindParser {
    type Value = String;

    fn parse_ref(
        &self,
        cmd: &Command,
        _arg: Option<&Arg>,
        value: &OsStr,
    ) -> Result<String, clap::Error> {
        let s = value.to_string_lossy().to_string();
        let kind = s.split_once(':').map_or(s.as_str(), |(k, _)| k);
        if AgentKind::parse(kind).is_none() {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::InvalidValue,
                tr!(
                    "未知的 agent：{kind}（opencode/claude/codex/antigravity，可加 `:model`）\n",
                    kind = kind
                ),
            )
            .with_cmd(cmd));
        }
        Ok(s)
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        Some(Box::new(
            [
                PossibleValue::new("opencode").alias("oc"),
                PossibleValue::new("claude").aliases(["claude-code", "claudecode"]),
                PossibleValue::new("codex"),
                PossibleValue::new("antigravity").aliases(["agy", "gemini"]),
            ]
            .into_iter(),
        ))
    }
}

pub(crate) fn transport_values() -> PossibleValuesParser {
    PossibleValuesParser::new([
        PossibleValue::new("auto"),
        PossibleValue::new("structured").alias("acp"),
        PossibleValue::new("pty"),
    ])
}

pub(crate) fn permission_values() -> PossibleValuesParser {
    PossibleValuesParser::new([
        PossibleValue::new("ask").alias("manual"),
        PossibleValue::new("auto").aliases(["accept-edits", "acceptedits"]),
        PossibleValue::new("bypass").aliases(["yolo", "dangerous"]),
    ])
}

// ---------------------------------------------------------------------------
// `termade completions <shell>`
// ---------------------------------------------------------------------------

/// 參數 id 屬於這份清單、且沒有固定可用值時，視為「要填 session」。
const SESSION_ARG_IDS: [&str; 6] = ["session", "peer", "parent", "to", "from", "assignee"];

fn takes_session(arg: &Arg) -> bool {
    SESSION_ARG_IDS.contains(&arg.get_id().as_str())
        && arg.get_num_args().is_none_or(|n| n.takes_values())
        && arg.get_possible_values().is_empty()
}

pub(crate) fn cmd_completions(shell: Shell) -> Result<()> {
    print!("{}", completion_script(shell));
    Ok(())
}

/// clap 產生的靜態補全，加上（zsh／bash）查 session 名稱的片段。
pub(crate) fn completion_script(shell: Shell) -> String {
    let mut cmd = command_for_completion();
    let mut buf = Vec::new();
    clap_complete::generate(shell, &mut cmd, "termade", &mut buf);
    let script = String::from_utf8_lossy(&buf).to_string();
    match shell {
        Shell::Zsh => zsh_with_sessions(&script),
        Shell::Bash => bash_with_sessions(&script, &cmd),
        _ => script,
    }
}

/// 排除隱藏子命令（`__complete`）：`clap_complete` 預設仍會把 `hide = true` 的子命令列進腳本。
fn command_for_completion() -> Command {
    let base = crate::Cli::command();
    let args: Vec<Arg> = base.get_arguments().cloned().collect();
    let subcommands: Vec<Command> = base
        .get_subcommands()
        .filter(|s| !s.is_hide_set())
        .cloned()
        .collect();
    let mut cmd = Command::new("termade").version(env!("CARGO_PKG_VERSION"));
    if let Some(a) = base.get_about() {
        cmd = cmd.about(a.clone());
    }
    cmd.args(args).subcommands(subcommands)
}

const ZSH_SESSIONS_FN: &str = r#"
# session 名稱（label，沒有 label 時是 id）；daemon 沒在跑時不列出任何東西，也不會啟動它。
_termade_sessions() {
    local -a sessions
    sessions=(${(f)"$(command termade __complete sessions 2>/dev/null)"})
    compadd -a sessions
}
"#;

/// 把要填 session 的參數從 `_default` 換成 `_termade_sessions`。
/// 函式定義放在第一行（`#compdef`）之後：檔案被 autoload 時，結尾會直接呼叫 `_termade`，定義要先出現。
fn zsh_with_sessions(script: &str) -> String {
    let mut out = String::with_capacity(script.len() + ZSH_SESSIONS_FN.len());
    for (i, line) in script.lines().enumerate() {
        let is_session_spec = line.contains(":_default")
            && SESSION_ARG_IDS.iter().any(|id| {
                line.contains(&format!(":{id}:_default"))
                    || line.contains(&format!(":{id} -- "))
                    || line.contains(&format!(":{}:_default", id.to_uppercase()))
            });
        if is_session_spec {
            out.push_str(&line.replace(":_default", ":_termade_sessions"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
        if i == 0 {
            out.push_str(ZSH_SESSIONS_FN);
        }
    }
    out
}

/// 包一層：先跑 clap 產生的 `_termade`，游標在 session 參數上時改列 session 名稱。
fn bash_with_sessions(script: &str, cmd: &Command) -> String {
    // 第一個位置參數是 session 的子命令，以及 `子命令:--旗標` 的組合，都從 CLI 定義取得。
    let mut positional = Vec::new();
    let mut flags = Vec::new();
    for sub in cmd.get_subcommands() {
        if sub.get_positionals().next().is_some_and(takes_session) {
            positional.push(sub.get_name().to_string());
        }
        for arg in sub.get_opts().filter(|a| takes_session(a)) {
            if let Some(long) = arg.get_long() {
                flags.push(format!("{}:--{long}", sub.get_name()));
            }
        }
    }
    format!(
        r#"{script}
# session 名稱（label，沒有 label 時是 id）；daemon 沒在跑時不列出任何東西，也不會啟動它。
_termade_with_sessions() {{
    _termade "$@"
    local cur="${{COMP_WORDS[COMP_CWORD]}}" prev="${{COMP_WORDS[COMP_CWORD-1]}}" sub="${{COMP_WORDS[1]}}" want=""
    [[ "$cur" == -* ]] && return 0
    case "$sub:$prev" in
        {flags}) want=1 ;;
    esac
    if [[ -z "$want" && $COMP_CWORD -eq 2 ]]; then
        case "$sub" in
            {positional}) want=1 ;;
        esac
    fi
    if [[ -n "$want" ]]; then
        local IFS=$'\n'
        COMPREPLY=( $(compgen -W "$(command termade __complete sessions 2>/dev/null)" -- "$cur") )
    fi
    return 0
}}
complete -F _termade_with_sessions -o bashdefault -o default termade
"#,
        flags = flags.join("|"),
        positional = positional.join("|"),
    )
}

// ---------------------------------------------------------------------------
// `termade __complete sessions`
// ---------------------------------------------------------------------------

/// 等 daemon 回覆的上限（毫秒）；可用 `TERMADE_COMPLETE_TIMEOUT_MS` 調整。
const DEFAULT_TIMEOUT_MS: u64 = 200;

/// 每行印出一個 session 名稱。任何失敗（daemon 沒跑、逾時、協定錯誤）都是「不輸出、結束碼 0」，
/// 而且絕不啟動 daemon：按 Tab 不該有副作用。
pub(crate) fn cmd_complete_sessions() -> Result<()> {
    let timeout = std::env::var("TERMADE_COMPLETE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    // 連線與讀取都放在另一條執行緒，主執行緒只等到時限；逾時就直接結束行程。
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(fetch_session_names());
    });
    if let Ok(Some(names)) = rx.recv_timeout(Duration::from_millis(timeout)) {
        for name in names {
            println!("{name}");
        }
    }
    Ok(())
}

fn fetch_session_names() -> Option<Vec<String>> {
    // 本機模式下 socket 不存在就不必嘗試連線。
    if std::env::var_os("TERMADE_REMOTE").is_none() && !termade_daemon::socket_path().exists() {
        return None;
    }
    // 直接連線，不經過 `ensure_daemon`。
    let mut client = termade_daemon::Client::connect().ok()?;
    client.send(&Request::Snapshot).ok()?;
    let state = loop {
        match client.recv().ok()? {
            Frame::Snapshot { state } => break state,
            Frame::Error { .. } => return None,
            _ => continue,
        }
    };
    let cwd = std::env::current_dir().ok()?;
    Some(session_names(&state, &home_project(&cwd)))
}

/// 屬於 `project` 的 session：有 label 用 label，否則用 id。
fn session_names(state: &State, project: &str) -> Vec<String> {
    state
        .sessions_sorted()
        .into_iter()
        .filter(|s| s.project_root() == project)
        .map(|s| {
            if s.label.is_empty() {
                s.id.to_string()
            } else {
                s.label.clone()
            }
        })
        .collect()
}

/// 專案根目錄：cwd 所在 git repo 的頂層，不在 repo 內就用 cwd 本身。
/// 與 TUI 預設範圍用的規則相同（`termade-tui` 的 `home_project`）。
fn home_project(cwd: &std::path::Path) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cwd.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Cmd};
    use clap::Parser;

    fn spawn_args(args: &[&str]) -> (String, String, Option<String>, Option<String>) {
        let mut argv = vec!["termade", "spawn"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv).map(|c| c.cmd) {
            Ok(Some(Cmd::Spawn {
                kind,
                transport,
                permission,
                effort,
                ..
            })) => (kind, transport, permission, effort),
            Ok(_) => panic!("應解析為 spawn"),
            Err(e) => panic!("解析失敗：{e}"),
        }
    }

    #[test]
    fn scripts_list_subcommands() {
        for shell in [Shell::Zsh, Shell::Bash, Shell::Fish] {
            let script = completion_script(shell);
            assert!(!script.trim().is_empty(), "{shell} 腳本不應為空");
            for sub in ["spawn", "merge", "completions"] {
                assert!(script.contains(sub), "{shell} 腳本應含子命令 {sub}");
            }
            assert!(
                !script.contains("__complete)") && !script.contains("'__complete:"),
                "{shell} 腳本不應把隱藏的 __complete 列為子命令"
            );
        }
    }

    #[test]
    fn zsh_script_lists_flag_values() {
        let script = completion_script(Shell::Zsh);
        for values in [
            "(opencode claude codex antigravity)",
            "(auto structured pty)",
            "(ask auto bypass)",
            "(low medium high xhigh max)",
        ] {
            assert!(script.contains(values), "zsh 腳本應含 {values}");
        }
    }

    #[test]
    fn kind_aliases_and_model_suffix_still_parse() {
        for kind in [
            "agy",
            "gemini",
            "oc",
            "claude-code",
            "Claude",
            "claude:opus",
        ] {
            assert_eq!(spawn_args(&["--kind", kind]).0, kind);
        }
        assert_eq!(spawn_args(&[]).0, "opencode");
        assert!(Cli::try_parse_from(["termade", "spawn", "--kind", "nope"]).is_err());
        assert!(Cli::try_parse_from(["termade", "handoff", "s1", "--to", "agy:pro"]).is_ok());
        assert!(Cli::try_parse_from(["termade", "handoff", "s1", "--to", "nope"]).is_err());
    }

    #[test]
    fn other_flag_aliases_still_parse() {
        let (_, transport, permission, effort) = spawn_args(&[
            "--transport",
            "acp",
            "--permission",
            "YOLO",
            "--effort",
            "xhigh",
        ]);
        assert_eq!(transport, "acp");
        assert_eq!(permission.as_deref(), Some("YOLO"));
        assert_eq!(effort.as_deref(), Some("xhigh"));
        assert_eq!(spawn_args(&[]).1, "auto");
        for bad in [
            ["--transport", "nope"],
            ["--permission", "nope"],
            ["--effort", "nope"],
        ] {
            let argv = ["termade", "spawn"].into_iter().chain(bad);
            assert!(Cli::try_parse_from(argv).is_err(), "{bad:?} 應被拒絕");
        }
    }

    #[test]
    fn effort_values_match_adapter() {
        assert_eq!(EFFORT_VALUES, termade_adapter::EFFORT_LEVELS);
    }

    #[test]
    fn zsh_script_completes_sessions() {
        let script = completion_script(Shell::Zsh);
        // 函式定義要在結尾呼叫 `_termade` 之前。
        let def = script.find("_termade_sessions() {").expect("應定義函式");
        let tail = script.rfind("_termade \"$@\"").expect("應有結尾呼叫");
        assert!(script.starts_with("#compdef termade\n") && def < tail);
        for spec in [
            "':session:_termade_sessions'",
            ":SESSION:_termade_sessions'",
            ":PARENT:_termade_sessions'",
            ":ASSIGNEE:_termade_sessions'",
        ] {
            assert!(script.contains(spec), "zsh 腳本應含 {spec}");
        }
        // `ask <peer>` 帶說明文字；`send --to` 是 session，`handoff --to` 是 agent 種類。
        assert!(script
            .lines()
            .any(|l| l.contains(":peer -- ") && l.contains(":_termade_sessions")));
        assert!(script.contains(":TO:_termade_sessions'"));
        assert!(script.contains(":TO:(opencode claude codex antigravity)'"));
        assert!(!script.contains(":REPLY_TO:_termade_sessions"));
    }

    #[test]
    fn bash_script_completes_sessions() {
        let script = completion_script(Shell::Bash);
        assert!(script.contains("complete -F _termade_with_sessions"));
        let line = |needle: &str| {
            script
                .lines()
                .find(|l| l.contains(needle) && l.contains("want=1"))
                .unwrap_or_else(|| panic!("找不到含 {needle} 的 case"))
                .to_string()
        };
        let positional = line("prompt|");
        for sub in ["prompt", "diff", "merge", "kill", "ask"] {
            assert!(
                positional.split(['|', ')', ' ']).any(|w| w == sub),
                "{sub} 的第一個參數應補 session"
            );
        }
        assert!(!positional.contains("spawn") && !positional.contains("task"));
        let flags = line("send:--to");
        assert!(flags.contains("spawn:--parent") && flags.contains("gc:--session"));
        assert!(!flags.contains("handoff:--to") && !flags.contains("send:--reply-to"));
    }

    #[test]
    fn session_names_prefer_label_and_scope_to_project() {
        let json = serde_json::json!({
            "id": "ses_1", "kind": "claude_code", "label": "worker/a", "cwd": "/p/a",
            "status": "idle", "created_at": 0,
        });
        let Ok(base) = serde_json::from_value::<termade_core::Session>(json) else {
            // Session 的必填欄位變動時，這個測試交給 tests/completions.rs 的端到端測試把關。
            return;
        };
        let mut state = State::default();
        let mut add = |id: &str, label: &str, cwd: &str, repo: Option<&str>| {
            let mut s = base.clone();
            s.id = id.into();
            s.label = label.into();
            s.cwd = cwd.into();
            s.repo_root = repo.map(str::to_string);
            state.sessions.insert(s.id.clone(), s);
        };
        add("ses_1", "worker/a", "/p/a", None);
        add("ses_2", "", "/p/a", None);
        add("ses_3", "wt", "/p/a/.termade/wt/ses_3", Some("/p/a"));
        add("ses_4", "other", "/p/b", None);
        let mut names = session_names(&state, "/p/a");
        names.sort();
        assert_eq!(names, ["ses_2", "worker/a", "wt"]);
    }
}
