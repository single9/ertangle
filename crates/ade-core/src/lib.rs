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
    /// 閒置休眠（程序已 SIGSTOP 暫停，收到新訊息或喚醒時自動 SIGCONT）。
    Sleeping,
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
    /// agent 透過 `ade_status` 回報的簡短進度說明。
    #[serde(default)]
    pub status_note: Option<String>,
    /// 建立 worktree 時所屬的主 repo 根目錄（merge／gc／pr 用）。
    #[serde(default)]
    pub repo_root: Option<String>,
}

// ---------------------------------------------------------------------------
// Bus / Task / Artifact
// ---------------------------------------------------------------------------

/// 訊息的收件者。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Direct(SessionId),
    Channel(String),
    Broadcast,
}

impl Target {
    /// 從字串解析收件目標：
    /// * `*` 或 `broadcast` → [`Target::Broadcast`]
    /// * `#name` 或 `channel:name` → [`Target::Channel`]
    /// * `@id` 或其他字串 → [`Target::Direct`]
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
    /// 交接工作。
    Handoff,
    Question,
    Answer,
    Status,
    /// 分享產出（檔案 / 分支 / diff）。
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
    /// 這則訊息是否應該進某個 session 的收件匣。
    ///
    /// * 自己發出的廣播/頻道訊息不會退回自己收件匣。
    /// * `Direct` 同時比對 `session_id` 與人類可讀 `label`。
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
    Todo,
    Doing,
    Blocked,
    Review,
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

/// agent 分享的產出物（檔案、分支或 diff）。
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

/// 審查者在 diff 行內留下的註解（M3 review 迴圈）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewComment {
    pub id: String,
    pub session: SessionId,
    /// 檔案與行號或 diff 行內容摘要。
    pub location: String,
    pub comment: String,
    pub ts: u64,
}

/// Worktree 摘要資訊（M3 工作區管理）。
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
    /// agent 回報（或設定指定）的實際 model。
    SessionModel {
        session: SessionId,
        model: String,
    },
    SessionLabel {
        session: SessionId,
        label: String,
    },
    /// agent 透過 `ade_status` 更新的簡短狀態說明。
    SessionNote {
        session: SessionId,
        note: String,
    },
    /// 使用者送出的 prompt（顯示在逐字稿，讓對話有上下文）。
    UserPrompt {
        session: SessionId,
        text: String,
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
/// artifact 保留上限。
pub const ARTIFACT_CAP: usize = 500;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct State {
    pub seq: Seq,
    pub sessions: BTreeMap<SessionId, Session>,
    pub tasks: BTreeMap<TaskId, Task>,
    pub messages: Vec<BusMessage>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub reviews: Vec<ReviewComment>,
    /// 尚未回覆的審批請求。
    pub pending_permissions: BTreeMap<String, PermissionRequest>,
    /// session id → 輸出文字行。
    pub transcripts: HashMap<SessionId, VecDeque<String>>,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    fn push_transcript_line(&mut self, session: &str, line: String) {
        let buf = self.transcripts.entry(session.to_string()).or_default();
        buf.push_back(line);
        while buf.len() > TRANSCRIPT_CAP {
            buf.pop_front();
        }
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
            Event::SessionModel { session, model } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.model = Some(model.clone());
                }
            }
            Event::SessionLabel { session, label } => {
                if let Some(s) = self.sessions.get_mut(session) {
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
            Event::UserPrompt { session, text } => {
                let buf = self.transcripts.entry(session.clone()).or_default();
                // 第一行加上 `❯ ` 前綴；多行 prompt 的後續行縮排對齊。
                let text = text.trim_end_matches('\n');
                if !text.is_empty() {
                    for (i, line) in text.split('\n').enumerate() {
                        if i == 0 {
                            buf.push_back(format!("❯ {line}"));
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
            Event::ToolCall {
                session,
                name,
                input,
                ok,
                ..
            } => {
                if ok.is_none() && !name.is_empty() {
                    let short_in: String = input.replace('\n', " ").chars().take(80).collect();
                    let line = if short_in.is_empty() || short_in == "null" {
                        format!("▸ {name}")
                    } else {
                        format!("▸ {name} {short_in}")
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

    /// 方便 TUI 依建立時間排序取得 session 清單。
    pub fn sessions_sorted(&self) -> Vec<&Session> {
        let mut v: Vec<&Session> = self.sessions.values().collect();
        v.sort_by_key(|s| s.created_ms);
        v
    }

    pub fn live_sessions(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values().filter(|s| s.status.is_live())
    }

    /// 計算所有 session 的累計成本與 token 用量。
    pub fn total_cost(&self) -> Cost {
        let mut total = Cost::default();
        for s in self.sessions.values() {
            total.usd += s.cost.usd;
            total.input_tokens += s.cost.input_tokens;
            total.output_tokens += s.cost.output_tokens;
        }
        total
    }

    /// 以 `id` 或 `label` 解析 session id（優先回傳完全匹配的 id，其次為存活中的同 label session）。
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

    /// 取得某個 session 在 `after_msg_id` 之後收到的訊息。
    pub fn inbox_for(&self, session_id: &str, after_msg_id: Option<&str>) -> Vec<BusMessage> {
        let resolved = self
            .resolve_session_id(session_id)
            .unwrap_or_else(|| session_id.to_string());
        let label = self.sessions.get(&resolved).map(|s| s.label.as_str());
        let start = after_msg_id
            .and_then(|id| self.messages.iter().position(|m| m.id == id))
            .map(|idx| idx + 1)
            .unwrap_or(0);
        self.messages[start..]
            .iter()
            .filter(|m| m.matches_recipient(&resolved, label))
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Policy Engine & 秘密保護（M4）
// ---------------------------------------------------------------------------

/// 權限規則引擎：在 `PermissionPolicy` 之前先過濾明確安全與高風險操作。
///
/// 可由 `~/.config/ade/policy.toml`（或專案 `.ade/policy.toml`）覆寫：
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
    /// 從簡易 TOML 文字解析規則（未指定的區塊保留預設值）。
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

    /// 評估一個審批請求：
    /// * 命中 `deny` → `Some(Decision::Deny)`
    /// * 命中 `allow` → `Some(Decision::Allow)`
    /// * 其餘 → `None`（交由 session 的 `PermissionPolicy` 或人類決定）
    pub fn evaluate(&self, title: &str, detail: &str) -> Option<Decision> {
        let combined = format!("{title} {detail}");
        for pat in &self.deny {
            if !pat.is_empty() && combined.contains(pat.as_str()) {
                return Some(Decision::Deny);
            }
        }
        for pat in &self.allow {
            if !pat.is_empty() && (title == pat || combined.contains(pat.as_str())) {
                return Some(Decision::Allow);
            }
        }
        None
    }
}

fn parse_string_array(s: &str) -> Vec<String> {
    let s = s.trim().trim_start_matches('[').trim_end_matches(']');
    s.split(',')
        .map(|item| item.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// 遮罩文字中的常見金鑰與憑證樣式（寫入 event log 前保護秘密）。
pub fn redact_secrets(input: &str) -> String {
    const PREFIXES: &[&str] = &["sk-", "ghp_", "gho_", "AIza", "AKIA", "xoxb-"];
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &input[i..];
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
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
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
            status_note: None,
            repo_root: None,
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
    fn session_model_event_updates_state() {
        let mut st = State::new();
        st.apply(&Record {
            seq: 1,
            ts: 0,
            event: Event::SessionStarted {
                session: sample_session("s1"),
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
}
