//! `ertangle-adapter` — Normalization layer for diverse agents.
//!
//! Protocols for each agent (ACP, stream-json, app-server, PTY) are translated here into
//! a uniform set of [`AgentEvent`]s, so layers above the daemon do not need to know which agent is underneath.
//!
//! Structured adapters (native, zero extra dependencies):
//! * [`acp`]: Agent Client Protocol — OpenCode (`opencode acp`), Antigravity (`agy acp`).
//! * [`claude`]: Claude Code's `--input-format/--output-format stream-json`.
//! * [`codex`]: Codex's `codex app-server` (stdio JSON-RPC).
//!
//! Universal fallback:
//! * [`pty`]: PTY passthrough, **works for any CLI agent**.

pub mod acp;
pub mod ansi;
pub mod claude;
pub mod codex;
pub mod models;
pub mod pty;
pub mod question;
pub mod stderr;
pub mod stream;

use anyhow::{bail, Context, Result};
use ertangle_core::{
    AgentKind, Cost, Decision, PermissionPolicy, PermissionRequest, SessionId, SessionStatus,
};
use ertangle_protocol::Transport;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use tr::tr;

/// Events sent upward after normalization by the adapter.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Status(SessionStatus),
    /// Text output from the agent (may contain newlines).
    Output(String),
    ToolCall {
        name: String,
        input: String,
        /// `None` = in progress.
        ok: Option<bool>,
        output: Option<String>,
    },
    Permission(PermissionRequest),
    Question(ertangle_core::UserQuestion),
    Cost(Cost),
    Model(String),
    /// Native slash command names of the agent (without `/`).
    Commands(Vec<String>),
    /// Native session ID of the agent (for resumption). Sent only when resumption is possible with this ID.
    NativeId(String),
    /// Quota exhausted / rate limited.
    Limited(String),
    /// Sets a status note for the session (e.g. failure reason).
    Note(String),
    /// Session context usage.
    Context {
        used: u64,
        window: Option<u64>,
    },
    /// Account quota status update.
    Quota(ertangle_core::Quota),
    /// Process exited.
    Exited(Option<i32>),
}

/// All information required to launch a session.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    pub id: SessionId,
    pub kind: AgentKind,
    pub cwd: PathBuf,
    pub transport: Transport,
    /// Custom executable path or name (overrides default PATH lookup).
    pub bin: Option<String>,
    /// Extra command-line arguments.
    pub args: Vec<String>,
    /// Model to use (currently only supported by Claude structured mode).
    pub model: Option<String>,
    /// Reasoning effort (Claude `--effort`, Codex `model_reasoning_effort`, ACP configOption).
    pub effort: Option<String>,
    /// Claude-specific: extra allowed / disallowed tools.
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    /// Read-only mode (achieved via plan mode in Claude).
    pub read_only: bool,
    /// Codex-specific sandbox mode.
    pub sandbox: Option<String>,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
    /// Unified permission policy.
    pub permission: PermissionPolicy,
    /// If the `ertangle` executable path is provided, the adapter injects `ertangle mcp` into the agent at startup.
    pub mcp_exe: Option<PathBuf>,
    /// Collaboration instructions appended to the system prompt (Claude `--append-system-prompt`, Codex `developerInstructions`)
    /// or prepended to the first prompt in ACP; not injected for PTY.
    pub system_append: Option<String>,
    /// First prompt sent immediately after startup.
    pub initial_prompt: Option<String>,
    /// Attachments for the initial prompt.
    pub initial_attachments: Vec<ertangle_core::Attachment>,
    /// Resume an existing conversation: agent's native conversation ID. `None` starts a new conversation.
    pub resume: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

