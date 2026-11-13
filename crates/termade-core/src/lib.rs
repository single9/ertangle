//! `termade-core` — 領域模型。
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

/// session 目前的 context 用量（對話已佔用的 token 數與模型的 context 視窗）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used: u64,
    /// 模型的 context 視窗大小；agent 尚未回報時為 `None`。
    pub window: Option<u64>,
}

impl ContextUsage {
    pub fn percent(&self) -> Option<f32> {
        self.window
            .filter(|w| *w > 0)
            .map(|w| self.used as f32 * 100.0 / w as f32)
    }

    /// `36k/1M 4%`；視窗未知時只顯示 `36k`。
    pub fn short(&self) -> String {
        match (self.window, self.percent()) {
            (Some(w), Some(p)) => {
                format!("{}/{} {:.0}%", human_tokens(self.used), human_tokens(w), p)
            }
            _ => human_tokens(self.used),
        }
    }
}

/// token 數的簡短寫法：`950`、`36k`、`1.5M`。
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
    /// agent 透過 `termade_status` 回報的簡短進度說明。
    #[serde(default)]
    pub status_note: Option<String>,
    /// 建立 worktree 時所屬的主 repo 根目錄（merge／gc／pr 用）。
    #[serde(default)]
    pub repo_root: Option<String>,
    /// 此 session 的權限政策（舊 log 沒有此欄位時為 `auto`）。
    #[serde(default)]
    pub permission: PermissionPolicy,
    /// context 用量（agent 回報；舊 log 沒有此欄位）。
    #[serde(default)]
    pub context: Option<ContextUsage>,
    /// agent 原生的對話 id（Claude session id、Codex thread id、ACP sessionId），接續用。
    /// `None` 表示這個 session 不能接續（舊 log、PTY，或 agent 沒有宣告載入能力）。
    #[serde(default)]
    pub native_id: Option<String>,
}

impl Session {
    /// 這個 session 屬於哪個專案：worktree 看主 repo，否則就是工作目錄。
    pub fn project_root(&self) -> &str {
        self.repo_root.as_deref().unwrap_or(&self.cwd)
    }
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
    /// 寬鬆程度：`ask` < `auto` < `bypass`。
    pub fn rank(&self) -> u8 {
        match self {
            PermissionPolicy::Ask => 0,
            PermissionPolicy::Auto => 1,
            PermissionPolicy::Bypass => 2,
        }
    }

