//! `termade-protocol` — daemon ↔ client 的控制介面。
//!
//! 傳輸：Unix domain socket，**newline-delimited JSON** (一行一個訊息)。
//! 語意：client 送出 [`Request`]，daemon 回覆 [`Frame`] (含事件串流與快照)。
//!
//! 這個 crate 只有 wire 型別與編解碼，不含任何 IO，方便日後換成 TCP / JSON-RPC。

use serde::{Deserialize, Serialize};
pub use termade_core::{ActiveTool, BgProc};
use termade_core::{
    AgentKind, ArtifactKind, Attachment, BusMessage, Decision, MsgId, MsgKind, PermissionPolicy,
    Record, Role, Seq, SessionId, State, Target, TaskId, TaskStatus, WorktreeInfo,
};

fn default_true() -> bool {
    true
}

fn default_wait_timeout() -> u64 {
    30_000
}

/// 一個 session 要用哪種傳輸方式跑。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// 有原生結構化協定就用 (ACP / stream-json / app-server)，否則退回 PTY。
    #[default]
    Auto,
    /// 強制原生結構化協定。
    #[serde(alias = "acp")]
    Structured,
    /// 強制 PTY 直通 (對任何 CLI agent 都有效)。
    Pty,
}

/// 建立 session 的請求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    /// `None` = 由 daemon 依 role／父 session／預設設定決定 (見 `Config::resolve_kind`)。
    #[serde(default)]
    pub kind: Option<AgentKind>,
    /// 工作目錄；`None` = daemon 的預設 (通常是 daemon 的 cwd)。
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub role: Role,
    /// 啟動後立刻送出的第一個 prompt。
    #[serde(default)]
    pub prompt: Option<String>,
    /// 是否建立 git worktree 並在其中工作。
    #[serde(default)]
    pub worktree: bool,
    #[serde(default)]
    pub transport: Transport,
    /// 統一權限政策。
    #[serde(default)]
    pub permission: Option<PermissionPolicy>,
    /// 由哪個 session 產生 (人類產生則為 `None`)。
    #[serde(default)]
    pub parent: Option<SessionId>,
    /// 指定 model；省略時依 `config.toml` 決定。
    #[serde(default)]
    pub model: Option<String>,
    /// 推理強度 (low／medium／high／xhigh／max)；省略時依 config.toml。
    #[serde(default)]
    pub effort: Option<String>,
    /// 初始 prompt 的附件。
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// 沿用該 session 的工作目錄、分支與 repo (交接用，不建立新 worktree)。
    #[serde(default)]
    pub inherit_from: Option<SessionId>,
    /// 一次性額外命令列參數 (附加在設定檔的 args 之後)。
    #[serde(default)]
    pub args: Vec<String>,
    /// 單一 session 的成本上限 (美元)；省略時依 `config.toml` 決定。
    #[serde(default)]
    pub budget_usd: Option<f64>,
}

