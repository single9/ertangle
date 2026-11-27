//! `ertangle-core` — Domain model.
//!
//! Contains environment-agnostic types only: agent kinds, sessions, events, message bus, tasks.
//! The daemon, adapter, and TUI all share this set of types, making it the contract for the entire system.
//!
//! Design principles:
//! * **Event is the single source of truth**: all state is the result of folding events into [`State`].
//! * Events are serializable, persistable, and replayable, enabling TUIs to rebuild views seamlessly after disconnects.

pub mod i18n;

/// Package version with short git commit hash when available.
pub const VERSION_WITH_HASH: &str = env!("ERTANGLE_VERSION_WITH_HASH");

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};
use tr::tr;

/// Environment variable prefix for the legacy name (termade); compatibility layer retained until v1.0.
const LEGACY_ENV_PREFIX: &str = "TERMADE_";
const ENV_PREFIX: &str = "ERTANGLE_";

/// Maps `ERTANGLE_X` to legacy `TERMADE_X`; names not starting with `ERTANGLE_` have no legacy counterpart.
fn legacy_env_name(name: &str) -> Option<String> {
    name.strip_prefix(ENV_PREFIX)
        .map(|rest| format!("{LEGACY_ENV_PREFIX}{rest}"))
}

/// Reads environment variable with injected lookup function: reads new name first, falls back to legacy name if unset.
fn env_lookup<T>(name: &str, get: impl Fn(&str) -> Option<T>) -> Option<T> {
    get(name).or_else(|| legacy_env_name(name).and_then(|old| get(&old)))
}

/// Reads environment variable (`ERTANGLE_X`, falling back to legacy `TERMADE_X` if unset); non-unicode values treated as unset.
pub fn env_var(name: &str) -> Option<String> {
    env_lookup(name, |k| std::env::var(k).ok())
}

/// Same as [`env_var`], but returns raw `OsString`.
pub fn env_var_os(name: &str) -> Option<std::ffi::OsString> {
    env_lookup(name, |k| std::env::var_os(k))
}

#[cfg(test)]
mod env_compat_tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(map: &HashMap<&str, &str>, name: &str) -> Option<String> {
        env_lookup(name, |k| map.get(k).map(|v| v.to_string()))
    }

    #[test]
    fn prefers_new_name() {
        let m = HashMap::from([("ERTANGLE_X", "new"), ("TERMADE_X", "old")]);
        assert_eq!(lookup(&m, "ERTANGLE_X").as_deref(), Some("new"));
    }

    #[test]
    fn falls_back_to_legacy_name() {
        let m = HashMap::from([("TERMADE_X", "old")]);
        assert_eq!(lookup(&m, "ERTANGLE_X").as_deref(), Some("old"));
    }

    #[test]
    fn unset_returns_none_and_other_prefixes_have_no_legacy() {
        let m = HashMap::from([("TERMADE_X", "old")]);
        assert_eq!(lookup(&m, "ERTANGLE_Y"), None);
        assert_eq!(lookup(&m, "OTHER_X"), None);
        assert_eq!(legacy_env_name("HOME"), None);
    }
}

/// Current time (Unix epoch milliseconds).
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Generates a prefixed random ID, e.g. `ses_3f2a…`.
pub fn new_id(prefix: &str) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}_{}", &u[..12])
}

pub type SessionId = String;
pub type MsgId = String;
pub type TaskId = String;

/// Monotonic sequence number for each event, used for incremental sync (`since`).
pub type Seq = u64;

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

/// Supported agent kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    #[serde(alias = "opencode", alias = "OpenCode")]
    OpenCode,
    #[serde(alias = "claude", alias = "claude-code", alias = "ClaudeCode")]
    ClaudeCode,
    #[serde(alias = "Codex")]
    Codex,
    #[serde(alias = "agy", alias = "gemini", alias = "Antigravity")]
    Antigravity,
    /// Custom user agent: specifies executable directly.
    Custom(String),
}

impl AgentKind {
    /// Stable identifier (for CLI / configuration).
    pub fn id(&self) -> &str {
        match self {
            AgentKind::OpenCode => "opencode",
            AgentKind::ClaudeCode => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Antigravity => "antigravity",
            AgentKind::Custom(name) => name,
        }
    }

    pub fn parse(s: &str) -> Option<AgentKind> {
        Some(match s.to_ascii_lowercase().as_str() {
            "opencode" | "oc" => AgentKind::OpenCode,
            "claude" | "claude-code" | "claudecode" => AgentKind::ClaudeCode,
            "codex" => AgentKind::Codex,
            "antigravity" | "agy" | "gemini" => AgentKind::Antigravity,
            _ => return None,
        })
    }

    /// Four built-in selectable agents (excluding Custom).
    pub fn builtins() -> [AgentKind; 4] {
        [
            AgentKind::OpenCode,
            AgentKind::ClaudeCode,
            AgentKind::Codex,
            AgentKind::Antigravity,
        ]
    }

    /// Launch command for interactive mode (PTY passthrough).
    pub fn pty_command(&self) -> (&str, Vec<&'static str>) {
        match self {
            AgentKind::OpenCode => ("opencode", vec![]),
            AgentKind::ClaudeCode => ("claude", vec![]),
            AgentKind::Codex => ("codex", vec![]),
            AgentKind::Antigravity => ("agy", vec![]),
            AgentKind::Custom(cmd) => (cmd.as_str(), vec![]),
        }
    }

    /// Launch command for native structured protocol (if supported).
    ///
    /// OpenCode and Antigravity provide native ACP directly (`opencode acp` / `agy acp`),
    /// without extra npm packages; Claude and Codex use their respective structured interfaces (see adapter crate).
    pub fn acp_command(&self) -> Option<(&str, Vec<&'static str>)> {
        match self {
            AgentKind::OpenCode => Some(("opencode", vec!["acp"])),
            AgentKind::Antigravity => Some(("agy", vec!["acp"])),
            _ => None,
        }
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// Role of an agent in this system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Breaks down tasks, spawns workers, and integrates results.
    Coordinator,
    /// Directly modifies files.
    #[default]
    Worker,
    /// Read-only, responsible for review.
    Reviewer,
    Custom(String),
}

impl Role {
    pub fn id(&self) -> &str {
        match self {
            Role::Coordinator => "coordinator",
            Role::Worker => "worker",
            Role::Reviewer => "reviewer",
            Role::Custom(n) => n,
        }
    }
    pub fn parse(s: &str) -> Role {
        match s.to_ascii_lowercase().as_str() {
            "coordinator" | "coord" => Role::Coordinator,
            "reviewer" | "review" => Role::Reviewer,
            "worker" => Role::Worker,
            other => Role::Custom(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Starting,
    Idle,
    Busy,
    WaitingApproval,
    /// Idle sleep (process paused with SIGSTOP, automatically resumed with SIGCONT on new messages or wakeup).
    Sleeping,
    Exited,
    Failed,
}

impl SessionStatus {
    /// Single-character indicator for TUI display.
    pub fn glyph(self) -> &'static str {
        match self {
            SessionStatus::Starting => "◌",
            SessionStatus::Idle => "○",
            SessionStatus::Busy => "●",
            SessionStatus::WaitingApproval => "⏸",
            SessionStatus::Sleeping => "☾",
            SessionStatus::Exited => "✓",
            SessionStatus::Failed => "✗",
        }
    }
    pub fn is_live(self) -> bool {
        !matches!(self, SessionStatus::Exited | SessionStatus::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Cost {
    pub usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Current context usage of a session (tokens used by conversation and the model context window).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used: u64,
    /// Model context window size; `None` when the agent has not yet reported.
    pub window: Option<u64>,
}

impl ContextUsage {
    pub fn percent(&self) -> Option<f32> {
        self.window
            .filter(|w| *w > 0)
            .map(|w| self.used as f32 * 100.0 / w as f32)
    }

    /// `36k/1M 4%`; displays only `36k` when window size is unknown.
    pub fn short(&self) -> String {
        match (self.window, self.percent()) {
            (Some(w), Some(p)) => {
                format!("{}/{} {:.0}%", human_tokens(self.used), human_tokens(w), p)
            }
            _ => human_tokens(self.used),
        }
    }
}

/// Short representation of token count: `950`, `36k`, `1.5M`.
pub fn human_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{}k", (n as f64 / 1000.0).round() as u64),
        _ => {
            let m = n as f64 / 1_000_000.0;
            if (m - m.round()).abs() < 0.05 {
                format!("{}M", m.round() as u64)
            } else {
                format!("{m:.1}M")
            }
        }
    }
}

/// Formats USD budget into short string: omits decimals for whole amounts (`$5`), otherwise retains two decimals (`$2.50`).
pub fn format_usd_limit(limit: f64) -> String {
    if (limit - limit.round()).abs() < 1e-6 {
        format!("${:.0}", limit)
    } else {
        format!("${:.2}", limit)
    }
}

/// A running instance of an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub kind: AgentKind,
    /// Human-readable label, e.g. `worker/eu-zip`.
    pub label: String,
    /// Working directory (may be a worktree path).
    pub cwd: String,
    /// Records branch name if within a worktree.
    pub branch: Option<String>,
    pub role: Role,
    pub status: SessionStatus,
    pub model: Option<String>,
    pub cost: Cost,
    /// Creator (human or session ID).
    pub parent: Option<SessionId>,
    pub created_ms: u64,
    /// Transport used by adapter (for display).
    pub transport: String,
    /// Short progress note reported by the agent via `ertangle_status`.
    #[serde(default)]
    pub status_note: Option<String>,
    /// Root directory of the main repo when creating a worktree (for merge/gc/pr).
    #[serde(default)]
    pub repo_root: Option<String>,
    /// Permission policy for this session (`auto` if absent in older logs).
    #[serde(default)]
    pub permission: PermissionPolicy,
    /// Context usage (reported by agent; absent in older logs).
    #[serde(default)]
    pub context: Option<ContextUsage>,
    /// Native conversation ID of the agent (Claude session id, Codex thread id, ACP sessionId) for resumption.
    /// `None` indicates this session cannot be resumed (older logs, PTY, or agent did not declare load capability).
    #[serde(default)]
    pub native_id: Option<String>,
    /// Cost limit for this session (USD; resolved actual limit).
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// Whether the cost limit has been reached (interrupts turn and rejects new prompts until manually raised).
    #[serde(default)]
    pub over_budget: bool,
    /// Successor session (set by `Event::SessionHandedOff` after handoff; absent in older logs).
    #[serde(default)]
    pub handed_off_to: Option<SessionId>,
    /// Current reasoning effort (resolved at startup or updated at runtime; absent in older logs).
    #[serde(default)]
    pub effort: Option<String>,
    /// Native slash commands reported by agent (without `/`; Claude `slash_commands`, ACP `available_commands_update`).
    #[serde(default)]
    pub slash_commands: Vec<String>,
    /// Currently executing tools or subagents.
    #[serde(default)]
    pub active_tools: Vec<ActiveTool>,
    /// List of remaining background child processes.
    #[serde(default)]
    pub bg_procs: Vec<BgProc>,
    /// Cumulative tool call count (counts start events only; approximate turn count, absent in older logs).
    #[serde(default)]
    pub tool_calls: u32,
    /// When the session last stopped being live (ms since epoch). Drives the retention
    /// clocks for ended sessions and archived entries; absent in older logs.
    #[serde(default)]
    pub ended_ms: Option<u64>,
}

impl Session {
    /// Time the session ended, falling back to creation time for older logs.
    pub fn ended_or_created_ms(&self) -> u64 {
        self.ended_ms.unwrap_or(self.created_ms)
    }

    /// Which project this session belongs to: main repo for worktrees, otherwise working directory.
    pub fn project_root(&self) -> &str {
        self.repo_root.as_deref().unwrap_or(&self.cwd)
    }

    /// Returns string formatted as `$1.20/$5` when cost limit is configured.
    pub fn format_budget(&self) -> Option<String> {
        self.budget_usd
            .map(|limit| format!("${:.2}/{}", self.cost.usd, format_usd_limit(limit)))
    }
}

/// Currently executing tool or subagent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveTool {
    pub name: String,
    pub summary: String,
    pub started_ms: u64,
    pub is_subagent: bool,
}

/// Remaining background child process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BgProc {
    pub pid: u32,
    pub name: String,
    pub cmdline: String,
    #[serde(default)]
    pub elapsed_secs: u64,
}

/// Checks if a tool name represents a native subagent tool.
pub fn is_subagent_tool(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "task" | "agent" | "dispatch_agent" | "invoke_subagent" | "spawn_agent" | "subagent"
    )
}

// ---------------------------------------------------------------------------
// Bus / Task / Artifact
// ---------------------------------------------------------------------------

/// Recipient of a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Direct(SessionId),
    Channel(String),
    Broadcast,
}