    /// 取兩者中較嚴格的。
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
    /// log 輪替後寫在新檔第一行的完整狀態；重播時以它為起點。seq 沿用輪替當下的值。
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
    /// agent 回報 session 的 context 用量。
    SessionContext {
        session: SessionId,
        used: u64,
        window: Option<u64>,
    },
    /// agent 回報帳號額度（用量百分比與重置時間）。
    QuotaUpdated {
        quota: Quota,
    },
    /// agent 回報額度用完／被限流（可交接給其他 agent）。
    SessionLimited {
        session: SessionId,
        detail: String,
    },
    /// session 從清單移除（啟動失敗或使用者清除已結束的 session）。
    SessionRemoved {
        session: SessionId,
    },
    /// agent 回報（或設定指定）的實際 model。
    SessionModel {
        session: SessionId,
        model: String,
    },
    /// agent 回報的原生對話 id（接續用）。
    SessionNativeId {
        session: SessionId,
        native_id: String,
    },
    SessionLabel {
        session: SessionId,
        label: String,
    },
    /// agent 透過 `termade_status` 更新的簡短狀態說明。
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
    /// event log 寫入健康狀態（`ok == false` 表示寫入失敗，TUI 顯示 `log ✗`）。
    LogHealth {
        ok: bool,
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

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// event log 寫入健康狀態（預設正常）。
    #[serde(default = "default_true")]
    pub log_ok: bool,
    /// 各 agent 最近一次回報的額度（agent id → 額度）。
    #[serde(default)]
    pub quotas: BTreeMap<String, Quota>,
}

impl Default for State {
    fn default() -> Self {
        State {
            seq: 0,
            sessions: BTreeMap::new(),
            tasks: BTreeMap::new(),
            messages: Vec::new(),
            artifacts: Vec::new(),
            reviews: Vec::new(),
            pending_permissions: BTreeMap::new(),
            transcripts: HashMap::new(),
            log_ok: true,
            quotas: BTreeMap::new(),
        }
    }
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
            Event::Snapshot { state } => {
                *self = (**state).clone();
                self.seq = rec.seq;
            }
            Event::SessionStarted { session } => {
                self.sessions.insert(session.id.clone(), session.clone());
            }
            Event::SessionStatus { session, status } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.status = *status;
                }
            }
            Event::SessionContext {
                session,
                used,
                window,
            } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    // 視窗大小只在 agent 回報時更新，沒回報就沿用上一次的。
                    let window = window.or(s.context.and_then(|c| c.window));
                    // used == 0 表示這次只補視窗大小，沿用上一次的用量。
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
                    let d: String = detail.chars().take(80).collect();
                    s.status_note = Some(format!("額度用完：{d}（可用 handoff 交接）"));
                }
            }
            Event::SessionRemoved { session } => {
                self.sessions.remove(session);
                self.transcripts.remove(session);
                self.pending_permissions
                    .retain(|_, p| &p.session != session);
            }
            Event::SessionModel { session, model } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.model = Some(model.clone());
                }
            }
            Event::SessionNativeId { session, native_id } => {
                if let Some(s) = self.sessions.get_mut(session) {
                    s.native_id = Some(native_id.clone());
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
                output,
            } => {
                if ok.is_none() && !name.is_empty() {
                    let line = match tool_summary(input) {
                        Some(sum) => format!("▸ {name}: {sum}"),
                        None => format!("▸ {name}"),
                    };
                    self.push_transcript_line(session, line);
                } else if let Some(ok) = ok {
                    // 工具結果：成功只顯示第一行摘要，失敗要看得到原因。
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
                            format!(" …(+{extra} 行)")
                        } else {
                            "…".into()
                        });
                    }
                    let line = match (ok, text.is_empty()) {
                        (true, true) => "  ✓".to_string(),
                        (true, false) => format!("  ✓ {text}"),
                        (false, true) => "  ✗ 失敗".to_string(),
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
            Event::LogHealth { ok } => {
                self.log_ok = *ok;
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

    /// 任務還沒完成的前置任務（`depends_on` 裡存在且狀態不是 `done` 的）。
    /// 找不到的 id 不算：它永遠不會完成，算進去會讓任務卡死。
    pub fn unfinished_deps(&self, task: &Task) -> Vec<&Task> {
        task.depends_on
            .iter()
            .filter_map(|id| self.tasks.get(id))
            .filter(|t| t.status != TaskStatus::Done)
            .collect()
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
/// 可由 `~/.config/termade/policy.toml`（或專案 `.termade/policy.toml`）覆寫：
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
        // deny 維持保守的子字串比對：寧可多擋。
        for pat in &self.deny {
            if !pat.is_empty() && combined.contains(pat.as_str()) {
                return Some(Decision::Deny);
            }
        }
        // allow 要嚴格：工具名稱完全相等，或指令以該規則為「整詞前綴」，
        // 避免 `Read` 命中 `README`。
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

    /// 疊上 repo 層級規則：`deny` 取聯集（不能移除全域 deny），`allow` 也合併。
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

/// `text` 是否以 `pat` 開頭，且 `pat` 結束處是詞邊界（空白或結尾）。
fn word_prefix(text: &str, pat: &str) -> bool {
    let t = text.trim_start();
    t.starts_with(pat)
        && t[pat.len()..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
}

/// 從審批請求的 `detail`（通常是 JSON rawInput）取出指令字串。
fn command_text(detail: &str) -> Option<String> {
    // 不是 JSON 時，整段當作指令文字。
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

/// 把工具輸入（JSON 字串）濃縮成一行人看得懂的摘要：優先 command／file_path／pattern 等欄位。
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
            if m.is_empty() {
                None
            } else {
                Some(one_line(&v.to_string()))
            }
        }
        _ => None,
    }
}

/// 某個額度視窗（例如 Claude 的 5 小時／7 天）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindow {
    /// 顯示名稱：`5h`、`7d`、`30d`…
    pub name: String,
    pub used_percent: f32,
    /// 重置時間（epoch 毫秒）。
    pub resets_at_ms: Option<u64>,
}

/// 某個 agent 帳號目前的額度狀態（由 agent 自己回報，非 termade 估算）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quota {
    /// agent 種類 id，例如 `claude`、`codex`。
    pub agent: String,
    pub windows: Vec<QuotaWindow>,
    /// 已達上限／被限流。
    pub reached: bool,
    pub updated_ms: u64,
}