/// client → daemon。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// 打招呼並 (可選)要求補齊 `since` 之後的事件；遠端連線可帶 `token`。
    Hello {
        client: String,
        #[serde(default)]
        since: Option<Seq>,
        #[serde(default)]
        token: Option<String>,
        /// client 上次連線時 daemon 回報的 epoch；不符時 daemon 忽略 `since`、改送完整快照。
        #[serde(default)]
        epoch: Option<String>,
    },
    /// 要一份完整狀態快照。
    Snapshot,
    Spawn(SpawnRequest),
    /// 送 prompt / 使用者輸入給某個 session。
    Prompt {
        session: SessionId,
        text: String,
        /// 附件 (檔案、圖片)。
        #[serde(default)]
        attachments: Vec<Attachment>,
        /// 若為 `true`：session 忙或佇列非空時改進待送佇列，閒下來再依序送出。
        #[serde(default)]
        queue: bool,
    },
    /// 清空某個 session 的待送 prompt 佇列。
    QueueClear {
        session: SessionId,
    },
    /// 取出 (移除)某個 session 待送佇列的最後一則，透過 `Frame::Ok { note }` 回傳原始文字。
    QueuePop {
        session: SessionId,
    },
    /// 中斷當前回合 (不殺處理程序)。
    Cancel {
        session: SessionId,
    },
    /// 從清單移除已結束的 session；有 worktree 分支時比照 gc (未合併或有未 commit 變更需 `force`)。
    SessionRemove {
        session: SessionId,
        #[serde(default)]
        force: bool,
    },
    /// 把 session 的工作交接給另一個 agent：產生交接簡報，新 agent 沿用同一個工作目錄。
    Handoff {
        session: SessionId,
        kind: AgentKind,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        effort: Option<String>,
        /// 額外補充給接手者的說明。
        #[serde(default)]
        note: Option<String>,
    },
    /// 結束 session 並殺掉處理程序。
    Kill {
        session: SessionId,
    },
    /// 終止某個 session 的背景子處理程序 (`pid: None` 代表清理全部)。
    KillBgProc {
        session: SessionId,
        #[serde(default)]
        pid: Option<u32>,
        #[serde(default)]
        force: bool,
    },
    /// 接續已結束的 session：用 agent 原生的對話 id 接回原對話 (session id 不變)。
    Resume {
        session: SessionId,
        /// 接續成功後立刻送出的 prompt。
        #[serde(default)]
        prompt: Option<String>,
    },
    /// 暫停閒置 session 子處理程序 (SIGSTOP)。
    Sleep {
        session: SessionId,
    },
    /// 唤醒休眠中的 session 子處理程序 (SIGCONT)。
    Wake {
        session: SessionId,
    },
    /// 執行期切換指定 session 的 model 與／或 effort (下一個回合起生效)。
    SessionConfig {
        session: SessionId,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        effort: Option<String>,
    },
    /// 設定或調高指定 session 的成本上限 (美元)，並解除 `over_budget`。
    SessionBudget {
        session: SessionId,
        budget_usd: f64,
    },
    /// agent (經 MCP `termade_approve`)請求核准並等待結果。
    McpPermission {
        session: SessionId,
        #[serde(alias = "tool")]
        title: String,
        #[serde(alias = "input")]
        detail: String,
    },
    /// agent (經 MCP `termade_approve`)請求核准並等待結果；回覆 `Ok` 的 note 為 `allow` 或 `deny`。
    ApprovalAsk {
        session: SessionId,
        tool: String,
        /// 工具輸入 (JSON 字串)。
        input: String,
    },
    /// 回覆核准請求。
    Permission {
        session: SessionId,
        req: String,
        decision: Decision,
    },
    /// 送訊息到匯流排 (agent 之間溝通)。
    BusSend {
        #[serde(default)]
        from: Option<SessionId>,
        to: Target,
        #[serde(default)]
        kind: MsgKind,
        body: String,
        #[serde(default)]
        reply_to: Option<MsgId>,
    },
    /// 讀取某個 session 的收件匣 (預設只讀未讀並推進游標)。
    BusInbox {
        session: SessionId,
        #[serde(default = "default_true")]
        unread_only: bool,
    },
    /// 阻塞等待下一則進收件匣的訊息 (可選篩選發送者或 `reply_to`)。
    BusWait {
        session: SessionId,
        #[serde(default)]
        from: Option<SessionId>,
        #[serde(default)]
        reply_to: Option<MsgId>,
        #[serde(default = "default_wait_timeout")]
        timeout_ms: u64,
    },
    TaskCreate {
        title: String,
        #[serde(default)]
        detail: String,
        #[serde(default)]
        assignee: Option<SessionId>,
        #[serde(default)]
        created_by: Option<SessionId>,
        #[serde(default)]
        depends_on: Vec<TaskId>,
    },
    TaskUpdate {
        id: TaskId,
        #[serde(default)]
        status: Option<TaskStatus>,
        #[serde(default)]
        assignee: Option<SessionId>,
        #[serde(default)]
        detail: Option<String>,
    },
    /// 更新某個 session 的簡短狀態字串 (對應 `termade_status`)。
    SessionNote {
        session: SessionId,
        note: String,
    },
    /// 分享產出 (檔案、分支或 diff)。
    ArtifactShare {
        session: SessionId,
        #[serde(default)]
        kind: ArtifactKind,
        path: String,
        #[serde(default)]
        note: String,
    },
    /// 列出所有受管 worktree。
    WorktreeList,
    /// 取得某個 session 工作區的 `git diff` (含未追蹤檔案摘要)。
    WorktreeDiff {
        session: SessionId,
    },
    /// 退回 (捨棄) 某個 session 工作區的單一 Hunk 或整個檔案變更。
    WorktreeDiscard {
        session: SessionId,
        file: String,
        #[serde(default)]
        patch: Option<String>,
    },
    /// 將某個 session 的 worktree 分支合併回主工作區。
    WorktreeMerge {
        session: SessionId,
        #[serde(default)]
        dry_run: bool,
        #[serde(default)]
        resolve: bool,
    },
    /// 為某個 session 的分支建立 Pull Request (透過 `gh`)。
    WorktreePr {
        session: SessionId,
        title: String,
        #[serde(default)]
        body: String,
    },
    /// 清除已結束 session 的 worktree (若指定 `session` 則清除該筆)。
    WorktreeGc {
        #[serde(default)]
        session: Option<SessionId>,
        /// 連未合併或有未 commit 變更的也一併移除。
        #[serde(default)]
        force: bool,
    },
    /// 對某個 session 的 diff 行留下審查註解並送回該 agent 修正。
    ReviewComment {
        session: SessionId,
        location: String,
        comment: String,
    },
    /// 控制或查詢 Web 版支援。
    WebControl {
        #[serde(default)]
        enabled: Option<bool>,
        #[serde(default)]
        addr: Option<String>,
    },
    /// 列出某個 agent 可選的 model (Claude 內建清單；其他 agent 向 CLI 查詢，最多數秒)。
    ListModels {
        /// agent 種類 (`claude`、`codex`、`opencode`、`antigravity`)。
        kind: String,
    },
    /// 要 daemon 關閉。
    Shutdown,
}