impl Target {
    /// Parses recipient target from string:
    /// * `*` or `broadcast` -> [`Target::Broadcast`]
    /// * `#name` or `channel:name` -> [`Target::Channel`]
    /// * `@id` or other string -> [`Target::Direct`]
    pub fn parse(s: &str) -> Target {
        let s = s.trim();
        if s == "*" || s.eq_ignore_ascii_case("broadcast") {
            Target::Broadcast
        } else if let Some(ch) = s.strip_prefix('#').or_else(|| s.strip_prefix("channel:")) {
            Target::Channel(ch.to_string())
        } else if let Some(d) = s.strip_prefix('@') {
            Target::Direct(d.to_string())
        } else {
            Target::Direct(s.to_string())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsgKind {
    #[default]
    Chat,
    /// Hands off task.
    Handoff,
    Question,
    Answer,
    Status,
    /// Shares artifacts (file / branch / diff).
    Artifact,
}

impl MsgKind {
    pub fn id(self) -> &'static str {
        match self {
            MsgKind::Chat => "chat",
            MsgKind::Handoff => "handoff",
            MsgKind::Question => "question",
            MsgKind::Answer => "answer",
            MsgKind::Status => "status",
            MsgKind::Artifact => "artifact",
        }
    }

    pub fn parse(s: &str) -> Option<MsgKind> {
        Some(match s.to_ascii_lowercase().as_str() {
            "chat" | "message" => MsgKind::Chat,
            "handoff" => MsgKind::Handoff,
            "question" | "ask" => MsgKind::Question,
            "answer" | "reply" => MsgKind::Answer,
            "status" => MsgKind::Status,
            "artifact" => MsgKind::Artifact,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusMessage {
    pub id: MsgId,
    pub from: SessionId,
    pub to: Target,
    #[serde(default)]
    pub kind: MsgKind,
    pub body: String,
    pub reply_to: Option<MsgId>,
    pub ts: u64,
}

impl BusMessage {
    /// Whether this message should enter a specific session's inbox.
    ///
    /// * Broadcast/channel messages sent by self are not delivered to own inbox.
    /// * `Direct` matches both `session_id` and human-readable `label`.
    pub fn matches_recipient(&self, session_id: &str, label: Option<&str>) -> bool {
        match &self.to {
            Target::Broadcast | Target::Channel(_) => self.from != session_id,
            Target::Direct(target) => target == session_id || label.is_some_and(|l| l == target),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[serde(alias = "Todo")]
    Todo,
    #[serde(alias = "Doing")]
    Doing,
    #[serde(alias = "Blocked")]
    Blocked,
    #[serde(alias = "Review")]
    Review,
    #[serde(alias = "Done")]
    Done,
}

impl TaskStatus {
    pub fn id(self) -> &'static str {
        match self {
            TaskStatus::Todo => "todo",
            TaskStatus::Doing => "doing",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Review => "review",
            TaskStatus::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Option<TaskStatus> {
        Some(match s.to_ascii_lowercase().as_str() {
            "todo" | "open" => TaskStatus::Todo,
            "doing" | "in_progress" | "in-progress" | "working" => TaskStatus::Doing,
            "blocked" => TaskStatus::Blocked,
            "review" => TaskStatus::Review,
            "done" | "completed" | "closed" => TaskStatus::Done,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    pub status: TaskStatus,
    pub assignee: Option<SessionId>,
    #[serde(default)]
    pub created_by: Option<SessionId>,
    #[serde(default)]
    pub depends_on: Vec<TaskId>,
    pub updated_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    #[default]
    File,
    Branch,
    Diff,
}

impl ArtifactKind {
    pub fn id(self) -> &'static str {
        match self {
            ArtifactKind::File => "file",
            ArtifactKind::Branch => "branch",
            ArtifactKind::Diff => "diff",
        }
    }

    pub fn parse(s: &str) -> Option<ArtifactKind> {
        Some(match s.to_ascii_lowercase().as_str() {
            "file" | "path" => ArtifactKind::File,
            "branch" => ArtifactKind::Branch,
            "diff" | "patch" => ArtifactKind::Diff,
            _ => return None,
        })
    }
}

/// Artifact shared by agent (file, branch, or diff).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub session: SessionId,
    pub kind: ArtifactKind,
    pub path: String,
    #[serde(default)]
    pub note: String,
    pub ts: u64,
}

/// Inline review comment on a diff line (M3 review loop).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewComment {
    pub id: String,
    pub session: SessionId,
    /// File and line number or summary of diff line content.
    pub location: String,
    pub comment: String,
    pub ts: u64,
}

/// Worktree summary information (M3 workspace management).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub session: SessionId,
    pub label: String,
    pub path: String,
    pub branch: String,
    pub live: bool,
    pub changed_files: usize,
    pub summary: String,
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

/// Unified permission policy: determines whether agent tool execution requires human approval.
///
/// Each adapter translates this policy into its protocol-supported form:
/// * ACP (OpenCode / Antigravity): `Ask` = forwards `session/request_permission`;
///   `Auto` / `Bypass` = auto-approved directly by adapter.
/// * Claude: maps to `--permission-mode` flags.
/// * Codex: maps to `approvalPolicy` in `thread/start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    /// Prompts for each item (ACP forwards requests to client; unsupported protocols remain conservative).
    Ask,
    /// Auto-approves safe operations (default): does not interrupt workflows while maintaining protocol boundaries.
    #[default]
    Auto,
    /// Bypasses all permission checks; agent can perform any operation (dangerous).
    Bypass,
}

impl PermissionPolicy {
    /// Permissiveness: `ask` < `auto` < `bypass`.
    pub fn rank(&self) -> u8 {
        match self {
            PermissionPolicy::Ask => 0,
            PermissionPolicy::Auto => 1,
            PermissionPolicy::Bypass => 2,
        }
    }

    /// Takes the more restrictive of the two.
    pub fn min_policy(self, other: PermissionPolicy) -> PermissionPolicy {
        if other.rank() < self.rank() {
            other
        } else {
            self
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            PermissionPolicy::Ask => "ask",
            PermissionPolicy::Auto => "auto",
            PermissionPolicy::Bypass => "bypass",
        }
    }

    pub fn parse(s: &str) -> Option<PermissionPolicy> {
        Some(match s.to_ascii_lowercase().as_str() {
            "ask" | "manual" => PermissionPolicy::Ask,
            "auto" | "accept-edits" | "acceptedits" => PermissionPolicy::Auto,
            "bypass" | "yolo" | "dangerous" => PermissionPolicy::Bypass,
            _ => return None,
        })
    }

    /// Whether the adapter auto-approves requests directly (without forwarding to human).
    pub fn auto_approves(&self) -> bool {
        matches!(self, PermissionPolicy::Auto | PermissionPolicy::Bypass)
    }

    /// CLI flags for Claude Code.
    pub fn claude_flags(&self) -> Vec<&'static str> {
        match self {
            PermissionPolicy::Ask => vec!["--permission-mode", "manual"],
            PermissionPolicy::Auto => vec!["--permission-mode", "acceptEdits"],
            PermissionPolicy::Bypass => vec!["--dangerously-skip-permissions"],
        }
    }

    /// CLI flags for Codex app-server.
    ///
    /// `codex app-server` does not accept permission settings via CLI flags; it uses
    /// `approvalPolicy` (see [`PermissionPolicy::codex_approval_policy`]), so this is always empty.
    pub fn codex_flags(&self) -> Vec<&'static str> {
        vec![]
    }

    /// `approvalPolicy` value for Codex `thread/start`.
    pub fn codex_approval_policy(&self) -> &'static str {
        match self {
            PermissionPolicy::Ask => "untrusted",
            PermissionPolicy::Auto => "on-request",
            PermissionPolicy::Bypass => "never",
        }
    }
}

impl std::fmt::Display for PermissionPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    #[serde(alias = "Allow")]
    Allow,
    /// Always allows similar requests within this session.
    #[serde(alias = "always", alias = "Always", alias = "AllowAlways")]
    AllowAlways,
    #[serde(alias = "Deny")]
    Deny,
}

/// Single option for interactive user question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Value submitted to the agent when this option is selected. Defaults to `label`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

impl QuestionOption {
    pub fn new(label: impl Into<String>, description: Option<&str>) -> Self {
        Self {
            label: label.into(),
            description: description.map(str::to_string),
            value: None,
        }
    }

    pub fn plain(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            description: None,
            value: None,
        }
    }

    /// Builds an option whose submitted value differs from its display label.
    pub fn with_value(
        label: impl Into<String>,
        description: Option<&str>,
        value: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            description: description.map(str::to_string),
            value: Some(value.into()),
        }
    }
}

/// Normalized interactive question from agent to user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserQuestion {
    /// Internal request ID (e.g. `qst_...`), returned when answered.
    pub id: String,
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub question: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default)]
    pub custom: bool,
    /// Groups questions that must be answered or dismissed together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// Answer payload for an interactive user question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub id: String,
    pub session: SessionId,
    pub answers: Vec<String>,
}

/// Normalized permission request sent by agent (regardless of source protocol).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// Internal request ID for adapter, returned upon reply.
    pub req: String,
    pub session: SessionId,
    /// Short title, e.g. `Bash`, `Edit`.
    pub title: String,
    /// Details (command, path, diff summary).
    pub detail: String,
}

// ---------------------------------------------------------------------------
// Event
// ---------------------------------------------------------------------------

/// System events. Append-only, serializable, replayable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "事件列舉變體大小差異大，部分變體攜帶完整快照狀態"
)]
pub enum Event {
    /// Complete state written to first line of new log file after rotation; replay begins from here. seq inherits value from rotation time.
    Snapshot {
        state: Box<State>,
    },
    SessionStarted {
        session: Session,
    },
    SessionStatus {
        session: SessionId,
        status: SessionStatus,
    },
    /// Agent reports session context usage.
    SessionContext {
        session: SessionId,
        used: u64,
        window: Option<u64>,
    },
    /// Agent reports account quota (usage percentage and reset time).
    QuotaUpdated {
        quota: Quota,
    },
    /// Agent reports quota exhausted / rate limited (can hand off to other agents).
    SessionLimited {
        session: SessionId,
        detail: String,
    },
    /// Session removed from list (startup failure or user cleared terminated session).
    SessionRemoved {
        session: SessionId,
    },
    /// Archived sessions dropped by retention (count or age limit).
    ArchivePruned {
        sessions: Vec<SessionId>,
    },
    /// Actual model reported by agent (or specified in settings).
    SessionModel {
        session: SessionId,
        model: String,
    },
    /// Native slash commands reported by agent.
    SessionCommands {
        session: SessionId,
        commands: Vec<String>,
    },
    /// Reasoning effort switched at runtime (or configured at startup).
    SessionEffort {
        session: SessionId,
        effort: String,
    },
    /// Native conversation ID reported by agent (for resumption).
    SessionNativeId {
        session: SessionId,
        native_id: String,
    },
    /// Context reset (`Request::Clear`): the agent restarts with a fresh native conversation
    /// under the same ertangle session id. Drops native id, cost, context, transcript and live tool state.
    SessionCleared {
        session: SessionId,
    },
    SessionLabel {
        session: SessionId,
        label: String,
    },
    /// Short status note updated by agent via `ertangle_status`.
    SessionNote {
        session: SessionId,
        note: String,
    },
    /// Handoff complete: `to` takes over work from `from` (bus messages are routed to successor).
    SessionHandedOff {
        from: SessionId,
        to: SessionId,
    },
    /// Prompt sent to agent (displayed in transcript for conversational context).
    UserPrompt {
        session: SessionId,
        text: String,
        /// Sender: `None` = user; otherwise sender agent session ID, or [`PROMPT_FROM_ERTANGLE`]
        /// (daemon-generated reminders, notifications, and handoff briefs).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// Text output from agent (streaming chunk or complete line).
    Output {
        session: SessionId,
        text: String,
    },
    /// Continuation of an unterminated [`Event::Output`] line: the first segment of `text`
    /// is appended to the last transcript line instead of starting a new one.
    OutputAppend {
        session: SessionId,
        text: String,
    },
    /// A session's inbox read cursor advanced to `msg` (persisted so restarts do not redeliver).
    InboxRead {
        session: SessionId,
        msg: String,
    },
    /// Session background child process list updated.
    SessionBgProcs {
        session: SessionId,
        procs: Vec<BgProc>,
    },
    ToolCall {
        session: SessionId,
        name: String,
        #[serde(default)]
        input: String,
        /// `None` = in progress; `Some(true/false)` = completed with success/failure.
        ok: Option<bool>,
        #[serde(default)]
        output: Option<String>,
    },
    PermissionRequested {
        request: PermissionRequest,
    },
    PermissionResolved {
        session: SessionId,
        req: String,
        decision: Decision,
    },
    QuestionRequested {
        question: UserQuestion,
    },
    QuestionResolved {
        session: SessionId,
        id: String,
        #[serde(default)]
        answers: Vec<String>,
    },
    BusMessage {
        message: BusMessage,
    },
    TaskChanged {
        task: Task,
    },
    ArtifactShared {
        artifact: Artifact,
    },
    ReviewCommented {
        review: ReviewComment,
    },
    SessionExited {
        session: SessionId,
        code: Option<i32>,
    },
    Cost {
        session: SessionId,
        cost: Cost,
    },
    /// Session reached cost limit (interrupted current turn and set `over_budget` to `true`).
    SessionOverBudget {
        session: SessionId,
        spent: f64,
        limit: f64,
    },
    /// Manually adjusted session cost limit (also clears `over_budget`).
    SessionBudget {
        session: SessionId,
        budget_usd: Option<f64>,
    },
    /// Event log write health status (`ok == false` indicates write failure, TUI displays `log ✗`).
    LogHealth {
        ok: bool,
    },
    /// Session queued prompts changed (`items` contains raw input text list, excluding attachment content).
    QueueChanged {
        session: SessionId,
        items: Vec<String>,
    },
}

/// Event + sequence number + timestamp; unit of external communication (socket / log).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub seq: Seq,
    pub ts: u64,
    #[serde(flatten)]
    pub event: Event,
}

// ---------------------------------------------------------------------------
// State: fold result of events
// ---------------------------------------------------------------------------

/// Maximum transcript lines retained per session (prevents memory growth on long conversations).
pub const TRANSCRIPT_CAP: usize = 4000;
/// Value of `UserPrompt.from`: daemon-generated prompt (unread reminder, conflict notification, handoff brief, auto-resume).
pub const PROMPT_FROM_ERTANGLE: &str = "ertangle";
/// Prefix for the first line of user prompts in transcript.
pub const USER_PROMPT_MARK: &str = "❯ ";
/// Prefix for the first line of non-user prompts (other agents or ertangle) in transcript, followed by `[source] `.
pub const OTHER_PROMPT_MARK: &str = "» ";

