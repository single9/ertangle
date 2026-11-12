//! `ade-core` — 領域模型。
//!
//! 這裡只放「與執行環境無關」的型別：agent 種類、session、事件、訊息匯流排、任務。
//! daemon、adapter、TUI 都共用這一組型別，因此它是整個系統的契約。
//!
//! 設計原則：
//! * **Event 是唯一真相**：所有狀態都是把事件 fold 進 [`State`] 的結果。
//! * 事件可序列化、可落地、可重播，讓 TUI 斷線後能無縫重建畫面。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

/// 目前時間（Unix epoch 毫秒）。
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 產生一個帶前綴的隨機 id，例如 `ses_3f2a…`。
pub fn new_id(prefix: &str) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}_{}", &u[..12])
}

pub type SessionId = String;
pub type MsgId = String;
pub type TaskId = String;

/// 每個事件的單調序號，用來做增量同步（`since`）。
pub type Seq = u64;

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

/// 支援的 agent 種類。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    OpenCode,
    ClaudeCode,
    Codex,
    Antigravity,
    /// 使用者自訂：直接給可執行程式。
    Custom(String),
}

impl AgentKind {
    /// 穩定識別字（給 CLI / 設定檔用）。
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

    /// 可選的四種內建 agent（不含 Custom）。
    pub fn builtins() -> [AgentKind; 4] {
        [
            AgentKind::OpenCode,
            AgentKind::ClaudeCode,
            AgentKind::Codex,
            AgentKind::Antigravity,
        ]
    }

    /// 互動模式（PTY 直通）的啟動指令。
    pub fn pty_command(&self) -> (&str, Vec<&'static str>) {
        match self {
            AgentKind::OpenCode => ("opencode", vec![]),
            AgentKind::ClaudeCode => ("claude", vec![]),
            AgentKind::Codex => ("codex", vec![]),
            AgentKind::Antigravity => ("agy", vec![]),
            AgentKind::Custom(cmd) => (cmd.as_str(), vec![]),
        }
    }

    /// 原生結構化協定的啟動指令（若有）。
    ///
    /// OpenCode 與 Antigravity 都直接提供原生 ACP（`opencode acp` / `agy acp`），
    /// 無需額外 npm 套件；Claude 與 Codex 走各自的結構化介面（見 adapter crate）。
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

/// 一個 agent 在此系統中的角色。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// 拆任務、產生 worker、整合結果。
    Coordinator,
    /// 實際動手改檔。
    #[default]
    Worker,
    /// 只讀、負責審查。
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
    Exited,
    Failed,
}

impl SessionStatus {
    /// 給 TUI 用的單字元指示。
    pub fn glyph(self) -> &'static str {
        match self {
            SessionStatus::Starting => "◌",
            SessionStatus::Idle => "○",
            SessionStatus::Busy => "●",
            SessionStatus::WaitingApproval => "⏸",
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

/// 一個 agent 的執行實例。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub kind: AgentKind,
    /// 人類可讀標籤，例如 `worker/eu-zip`。
    pub label: String,
    /// 工作目錄（可能是 worktree 路徑）。
    pub cwd: String,
    /// 若在 worktree 中，記錄分支名。
    pub branch: Option<String>,
    pub role: Role,
    pub status: SessionStatus,
    pub model: Option<String>,
    pub cost: Cost,
    /// 誰產生的（人或某個 session）。
    pub parent: Option<SessionId>,
    pub created_ms: u64,
    /// adapter 用的傳輸方式（顯示用）。
    pub transport: String,
}

// ---------------------------------------------------------------------------
// Bus / Task
// ---------------------------------------------------------------------------

/// 訊息的收件者。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Direct(SessionId),
    Channel(String),
    Broadcast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsgKind {
    #[default]
    Chat,
    /// 交接工作。
    Handoff,
    Question,
    Answer,
    Status,
    /// 分享產出（檔案 / 分支 / diff）。
    Artifact,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Todo,
    Doing,
    Blocked,
    Review,
    Done,
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
    pub updated_ms: u64,
}

// ---------------------------------------------------------------------------
// 審批
// ---------------------------------------------------------------------------

/// 統一權限政策：決定 agent 執行工具時要不要問人。
///
/// 每個 adapter 會把這個政策翻譯成自己協定支援的形式：
/// * ACP（OpenCode / Antigravity）：`Ask` = 轉發 `session/request_permission`；
///   `Auto` / `Bypass` = adapter 直接代為核准。
/// * Claude：以 `--permission-mode` 旗標對應。
/// * Codex：以 `thread/start` 的 `approvalPolicy` 對應。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    /// 逐項詢問（ACP 會把請求轉給 client；不支援的協定則盡量保守）。
    Ask,
    /// 自動核准安全操作（預設）：不中斷流程，但仍保留協定層級的界線。
    #[default]
    Auto,
    /// 完全跳過權限檢查，agent 想做什麼都行（危險）。
    Bypass,
}

impl PermissionPolicy {
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

    /// 是否要由 adapter 自行代為核准（不轉發給人）。
    pub fn auto_approves(&self) -> bool {
        matches!(self, PermissionPolicy::Auto | PermissionPolicy::Bypass)
    }