/// daemon → client。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Frame {
    /// 連線建立。
    Ready {
        pid: u32,
        version: String,
        /// daemon 啟動世代 id；log 被換掉或刪除時 client 可據此丟棄本地狀態。
        #[serde(default)]
        epoch: String,
    },
    /// 完整狀態 (初次連線或要求 Snapshot 時)。
    Snapshot {
        state: Box<State>,
    },
    /// 單一事件。
    Record {
        record: Box<Record>,
    },
    /// 收件匣或等待結果。
    Messages {
        messages: Vec<BusMessage>,
        #[serde(default)]
        timed_out: bool,
    },
    /// 某個 session 工作區的 git diff。
    Diff {
        session: SessionId,
        #[serde(default)]
        branch: Option<String>,
        cwd: String,
        diff: String,
    },
    /// 所有受管 worktree 清單。
    Worktrees {
        worktrees: Vec<WorktreeInfo>,
    },
    /// 核准決策回覆 (給 MCP `termade_approve` 或 client)。
    PermissionDecision {
        decision: Decision,
    },
    /// 操作成功 (可帶說明)。
    Ok {
        #[serde(default)]
        note: Option<String>,
    },
    Error {
        message: String,
    },
    /// `ListModels` 的回覆：`id` 為空字串代表「依設定檔預設」。
    Models {
        kind: String,
        models: Vec<ModelInfo>,
    },
}

/// 一個可選的 model。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
}

impl Frame {
    pub fn ok() -> Self {
        Frame::Ok { note: None }
    }
    pub fn ok_note(note: impl Into<String>) -> Self {
        Frame::Ok {
            note: Some(note.into()),
        }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Frame::Error {
            message: message.into(),
        }
    }
}

/// 把訊息編成一行 JSON (含結尾 `\n`)。
pub fn encode_line<T: Serialize>(value: &T) -> std::io::Result<String> {
    let mut s = serde_json::to_string(value)?;
    s.push('\n');
    Ok(s)
}

/// 解析一行 JSON。
pub fn decode_line<T: for<'de> Deserialize<'de>>(line: &str) -> serde_json::Result<T> {
    serde_json::from_str(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = Request::Spawn(SpawnRequest {
            kind: Some(AgentKind::OpenCode),
            cwd: Some("/tmp".into()),
            label: Some("worker".into()),
            role: Role::Worker,
            prompt: Some("hi".into()),
            worktree: false,
            transport: Transport::Auto,
            permission: None,
            parent: None,
            model: None,
            effort: None,
            attachments: vec![],
            inherit_from: None,
            args: vec![],
            budget_usd: None,
        });
        let line = encode_line(&req).unwrap();
        assert!(line.ends_with('\n'));
        let back: Request = decode_line(line.trim()).unwrap();
        match back {
            Request::Spawn(s) => assert_eq!(s.kind, Some(AgentKind::OpenCode)),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn frame_tag_is_ev() {
        let f = Frame::ok_note("done");
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(v["ev"], "ok");
    }

    #[test]
    fn transport_accepts_acp_alias() {
        let t: Transport = serde_json::from_str("\"acp\"").unwrap();
        assert_eq!(t, Transport::Structured);
        let t: Transport = serde_json::from_str("\"structured\"").unwrap();
        assert_eq!(t, Transport::Structured);
        assert_eq!(
            serde_json::to_string(&Transport::Structured).unwrap(),
            "\"structured\""
        );
    }

    #[test]
    fn old_spawn_json_without_model_still_parses() {
        let req: Request = serde_json::from_str(r#"{"op":"spawn","kind":"claude_code"}"#).unwrap();
        let Request::Spawn(s) = req else { panic!() };
        assert!(s.model.is_none() && s.args.is_empty() && s.budget_usd.is_none());
    }
}