/// 把剩餘毫秒轉成 `2h13m`／`45m`／`3d4h`。
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
    /// 單行摘要：`claude 5h 39% (重置 1h20m) 7d 22%`。已過重置時間的視窗顯示 `已重置`。
    pub fn summary(&self, now_ms: u64) -> String {
        let parts: Vec<String> = self
            .windows
            .iter()
            .map(|w| match w.resets_at_ms {
                Some(r) if r <= now_ms => format!("{} 已重置", w.name),
                Some(r) => format!(
                    "{} {:.0}% (重置 {})",
                    w.name,
                    w.used_percent,
                    format_remaining(r - now_ms)
                ),
                None => format!("{} {:.0}%", w.name, w.used_percent),
            })
            .collect();
        format!(
            "{}{} {}",
            self.agent,
            if self.reached { " ✗額度已滿" } else { "" },
            parts.join(" · ")
        )
    }

    /// 最吃緊的視窗用量（百分比）。
    pub fn max_used(&self) -> f32 {
        self.windows
            .iter()
            .map(|w| w.used_percent)
            .fold(0.0, f32::max)
    }
}

/// 視窗長度（分鐘）轉成顯示名稱。
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

/// 錯誤文字是否像額度用完／被限流（各 agent 的訊息不一，採保守的關鍵字比對）。
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

/// 隨 prompt 一起送出的附件（檔案或圖片）。內容由送出端（TUI／CLI）讀好並內嵌，
/// 所以遠端 daemon 也不需要存取同一個檔案系統。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    /// 顯示用檔名。
    pub name: String,
    /// MIME 類型，例如 `image/png`、`text/plain`。
    pub mime: String,
    /// 原始路徑（僅供參考／PTY 使用）。
    #[serde(default)]
    pub path: Option<String>,
    /// 圖片的 base64 內容。
    #[serde(default)]
    pub data: Option<String>,
    /// 文字檔的內容。
    #[serde(default)]
    pub text: Option<String>,
    pub size: u64,
}

/// 圖片附件大小上限（5 MB）與文字檔內嵌上限（256 KB）。
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_TEXT_BYTES: u64 = 256 * 1024;

/// 依副檔名猜測圖片 MIME；不是圖片回傳 `None`。
pub fn image_mime(path: &std::path::Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// 標準 base64 編碼（含 padding）。
pub fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

impl Attachment {
    /// 讀取檔案成附件：圖片內嵌 base64（上限 5 MB）；UTF-8 文字檔內嵌內容（上限 256 KB，
    /// 超過就只帶路徑）；其他二進位檔只帶路徑。
    pub fn load(path: &std::path::Path) -> Result<Attachment, String> {
        let meta =
            std::fs::metadata(path).map_err(|e| format!("無法讀取 {}：{e}", path.display()))?;
        if !meta.is_file() {
            return Err(format!("{} 不是檔案", path.display()));
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
                return Err(format!(
                    "圖片 {} 太大（{} KB，上限 {} KB）",
                    att.name,
                    meta.len() / 1024,
                    MAX_IMAGE_BYTES / 1024
                ));
            }
            let bytes = std::fs::read(path).map_err(|e| format!("讀取 {} 失敗：{e}", att.name))?;
            att.mime = mime.into();
            att.data = Some(base64_encode(&bytes));
            return Ok(att);
        }
        if meta.len() <= MAX_TEXT_BYTES {
            let bytes = std::fs::read(path).map_err(|e| format!("讀取 {} 失敗：{e}", att.name))?;
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

    /// 一行摘要，用於 transcript：`📎 name (image/png, 12 KB)`。
    pub fn summary(&self) -> String {
        format!(
            "📎 {} ({}, {} KB)",
            self.name,
            self.mime,
            self.size.div_ceil(1024)
        )
    }
}

/// 把附件併入 prompt 文字：文字檔以 fenced block 內嵌；圖片在 `images_as_text` 為真時改成一行說明
/// （給不支援圖片區塊的 agent）。
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
                "\n\n[檔案附件：{}{}（內容未內嵌）]",
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
        }
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
        // 接續後 agent 換了 id：以最新的為準。
        st.apply(&rec(
            3,
            Event::SessionNativeId {
                session: "s1".into(),
                native_id: "def".into(),
            },
        ));
        assert_eq!(st.sessions["s1"].native_id.as_deref(), Some("def"));

        // 舊 log 的 Session 沒有 native_id 欄位。
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
        // `cat README.md` 不能因 `Read` 被放行
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
        // 之後只有用量更新時沿用視窗
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
}