/// If a transcript line is the first line of a prompt, returns (source name, `None` = user; prompt text).
pub fn parse_prompt_line(line: &str) -> Option<(Option<&str>, &str)> {
    if let Some(rest) = line.strip_prefix(USER_PROMPT_MARK) {
        return Some((None, rest));
    }
    let rest = line.strip_prefix(OTHER_PROMPT_MARK)?.strip_prefix('[')?;
    let (name, text) = rest.split_once("] ")?;
    Some((Some(name), text))
}
/// Default maximum retained archived sessions with `native_id` (for browsing in resumption selector).
pub const ARCHIVED_SESSIONS_CAP: usize = 100;
/// Default maximum age of archived sessions, counted from when they ended.
pub const ARCHIVED_MAX_DAYS: u64 = 30;
/// Maximum transcript lines retained for archived historical sessions.
pub const ARCHIVED_TRANSCRIPT_CAP: usize = 200;
/// Maximum retained bus messages.
pub const MESSAGE_CAP: usize = 1000;
/// Maximum retained artifacts.
pub const ARTIFACT_CAP: usize = 500;

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub seq: Seq,
    pub sessions: BTreeMap<SessionId, Session>,
    /// Historical sessions removed from active list with `native_id` (for resuming original conversation).
    #[serde(default)]
    pub archived_sessions: BTreeMap<SessionId, Session>,
    pub tasks: BTreeMap<TaskId, Task>,
    pub messages: Vec<BusMessage>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub reviews: Vec<ReviewComment>,
    /// Pending approval requests.
    pub pending_permissions: BTreeMap<String, PermissionRequest>,
    /// Pending interactive user questions.
    #[serde(default)]
    pub pending_questions: BTreeMap<String, UserQuestion>,
    /// Session ID -> output text lines.
    pub transcripts: HashMap<SessionId, VecDeque<String>>,
    /// Event log write health status (defaults to normal).
    #[serde(default = "default_true")]
    pub log_ok: bool,
    /// Latest quota reported by each agent (agent ID -> quota).
    #[serde(default)]
    pub quotas: BTreeMap<String, Quota>,
    /// Queued raw prompt text list per session (session ID -> items).
    #[serde(default)]
    pub queues: BTreeMap<SessionId, Vec<String>>,
    /// Last bus message id already delivered to each session (session ID -> message ID).
    #[serde(default)]
    pub inbox_cursors: BTreeMap<SessionId, String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            seq: 0,
            sessions: BTreeMap::new(),
            archived_sessions: BTreeMap::new(),
            tasks: BTreeMap::new(),
            messages: Vec::new(),
            artifacts: Vec::new(),
            reviews: Vec::new(),
            pending_permissions: BTreeMap::new(),
            pending_questions: BTreeMap::new(),
            transcripts: HashMap::new(),
            log_ok: true,
            quotas: BTreeMap::new(),
            queues: BTreeMap::new(),
            inbox_cursors: BTreeMap::new(),
        }
    }
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops pending questions owned by a session that exited or was removed.
    fn remove_session_questions(&mut self, session: &str) {
        let ids: Vec<String> = self
            .pending_questions
            .values()
            .filter(|q| q.session == session)
            .map(|q| q.id.clone())
            .collect();
        for id in ids {
            self.pending_questions.remove(&id);
        }
    }

    fn push_transcript_line(&mut self, session: &str, line: String) {
        let buf = self.transcripts.entry(session.to_string()).or_default();
        buf.push_back(line);
        while buf.len() > TRANSCRIPT_CAP {
            buf.pop_front();
        }
    }

    /// Moves terminated session from active list; archives to `archived_sessions` if possessing `native_id` for future resumption.
    pub fn archive_session(&mut self, session: &str) {
        self.remove_session_questions(session);
        if let Some(mut s) = self.sessions.remove(session) {
            if s.native_id.is_some() {
                s.status = SessionStatus::Exited;
                s.active_tools.clear();
                s.bg_procs.clear();
                let need_summary = s
                    .status_note
                    .as_deref()
                    .is_none_or(|n| n.is_empty() || n.contains(":resume"));
                if need_summary {
                    if let Some(last_prompt) = self.transcripts.get(session).and_then(|buf| {
                        buf.iter()
                            .rev()
                            .find_map(|line| parse_prompt_line(line).map(|(_, t)| t))
                    }) {
                        let snippet: String = last_prompt.trim().chars().take(80).collect();
                        if !snippet.is_empty() {
                            s.status_note = Some(snippet);
                        }
                    }
                }
                if let Some(buf) = self.transcripts.get_mut(session) {
                    while buf.len() > ARCHIVED_TRANSCRIPT_CAP {
                        buf.pop_front();
                    }
                }
                self.archived_sessions.insert(session.to_string(), s);
            } else {
                self.transcripts.remove(session);
            }
        } else {
            self.transcripts.remove(session);
        }
        self.queues.remove(session);
        self.pending_permissions.retain(|_, p| p.session != session);
        self.pending_questions.retain(|_, q| q.session != session);
    }

    /// Archived session ids that exceed the retention limits: older than `max_days`
    /// (counted from when they ended) or beyond the newest `max_count` entries.
    pub fn archived_to_prune(
        &self,
        now_ms: u64,
        max_count: usize,
        max_days: u64,
    ) -> Vec<SessionId> {
        let max_age_ms = max_days.saturating_mul(86_400_000);
        let mut by_age: Vec<(&SessionId, u64)> = self
            .archived_sessions
            .iter()
            .map(|(id, s)| (id, s.ended_or_created_ms()))
            .collect();
        // Newest first; ties broken by id for determinism.
        by_age.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        by_age
            .into_iter()
            .enumerate()
            .filter(|(i, (_, ended))| *i >= max_count || now_ms.saturating_sub(*ended) > max_age_ms)
            .map(|(_, (id, _))| id.clone())
            .collect()
    }

    /// Applies an event to state. `rec.seq` updates `self.seq`.
    pub fn apply(&mut self, rec: &Record) {
        self.seq = self.seq.max(rec.seq);
        match &rec.event {
            Event::Snapshot { state } => {
                *self = (**state).clone();
                self.seq = rec.seq;
            }
            Event::SessionStarted { session } => {
                self.archived_sessions.remove(&session.id);
                let mut session = session.clone();
                if session.status.is_live() {
                    session.ended_ms = None;
                }
                self.sessions.insert(session.id.clone(), session);
            }
            Event::SessionStatus { session, status } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.status = *status;
                    s.ended_ms = if status.is_live() {
                        None
                    } else {
                        s.ended_ms.or(Some(rec.ts))
                    };
                    if *status != SessionStatus::Busy && *status != SessionStatus::WaitingApproval {
                        s.active_tools.clear();
                    }
                }
            }
            Event::InboxRead { session, msg } => {
                self.inbox_cursors.insert(session.clone(), msg.clone());
            }
            Event::SessionBgProcs { session, procs } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.bg_procs = procs.clone();
                }
            }
            Event::SessionContext {
                session,
                used,
                window,
            } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    // Window size is updated only when reported by agent; reuses previous size otherwise.
                    let window = window.or(s.context.and_then(|c| c.window));
                    // used == 0 indicates window size update only; reuses previous usage.
                    let used = if *used == 0 {
                        s.context.map_or(0, |c| c.used)
                    } else {
                        *used
                    };
                    s.context = Some(ContextUsage { used, window });
                }
            }
            Event::QuotaUpdated { quota } => {
                self.quotas.insert(quota.agent.clone(), quota.clone());
            }
            Event::SessionLimited { session, detail } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.status = SessionStatus::Failed;
                    s.ended_ms = s.ended_ms.or(Some(rec.ts));
                    let d: String = detail.chars().take(80).collect();
                    s.status_note = Some(tr!("額度用完：{d} (可用 handoff 交接)", d = d));
                }
            }
            Event::SessionRemoved { session } => {
                self.archive_session(session);
            }
            Event::ArchivePruned { sessions } => {
                for id in sessions {
                    self.archived_sessions.remove(id);
                    self.transcripts.remove(id);
                }
            }
            Event::SessionModel { session, model } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.model = Some(model.clone());
                }
            }
            Event::SessionCommands { session, commands } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.slash_commands = commands.clone();
                }
            }
            Event::SessionEffort { session, effort } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.effort = Some(effort.clone());
                }
            }
            Event::SessionNativeId { session, native_id } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.native_id = Some(native_id.clone());
                }
            }
            Event::SessionCleared { session } => {
                self.remove_session_questions(session);
                self.pending_permissions
                    .retain(|_, p| p.session != *session);
                self.transcripts.remove(session);
                if let Some(s) = self.sessions.get_mut(session) {
                    s.native_id = None;
                    s.cost = Cost::default();
                    s.context = None;
                    s.status_note = None;
                    s.slash_commands.clear();
                    s.active_tools.clear();
                    s.bg_procs.clear();
                    s.tool_calls = 0;
                    s.over_budget = false;
                }
            }
            Event::SessionLabel { session, label } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.label = label.clone();
                } else if let Some(s) = self.archived_sessions.get_mut(session) {
                    s.label = label.clone();
                }
            }
            Event::SessionNote { session, note } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.status_note = if note.is_empty() {
                        None
                    } else {
                        Some(note.clone())
                    };
                }
            }
            Event::SessionHandedOff { from, to } => {
                if let Some(s) = self.sessions.get_mut(from) {
                    s.handed_off_to = Some(to.clone());
                }
            }
            Event::UserPrompt {
                session,
                text,
                from,
            } => {
                // Prefix first line: `❯ ` for user, `» [source] ` for other sources (agent shown by label);
                // indent subsequent lines of multi-line prompts.
                let head = match from {
                    None => USER_PROMPT_MARK.to_string(),
                    Some(f) => {
                        let name = self
                            .sessions
                            .get(f)
                            .map(|s| s.label.clone())
                            .unwrap_or_else(|| f.clone());
                        format!("{OTHER_PROMPT_MARK}[{name}] ")
                    }
                };
                let buf = self.transcripts.entry(session.clone()).or_default();
                let text = text.trim_end_matches('\n');
                if !text.is_empty() {
                    for (i, line) in text.split('\n').enumerate() {
                        if i == 0 {
                            buf.push_back(format!("{head}{line}"));
                        } else {
                            buf.push_back(format!("  {line}"));
                        }
                    }
                }
                while buf.len() > TRANSCRIPT_CAP {
                    buf.pop_front();
                }
            }
            Event::Output { session, text } => {
                let buf = self.transcripts.entry(session.clone()).or_default();
                // Trailing newline should not produce an empty line; intermediate empty lines are preserved.
                let mut parts: Vec<&str> = text.split('\n').collect();
                if parts.last() == Some(&"") {
                    parts.pop();
                }
                for line in parts {
                    buf.push_back(line.to_string());
                }
                while buf.len() > TRANSCRIPT_CAP {
                    buf.pop_front();
                }
            }
            Event::OutputAppend { session, text } => {
                let buf = self.transcripts.entry(session.clone()).or_default();
                let mut parts = text.split('\n');
                if let Some(first) = parts.next() {
                    match buf.back_mut() {
                        Some(last) => last.push_str(first),
                        None => buf.push_back(first.to_string()),
                    }
                }
                let rest: Vec<&str> = parts.collect();
                // Same rule as `Output`: a trailing newline does not make an empty line.
                let rest = match rest.split_last() {
                    Some((&"", init)) => init,
                    _ => &rest[..],
                };
                for line in rest {
                    buf.push_back((*line).to_string());
                }
                while buf.len() > TRANSCRIPT_CAP {
                    buf.pop_front();
                }
            }
            Event::ToolCall {
                session,
                name,
                input,
                ok,
                output,
            } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    if ok.is_none() && !name.is_empty() {
                        s.tool_calls = s.tool_calls.saturating_add(1);
                        let is_sub = is_subagent_tool(name);
                        let sum = tool_summary(input).unwrap_or_default();
                        s.active_tools.push(ActiveTool {
                            name: name.clone(),
                            summary: sum,
                            started_ms: now_millis(),
                            is_subagent: is_sub,
                        });
                    } else if ok.is_some() {
                        if !name.is_empty() {
                            if let Some(pos) = s.active_tools.iter().position(|t| t.name == *name) {
                                s.active_tools.remove(pos);
                            } else if !s.active_tools.is_empty() {
                                s.active_tools.remove(0);
                            }
                        } else if !s.active_tools.is_empty() {
                            s.active_tools.remove(0);
                        }
                    }
                }
                if ok.is_none() && !name.is_empty() {
                    let line = match tool_summary(input) {
                        Some(sum) => format!("▸ {name}: {sum}"),
                        None => format!("▸ {name}"),
                    };
                    self.push_transcript_line(session, line);
                } else if let Some(ok) = ok {
                    // Tool results: show one-line summary on success, show failure reason on error.
                    let out = output.as_deref().unwrap_or("");
                    let first = out.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                    let extra = out
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .count()
                        .saturating_sub(1);
                    let mut text: String = first.trim().chars().take(100).collect();
                    if first.trim().chars().count() > 100 || extra > 0 {
                        text.push_str(&if extra > 0 {
                            tr!(" …(+{extra} 行)", extra = extra)
                        } else {
                            "…".into()
                        });
                    }
                    let line = match (ok, text.is_empty()) {
                        (true, true) => "  ✓".to_string(),
                        (true, false) => format!("  ✓ {text}"),
                        (false, true) => format!("  ✗ {}", tr!("失敗")),
                        (false, false) => format!("  ✗ {text}"),
                    };
                    self.push_transcript_line(session, line);
                }
            }
            Event::PermissionRequested { request } => {
                self.pending_permissions
                    .insert(request.req.clone(), request.clone());
                if let Some(s) = self.sessions.get_mut(&request.session) {
                    s.status = SessionStatus::WaitingApproval;
                }
            }
            Event::PermissionResolved { req, .. } => {
                self.pending_permissions.remove(req);
            }
            Event::QuestionRequested { question } => {
                self.pending_questions
                    .insert(question.id.clone(), question.clone());
                if let Some(s) = self.sessions.get_mut(&question.session) {
                    s.status = SessionStatus::WaitingApproval;
                }
            }
            Event::QuestionResolved { id, .. } => {
                self.pending_questions.remove(id);
            }
            Event::BusMessage { message } => {
                self.messages.push(message.clone());
                while self.messages.len() > MESSAGE_CAP {
                    self.messages.remove(0);
                }
            }
            Event::TaskChanged { task } => {
                self.tasks.insert(task.id.clone(), task.clone());
            }
            Event::ArtifactShared { artifact } => {
                self.artifacts.push(artifact.clone());
                while self.artifacts.len() > ARTIFACT_CAP {
                    self.artifacts.remove(0);
                }
            }
            Event::ReviewCommented { review } => {
                let line = format!("💬 review [{}]: {}", review.location, review.comment);
                self.push_transcript_line(&review.session, line);
                self.reviews.push(review.clone());
                while self.reviews.len() > ARTIFACT_CAP {
                    self.reviews.remove(0);
                }
            }
            Event::SessionExited { session, .. } => {
                self.remove_session_questions(session);
                if let Some(s) = self.sessions.get_mut(session) {
                    if s.status.is_live() {
                        s.status = SessionStatus::Exited;
                    }
                    s.ended_ms = s.ended_ms.or(Some(rec.ts));
                }
            }
            Event::Cost { session, cost } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.cost = *cost;
                }
            }
            Event::SessionOverBudget { session, .. } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.over_budget = true;
                }
            }
            Event::SessionBudget {
                session,
                budget_usd,
            } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.budget_usd = *budget_usd;
                    s.over_budget = false;
                }
            }
            Event::LogHealth { ok } => {
                self.log_ok = *ok;
            }
            Event::QueueChanged { session, items } => {
                if items.is_empty() {
                    self.queues.remove(session);
                } else {
                    self.queues.insert(session.clone(), items.clone());
                }
            }
        }
    }

    /// Calculates depth of `sid` (human-created sessions are level 0, child agents increment sequentially).
    pub fn session_depth(&self, sid: &str) -> usize {
        let mut depth = 0;
        let mut cur = sid;
        let mut seen = std::collections::HashSet::new();
        while seen.insert(cur) {
            let Some(parent) = self
                .sessions
                .get(cur)
                .and_then(|s| s.parent.as_deref())
                .filter(|p| self.sessions.contains_key(*p))
            else {
                break;
            };
            depth += 1;
            cur = parent;
        }
        depth
    }

    /// Traverses upward along `parent` to find the root session ID of the tree.
    pub fn tree_root(&self, sid: &str) -> SessionId {
        let mut cur = sid;
        let mut seen = std::collections::HashSet::new();
        while seen.insert(cur) {
            let Some(parent) = self
                .sessions
                .get(cur)
                .and_then(|s| s.parent.as_deref())
                .filter(|p| self.sessions.contains_key(*p))
            else {
                break;
            };
            cur = parent;
        }
        cur.to_string()
    }

    /// Returns all session IDs in the same tree as `sid` (root and all descendants).
    pub fn tree_sessions(&self, sid: &str) -> Vec<SessionId> {
        let root = self.tree_root(sid);
        self.sessions
            .keys()
            .filter(|id| self.tree_root(id) == root)
            .cloned()
            .collect()
    }

    /// Calculates total USD cost for the same tree as `sid` (root and all descendants).
    pub fn tree_cost_usd(&self, sid: &str) -> f64 {
        let root = self.tree_root(sid);
        self.sessions
            .values()
            .filter(|s| self.tree_root(&s.id) == root)
            .map(|s| s.cost.usd)
            .sum()
    }

    /// Counts live direct child sessions currently running under `parent_id`.
    pub fn live_children_count(&self, parent_id: &str) -> usize {
        self.sessions
            .values()
            .filter(|s| s.parent.as_deref() == Some(parent_id) && s.status.is_live())
            .count()
    }

    /// Helper for TUI to retrieve sessions sorted by creation time.
    pub fn sessions_sorted(&self) -> Vec<&Session> {
        let mut v: Vec<&Session> = self.sessions.values().collect();
        v.sort_by_key(|s| s.created_ms);
        v
    }

    pub fn live_sessions(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values().filter(|s| s.status.is_live())
    }

    /// Calculates cumulative cost and token usage across all sessions.
    pub fn total_cost(&self) -> Cost {
        let mut total = Cost::default();
        for s in self.sessions.values() {
            total.usd += s.cost.usd;
            total.input_tokens = total.input_tokens.saturating_add(s.cost.input_tokens);
            total.output_tokens = total.output_tokens.saturating_add(s.cost.output_tokens);
        }
        total
    }

    /// Incomplete prerequisite tasks (present in `depends_on` with status other than `done`).
    /// Missing IDs do not count: they will never complete, and counting them would deadlock the task.
    pub fn unfinished_deps(&self, task: &Task) -> Vec<&Task> {
        task.depends_on
            .iter()
            .filter_map(|id| self.tasks.get(id))
            .filter(|t| t.status != TaskStatus::Done)
            .collect()
    }

    /// Resolves session ID by `id` or `label` (prefers exact ID match, then live session with matching label).
    pub fn resolve_session_id(&self, id_or_label: &str) -> Option<SessionId> {
        let needle = id_or_label.trim().trim_start_matches('@');
        if self.sessions.contains_key(needle) {
            return Some(needle.to_string());
        }
        self.sessions
            .values()
            .filter(|s| s.label == needle && s.status.is_live())
            .max_by_key(|s| s.created_ms)
            .or_else(|| {
                self.sessions
                    .values()
                    .filter(|s| s.label == needle)
                    .max_by_key(|s| s.created_ms)
            })
            .map(|s| s.id.clone())
    }

    /// Finds target session for `:resume` by `id`, `label`, or `native_id`; returns `(Session, is_archived)`.
    pub fn resolve_resumable_session(&self, id_or_label: &str) -> Option<(Session, bool)> {
        let needle = id_or_label.trim().trim_start_matches('@');
        if needle.is_empty() {
            return None;
        }
        if let Some(s) = self.sessions.get(needle) {
            return Some((s.clone(), false));
        }
        if let Some(s) = self.archived_sessions.get(needle) {
            return Some((s.clone(), true));
        }
        if let Some(s) = self
            .sessions
            .values()
            .filter(|s| s.label == needle && !s.status.is_live())
            .max_by_key(|s| s.created_ms)
        {
            return Some((s.clone(), false));
        }
        if let Some(s) = self
            .archived_sessions
            .values()
            .filter(|s| s.label == needle)
            .max_by_key(|s| s.created_ms)
        {
            return Some((s.clone(), true));
        }
        if let Some(s) = self
            .sessions
            .values()
            .filter(|s| s.label == needle)
            .max_by_key(|s| s.created_ms)
        {
            return Some((s.clone(), false));
        }
        if let Some(s) = self
            .sessions
            .values()
            .filter(|s| s.native_id.as_deref() == Some(needle))
            .max_by_key(|s| s.created_ms)
        {
            return Some((s.clone(), false));
        }
        if let Some(s) = self
            .archived_sessions
            .values()
            .filter(|s| s.native_id.as_deref() == Some(needle))
            .max_by_key(|s| s.created_ms)
        {
            return Some((s.clone(), true));
        }
        None
    }

    /// Follows `handed_off_to` chain to final successor; stops at current node on cycles or exceeding 16 levels.
    pub fn successor_of(&self, id: &str) -> SessionId {
        let mut cur = id.to_string();
        let mut seen = vec![cur.clone()];
        for _ in 0..16 {
            let Some(next) = self
                .sessions
                .get(&cur)
                .and_then(|s| s.handed_off_to.clone())
            else {
                break;
            };
            if seen.contains(&next) {
                break;
            }
            seen.push(next.clone());
            cur = next;
        }
        cur
    }

    /// Retrieves messages received by a session after `after_msg_id`.
    pub fn inbox_for(&self, session_id: &str, after_msg_id: Option<&str>) -> Vec<BusMessage> {
        let resolved = self
            .resolve_session_id(session_id)
            .unwrap_or_else(|| session_id.to_string());
        let label = self.sessions.get(&resolved).map(|s| s.label.as_str());
        let start = after_msg_id
            .and_then(|id| self.messages.iter().position(|m| m.id == id))
            .map(|idx| idx + 1)
            .unwrap_or(0);
        self.messages
            .get(start..)
            .unwrap_or(&[])
            .iter()
            .filter(|m| m.matches_recipient(&resolved, label))
            .cloned()
            .collect()
    }

    /// Exports a session (and optionally entire child session tree) into a Markdown report suitable for PR / Issue description.
    ///
    /// Includes metadata, child session hierarchy, associated tasks, transcript, and optional git diff;
    /// entire output is masked with [`redact_secrets`] to prevent leaking keys to external services.
    pub fn export_markdown(
        &self,
        root_id: &str,
        include_tree: bool,
        diff: Option<&str>,
    ) -> Result<String, String> {
        let root = self
            .resolve_session_id(root_id)
            .ok_or_else(|| tr!("找不到 session：{id}", id = root_id))?;
        let root_session = self
            .sessions
            .get(&root)
            .ok_or_else(|| tr!("找不到 session：{id}", id = root))?;

        let mut members: Vec<&Session> = if include_tree {
            let mut v: Vec<&Session> = self
                .tree_sessions(&root)
                .iter()
                .filter_map(|id| self.sessions.get(id))
                .collect();
            v.sort_by_key(|s| (self.session_depth(&s.id), s.created_ms, s.id.clone()));
            v
        } else {
            vec![root_session]
        };
        // Root session always comes first, even if child sessions were created earlier.
        if let Some(pos) = members.iter().position(|s| s.id == root) {
            let root_ref = members.remove(pos);
            members.insert(0, root_ref);
        }

        let member_ids: Vec<&str> = members.iter().map(|s| s.id.as_str()).collect();
        let total_usd: f64 = members.iter().map(|s| s.cost.usd).sum();
        let total_in: u64 = members
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(s.cost.input_tokens));
        let total_out: u64 = members
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(s.cost.output_tokens));

        let mut out = String::new();
        out.push_str(&tr!("# 協作報告：{label}\n\n", label = root_session.label));

        out.push_str(&format!("## {}\n\n", tr!("元資料")));
        out.push_str(&format!("| {} | {} |\n", tr!("項目"), tr!("值")));
        out.push_str("| --- | --- |\n");
        let row = |k: String, v: String| format!("| {k} | {v} |\n");
        out.push_str(&row(tr!("標籤"), root_session.label.clone()));
        out.push_str(&row(
            tr!("Agent / Model"),
            format!(
                "{}{}{}",
                root_session.kind.id(),
                root_session
                    .model
                    .as_deref()
                    .map(|m| format!(" / {m}"))
                    .unwrap_or_default(),
                root_session
                    .effort
                    .as_deref()
                    .map(|e| format!(" (effort: {e})"))
                    .unwrap_or_default()
            ),
        ));
        out.push_str(&row(tr!("角色"), root_session.role.id().to_string()));
        out.push_str(&row(
            tr!("分支"),
            root_session.branch.clone().unwrap_or_else(|| "-".into()),
        ));
        out.push_str(&row(
            tr!("狀態"),
            format!(
                "{} {}",
                root_session.status.glyph(),
                status_text(root_session.status)
            ),
        ));
        out.push_str(&row(
            tr!("總花費"),
            format!(
                "${total_usd:.4} ({} in / {} out tokens)",
                total_in, total_out
            ),
        ));
        out.push('\n');

        if include_tree && members.len() > 1 {
            out.push_str(&format!("## {}\n\n", tr!("子 Session 階層")));
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                tr!("標籤"),
                tr!("角色"),
                tr!("Agent / Model"),
                tr!("狀態"),
                tr!("花費")
            ));
            out.push_str("| --- | --- | --- | --- | --- |\n");
            for s in &members {
                let indent = "  ".repeat(self.session_depth(&s.id));
                out.push_str(&format!(
                    "| {indent}{} | {} | {}{} | {} {} | ${:.4} |\n",
                    s.label,
                    s.role.id(),
                    s.kind.id(),
                    s.model
                        .as_deref()
                        .map(|m| format!(" / {m}"))
                        .unwrap_or_default(),
                    s.status.glyph(),
                    status_text(s.status),
                    s.cost.usd,
                ));
            }
            out.push('\n');
        }

        let related: Vec<&Task> = self
            .tasks
            .values()
            .filter(|t| {
                t.assignee
                    .as_deref()
                    .is_some_and(|id| member_ids.contains(&id))
                    || t.created_by
                        .as_deref()
                        .is_some_and(|id| member_ids.contains(&id))
            })
            .collect();
        if !related.is_empty() {
            out.push_str(&format!("## {}\n\n", tr!("關聯任務")));
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                tr!("編號"),
                tr!("標題"),
                tr!("狀態"),
                tr!("負責人")
            ));
            out.push_str("| --- | --- | --- | --- |\n");
            for t in related {
                let assignee = t.assignee.as_deref().and_then(|id| self.sessions.get(id));
                out.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    t.id,
                    t.title,
                    t.status.id(),
                    assignee.map(|s| s.label.as_str()).unwrap_or("-")
                ));
            }
            out.push('\n');
        }

        out.push_str(&format!("## {}\n\n", tr!("對話與執行紀錄")));
        for s in &members {
            out.push_str(&format!("### {} ({})\n\n", s.label, s.role.id()));
            let lines = self.transcripts.get(&s.id);
            match lines {
                Some(lines) if !lines.is_empty() => {
                    out.push_str("```text\n");
                    // Successful tool results are noise in transcripts; failures (`✗`) and user prompts (`❯`) are retained.
                    for line in lines.iter().filter(|l| !l.starts_with("  ✓")) {
                        out.push_str(line);
                        out.push('\n');
                    }
                    out.push_str("```\n\n");
                }
                _ => {
                    out.push_str(&format!("_{}_\n\n", tr!("(尚無逐字稿)")));
                }
            }
        }

        if let Some(diff) = diff.filter(|d| !d.trim().is_empty()) {
            out.push_str(&format!(
                "## Git Diff\n\n```diff\n{}\n```\n",
                diff.trim_end()
            ));
        }

        Ok(redact_secrets(&out))
    }
}