/// Control handle for a running session.
pub trait AgentControl: Send + Sync {
    /// Sends user input / prompt text.
    fn send_input(&self, text: &str) -> Result<()>;
    /// Sends a prompt with files/images attached. By default inlines attachments into text; adapters supporting image blocks override this.
    fn send_input_with(&self, text: &str, attachments: &[ertangle_core::Attachment]) -> Result<()> {
        self.send_input(&ertangle_core::compose_text(text, attachments, true))
    }
    /// Interrupts the current turn (without killing the process).
    fn cancel(&self) -> Result<()>;
    /// Terminates and kills the process.
    fn kill(&self) -> Result<()>;
    /// Pauses child processes (SIGSTOP, used for idle sleep).
    fn sleep(&self) -> Result<()> {
        Ok(())
    }
    /// Wakes child processes (SIGCONT).
    fn wake(&self) -> Result<()> {
        Ok(())
    }
    /// Resolves a permission request.
    fn resolve_permission(&self, req: &str, decision: Decision) -> Result<()>;
    /// Resolves an interactive user question.
    fn resolve_question(&self, id: &str, answers: &[String]) -> Result<()> {
        let _ = (id, answers);
        bail!(
            "{}",
            tr!("{transport} 不支援回答問題", transport = self.transport())
        )
    }
    /// Actual transport used by this session (for display).
    fn transport(&self) -> &'static str;
    /// Gets the agent main process PID.
    fn pid(&self) -> Option<u32> {
        None
    }
    /// Switches model and/or effort at runtime (takes effect from the next turn).
    /// On success returns the model actually in effect (if reported by the agent); unsupported adapters return a clear error.
    fn set_config(&self, model: Option<&str>, effort: Option<&str>) -> Result<Option<String>> {
        let _ = (model, effort);
        bail!(
            "{}",
            tr!(
                "{transport} 不支援執行期切換 model／effort",
                transport = self.transport()
            )
        )
    }
}

/// Checks if an effort value is valid.
pub fn check_effort(e: &str) -> Result<()> {
    if !EFFORT_LEVELS.contains(&e) {
        bail!(
            "{}",
            tr!(
                "未知的 effort `{e}`；可用：{list}",
                e = e,
                list = EFFORT_LEVELS.join(", ")
            )
        );
    }
    Ok(())
}

/// Builds arguments for `kill -s <NAME> -- <target>`.
///
/// Always use the `-s NAME --` form: procps-ng 4.0.4's `kill -KILL -<pgid>` misparses
/// `-<pgid>` as `-1` (all processes for the user), equivalent to `kill -9 -1`, which logs out the entire desktop.
fn kill_args(sig: &str, target: String) -> [String; 4] {
    let name = sig.trim_start_matches('-').trim_start_matches("SIG");
    ["-s".into(), name.into(), "--".into(), target]
}

fn run_kill(args: [String; 4]) -> Result<()> {
    let status = std::process::Command::new("kill")
        .args(&args)
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| tr!("執行 kill 失敗"))?;
    if !status.success() {
        bail!("{}", tr!("kill {args} 失敗", args = args.join(" ")));
    }
    Ok(())
}

/// Sends a Unix signal (e.g. `-STOP` / `-CONT`) to a specific PID.
pub fn signal_pid(pid: u32, sig: &str) -> Result<()> {
    // Never send to 0 / 1 as they refer to "own group" or init.
    if pid <= 1 {
        bail!("{}", tr!("拒絕對 PID {pid} 送訊號", pid = pid));
    }
    run_kill(kill_args(sig, pid.to_string()))
}

/// Sends a Unix signal (e.g. `-STOP` / `-CONT` / `-KILL`) to a specific process group (PGID).
pub fn signal_pgid(pgid: u32, sig: &str) -> Result<()> {
    // Never send to `-0` (own group) or `-1` (all processes).
    if pgid <= 1 {
        bail!("{}", tr!("拒絕對 PID {pid} 送訊號", pid = pgid));
    }
    run_kill(kill_args(sig, format!("-{pgid}")))
}

#[cfg(test)]
mod signal_tests {
    use super::*;

    #[test]
    fn kill_args_never_put_negative_target_before_double_dash() {
        assert_eq!(
            kill_args("-KILL", "-4242".into()),
            ["-s", "KILL", "--", "-4242"]
        );
        assert_eq!(kill_args("-STOP", "42".into()), ["-s", "STOP", "--", "42"]);
        assert_eq!(
            kill_args("SIGCONT", "42".into()),
            ["-s", "CONT", "--", "42"]
        );
    }

