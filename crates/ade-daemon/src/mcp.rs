//! `ade mcp` — 內建的 MCP（Model Context Protocol）stdio server。
//!
//! 每個由 daemon 啟動的 agent 都會自動注入這個 MCP server（`ade mcp --session <id>`），
//! 讓 agent 直接用原生工具呼叫與其他 agent 溝通、查任務看板、回報狀態，甚至自行
//! `ade_spawn` 出子 agent。
//!
//! 協定：JSON-RPC 2.0 over stdio（newline-delimited JSON）。

use crate::Client;
use ade_core::{
    AgentKind, ArtifactKind, MsgKind, PermissionPolicy, Role, SessionId, Target, TaskStatus,
};
use ade_protocol::{Frame, Request, SpawnRequest, Transport};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 啟動 stdio MCP server。
pub fn run(session_override: Option<String>) -> Result<()> {
    let session = session_override
        .or_else(|| std::env::var("ADE_SESSION").ok())
        .unwrap_or_else(|| "human".to_string());

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("Content-Length:") {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(resp) = handle_rpc(&session, &msg) {
            let mut out = serde_json::to_string(&resp)?;
            out.push('\n');
            stdout.write_all(out.as_bytes())?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// 處理單一 JSON-RPC 訊息；若是 notification（無 `id`）則回傳 `None`。
pub fn handle_rpc(session: &str, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or("");
    let params = &msg["params"];

    // Notification（例如 `notifications/initialized`）不需回覆。
    let id = id?;

    let result = match method {
        "initialize" => {
            let client_ver = params["protocolVersion"]
                .as_str()
                .unwrap_or(MCP_PROTOCOL_VERSION);
            Ok(json!({
                "protocolVersion": client_ver,
                "capabilities": {
                    "tools": { "listChanged": false }
                },
                "serverInfo": {
                    "name": "ade",
                    "version": VERSION
                }
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            let args = &params["arguments"];
            match call_tool(session, name, args) {
                Ok(val) => {
                    let text = if let Some(s) = val.as_str() {
                        s.to_string()
                    } else {
                        serde_json::to_string_pretty(&val).unwrap_or_else(|_| val.to_string())
                    };
                    Ok(json!({
                        "content": [{ "type": "text", "text": text }],
                        "isError": false
                    }))
                }
                Err(e) => Ok(json!({
                    "content": [{ "type": "text", "text": format!("錯誤：{e}") }],
                    "isError": true
                })),
            }
        }
        other => Err((
            -32601,
            format!("不支援的 MCP 方法：{other}"),
        )),
    };

    Some(match result {
        Ok(res) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": res
        }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        }),
    })
}

/// 曝露給 agent 的協作工具清單。
pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ade_whoami",
            "description": "查詢自己目前的 session 資訊（id、label、role、cwd、branch、status）。",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "ade_peers",
            "description": "列出環境中所有 agent sessions（含 id、label、role、status、cwd、branch、status_note）。",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "ade_send",
            "description": "傳送訊息給另一個 session（可用 id 或 label）、頻道（#channel）或全體廣播（*）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "收件目標：session id（如 ses_...）、label（如 worker/impl）、#channel 或 *"
                    },
                    "body": {
                        "type": "string",
                        "description": "訊息內容"
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["chat", "handoff", "question", "answer", "status", "artifact"],
                        "description": "訊息種類（預設 chat）"
                    },
                    "reply_to": {
                        "type": "string",
                        "description": "選填：回覆的訊息 id（msg_...）"
                    }
                },
                "required": ["to", "body"]
            }
        }),
        json!({
            "name": "ade_inbox",
            "description": "讀取寄給自己的訊息（預設只取尚未讀取的訊息並推進已讀游標）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "unread_only": {
                        "type": "boolean",
                        "description": "是否只取未讀訊息（預設 true）"
                    }
                }
            }
        }),
        json!({
            "name": "ade_wait",
            "description": "阻塞等待下一則寄給自己的訊息（有 timeout 上限，不會永久卡住）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "timeout_ms": {
                        "type": "integer",
                        "description": "等待毫秒數（預設 30000，最大 300000）"
                    },
                    "from": {
                        "type": "string",
                        "description": "選填：只等待來自特定 peer（id 或 label）的訊息"
                    }
                }
            }
        }),
        json!({
            "name": "ade_ask",
            "description": "向某個 peer 送出問題並阻塞等待其回覆（request/response 協作交握）。若對方處於閒置，daemon 會自動喚醒對方處理問題。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "peer": {
                        "type": "string",
                        "description": "對象的 session id 或 label"
                    },
                    "question": {
                        "type": "string",
                        "description": "要詢問的問題內容"
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "等待回覆的毫秒數（預設 60000）"
                    }
                },
                "required": ["peer", "question"]
            }
        }),
        json!({
            "name": "ade_task_list",
            "description": "列出共用任務看板上的所有任務。",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "ade_task_create",
            "description": "在共用任務看板建立新任務。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "任務標題"
                    },
                    "detail": {
                        "type": "string",
                        "description": "任務細節說明"
                    },
                    "assignee": {
                        "type": "string",
                        "description": "指派對象（session id 或 label）"
                    },
                    "depends_on": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "前置任務 id 清單"
                    }
                },
                "required": ["title"]
            }
        }),
        json!({
            "name": "ade_task_update",
            "description": "更新共用任務看板上的任務狀態、負責人或說明。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "任務 id（task_...）"
                    },
                    "status": {
                        "type": "string",
                        "enum": ["todo", "doing", "blocked", "review", "done"],
                        "description": "更新後的任務狀態"
                    },
                    "assignee": {
                        "type": "string",
                        "description": "更新指派對象"
                    },
                    "detail": {
                        "type": "string",
                        "description": "更新任務細節"
                    }
                },
                "required": ["id"]
            }
        }),
        json!({
            "name": "ade_spawn",
            "description": "產生一個新的子 agent session（例如 coordinator 派生 worker 到獨立 git worktree 平行實作）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "description": "agent 種類：opencode | claude | codex | antigravity（預設 opencode）"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "啟動後立即執行的任務提示"
                    },
                    "label": {
                        "type": "string",
                        "description": "人類可讀標籤，例如 worker/impl"
                    },
                    "role": {
                        "type": "string",
                        "description": "角色：coordinator | worker | reviewer（預設 worker）"
                    },
                    "worktree": {
                        "type": "boolean",
                        "description": "是否建立獨立 git worktree（預設 false）"
                    },
                    "permission": {
                        "type": "string",
                        "enum": ["ask", "auto", "bypass"],
                        "description": "權限政策（預設 auto）"
                    }
                },
                "required": ["prompt"]
            }
        }),
        json!({
            "name": "ade_artifact_share",
            "description": "分享產出物（檔案路徑、git 分支或 diff）到匯流排與看板。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "檔案路徑、分支名稱或 diff 摘要"
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["file", "branch", "diff"],
                        "description": "產出物類型（預設 file）"
                    },
                    "note": {
                        "type": "string",
                        "description": "補充說明"
                    }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "ade_status",
            "description": "更新自己在 TUI 上的簡短進度狀態文字。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "簡短進度文字，例如「正在跑單元測試」"
                    }
                },
                "required": ["text"]
            }
        }),
    ]
}