/// Human-readable text for `SessionStatus` (for reports).
fn status_text(status: SessionStatus) -> String {
    match status {
        SessionStatus::Starting => tr!("啟動中"),
        SessionStatus::Idle => tr!("閒置"),
        SessionStatus::Busy => tr!("執行中"),
        SessionStatus::WaitingApproval => tr!("等待核准"),
        SessionStatus::Sleeping => tr!("休眠"),
        SessionStatus::Exited => tr!("已結束"),
        SessionStatus::Failed => tr!("失敗"),
    }
}

// ---------------------------------------------------------------------------
// Policy Engine & Secret Protection (M4)
// ---------------------------------------------------------------------------

/// Permission rule engine: filters unambiguously safe and high-risk operations before `PermissionPolicy`.
///
/// Overridable via `~/.config/ertangle/policy.toml` (or project `.ertangle/policy.toml`):
/// ```toml
/// allow = ["Read", "Grep", "Glob", "git status"]
/// deny = ["rm -rf /", "git push --force", "/.ssh/"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyEngine {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self {
            allow: vec![
                "Read".into(),
                "Grep".into(),
                "Glob".into(),
                "git status".into(),
                "git diff".into(),
                "git log".into(),
            ],
            deny: vec![
                "rm -rf /".into(),
                "rm -rf ~".into(),
                "git push --force".into(),
                "git push -f".into(),
                "/.ssh/".into(),
                "id_rsa".into(),
                "mkfs.".into(),
            ],
        }
    }
}