    #[test]
    fn refuses_pid_zero_and_one() {
        assert!(signal_pgid(0, "-KILL").is_err());
        assert!(signal_pgid(1, "-KILL").is_err());
        assert!(signal_pid(0, "-KILL").is_err());
        assert!(signal_pid(1, "-KILL").is_err());
    }

    #[test]
    fn pgid_signal_hits_only_the_group() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        signal_pgid(child.id(), "-KILL").unwrap();
        let st = child.wait().unwrap();
        assert!(!st.success());
    }
}

/// Probes whether an executable exists and returns its absolute path.
pub fn which(prog: &str) -> Option<PathBuf> {
    if prog.contains('/') {
        let p = PathBuf::from(prog);
        return p.is_file().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(prog);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Finds the agent executable: checks environment variable override first (for testing and custom installs), then searches PATH.
pub fn agent_bin(prog: &str, env_var: &str) -> Option<PathBuf> {
    match ertangle_core::env_var(env_var) {
        Some(p) if !p.is_empty() => which(&p),
        _ => which(prog),
    }
}

/// Valid reasoning effort levels.
pub const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Starts a session and returns a control handle; events stream through `tx`.
///
/// Determines whether to use native structured protocols or PTY passthrough based on `spec.transport`.
/// `Auto` chooses the best native protocol for the agent; falls back to PTY if native startup fails.
pub fn spawn(spec: SessionSpec, tx: Sender<AgentEvent>) -> Result<Box<dyn AgentControl>> {
    // Explicitly reject unsupported granular permission options instead of silently ignoring them.
    let is_claude = matches!(spec.kind, AgentKind::ClaudeCode);
    if let Some(e) = &spec.effort {
        if !EFFORT_LEVELS.contains(&e.as_str()) {
            bail!(
                "{}",
                tr!(
                    "未知的 effort `{e}`；可用：{list}",
                    e = e,
                    list = EFFORT_LEVELS.join(", ")
                )
            );
        }
    }
    let is_codex = matches!(spec.kind, AgentKind::Codex);
    if !is_claude && (!spec.allowed_tools.is_empty() || !spec.disallowed_tools.is_empty()) {
        bail!(
            "{}",
            tr!(
                "{kind} 尚不支援 allowed_tools／disallowed_tools；請改用 claude",
                kind = spec.kind
            )
        );
    }
    if spec.read_only && !is_claude && !is_codex {
        bail!(
            "{}",
            tr!(
                "{kind} 尚不支援 read_only；請改用 claude 或 codex",
                kind = spec.kind
            )
        );
    }
    if spec.sandbox.is_some() && !is_codex {
        bail!("{}", tr!("sandbox 只適用於 codex"));
    }
    // Resumption is only supported via native protocols; do not silently start a new conversation.
    if spec.resume.is_some() {
        if spec.transport == Transport::Pty {
            bail!("{}", tr!("PTY 模式無法接續對話"));
        }
        if matches!(spec.kind, AgentKind::Custom(_)) {
            bail!(
                "{}",
                tr!("{kind} 沒有原生結構化協定，無法接續對話", kind = spec.kind)
            );
        }
    }
    let want_structured = match spec.transport {
        _ if spec.resume.is_some() => true,
        Transport::Pty => false,
        Transport::Structured => true,
        Transport::Auto => supports_structured(&spec.kind),
    };

    if want_structured {
        match structured_start(spec.clone(), tx.clone()) {
            Ok(ctl) => return Ok(ctl),
            Err(e)
                if spec.transport == Transport::Auto
                    && spec.model.is_none()
                    && spec.resume.is_none() =>
            {
                tracing::warn!("{} 原生協定啟動失敗，退回 PTY：{e}", spec.kind);
            }
            Err(e) => return Err(e),
        }
    }

    if let Some(m) = &spec.model {
        if spec.transport != Transport::Pty {
            bail!(
                "{}",
                tr!(
                    "{kind} 尚不支援以結構化模式指定 model ({m})；請改用 --transport pty 並以參數傳入",
                    kind = spec.kind,
                    m = m
                )
            );
        }
    }
    let (prog, _) = spec.kind.pty_command();
    if which(prog).is_none() {
        bail!("{}", tr!("找不到 `{prog}`，請先安裝", prog = prog));
    }
    pty::start(spec, tx)
}

/// Whether this agent has a native structured protocol available.
pub fn supports_structured(kind: &AgentKind) -> bool {
    match kind {
        AgentKind::OpenCode | AgentKind::Antigravity => kind.acp_command().is_some(),
        AgentKind::ClaudeCode => agent_bin("claude", "ERTANGLE_CLAUDE_BIN").is_some(),
        AgentKind::Codex => which("codex").is_some(),
        AgentKind::Custom(_) => false,
    }
}

/// Checks whether the corresponding structured agent binary is installed in local `$PATH`.
pub fn is_installed(kind: &AgentKind) -> bool {
    match kind {
        AgentKind::ClaudeCode => agent_bin("claude", "ERTANGLE_CLAUDE_BIN").is_some(),
        _ => is_installed_with(kind, |prog| which(prog).is_some()),
    }
}

fn is_installed_with<F: Fn(&str) -> bool>(kind: &AgentKind, has_bin: F) -> bool {
    match kind {
        AgentKind::OpenCode | AgentKind::Antigravity => {
            kind.acp_command().is_some_and(|(prog, _)| has_bin(prog))
        }
        AgentKind::ClaudeCode => has_bin("claude"),
        AgentKind::Codex => has_bin("codex"),
        AgentKind::Custom(_) => false,
    }
}

/// Starts the corresponding native structured adapter based on agent kind.
fn structured_start(spec: SessionSpec, tx: Sender<AgentEvent>) -> Result<Box<dyn AgentControl>> {
    match spec.kind {
        AgentKind::OpenCode | AgentKind::Antigravity => {
            let (prog, _) = spec
                .kind
                .acp_command()
                .with_context(|| tr!("此 agent 不支援 ACP"))?;
            let target = spec.bin.as_deref().unwrap_or(prog);
            if which(target).is_none() {
                bail!(
                    "{}",
                    tr!(
                        "找不到 `{target}`，請先安裝或改用 --transport pty",
                        target = target
                    )
                );
            }
            acp::start(spec, tx)
        }
        AgentKind::ClaudeCode => {
            let target = spec.bin.as_deref().unwrap_or("claude");
            if spec.bin.is_none() && ertangle_core::env_var_os("ERTANGLE_CLAUDE_BIN").is_some() {
                // Handled by claude::start via ERTANGLE_CLAUDE_BIN
            } else if which(target).is_none() {
                bail!(
                    "{}",
                    tr!(
                        "找不到 `{target}`，請先安裝或改用 --transport pty",
                        target = target
                    )
                );
            }
            claude::start(spec, tx)
        }
        AgentKind::Codex => {
            let target = spec.bin.as_deref().unwrap_or("codex");
            if which(target).is_none() {
                bail!(
                    "{}",
                    tr!(
                        "找不到 `{target}`，請先安裝或改用 --transport pty",
                        target = target
                    )
                );
            }
            codex::start(spec, tx)
        }
        AgentKind::Custom(_) => bail!(
            "{}",
            tr!("自訂 agent 沒有原生結構化協定，請用 --transport pty")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_sh_and_misses_nonsense() {
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn is_installed_checks_each_agent_binary() {
        let only_agy_and_claude = |bin: &str| matches!(bin, "agy" | "claude");
        assert!(is_installed_with(
            &AgentKind::Antigravity,
            only_agy_and_claude
        ));
        assert!(is_installed_with(
            &AgentKind::ClaudeCode,
            only_agy_and_claude
        ));
        assert!(!is_installed_with(
            &AgentKind::OpenCode,
            only_agy_and_claude
        ));
        assert!(!is_installed_with(&AgentKind::Codex, only_agy_and_claude));
        assert!(!is_installed_with(&AgentKind::Custom("foo".into()), |_| {
            true
        }));
    }
}
