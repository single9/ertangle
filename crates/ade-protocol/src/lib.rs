//! `ade-protocol` — daemon ↔ client 的控制介面。
//!
//! 傳輸：Unix domain socket，**newline-delimited JSON**（一行一個訊息）。
//! 語意：client 送出 [`Request`]，daemon 回覆 [`Frame`]（含事件串流與快照）。
//!
//! 這個 crate 只有 wire 型別與編解碼，不含任何 IO，方便日後換成 TCP / JSON-RPC。

use ade_core::{
    AgentKind, ArtifactKind, BusMessage, Decision, MsgId, MsgKind, PermissionPolicy, Record, Role,
    Seq, SessionId, State, Target, TaskId, TaskStatus,
};
use serde::{Deserialize, Serialize};

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
    /// 有原生結構化協定就用（ACP / stream-json / app-server），否則退回 PTY。
    #[default]
    Auto,
    /// 強制原生結構化協定。
    #[serde(alias = "acp")]
    Structured,
    /// 強制 PTY 直通（對任何 CLI agent 都有效）。
    Pty,
}

/// 建立 session 的請求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub kind: AgentKind,
    /// 工作目錄；`None` = daemon 的預設（通常是 daemon 的 cwd）。
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
    pub permission: PermissionPolicy,
    /// 由哪個 session 產生（人類產生則為 `None`）。
    #[serde(default)]
    pub parent: Option<SessionId>,
}

/// client → daemon。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// 打招呼並（可選）要求補齊 `since` 之後的事件。
    Hello {
        client: String,
        #[serde(default)]
        since: Option<Seq>,
    },
    /// 要一份完整狀態快照。
    Snapshot,
    Spawn(SpawnRequest),
    /// 送 prompt / 使用者輸入給某個 session。
    Prompt {
        session: SessionId,
        text: String,
    },
    /// 中斷當前回合（不殺程序）。
    Cancel {
        session: SessionId,
    },
    /// 結束 session 並殺掉程序。
    Kill {
        session: SessionId,
    },
    /// 回覆審批請求。
    Permission {
        session: SessionId,
        req: String,
        decision: Decision,
    },
    /// 送訊息到匯流排（agent 之間溝通）。
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
    /// 讀取某個 session 的收件匣（預設只讀未讀並推進游標）。
    BusInbox {
        session: SessionId,
        #[serde(default = "default_true")]
        unread_only: bool,
    },
    /// 阻塞等待下一則進收件匣的訊息（可選篩選發送者或 `reply_to`）。
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
    /// 更新某個 session 的簡短狀態字串（對應 `ade_status`）。
    SessionNote {
        session: SessionId,
        note: String,
    },
    /// 分享產出（檔案、分支或 diff）。
    ArtifactShare {
        session: SessionId,
        #[serde(default)]
        kind: ArtifactKind,
        path: String,
        #[serde(default)]
        note: String,
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
    },
    /// 完整狀態（初次連線或要求 Snapshot 時）。
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
    /// 操作成功（可帶說明）。
    Ok {
        #[serde(default)]
        note: Option<String>,
    },
    Error {
        message: String,
    },
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

/// 把訊息編成一行 JSON（含結尾 `\n`）。
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
            kind: AgentKind::OpenCode,
            cwd: Some("/tmp".into()),
            label: Some("worker".into()),
            role: Role::Worker,
            prompt: Some("hi".into()),
            worktree: false,
            transport: Transport::Auto,
            permission: PermissionPolicy::default(),
            parent: None,
        });
        let line = encode_line(&req).unwrap();
        assert!(line.ends_with('\n'));
        let back: Request = decode_line(line.trim()).unwrap();
        match back {
            Request::Spawn(s) => assert_eq!(s.kind, AgentKind::OpenCode),
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
}