impl PolicyEngine {
    /// Parses rules from simple TOML text (unspecified sections retain default values).
    pub fn parse_toml(src: &str) -> Self {
        let mut engine = Self::default();
        for raw_line in src.lines() {
            let line = raw_line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let key = k.trim();
                let list = parse_string_array(v.trim());
                if key == "allow" && !list.is_empty() {
                    engine.allow = list;
                } else if key == "deny" && !list.is_empty() {
                    engine.deny = list;
                }
            }
        }
        engine
    }

    /// Evaluates a permission request:
    /// * Matches `deny` -> `Some(Decision::Deny)`
    /// * Matches `allow` -> `Some(Decision::Allow)`
    /// * Otherwise -> `None` (deferred to session `PermissionPolicy` or human)
    pub fn evaluate(&self, title: &str, detail: &str) -> Option<Decision> {
        let combined = format!("{title} {detail}");
        // deny maintains conservative substring matching: prefer false positives.
        for pat in &self.deny {
            if !pat.is_empty() && combined.contains(pat.as_str()) {
                return Some(Decision::Deny);
            }
        }
        // allow must be strict: exact tool name match, or command matches rule as full-word prefix,
        // preventing `Read` from matching `README`.
        let command = command_text(detail);
        for pat in &self.allow {
            if pat.is_empty() {
                continue;
            }
            let title_tool = title.split_whitespace().next().unwrap_or("");
            if title_tool == pat
                || word_prefix(title, pat)
                || command.as_deref().is_some_and(|c| word_prefix(c, pat))
            {
                return Some(Decision::Allow);
            }
        }
        None
    }

    /// Layers repo-level rules: `deny` takes union (cannot remove global deny), `allow` also merges.
    pub fn merged_with(&self, local: &PolicyEngine) -> PolicyEngine {
        let mut out = self.clone();
        for d in &local.deny {
            if !out.deny.contains(d) {
                out.deny.push(d.clone());
            }
        }
        for a in &local.allow {
            if !out.allow.contains(a) {
                out.allow.push(a.clone());
            }
        }
        out
    }
}

/// Whether `text` starts with `pat` and ends at a word boundary (whitespace or end of string).
fn word_prefix(text: &str, pat: &str) -> bool {
    let t = text.trim_start();
    t.starts_with(pat)
        && t[pat.len()..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
}

/// Extracts command string from permission request `detail` (usually JSON rawInput).
fn command_text(detail: &str) -> Option<String> {
    // When not JSON, treat entire string as command text.
    let Ok(v) = serde_json::from_str::<serde_json::Value>(detail) else {
        return Some(detail.to_string());
    };
    match &v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(m) => ["command", "cmd"]
            .iter()
            .find_map(|k| m.get(*k).and_then(|x| x.as_str()))
            .map(str::to_string),
        _ => None,
    }
}