    /// Claude Code 的 CLI 旗標。
    pub fn claude_flags(&self) -> Vec<&'static str> {
        match self {
            PermissionPolicy::Ask => vec!["--permission-mode", "manual"],
            PermissionPolicy::Auto => vec!["--permission-mode", "acceptEdits"],
            PermissionPolicy::Bypass => vec!["--dangerously-skip-permissions"],
        }
    }

    /// Codex app-server 的 CLI 旗標。
    ///
    /// `codex app-server` 不透過命令列吃權限設定，而是用 `thread/start` 的
    /// `approvalPolicy`（見 [`PermissionPolicy::codex_approval_policy`]），因此這裡一律為空。
    pub fn codex_flags(&self) -> Vec<&'static str> {
        vec![]
    }

    /// Codex `thread/start` 的 `approvalPolicy` 值。
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
    Allow,
    /// 本次 session 內一律允許同類請求。
    AllowAlways,
    Deny,
}

/// agent 送出的審批請求（已正規化，不分來源協定）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// adapter 內部的請求 id，回覆時要帶回。
    pub req: String,
    pub session: SessionId,
    /// 簡短標題，例如 `Bash`、`Edit`。
    pub title: String,
    /// 細節（指令、路徑、diff 摘要）。
    pub detail: String,
}

// ---------------------------------------------------------------------------
// Event
// ---------------------------------------------------------------------------

/// 系統中發生的事。append-only、可序列化、可重播。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    SessionStarted {
        session: Session,
    },
    SessionStatus {
        session: SessionId,
        status: SessionStatus,
    },
    SessionLabel {
        session: SessionId,
        label: String,
    },
    /// agent 的文字輸出（串流 chunk 或整行）。
    Output {
        session: SessionId,
        text: String,
    },
    ToolCall {
        session: SessionId,
        name: String,
        #[serde(default)]
        input: String,
        /// `None` = 進行中；`Some(true/false)` = 結束且成功/失敗。
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
    BusMessage {
        message: BusMessage,
    },
    TaskChanged {
        task: Task,
    },
    SessionExited {
        session: SessionId,
        code: Option<i32>,
    },
    Cost {
        session: SessionId,
        cost: Cost,
    },
}

/// 事件 + 序號 + 時間，是對外（socket / log）傳遞的單位。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub seq: Seq,
    pub ts: u64,
    #[serde(flatten)]
    pub event: Event,
}

// ---------------------------------------------------------------------------
// State：事件的 fold 結果
// ---------------------------------------------------------------------------

/// transcript 每個 session 最多保留的行數（避免長對話吃記憶體）。
pub const TRANSCRIPT_CAP: usize = 4000;
/// bus 訊息保留上限。
pub const MESSAGE_CAP: usize = 1000;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct State {
    pub seq: Seq,
    pub sessions: BTreeMap<SessionId, Session>,
    pub tasks: BTreeMap<TaskId, Task>,
    pub messages: Vec<BusMessage>,
    /// 尚未回覆的審批請求。
    pub pending_permissions: BTreeMap<String, PermissionRequest>,
    /// session id → 輸出文字行。
    pub transcripts: HashMap<SessionId, VecDeque<String>>,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// 把一個事件套用到狀態上。`rec.seq` 會更新 `self.seq`。
    pub fn apply(&mut self, rec: &Record) {
        self.seq = self.seq.max(rec.seq);
        match &rec.event {
            Event::SessionStarted { session } => {
                self.sessions.insert(session.id.clone(), session.clone());
            }
            Event::SessionStatus { session, status } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.status = *status;
                }
            }
            Event::SessionLabel { session, label } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.label = label.clone();
                }
            }
            Event::Output { session, text } => {
                let buf = self.transcripts.entry(session.clone()).or_default();
                // 尾端換行不該產生一個空行；中間的空行則保留。
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
            Event::ToolCall { .. } => { /* 由 TUI 從事件流即時呈現 */ }
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
            Event::BusMessage { message } => {
                self.messages.push(message.clone());
                while self.messages.len() > MESSAGE_CAP {
                    self.messages.remove(0);
                }
            }
            Event::TaskChanged { task } => {
                self.tasks.insert(task.id.clone(), task.clone());
            }
            Event::SessionExited { session, .. } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    if s.status.is_live() {
                        s.status = SessionStatus::Exited;
                    }
                }
            }
            Event::Cost { session, cost } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.cost = *cost;
                }
            }
        }
    }

    /// 方便 TUI 依 label 排序取得 session 清單。
    pub fn sessions_sorted(&self) -> Vec<&Session> {
        let mut v: Vec<&Session> = self.sessions.values().collect();
        v.sort_by_key(|s| s.created_ms);
        v
    }

    pub fn live_sessions(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values().filter(|s| s.status.is_live())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(seq: u64, event: Event) -> Record {
        Record { seq, ts: 0, event }
    }

    fn sample_session(id: &str) -> Session {
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
        }
    }

    #[test]
    fn fold_tracks_status_and_transcript() {
        let mut st = State::new();
        st.apply(&rec(
            1,
            Event::SessionStarted {
                session: sample_session("s1"),
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
                session: sample_session("s1"),
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
    fn agent_kind_roundtrip() {
        for k in AgentKind::builtins() {
            assert_eq!(AgentKind::parse(k.id()).as_ref(), Some(&k));
        }
        assert_eq!(AgentKind::OpenCode.acp_command().unwrap().0, "opencode");
        assert!(AgentKind::Codex.acp_command().is_none());
    }
}