fn roundtrip(req: &Request) -> Result<Frame> {
    let mut c = Client::connect()?;
    c.send(req)?;
    loop {
        match c.recv()? {
            Frame::Ready { .. } => continue,
            Frame::Error { message } => bail!("{message}"),
            other => return Ok(other),
        }
    }
}

/// 執行單一 MCP 工具呼叫。
pub fn call_tool(session: &str, name: &str, args: &Value) -> Result<Value> {
    match name {
        "ade_whoami" => {
            let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
                bail!("無法取得狀態快照");
            };
            let resolved: SessionId = state
                .resolve_session_id(session)
                .unwrap_or_else(|| session.to_string());
            if let Some(s) = state.sessions.get(&resolved) {
                Ok(serde_json::to_value(s)?)
            } else {
                Ok(json!({ "id": session, "role": "human" }))
            }
        }

        "ade_peers" => {
            let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
                bail!("無法取得狀態快照");
            };
            let peers: Vec<Value> = state
                .sessions_sorted()
                .into_iter()
                .map(|s| {
                    json!({
                        "id": s.id,
                        "label": s.label,
                        "kind": s.kind.id(),
                        "role": s.role.id(),
                        "status": s.status,
                        "status_note": s.status_note,
                        "cwd": s.cwd,
                        "branch": s.branch,
                        "parent": s.parent,
                    })
                })
                .collect();
            Ok(json!({ "self": session, "peers": peers }))
        }

        "ade_send" => {
            let to_str = args["to"].as_str().context("缺少 `to` 參數")?;
            let body = args["body"].as_str().context("缺少 `body` 參數")?.to_string();
            let kind = args["kind"]
                .as_str()
                .and_then(MsgKind::parse)
                .unwrap_or(MsgKind::Chat);
            let reply_to = args["reply_to"].as_str().map(|s| s.to_string());
            let frame = roundtrip(&Request::BusSend {
                from: Some(session.to_string()),
                to: Target::parse(to_str),
                kind,
                body,
                reply_to,
            })?;
            let note = match frame {
                Frame::Ok { note } => note.unwrap_or_default(),
                _ => String::new(),
            };
            let id = note.rsplit_once(' ').map(|(_, id)| id).unwrap_or(&note);
            Ok(json!({ "ok": true, "id": id, "note": note }))
        }

        "ade_inbox" => {
            let unread_only = args["unread_only"].as_bool().unwrap_or(true);
            match roundtrip(&Request::BusInbox {
                session: session.to_string(),
                unread_only,
            })? {
                Frame::Messages { messages, .. } => Ok(json!({ "messages": messages })),
                other => bail!("未預期的回應：{other:?}"),
            }
        }

        "ade_wait" => {
            let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(30_000);
            let from = args["from"].as_str().map(|s| s.to_string());
            match roundtrip(&Request::BusWait {
                session: session.to_string(),
                from,
                reply_to: None,
                timeout_ms,
            })? {
                Frame::Messages {
                    messages,
                    timed_out,
                } => Ok(json!({ "messages": messages, "timed_out": timed_out })),
                other => bail!("未預期的回應：{other:?}"),
            }
        }

        "ade_ask" => {
            let peer = args["peer"].as_str().context("缺少 `peer` 參數")?;
            let question = args["question"]
                .as_str()
                .context("缺少 `question` 參數")?
                .to_string();
            let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(60_000);

            let send_frame = roundtrip(&Request::BusSend {
                from: Some(session.to_string()),
                to: Target::Direct(peer.to_string()),
                kind: MsgKind::Question,
                body: question,
                reply_to: None,
            })?;
            let qid = match send_frame {
                Frame::Ok { note: Some(n) } => {
                    n.rsplit_once(' ').map(|(_, id)| id.to_string()).unwrap_or(n)
                }
                _ => String::new(),
            };

            match roundtrip(&Request::BusWait {
                session: session.to_string(),
                from: Some(peer.to_string()),
                reply_to: if qid.is_empty() { None } else { Some(qid.clone()) },
                timeout_ms,
            })? {
                Frame::Messages {
                    mut messages,
                    timed_out,
                } => Ok(json!({
                    "question_id": qid,
                    "reply": messages.pop(),
                    "timed_out": timed_out
                })),
                other => bail!("未預期的回應：{other:?}"),
            }
        }

        "ade_task_list" => {
            let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
                bail!("無法取得狀態快照");
            };
            let tasks: Vec<_> = state.tasks.values().cloned().collect();
            Ok(json!({ "tasks": tasks }))
        }

        "ade_task_create" => {
            let title = args["title"].as_str().context("缺少 `title` 參數")?.to_string();
            let detail = args["detail"].as_str().unwrap_or("").to_string();
            let assignee = args["assignee"].as_str().map(|s| s.to_string());
            let depends_on = args["depends_on"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let frame = roundtrip(&Request::TaskCreate {
                title,
                detail,
                assignee,
                created_by: Some(session.to_string()),
                depends_on,
            })?;
            let note = match frame {
                Frame::Ok { note } => note.unwrap_or_default(),
                _ => String::new(),
            };
            let id = note.rsplit_once(' ').map(|(_, id)| id).unwrap_or(&note);
            Ok(json!({ "ok": true, "id": id, "note": note }))
        }

        "ade_task_update" => {
            let id = args["id"].as_str().context("缺少 `id` 參數")?.to_string();
            let status = match args.get("status").and_then(|v| v.as_str()) {
                Some(s) => Some(
                    TaskStatus::parse(s)
                        .with_context(|| format!("未知的任務狀態：{s}（todo/doing/blocked/review/done）"))?,
                ),
                None => None,
            };
            let assignee = args["assignee"].as_str().map(|s| s.to_string());
            let detail = args["detail"].as_str().map(|s| s.to_string());

            roundtrip(&Request::TaskUpdate {
                id: id.clone(),
                status,
                assignee,
                detail,
            })?;
            Ok(json!({ "ok": true, "id": id }))
        }

        "ade_spawn" => {
            let kind_str = args["kind"].as_str().unwrap_or("opencode");
            let kind = AgentKind::parse(kind_str)
                .with_context(|| format!("未知的 agent：{kind_str}"))?;
            let prompt = args["prompt"]
                .as_str()
                .context("缺少 `prompt` 參數")?
                .to_string();
            let label = args["label"].as_str().map(|s| s.to_string());
            let role = args["role"]
                .as_str()
                .map(Role::parse)
                .unwrap_or(Role::Worker);
            let worktree = args["worktree"].as_bool().unwrap_or(false);
            let permission = args["permission"]
                .as_str()
                .and_then(PermissionPolicy::parse)
                .unwrap_or(PermissionPolicy::Auto);

            let frame = roundtrip(&Request::Spawn(SpawnRequest {
                kind,
                cwd: None,
                label,
                role,
                prompt: Some(prompt),
                worktree,
                transport: Transport::Auto,
                permission,
                parent: Some(session.to_string()),
            }))?;
            let note = match frame {
                Frame::Ok { note } => note.unwrap_or_default(),
                _ => String::new(),
            };
            let id = note.rsplit_once(' ').map(|(_, id)| id).unwrap_or(&note);
            Ok(json!({ "ok": true, "id": id, "note": note }))
        }

        "ade_artifact_share" => {
            let path = args["path"].as_str().context("缺少 `path` 參數")?.to_string();
            let kind = args["kind"]
                .as_str()
                .and_then(ArtifactKind::parse)
                .unwrap_or(ArtifactKind::File);
            let note = args["note"].as_str().unwrap_or("").to_string();
            let frame = roundtrip(&Request::ArtifactShare {
                session: session.to_string(),
                kind,
                path,
                note,
            })?;
            let msg = match frame {
                Frame::Ok { note } => note.unwrap_or_default(),
                _ => String::new(),
            };
            Ok(json!({ "ok": true, "note": msg }))
        }

        "ade_status" => {
            let text = args["text"].as_str().context("缺少 `text` 參數")?.to_string();
            roundtrip(&Request::SessionNote {
                session: session.to_string(),
                note: text,
            })?;
            Ok(json!({ "ok": true }))
        }

        other => bail!("未知的 MCP 工具：{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_initialize_and_tools_list() {
        let init = handle_rpc(
            "ses_1",
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": "2024-11-05" }
            }),
        )
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "ade");

        // Notification 不該產生回應。
        let notif = handle_rpc(
            "ses_1",
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
        );
        assert!(notif.is_none());

        let list = handle_rpc(
            "ses_1",
            &json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list"
            }),
        )
        .unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in [
            "ade_whoami",
            "ade_peers",
            "ade_send",
            "ade_inbox",
            "ade_wait",
            "ade_ask",
            "ade_task_list",
            "ade_task_create",
            "ade_task_update",
            "ade_spawn",
            "ade_artifact_share",
            "ade_status",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
    }
}