fn parse_string_array(s: &str) -> Vec<String> {
    let s = s.trim().trim_start_matches('[').trim_end_matches(']');
    s.split(',')
        .map(|item| item.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// Summarizes tool input (JSON string) into a human-readable one-line summary: prefers command/file_path/pattern fields.
pub fn tool_summary(input: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    let one_line = |s: &str| -> String {
        let t: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.chars().count() > 100 {
            format!("{}…", t.chars().take(100).collect::<String>())
        } else {
            t
        }
    };
    match &v {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(one_line(s)),
        serde_json::Value::Object(m) => {
            for k in [
                "command",
                "cmd",
                "file_path",
                "path",
                "pattern",
                "url",
                "query",
                "description",
                "prompt",
            ] {
                if let Some(x) = m.get(k).and_then(|x| x.as_str()) {
                    if !x.trim().is_empty() {
                        return Some(one_line(x));
                    }
                }
            }
            // Shell tool in ACP only provides `cwd` during `tool_call`; dumping the full JSON is noise.
            if m.keys()
                .all(|k| matches!(k.as_str(), "cwd" | "workdir" | "timeout"))
            {
                None
            } else {
                Some(one_line(&v.to_string()))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tool_summary_tests {
    use super::tool_summary;

    #[test]
    fn picks_the_descriptive_field() {
        assert_eq!(
            tool_summary(r#"{"command":"cargo fmt","cwd":"/tmp"}"#).as_deref(),
            Some("cargo fmt")
        );
        assert!(
            tool_summary(r#"{"cwd":"/mnt"}"#).is_none(),
            "只有 cwd 的物件沒有資訊量，不該印 JSON"
        );
        assert!(tool_summary(r#"{"workdir":"/mnt","timeout":10}"#).is_none());
        let other = tool_summary(r#"{"mcp":"x","args":["a"]}"#);
        assert!(
            other.as_deref().is_some_and(|s| s.contains("\"mcp\"")),
            "未知欄位仍要能看到原文：{other:?}"
        );
        assert!(tool_summary("not json").is_none());
    }
}

/// A specific quota window (e.g. Claude 5 hours / 7 days).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindow {
    /// Display name: `5h`, `7d`, `30d`…
    pub name: String,
    pub used_percent: f32,
    /// Reset timestamp (epoch milliseconds).
    pub resets_at_ms: Option<u64>,
}

/// Current quota status for an agent account (reported by agent itself, not estimated by ertangle).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quota {
    /// Agent kind ID, e.g. `claude`, `codex`.
    pub agent: String,
    pub windows: Vec<QuotaWindow>,
    /// Quota limit reached / rate limited.
    pub reached: bool,
    /// Agent reported approaching limit warning (e.g. Claude `allowed_warning`), still usable.
    #[serde(default)]
    pub warning: bool,
    pub updated_ms: u64,
}

/// Converts remaining milliseconds into `2h13m` / `45m` / `3d4h`.
pub fn format_remaining(ms: u64) -> String {
    let mins = ms / 60_000;
    match mins {
        0 => "<1m".into(),
        1..=59 => format!("{mins}m"),
        60..=1439 => format!("{}h{:02}m", mins / 60, mins % 60),
        _ => format!("{}d{}h", mins / 1440, (mins % 1440) / 60),
    }
}

impl Quota {
    fn is_inactive_window(&self, window: &QuotaWindow) -> bool {
        // Antigravity 3p windows cover third-party models only and must not drive agent-level quota decisions.
        self.agent == "antigravity" && window.name.starts_with("3p-")
    }

    /// One-line summary: `claude 5h 39% (reset 1h20m) 7d 22%`. Windows past reset time show `reset`.
    pub fn summary(&self, now_ms: u64) -> String {
        let parts: Vec<String> = self
            .windows
            .iter()
            .map(|w| match w.resets_at_ms {
                Some(r) if r <= now_ms => tr!("{name} 已重置", name = w.name),
                Some(r) => tr!(
                    "{name} {pct}% (重置 {remaining})",
                    name = w.name,
                    pct = format!("{:.0}", w.used_percent),
                    remaining = format_remaining(r.saturating_sub(now_ms))
                ),
                None => format!("{} {:.0}%", w.name, w.used_percent),
            })
            .collect();
        let reached = if self.is_reached(now_ms) {
            format!(" {}", tr!("✗額度已滿"))
        } else {
            String::new()
        };
        format!("{}{} {}", self.agent, reached, parts.join(" · "))
    }

    /// Whether this agent's quota is currently reached/exhausted at `now_ms`.
    ///
    /// If there are no windows (e.g. error-only rate limit without window timestamps),
    /// falls back to `self.reached`. Otherwise, returns `true` if any active live window
    /// has `used_percent >= 100.0`, OR if `self.reached` is `true` and no active window
    /// has reset yet.
    pub fn is_reached(&self, now_ms: u64) -> bool {
        if self.windows.is_empty() {
            return self.reached;
        }
        let any_live_exhausted = self
            .windows
            .iter()
            .filter(|w| !self.is_inactive_window(w))
            .filter(|w| w.resets_at_ms.is_none_or(|r| r > now_ms))
            .any(|w| w.used_percent >= 100.0);
        if any_live_exhausted {
            return true;
        }
        if self.reached {
            let any_reset = self.windows.iter().any(|w| {
                !self.is_inactive_window(w) && w.resets_at_ms.is_some_and(|r| r <= now_ms)
            });
            if !any_reset {
                return true;
            }
        }
        false
    }

    /// Tightest window usage (percentage).
    pub fn max_used(&self) -> f32 {
        self.windows
            .iter()
            .filter(|w| !self.is_inactive_window(w))
            .map(|w| w.used_percent)
            .fold(0.0, f32::max)
    }

    /// Tightest usage among windows that have not reset yet (percentage).
    pub fn max_used_live(&self, now_ms: u64) -> f32 {
        self.windows
            .iter()
            .filter(|w| !self.is_inactive_window(w))
            .filter(|w| w.resets_at_ms.is_none_or(|reset| reset > now_ms))
            .map(|w| w.used_percent)
            .fold(0.0, f32::max)
    }
}

/// Converts window duration (minutes) to display name.
pub fn window_name(mins: u64) -> String {
    match mins {
        300 => "5h".into(),
        10080 => "7d".into(),
        43200 => "30d".into(),
        m if m % 1440 == 0 => format!("{}d", m / 1440),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}

/// Whether error text resembles quota exhaustion / rate limiting (conservative keyword matching across diverse agent messages).
pub fn looks_like_limit(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    (t.contains("limit")
        && (t.contains("hit your")
            || t.contains("usage")
            || t.contains("rate")
            || t.contains("resets")))
        || t.contains("rate_limit")
        || t.contains("rate limit")
        || t.contains("insufficient_quota")
        || t.contains("quota exceeded")
        || t.contains("exceeded your current quota")
        || t.contains("429")
}

/// Attachments sent with a prompt (files or images). Content is read and inlined by the sender (TUI/CLI),
/// so the remote daemon does not need access to the same filesystem.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    /// Display filename.
    pub name: String,
    /// MIME type, e.g. `image/png`, `text/plain`.
    pub mime: String,
    /// Original path (for reference / PTY use only).
    #[serde(default)]
    pub path: Option<String>,
    /// Base64 content of image.
    #[serde(default)]
    pub data: Option<String>,
    /// Content of text file.
    #[serde(default)]
    pub text: Option<String>,
    pub size: u64,
    /// Raw user input text (including `@filename`, for queue preview and `:unqueue` restoration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_prompt: Option<String>,
}

/// Image attachment size limit (5 MB) and text file embedding limit (256 KB).
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_TEXT_BYTES: u64 = 256 * 1024;

/// Guesses image MIME type from file extension; returns `None` if not an image.
pub fn image_mime(path: &std::path::Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Standard base64 encoding (with padding).
pub fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let Some(&first) = chunk.first() else {
            continue;
        };
        let n = (u32::from(first) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        if let Some(&ch) = T.get((n >> 18) as usize & 63) {
            out.push(ch as char);
        }
        if let Some(&ch) = T.get((n >> 12) as usize & 63) {
            out.push(ch as char);
        }
        out.push(if chunk.len() > 1 {
            T.get((n >> 6) as usize & 63)
                .copied()
                .map(char::from)
                .unwrap_or('=')
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T.get(n as usize & 63)
                .copied()
                .map(char::from)
                .unwrap_or('=')
        } else {
            '='
        });
    }
    out
}

impl Attachment {
    /// Reads file into an attachment: inlines base64 for images (up to 5 MB); inlines content for UTF-8 text files
    /// (up to 256 KB; carries path only if exceeded); other binary files carry path only.
    pub fn load(path: &std::path::Path) -> Result<Attachment, String> {
        let meta = std::fs::metadata(path)
            .map_err(|e| tr!("無法讀取 {path}：{e}", path = path.display(), e = e))?;
        if !meta.is_file() {
            return Err(tr!("{path} 不是檔案", path = path.display()));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let mut att = Attachment {
            name,
            mime: "application/octet-stream".into(),
            path: Some(abs.to_string_lossy().to_string()),
            size: meta.len(),
            ..Default::default()
        };
        if let Some(mime) = image_mime(path) {
            if meta.len() > MAX_IMAGE_BYTES {
                return Err(tr!(
                    "圖片 {name} 太大 ({size} KB，上限 {max} KB)",
                    name = att.name,
                    size = meta.len() / 1024,
                    max = MAX_IMAGE_BYTES / 1024
                ));
            }
            let bytes = std::fs::read(path)
                .map_err(|e| tr!("讀取 {name} 失敗：{e}", name = att.name, e = e))?;
            att.mime = mime.into();
            att.data = Some(base64_encode(&bytes));
            return Ok(att);
        }
        if meta.len() <= MAX_TEXT_BYTES {
            let bytes = std::fs::read(path)
                .map_err(|e| tr!("讀取 {name} 失敗：{e}", name = att.name, e = e))?;
            if !bytes.contains(&0) {
                if let Ok(text) = String::from_utf8(bytes) {
                    att.mime = "text/plain".into();
                    att.text = Some(text);
                }
            }
        }
        Ok(att)
    }

    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }

    /// One-line summary for transcript: `📎 name (image/png, 12 KB)`.
    pub fn summary(&self) -> String {
        format!(
            "📎 {} ({}, {} KB)",
            self.name,
            self.mime,
            self.size.div_ceil(1024)
        )
    }
}

/// Merges attachments into prompt text: inlines text files in fenced blocks; converts images to a descriptive line
/// when `images_as_text` is true (for agents without image block support).
pub fn compose_text(text: &str, atts: &[Attachment], images_as_text: bool) -> String {
    let mut out = text.to_string();
    for a in atts {
        if a.is_image() {
            if images_as_text {
                out.push_str(&format!(
                    "\n\n[圖片附件：{}{}]",
                    a.name,
                    a.path
                        .as_deref()
                        .map(|p| format!("，路徑 {p}"))
                        .unwrap_or_default()
                ));
            }
        } else if let Some(t) = &a.text {
            out.push_str(&format!(
                "\n\n[檔案：{}]\n```\n{}\n```",
                a.name,
                t.trim_end()
            ));
        } else {
            out.push_str(&format!(
                "\n\n[檔案附件：{}{} (內容未內嵌)]",
                a.name,
                a.path
                    .as_deref()
                    .map(|p| format!("，路徑 {p}"))
                    .unwrap_or_default()
            ));
        }
    }
    out
}

/// Masks common secrets and token patterns in text (protects secrets before writing to event log).
pub fn redact_secrets(input: &str) -> String {
    const PREFIXES: &[&str] = &[
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "ghp_",
        "gho_",
        "ghu_",
        "AKIA",
        "AIza",
        "sk-",
    ];
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let Some(rest) = input.get(i..) else {
            break;
        };
        if let Some(pref) = PREFIXES.iter().find(|&&p| rest.starts_with(p)) {
            let after = &rest[pref.len()..];
            let token_len = after
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
                .count();
            if token_len >= 12 {
                out.push_str(pref);
                out.push_str("<REDACTED>");
                i += pref.len() + token_len;
                continue;
            }
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn rec(seq: u64, event: Event) -> Record {
        Record { seq, ts: 0, event }
    }

    pub(crate) fn sample_session(id: &str) -> Session {
        Session {
            id: id.to_string(),
            kind: AgentKind::OpenCode,
            label: "worker".into(),
            cwd: "/tmp".into(),
            branch: None,
            role: Role::Worker,
            status: SessionStatus::Starting,
            model: None,
            cost: Cost::default(),
            parent: None,
            created_ms: 1,
            transport: "acp".into(),
            status_note: None,
            repo_root: None,
            context: None,
            native_id: None,
            permission: Default::default(),
            budget_usd: None,
            over_budget: false,
            handed_off_to: None,
            effort: None,
            slash_commands: Vec::new(),
            active_tools: Vec::new(),
            bg_procs: Vec::new(),
            tool_calls: 0,
            ended_ms: None,
        }
    }

    #[test]
    fn output_append_continues_last_line() {
        let mut st = State::new();
        let mut seq = 0;
        let mut feed = |ev: Event| {
            seq += 1;
            st.apply(&rec(seq, ev));
        };
        let out = |text: &str| Event::Output {
            session: "s1".into(),
            text: text.into(),
        };
        let app = |text: &str| Event::OutputAppend {
            session: "s1".into(),
            text: text.into(),
        };
        feed(out("| a | b"));
        feed(app(" |\n| c"));
        feed(app(" | d |\n"));
        feed(out("next\n"));
        // Bare newline closes a dangling line without producing an empty one.
        feed(out("tail"));
        feed(app("\n"));
        let lines: Vec<_> = st.transcripts["s1"].iter().cloned().collect();
        assert_eq!(lines, ["| a | b |", "| c | d |", "next", "tail"]);
    }

    #[test]
    fn output_append_on_empty_transcript_starts_a_line() {
        let mut st = State::new();
        st.apply(&rec(
            1,
            Event::OutputAppend {
                session: "s1".into(),
                text: "x\n".into(),
            },
        ));
        let lines: Vec<_> = st.transcripts["s1"].iter().cloned().collect();
        assert_eq!(lines, ["x"]);
    }

    #[test]
    fn fold_tracks_status_and_transcript() {
        let mut st = State::new();
        st.apply(&rec(
            1,
            Event::SessionStarted {
                session: crate::tests::sample_session("s1"),
            },
        ));
        st.apply(&rec(
            2,
            Event::Output {
                session: "s1".into(),
                text: "hello\nworld".into(),
            },
        ));
        st.apply(&rec(
            3,
            Event::SessionStatus {
                session: "s1".into(),
                status: SessionStatus::Busy,
            },
        ));

        assert_eq!(st.seq, 3);
        assert_eq!(st.sessions["s1"].status, SessionStatus::Busy);
        let lines: Vec<_> = st.transcripts["s1"].iter().cloned().collect();
        assert_eq!(lines, vec!["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn permission_lifecycle() {
        let mut st = State::new();
        st.apply(&rec(
            1,
            Event::SessionStarted {
                session: crate::tests::sample_session("s1"),
            },
        ));
        st.apply(&rec(
            2,
            Event::PermissionRequested {
                request: PermissionRequest {
                    req: "r1".into(),
                    session: "s1".into(),
                    title: "Bash".into(),
                    detail: "rm -rf /".into(),
                },
            },
        ));
        assert_eq!(st.sessions["s1"].status, SessionStatus::WaitingApproval);
        assert_eq!(st.pending_permissions.len(), 1);

        st.apply(&rec(
            3,
            Event::PermissionResolved {
                session: "s1".into(),
                req: "r1".into(),
                decision: Decision::Allow,
            },
        ));
        assert!(st.pending_permissions.is_empty());
    }

    #[test]
    fn question_requested_and_resolved() {
        let mut st = State::new();
        st.apply(&rec(
            1,
            Event::SessionStarted {
                session: crate::tests::sample_session("s1"),
            },
        ));
        st.apply(&rec(
            2,
            Event::QuestionRequested {
                question: UserQuestion {
                    id: "q1".into(),
                    session: "s1".into(),
                    header: Some("Database".into()),
                    question: "Which database?".into(),
                    options: vec![
                        QuestionOption::new("PostgreSQL", Some("Recommended")),
                        QuestionOption::new("SQLite", None),
                    ],
                    multi_select: false,
                    custom: true,
                    group: None,
                },
            },
        ));
        assert_eq!(st.sessions["s1"].status, SessionStatus::WaitingApproval);
        assert_eq!(st.pending_questions.len(), 1);
        assert_eq!(st.pending_questions["q1"].options[0].label, "PostgreSQL");

        st.apply(&rec(
            3,
            Event::QuestionResolved {
                session: "s1".into(),
                id: "q1".into(),
                answers: vec!["PostgreSQL".into()],
            },
        ));
        assert!(st.pending_questions.is_empty());
    }

    #[test]
    fn session_exit_clears_pending_questions() {
        let mut st = State::new();
        st.apply(&rec(
            1,
            Event::SessionStarted {
                session: crate::tests::sample_session("s1"),
            },
        ));
        st.apply(&rec(
            2,
            Event::QuestionRequested {
                question: UserQuestion {
                    id: "q1".into(),
                    session: "s1".into(),
                    header: None,
                    question: "Pick one".into(),
                    options: Vec::new(),
                    multi_select: false,
                    custom: true,
                    group: None,
                },
            },
        ));
        assert_eq!(st.pending_questions.len(), 1);

        st.apply(&rec(
            3,
            Event::SessionExited {
                session: "s1".into(),
                code: Some(0),
            },
        ));
        assert!(st.pending_questions.is_empty());
    }

    #[test]
    fn session_cleared_resets_context_but_keeps_identity() {
        let mut st = State::new();
        let mut s = sample_session("s1");
        s.native_id = Some("nat".into());
        s.cost = Cost {
            usd: 1.5,
            input_tokens: 10,
            output_tokens: 20,
        };
        s.context = Some(ContextUsage {
            used: 5,
            window: None,
        });
        s.status_note = Some("note".into());
        s.tool_calls = 7;
        let label = s.label.clone();
        st.apply(&rec(1, Event::SessionStarted { session: s }));
        st.transcripts
            .entry("s1".into())
            .or_default()
            .push_back("❯ hi".into());
        st.apply(&rec(
            2,
            Event::SessionCleared {
                session: "s1".into(),
            },
        ));
        let s = &st.sessions["s1"];
        assert_eq!(s.native_id, None);
        assert_eq!(s.cost, Cost::default());
        assert_eq!(s.context, None);
        assert_eq!(s.status_note, None);
        assert_eq!(s.tool_calls, 0);
        assert_eq!(s.label, label, "label 應保留");
        assert!(!st.transcripts.contains_key("s1"));
        // 清除後 agent 回報的新 native id 會被套用。
        st.apply(&rec(
            3,
            Event::SessionNativeId {
                session: "s1".into(),
                native_id: "fresh".into(),
            },
        ));
        assert_eq!(st.sessions["s1"].native_id.as_deref(), Some("fresh"));
    }

    #[test]
    fn agent_kind_roundtrip() {
        for k in AgentKind::builtins() {
            assert_eq!(AgentKind::parse(k.id()).as_ref(), Some(&k));
        }
        assert_eq!(AgentKind::OpenCode.acp_command().unwrap().0, "opencode");
        assert!(AgentKind::Codex.acp_command().is_none());
    }

    #[test]
    fn resolve_session_by_label_and_filter_inbox() {
        let mut st = State::new();
        let mut s1 = sample_session("ses_1");
        s1.label = "worker/impl".into();
        let mut s2 = sample_session("ses_2");
        s2.label = "worker/test".into();
        st.apply(&rec(1, Event::SessionStarted { session: s1 }));
        st.apply(&rec(2, Event::SessionStarted { session: s2 }));

        assert_eq!(
            st.resolve_session_id("worker/impl"),
            Some("ses_1".to_string())
        );
        assert_eq!(
            st.resolve_session_id("@worker/test"),
            Some("ses_2".to_string())
        );

        st.apply(&rec(
            3,
            Event::BusMessage {
                message: BusMessage {
                    id: "m1".into(),
                    from: "ses_2".into(),
                    to: Target::Direct("worker/impl".into()),
                    kind: MsgKind::Question,
                    body: "API shape?".into(),
                    reply_to: None,
                    ts: 10,
                },
            },
        ));
        st.apply(&rec(
            4,
            Event::BusMessage {
                message: BusMessage {
                    id: "m2".into(),
                    from: "ses_1".into(),
                    to: Target::Broadcast,
                    kind: MsgKind::Chat,
                    body: "ready".into(),
                    reply_to: None,
                    ts: 11,
                },
            },
        ));

        let inbox1 = st.inbox_for("ses_1", None);
        assert_eq!(inbox1.len(), 1);
        assert_eq!(inbox1[0].id, "m1");

        let inbox1_after = st.inbox_for("ses_1", Some("m1"));
        assert!(inbox1_after.is_empty());

        let inbox2 = st.inbox_for("ses_2", None);
        assert_eq!(inbox2.len(), 1);
        assert_eq!(inbox2[0].id, "m2");
    }

    #[test]
    fn policy_engine_and_secret_redaction() {
        let policy = PolicyEngine::default();
        assert_eq!(
            policy.evaluate("Bash", "rm -rf / --no-preserve-root"),
            Some(Decision::Deny)
        );
        assert_eq!(
            policy.evaluate("Read", "src/main.rs"),
            Some(Decision::Allow)
        );
        assert_eq!(policy.evaluate("Bash", "cargo test"), None);

        let custom = PolicyEngine::parse_toml(
            r#"
            allow = ["cargo test", "Read"]
            deny = ["curl http"]
            "#,
        );
        assert_eq!(custom.evaluate("Bash", "cargo test"), Some(Decision::Allow));
        assert_eq!(
            custom.evaluate("Bash", "curl http://evil"),
            Some(Decision::Deny)
        );

        let raw = "key is sk-1234567890abcdefghij and ghp_abcdefghijklmnopqrst1234";
        let scrubbed = redact_secrets(raw);
        assert!(scrubbed.contains("sk-<REDACTED>"));
        assert!(scrubbed.contains("ghp_<REDACTED>"));
        assert!(!scrubbed.contains("1234567890abcdefghij"));
    }

    #[test]
    fn redact_secrets_tricky_input() {
        assert_eq!(redact_secrets(""), "");
        assert_eq!(redact_secrets("sk-short"), "sk-short");
        assert_eq!(
            redact_secrets("金鑰 sk-1234567890abcdefghij 結束"),
            "金鑰 sk-<REDACTED> 結束"
        );
        assert_eq!(
            redact_secrets("🦀 ghp_1234567890abcdefghij 🦀"),
            "🦀 ghp_<REDACTED> 🦀"
        );
        assert_eq!(redact_secrets("sk-"), "sk-");
        assert_eq!(redact_secrets("ghp_12345678901"), "ghp_12345678901");
        assert_eq!(redact_secrets("ghp_123456789012"), "ghp_<REDACTED>");
        assert_eq!(redact_secrets("xoxb-123456789012"), "xoxb-<REDACTED>");
        assert_eq!(
            redact_secrets("github_pat_123456789012"),
            "github_pat_<REDACTED>"
        );
        assert_eq!(redact_secrets("gho_123456789012"), "gho_<REDACTED>");
        assert_eq!(redact_secrets("ghu_123456789012"), "ghu_<REDACTED>");
        assert_eq!(redact_secrets("glpat-123456789012"), "glpat-<REDACTED>");
        assert_eq!(redact_secrets("xoxp-123456789012"), "xoxp-<REDACTED>");
        assert_eq!(redact_secrets("AIza123456789012"), "AIza<REDACTED>");
        assert_eq!(redact_secrets("AIzashort"), "AIzashort");
    }

    #[test]
    fn session_model_event_updates_state() {
        let mut st = State::new();
        st.apply(&Record {
            seq: 1,
            ts: 0,
            event: Event::SessionStarted {
                session: crate::tests::sample_session("s1"),
            },
        });
        st.apply(&Record {
            seq: 2,
            ts: 0,
            event: Event::SessionModel {
                session: "s1".into(),
                model: "opus".into(),
            },
        });
        assert_eq!(st.sessions["s1"].model.as_deref(), Some("opus"));
    }

    #[test]
    fn native_id_event_updates_state_and_old_json_parses_without_it() {
        let mut st = State::new();
        st.apply(&rec(
            1,
            Event::SessionStarted {
                session: sample_session("s1"),
            },
        ));
        assert_eq!(st.sessions["s1"].native_id, None);
        st.apply(&rec(
            2,
            Event::SessionNativeId {
                session: "s1".into(),
                native_id: "abc".into(),
            },
        ));
        assert_eq!(st.sessions["s1"].native_id.as_deref(), Some("abc"));
        // After resumption the agent changed its ID: latest takes precedence.
        st.apply(&rec(
            3,
            Event::SessionNativeId {
                session: "s1".into(),
                native_id: "def".into(),
            },
        ));
        assert_eq!(st.sessions["s1"].native_id.as_deref(), Some("def"));

        // Sessions in older logs do not have the native_id field.
        let mut v = serde_json::to_value(sample_session("old")).unwrap();
        v.as_object_mut().unwrap().remove("native_id");
        let s: Session = serde_json::from_value(v).unwrap();
        assert_eq!(s.native_id, None);
    }

    #[test]
    fn log_health_event_updates_state_and_defaults_to_true() {
        let mut st = State::new();
        assert!(st.log_ok);
        st.apply(&rec(1, Event::LogHealth { ok: false }));
        assert!(!st.log_ok);
        st.apply(&rec(2, Event::LogHealth { ok: true }));
        assert!(st.log_ok);
    }

    #[test]
    fn old_session_json_without_permission_parses() {
        let mut v = serde_json::to_value(sample_session("s1")).unwrap();
        v.as_object_mut().unwrap().remove("permission");
        let s: Session = serde_json::from_value(v).unwrap();
        assert_eq!(s.permission, PermissionPolicy::Auto);
    }

    #[test]
    fn allow_rules_match_whole_words_only() {
        let p = PolicyEngine::default();
        // `cat README.md` must not be allowed because of `Read`
        assert_eq!(
            p.evaluate("cat README.md", r#"{"command":"cat README.md"}"#),
            None
        );
        assert_eq!(p.evaluate("Read", "{}"), Some(Decision::Allow));
        assert_eq!(
            p.evaluate("bash", r#"{"command":"git status -s"}"#),
            Some(Decision::Allow)
        );
        assert_eq!(p.evaluate("bash", r#"{"command":"git statusx"}"#), None);
    }

    #[test]
    fn deny_beats_allow_and_repo_cannot_remove_deny() {
        let p = PolicyEngine::default();
        assert_eq!(
            p.evaluate("bash", r#"{"command":"git status && git push --force"}"#),
            Some(Decision::Deny)
        );
        let local = PolicyEngine {
            allow: vec![],
            deny: vec!["curl".into()],
        };
        let m = p.merged_with(&local);
        assert!(m.deny.contains(&"git push --force".to_string()));
        assert!(m.deny.contains(&"curl".to_string()));
    }

    #[test]
    fn tool_calls_render_readable_summary_and_results() {
        let mut st = State::new();
        st.apply(&rec(
            1,
            Event::SessionStarted {
                session: crate::tests::sample_session("s1"),
            },
        ));
        st.apply(&rec(
            2,
            Event::ToolCall {
                session: "s1".into(),
                name: "Bash".into(),
                input: r#"{"command":"git status\n  -s","description":"x"}"#.into(),
                ok: None,
                output: None,
            },
        ));
        st.apply(&rec(
            3,
            Event::ToolCall {
                session: "s1".into(),
                name: String::new(),
                input: String::new(),
                ok: Some(false),
                output: Some("fatal: not a repo\nmore".into()),
            },
        ));
        let t: Vec<_> = st.transcripts["s1"].iter().cloned().collect();
        assert_eq!(t[0], "▸ Bash: git status -s");
        assert_eq!(t[1], "  ✗ fatal: not a repo …(+1 行)");
    }

    #[test]
    fn prompts_from_others_are_marked_with_source() {
        let mut st = State::new();
        st.apply(&rec(
            1,
            Event::UserPrompt {
                session: "w".into(),
                text: "do it".into(),
                from: Some("ghost".into()),
            },
        ));
        st.apply(&rec(
            2,
            Event::UserPrompt {
                session: "w".into(),
                text: "提醒\n第二行".into(),
                from: Some(PROMPT_FROM_ERTANGLE.into()),
            },
        ));
        st.apply(&rec(
            3,
            Event::UserPrompt {
                session: "w".into(),
                text: "mine".into(),
                from: None,
            },
        ));
        let t: Vec<&str> = st.transcripts["w"].iter().map(String::as_str).collect();
        assert_eq!(
            t,
            ["» [ghost] do it", "» [ertangle] 提醒", "  第二行", "❯ mine"]
        );
        assert_eq!(parse_prompt_line(t[0]), Some((Some("ghost"), "do it")));
        assert_eq!(parse_prompt_line(t[3]), Some((None, "mine")));
        assert_eq!(parse_prompt_line("» quote"), None);
        assert_eq!(parse_prompt_line("plain"), None);
        // Older events do not have `from`: treat as user.
        let old: Event =
            serde_json::from_str(r#"{"kind":"user_prompt","session":"w","text":"x"}"#).unwrap();
        assert!(matches!(old, Event::UserPrompt { from: None, .. }));
    }

    #[test]
    fn record_wire_format_is_flat() {
        // Web version (applyRecord in `web/index.html`) relies on top-level event fields; nesting would break all real-time updates.
        let r = Record {
            seq: 7,
            ts: 1,
            event: Event::Output {
                session: "s1".into(),
                text: "hi".into(),
            },
        };
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        assert_eq!(v["seq"], 7);
        assert_eq!(v["kind"], "output");
        assert_eq!(v["session"], "s1");
        assert!(v.get("event").is_none());
    }

    #[test]
    fn tool_calls_count_only_start_events() {
        let mut st = State::new();
        st.apply(&rec(
            1,
            Event::SessionStarted {
                session: sample_session("s1"),
            },
        ));
        let call = |ok: Option<bool>, name: &str| Event::ToolCall {
            session: "s1".into(),
            name: name.into(),
            input: String::new(),
            ok,
            output: None,
        };
        st.apply(&rec(2, call(None, "Bash")));
        st.apply(&rec(3, call(Some(true), "Bash")));
        st.apply(&rec(4, call(None, "Read")));
        // Start events without a name do not count.
        st.apply(&rec(5, call(None, "")));
        assert_eq!(st.sessions["s1"].tool_calls, 2);
    }

    #[test]
    fn active_tools_and_bg_procs_lifecycle() {
        assert!(is_subagent_tool("Task"));
        assert!(is_subagent_tool("Agent"));
        assert!(is_subagent_tool("invoke_subagent"));
        assert!(is_subagent_tool("spawn_agent"));
        assert!(!is_subagent_tool("Bash"));

        let mut st = State::new();
        st.apply(&rec(
            1,
            Event::SessionStarted {
                session: sample_session("s1"),
            },
        ));

        // Tool starts executing
        st.apply(&rec(
            2,
            Event::ToolCall {
                session: "s1".into(),
                name: "Bash".into(),
                input: r#"{"command":"cargo test"}"#.into(),
                ok: None,
                output: None,
            },
        ));
        assert_eq!(st.sessions["s1"].active_tools.len(), 1);
        assert_eq!(st.sessions["s1"].active_tools[0].name, "Bash");
        assert!(!st.sessions["s1"].active_tools[0].is_subagent);

        // Native subagent starts executing
        st.apply(&rec(
            3,
            Event::ToolCall {
                session: "s1".into(),
                name: "Task".into(),
                input: r#"{"prompt":"review"}"#.into(),
                ok: None,
                output: None,
            },
        ));
        assert_eq!(st.sessions["s1"].active_tools.len(), 2);
        assert!(st.sessions["s1"].active_tools[1].is_subagent);

        // First tool completes
        st.apply(&rec(
            4,
            Event::ToolCall {
                session: "s1".into(),
                name: "Bash".into(),
                input: String::new(),
                ok: Some(true),
                output: Some("ok".into()),
            },
        ));
        assert_eq!(st.sessions["s1"].active_tools.len(), 1);
        assert_eq!(st.sessions["s1"].active_tools[0].name, "Task");

        // Turn ends and transitions to Idle, clearing remaining active_tools
        st.apply(&rec(
            5,
            Event::SessionStatus {
                session: "s1".into(),
                status: SessionStatus::Idle,
            },
        ));
        assert!(st.sessions["s1"].active_tools.is_empty());

        // Background process event updates
        st.apply(&rec(
            6,
            Event::SessionBgProcs {
                session: "s1".into(),
                procs: vec![BgProc {
                    pid: 1234,
                    name: "node".into(),
                    cmdline: "node server.js".into(),
                    elapsed_secs: 15,
                }],
            },
        ));
        assert_eq!(st.sessions["s1"].bg_procs.len(), 1);
        assert_eq!(st.sessions["s1"].bg_procs[0].pid, 1234);
    }

    #[test]
    fn export_markdown_includes_tree_tasks_cost_diff_and_redacts_secrets() {
        let mut st = State::new();
        let mut root = sample_session("root");
        root.label = "coord/main".into();
        root.role = Role::Coordinator;
        root.branch = Some("ertangle/main".into());
        root.status = SessionStatus::Exited;
        root.cost = Cost {
            usd: 1.25,
            input_tokens: 1000,
            output_tokens: 200,
        };
        st.apply(&rec(1, Event::SessionStarted { session: root }));

        for (id, label, usd) in [("c1", "worker/impl", 0.5), ("c2", "reviewer/code", 0.25)] {
            let mut child = sample_session(id);
            child.label = label.into();
            child.parent = Some("root".into());
            child.role = if id == "c2" {
                Role::Reviewer
            } else {
                Role::Worker
            };
            child.status = SessionStatus::Exited;
            child.cost = Cost {
                usd,
                input_tokens: 100,
                output_tokens: 20,
            };
            st.apply(&rec(2, Event::SessionStarted { session: child }));
        }
        st.apply(&rec(
            3,
            Event::TaskChanged {
                task: Task {
                    id: "task_1".into(),
                    title: "完成匯出功能".into(),
                    detail: String::new(),
                    status: TaskStatus::Done,
                    assignee: Some("c1".into()),
                    created_by: Some("root".into()),
                    depends_on: vec![],
                    updated_ms: 0,
                },
            },
        ));
        st.apply(&rec(
            4,
            Event::UserPrompt {
                session: "root".into(),
                text: "API key: sk-1234567890abcdefghijklmnop".into(),
                from: None,
            },
        ));
        st.apply(&rec(
            5,
            Event::Output {
                session: "c1".into(),
                text: "done".into(),
            },
        ));

        let md = st
            .export_markdown("coord/main", true, Some("diff --git a/x b/x\n+new"))
            .unwrap();
        assert!(md.starts_with("# 協作報告：coord/main"));
        assert!(md.contains("子 Session 階層"), "{md}");
        assert!(
            md.contains("worker/impl") && md.contains("reviewer/code"),
            "{md}"
        );
        assert!(md.contains("$2.0000"), "{md}"); // 1.25 + 0.5 + 0.25
        assert!(md.contains("1200 in / 240 out"), "{md}");
        assert!(md.contains("task_1") && md.contains("完成匯出功能"), "{md}");
        assert!(md.contains("```diff") && md.contains("+new"), "{md}");
        assert!(md.contains("sk-<REDACTED>"), "{md}");
        assert!(!md.contains("sk-1234567890abcdefghijklmnop"), "{md}");

        // Without subtree, contains only root session tasks and costs.
        let md_flat = st.export_markdown("root", false, None).unwrap();
        assert!(!md_flat.contains("子 Session 階層"), "{md_flat}");
        assert!(!md_flat.contains("### worker/impl"), "{md_flat}");
        assert!(md_flat.contains("$1.2500"), "{md_flat}");

        assert!(st.export_markdown("nope", true, None).is_err());
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::*;

    #[test]
    fn limit_detection_matches_real_messages_but_not_normal_errors() {
        assert!(looks_like_limit(
            "You've hit your 5-hour limit · resets 4:30am (Asia/Taipei)"
        ));
        assert!(looks_like_limit("Error: 429 Too Many Requests"));
        assert!(looks_like_limit("exceeded your current quota"));
        assert!(!looks_like_limit("File not found: limits.rs"));
        assert!(!looks_like_limit("compile error"));
    }

    #[test]
    fn quota_summary_and_reset_handling() {
        let q = Quota {
            agent: "claude".into(),
            windows: vec![
                QuotaWindow {
                    name: "5h".into(),
                    used_percent: 39.0,
                    resets_at_ms: Some(10_000_000 + 80 * 60_000),
                },
                QuotaWindow {
                    name: "7d".into(),
                    used_percent: 22.0,
                    resets_at_ms: Some(1),
                },
            ],
            reached: false,
            warning: false,
            updated_ms: 0,
        };
        let s = q.summary(10_000_000);
        assert!(s.contains("5h 39% (重置 1h20m)"), "{s}");
        assert!(s.contains("7d 已重置"));
        assert_eq!(window_name(300), "5h");
        assert_eq!(window_name(43200), "30d");
        assert_eq!(format_remaining(30_000), "<1m");
        assert_eq!(format_remaining(90 * 60_000), "1h30m");
        assert_eq!(format_remaining(50 * 3_600_000), "2d2h");
    }

    #[test]
    fn quota_maxima_exclude_inactive_and_reset_windows() {
        let q = Quota {
            agent: "antigravity".into(),
            windows: vec![
                QuotaWindow {
                    name: "3p-7d".into(),
                    used_percent: 90.33,
                    resets_at_ms: Some(300),
                },
                QuotaWindow {
                    name: "7d".into(),
                    used_percent: 61.0,
                    resets_at_ms: Some(300),
                },
                QuotaWindow {
                    name: "5h".into(),
                    used_percent: 99.0,
                    resets_at_ms: Some(100),
                },
            ],
            reached: false,
            warning: false,
            updated_ms: 0,
        };

        assert_eq!(q.max_used(), 99.0);
        assert_eq!(q.max_used_live(200), 61.0);
        assert_eq!(q.max_used_live(300), 0.0);
        assert!(q.summary(0).contains("3p-7d 90%"));
    }

    #[test]
    fn quota_is_reached_clears_after_exhausted_window_resets() {
        let mut q = Quota {
            agent: "claude".into(),
            windows: vec![
                QuotaWindow {
                    name: "5h".into(),
                    used_percent: 100.0,
                    resets_at_ms: Some(100),
                },
                QuotaWindow {
                    name: "7d".into(),
                    used_percent: 51.0,
                    resets_at_ms: Some(500),
                },
            ],
            reached: true,
            warning: false,
            updated_ms: 0,
        };

        // Before 5h reset: still reached, summary contains ✗額度已滿.
        assert!(q.is_reached(50));
        let sum_before = q.summary(50);
        assert!(sum_before.contains("✗額度已滿"), "{sum_before}");

        // After 5h reset: is_reached is false, summary no longer contains ✗額度已滿.
        assert!(!q.is_reached(150));
        let sum_after = q.summary(150);
        assert!(!sum_after.contains("✗額度已滿"), "{sum_after}");
        assert!(sum_after.contains("5h 已重置"), "{sum_after}");

        // If another active live window is >= 100%: still reached.
        q.windows[1].used_percent = 100.0;
        assert!(q.is_reached(150));
        assert!(q.summary(150).contains("✗額度已滿"));

        // If windows is empty: falls back to self.reached.
        let empty_reached = Quota {
            agent: "claude".into(),
            windows: vec![],
            reached: true,
            warning: false,
            updated_ms: 0,
        };
        assert!(empty_reached.is_reached(150));

        let empty_not_reached = Quota {
            agent: "claude".into(),
            windows: vec![],
            reached: false,
            warning: false,
            updated_ms: 0,
        };
        assert!(!empty_not_reached.is_reached(150));
    }

    #[test]
    fn third_party_windows_remain_active_for_other_agents() {
        let q = Quota {
            agent: "claude".into(),
            windows: vec![QuotaWindow {
                name: "3p-7d".into(),
                used_percent: 90.0,
                resets_at_ms: None,
            }],
            reached: false,
            warning: false,
            updated_ms: 0,
        };
        assert_eq!(q.max_used(), 90.0);
        assert_eq!(q.max_used_live(0), 90.0);
    }

    #[test]
    fn context_usage_formats_and_keeps_last_used_when_only_window_arrives() {
        assert_eq!(human_tokens(950), "950");
        assert_eq!(human_tokens(36_267), "36k");
        assert_eq!(human_tokens(1_000_000), "1M");
        assert_eq!(human_tokens(1_500_000), "1.5M");
        let c = ContextUsage {
            used: 36_000,
            window: Some(1_000_000),
        };
        assert_eq!(c.short(), "36k/1M 4%");
        assert_eq!(
            ContextUsage {
                used: 5_000,
                window: None
            }
            .short(),
            "5k"
        );
        let mut st = State::new();
        st.apply(&Record {
            seq: 1,
            ts: 0,
            event: Event::SessionStarted {
                session: crate::tests::sample_session("s1"),
            },
        });
        st.apply(&Record {
            seq: 2,
            ts: 0,
            event: Event::SessionContext {
                session: "s1".into(),
                used: 36_000,
                window: None,
            },
        });
        st.apply(&Record {
            seq: 3,
            ts: 0,
            event: Event::SessionContext {
                session: "s1".into(),
                used: 0,
                window: Some(200_000),
            },
        });
        let c = st.sessions["s1"].context.unwrap();
        assert_eq!((c.used, c.window), (36_000, Some(200_000)));
        // Reuses window when only usage updates later
        st.apply(&Record {
            seq: 4,
            ts: 0,
            event: Event::SessionContext {
                session: "s1".into(),
                used: 50_000,
                window: None,
            },
        });
        assert_eq!(st.sessions["s1"].context.unwrap().window, Some(200_000));
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn compose_text_embeds_files_and_notes_images() {
        let f = Attachment {
            name: "a.rs".into(),
            mime: "text/plain".into(),
            text: Some("fn main() {}\n".into()),
            ..Default::default()
        };
        let img = Attachment {
            name: "p.png".into(),
            mime: "image/png".into(),
            path: Some("/tmp/p.png".into()),
            data: Some("AAAA".into()),
            ..Default::default()
        };
        let t = compose_text("看", &[f.clone(), img.clone()], true);
        assert!(t.contains("```\nfn main() {}\n```") && t.contains("圖片附件：p.png"));
        let t2 = compose_text("看", &[f, img], false);
        assert!(!t2.contains("圖片附件"));
    }

    #[test]
    fn old_session_json_without_budget_fields_parses() {
        let mut val = serde_json::to_value(crate::tests::sample_session("s1")).unwrap();
        let obj = val.as_object_mut().unwrap();
        obj.remove("budget_usd");
        obj.remove("over_budget");
        let parsed: Session = serde_json::from_value(val).unwrap();
        assert_eq!(parsed.budget_usd, None);
        assert!(!parsed.over_budget);
    }

    #[test]
    fn old_session_json_without_handed_off_to_parses() {
        let mut val = serde_json::to_value(crate::tests::sample_session("s1")).unwrap();
        val.as_object_mut().unwrap().remove("handed_off_to");
        let parsed: Session = serde_json::from_value(val).unwrap();
        assert_eq!(parsed.handed_off_to, None);
    }

    fn handoff(st: &mut State, seq: u64, from: &str, to: &str) {
        st.apply(&Record {
            seq,
            ts: 0,
            event: Event::SessionHandedOff {
                from: from.into(),
                to: to.into(),
            },
        });
    }

    #[test]
    fn successor_follows_chain_and_survives_cycles() {
        let mut st = State::new();
        for (i, id) in ["a", "b", "c"].into_iter().enumerate() {
            st.apply(&Record {
                seq: i as u64,
                ts: 0,
                event: Event::SessionStarted {
                    session: crate::tests::sample_session(id),
                },
            });
        }
        assert_eq!(st.successor_of("a"), "a");
        handoff(&mut st, 10, "a", "b");
        assert_eq!(st.successor_of("a"), "b");
        handoff(&mut st, 11, "b", "c");
        assert_eq!(st.successor_of("a"), "c");
        assert_eq!(st.successor_of("unknown"), "unknown");
        // Cycle: c -> a, terminates on the last new node in the cycle without infinite looping.
        handoff(&mut st, 12, "c", "a");
        assert_eq!(st.successor_of("a"), "c");
        // Handoff to itself.
        handoff(&mut st, 13, "a", "a");
        assert_eq!(st.successor_of("a"), "a");
    }

    #[test]
    fn session_budget_formatting_and_state_events() {
        let mut s = crate::tests::sample_session("s1");
        s.cost.usd = 1.2;
        assert_eq!(s.format_budget(), None);
        s.budget_usd = Some(5.0);
        assert_eq!(s.format_budget().as_deref(), Some("$1.20/$5"));
        s.budget_usd = Some(2.5);
        assert_eq!(s.format_budget().as_deref(), Some("$1.20/$2.50"));

        let mut st = State::new();
        st.apply(&Record {
            seq: 1,
            ts: 0,
            event: Event::SessionStarted { session: s },
        });
        st.apply(&Record {
            seq: 2,
            ts: 0,
            event: Event::SessionOverBudget {
                session: "s1".into(),
                spent: 2.5,
                limit: 2.5,
            },
        });
        assert!(st.sessions["s1"].over_budget);

        st.apply(&Record {
            seq: 3,
            ts: 0,
            event: Event::SessionBudget {
                session: "s1".into(),
                budget_usd: Some(10.0),
            },
        });
        assert!(!st.sessions["s1"].over_budget);
        assert_eq!(st.sessions["s1"].budget_usd, Some(10.0));
    }

    #[test]
    fn removed_resumable_session_is_archived_and_resolvable() {
        let mut st = State::new();
        let mut s = crate::tests::sample_session("ses_arch");
        s.label = "worker/arch".into();
        s.native_id = Some("nat-arch-1".into());
        st.apply(&Record {
            seq: 1,
            ts: 100,
            event: Event::SessionStarted { session: s },
        });
        st.apply(&Record {
            seq: 2,
            ts: 101,
            event: Event::UserPrompt {
                session: "ses_arch".into(),
                text: "請幫我檢查這份報表".into(),
                from: None,
            },
        });
        st.apply(&Record {
            seq: 3,
            ts: 102,
            event: Event::SessionRemoved {
                session: "ses_arch".into(),
            },
        });
        assert!(!st.sessions.contains_key("ses_arch"));
        let arch = st
            .archived_sessions
            .get("ses_arch")
            .expect("具備 native_id 的已移除 session 應被封存");
        assert_eq!(arch.status, SessionStatus::Exited);
        assert_eq!(arch.status_note.as_deref(), Some("請幫我檢查這份報表"));
        assert!(
            st.transcripts.contains_key("ses_arch"),
            "封存的 session 應保留逐字稿"
        );

        for query in ["ses_arch", "worker/arch", "nat-arch-1"] {
            let (found, is_arch) = st
                .resolve_resumable_session(query)
                .unwrap_or_else(|| panic!("應能以 {query} 解析封存的 session"));
            assert!(is_arch);
            assert_eq!(found.id, "ses_arch");
        }

        // Upon relaunch (resumption), it should be removed from archived_sessions and return to sessions.
        st.apply(&Record {
            seq: 4,
            ts: 103,
            event: Event::SessionStarted {
                session: arch.clone(),
            },
        });
        assert!(!st.archived_sessions.contains_key("ses_arch"));
        assert!(st.sessions.contains_key("ses_arch"));
    }
}

#[cfg(test)]
mod inbox_cursor_tests {
    use super::*;

    #[test]
    fn inbox_read_event_sets_cursor_and_roundtrips() {
        let mut st = State::new();
        st.apply(&Record {
            seq: 1,
            ts: 0,
            event: Event::InboxRead {
                session: "s1".into(),
                msg: "m9".into(),
            },
        });
        assert_eq!(st.inbox_cursors.get("s1").map(String::as_str), Some("m9"));
        let back: State = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        assert_eq!(back.inbox_cursors, st.inbox_cursors);
    }
}

#[cfg(test)]
mod archive_retention_tests {
    use super::*;

    const DAY: u64 = 86_400_000;

    fn archived(id: &str, ended_ms: Option<u64>, created_ms: u64) -> Session {
        let mut s = crate::tests::sample_session(id);
        s.created_ms = created_ms;
        s.ended_ms = ended_ms;
        s
    }

    fn rec(seq: u64, ts: u64, event: Event) -> Record {
        Record { seq, ts, event }
    }

    #[test]
    fn ended_ms_is_set_once_on_exit_and_cleared_when_live_again() {
        let mut st = State::new();
        st.apply(&rec(
            1,
            10,
            Event::SessionStarted {
                session: crate::tests::sample_session("a"),
            },
        ));
        st.apply(&rec(
            2,
            100,
            Event::SessionExited {
                session: "a".into(),
                code: Some(0),
            },
        ));
        assert_eq!(st.sessions["a"].ended_ms, Some(100));
        st.apply(&rec(
            3,
            200,
            Event::SessionStatus {
                session: "a".into(),
                status: SessionStatus::Failed,
            },
        ));
        assert_eq!(st.sessions["a"].ended_ms, Some(100), "first end time wins");
        st.apply(&rec(
            4,
            300,
            Event::SessionStatus {
                session: "a".into(),
                status: SessionStatus::Idle,
            },
        ));
        assert_eq!(st.sessions["a"].ended_ms, None);
    }

    #[test]
    fn retention_drops_entries_older_than_max_days_by_ended_time() {
        let mut st = State::new();
        let now = 100 * DAY;
        // Created long ago but ended recently: kept. Ended 31 days ago: dropped.
        st.archived_sessions
            .insert("fresh".into(), archived("fresh", Some(now - DAY), 0));
        st.archived_sessions
            .insert("old".into(), archived("old", Some(now - 31 * DAY), 0));
        // Old logs without ended time fall back to creation time.
        st.archived_sessions
            .insert("legacy".into(), archived("legacy", None, now - 40 * DAY));
        let mut dropped = st.archived_to_prune(now, 100, 30);
        dropped.sort();
        assert_eq!(dropped, vec!["legacy".to_string(), "old".to_string()]);
    }

    #[test]
    fn retention_keeps_newest_entries_within_count_limit() {
        let mut st = State::new();
        let now = 10 * DAY;
        for (i, id) in ["a", "b", "c"].into_iter().enumerate() {
            st.archived_sessions
                .insert(id.into(), archived(id, Some(now - 3 + i as u64), 0));
        }
        assert_eq!(st.archived_to_prune(now, 2, 30), vec!["a".to_string()]);
        assert!(st.archived_to_prune(now, 3, 30).is_empty());
    }

    #[test]
    fn archive_pruned_event_removes_entries_and_transcripts() {
        let mut st = State::new();
        st.archived_sessions
            .insert("x".into(), archived("x", Some(1), 0));
        st.transcripts
            .insert("x".into(), VecDeque::from(["l".to_string()]));
        st.apply(&rec(
            1,
            2,
            Event::ArchivePruned {
                sessions: vec!["x".into()],
            },
        ));
        assert!(st.archived_sessions.is_empty());
        assert!(!st.transcripts.contains_key("x"));
    }

    #[test]
    fn version_with_hash_starts_with_pkg_version() {
        assert!(VERSION_WITH_HASH.starts_with(env!("CARGO_PKG_VERSION")));
    }
}
