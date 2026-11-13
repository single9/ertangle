//! `termade mcp` — 內建的 MCP（Model Context Protocol）stdio server。
//!
//! 每個由 daemon 啟動的 agent 都會自動注入這個 MCP server（`termade mcp --session <id>`），
//! 讓 agent 直接用原生工具呼叫與其他 agent 溝通、查任務看板、回報狀態，甚至自行
//! `termade_spawn` 出子 agent。
//!
//! 協定：JSON-RPC 2.0 over stdio（newline-delimited JSON）。

use crate::Client;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use termade_core::{
    AgentKind, ArtifactKind, MsgKind, PermissionPolicy, Role, SessionId, Target, TaskStatus,
};
use termade_protocol::{Frame, Request, SpawnRequest, Transport};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 啟動 stdio MCP server。
pub fn run(session_override: Option<String>) -> Result<()> {
    let session = session_override
        .or_else(|| std::env::var("TERMADE_SESSION").ok())
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
                    "name": "termade",
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
                    "content": [{ "type": "text", "text": format!("Error: {e}") }],
                    "isError": true
                })),
            }
        }
        other => Err((-32601, format!("Unsupported MCP method: {other}"))),
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
            "name": "termade_whoami",
            "description": "Return your own session info (id, label, role, cwd, branch, status).",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "termade_peers",
            "description": "List all agent sessions in the environment (id, label, role, status, cwd, branch, status_note).",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "termade_send",
            "description": "Send a message to another session (by id or label), a channel (#channel), or broadcast to everyone (*). Idle or sleeping recipients are woken automatically; status and artifact messages only land in the inbox without waking them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "Recipient: session id (e.g. ses_...), label (e.g. worker/impl), #channel, or *"
                    },
                    "body": {
                        "type": "string",
                        "description": "Message body"
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["chat", "handoff", "question", "answer", "status", "artifact"],
                        "description": "Message kind (default: chat)"
                    },
                    "reply_to": {
                        "type": "string",
                        "description": "Optional: id of the message being replied to (msg_...)"
                    }
                },
                "required": ["to", "body"]
            }
        }),
        json!({
            "name": "termade_inbox",
            "description": "Read messages addressed to you (by default only unread ones, advancing the read cursor).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "unread_only": {
                        "type": "boolean",
                        "description": "Only fetch unread messages (default: true)"
                    }
                }
            }
        }),
        json!({
            "name": "termade_wait",
            "description": "Block until the next message addressed to you arrives (bounded by a timeout, never blocks forever).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Timeout in milliseconds (default 30000, max 300000)"
                    },
                    "from": {
                        "type": "string",
                        "description": "Optional: only wait for messages from this peer (id or label)"
                    }
                }
            }
        }),
        json!({
            "name": "termade_ask",
            "description": "Ask a peer a question and block until it replies (request/response handshake). If the peer is idle, the daemon wakes it automatically.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "peer": {
                        "type": "string",
                        "description": "Session id or label of the peer"
                    },
                    "question": {
                        "type": "string",
                        "description": "Question to ask"
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Timeout for the reply in milliseconds (default 60000)"
                    }
                },
                "required": ["peer", "question"]
            }
        }),
        json!({
            "name": "termade_task_list",
            "description": "List every task on the shared task board. Each task carries `depends_on` (prerequisite task ids) and `waiting_on` (the prerequisites that are not done yet, with title, status and assignee); a `blocked` task with a non-empty `waiting_on` is unblocked automatically when they finish.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "termade_task_create",
            "description": "Create a task on the shared task board. When an assignee is given, a handoff message is delivered and an idle assignee is woken. With `depends_on`, a task whose prerequisites are not all done is created as `blocked` and the assignee is NOT notified yet; once every prerequisite is done it turns `todo` and the assignee is notified and woken automatically, so you do not need to wait and re-send.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Task title"
                    },
                    "detail": {
                        "type": "string",
                        "description": "Task details"
                    },
                    "assignee": {
                        "type": "string",
                        "description": "Assignee (session id or label)"
                    },
                    "depends_on": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Ids of prerequisite tasks (must exist). The task stays blocked until all of them are done."
                    }
                },
                "required": ["title"]
            }
        }),
        json!({
            "name": "termade_task_update",
            "description": "Update a task's status, assignee, or details. Reassigning wakes the new assignee. Setting a task to `done` unblocks the tasks that depend on it and notifies their assignees.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Task id (task_...)"
                    },
                    "status": {
                        "type": "string",
                        "enum": ["todo", "doing", "blocked", "review", "done"],
                        "description": "New task status"
                    },
                    "assignee": {
                        "type": "string",
                        "description": "New assignee"
                    },
                    "detail": {
                        "type": "string",
                        "description": "New task details"
                    }
                },
                "required": ["id"]
            }
        }),
        json!({
            "name": "termade_spawn",
            "description": "Spawn a new child agent session (e.g. a coordinator spawning a worker into its own git worktree to implement in parallel).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "description": "Agent kind: opencode | claude | codex | antigravity (default: opencode)"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Task prompt to run immediately after startup"
                    },
                    "label": {
                        "type": "string",
                        "description": "Human-readable label, e.g. worker/impl"
                    },
                    "role": {
                        "type": "string",
                        "description": "Role: coordinator | worker | reviewer (default: worker)"
                    },
                    "worktree": {
                        "type": "boolean",
                        "description": "Create a dedicated git worktree (default: false)"
                    },
                    "permission": {
                        "type": "string",
                        "enum": ["ask", "auto", "bypass"],
                        "description": "Permission policy (default from config.toml; never higher than the parent's)"
                    },
                    "effort": {
                        "type": "string",
                        "enum": ["low", "medium", "high", "xhigh", "max"],
                        "description": "Reasoning effort for the child agent (supported by claude, codex; others may reject it)"
                    },
                    "model": {
                        "type": "string",
                        "description": "Model for the child agent (e.g. a cheaper model for workers). Defaults to config.toml"
                    }
                },
                "required": ["prompt"]
            }
        }),
        json!({
            "name": "termade_artifact_share",
            "description": "Share an artifact (file path, git branch, or diff) on the bus and task board.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path, branch name, or diff summary"
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["file", "branch", "diff"],
                        "description": "Artifact kind (default: file)"
                    },
                    "note": {
                        "type": "string",
                        "description": "Additional note"
                    }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "termade_approve",
            "description": "Internal: permission prompt tool used by Claude Code (--permission-prompt-tool). Forwards a tool-use approval request to termade and returns the decision.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tool_name": { "type": "string" },
                    "input": { "type": "object" },
                    "tool_use_id": { "type": "string" }
                },
                "required": ["tool_name"]
            }
        }),
        json!({
            "name": "termade_status",
            "description": "Update your short progress status shown in the TUI.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Short progress text, e.g. \"running unit tests\""
                    }
                },
                "required": ["text"]
            }
        }),
        json!({
            "name": "termade_diff",
            "description": "Get the git diff of a session's workspace (for a coordinator or reviewer to inspect changes).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": {
                        "type": "string",
                        "description": "Target session id or label (defaults to yourself)"
                    }
                }
            }
        }),
        json!({
            "name": "termade_merge",
            "description": "Merge a worker session's worktree branch back into the main workspace.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": {
                        "type": "string",
                        "description": "Worker session id or label to merge"
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": "If true, only report files to commit and fast-forwardability without modifying the workspace"
                    }
                },
                "required": ["session"]
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

/// Claude `--permission-prompt-tool` 要求的回應格式（以 JSON 字串回傳）。
fn approval_response(allowed: bool, input: &Value) -> Value {
    let v = if allowed {
        json!({ "behavior": "allow", "updatedInput": input })
    } else {
        json!({ "behavior": "deny", "message": "Denied by termade policy or user" })
    };
    Value::String(v.to_string())
}

/// 執行單一 MCP 工具呼叫。
pub fn call_tool(session: &str, name: &str, args: &Value) -> Result<Value> {
    match name {
        "termade_whoami" => {
            let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
                bail!("failed to fetch state snapshot");
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

        "termade_peers" => {
            let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
                bail!("failed to fetch state snapshot");
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

        "termade_send" => {
            let to_str = args["to"]
                .as_str()
                .context("missing required parameter `to`")?;
            let body = args["body"]
                .as_str()
                .context("missing required parameter `body`")?
                .to_string();
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
            let id = note
                .split_whitespace()
                .find(|t| t.starts_with("task_"))
                .unwrap_or(&note);
            Ok(json!({ "ok": true, "id": id, "note": note }))
        }

        "termade_inbox" => {
            let unread_only = args["unread_only"].as_bool().unwrap_or(true);
            match roundtrip(&Request::BusInbox {
                session: session.to_string(),
                unread_only,
            })? {
                Frame::Messages { messages, .. } => Ok(json!({ "messages": messages })),
                other => bail!("unexpected response: {other:?}"),
            }
        }

        "termade_wait" => {
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
                other => bail!("unexpected response: {other:?}"),
            }
        }

        "termade_ask" => {
            let peer = args["peer"]
                .as_str()
                .context("missing required parameter `peer`")?;
            let question = args["question"]
                .as_str()
                .context("missing required parameter `question`")?
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
                Frame::Ok { note: Some(n) } => n
                    .rsplit_once(' ')
                    .map(|(_, id)| id.to_string())
                    .unwrap_or(n),
                _ => String::new(),
            };

            match roundtrip(&Request::BusWait {
                session: session.to_string(),
                from: Some(peer.to_string()),
                reply_to: if qid.is_empty() {
                    None
                } else {
                    Some(qid.clone())
                },
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
                other => bail!("unexpected response: {other:?}"),
            }
        }

        "termade_task_list" => {
            let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
                bail!("failed to fetch state snapshot");
            };
            Ok(task_list_json(&state))
        }

        "termade_task_create" => {
            let title = args["title"]
                .as_str()
                .context("missing required parameter `title`")?
                .to_string();
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
            let id = note
                .split_whitespace()
                .find(|t| t.starts_with("task_"))
                .unwrap_or(&note);
            Ok(json!({ "ok": true, "id": id, "note": note }))
        }

        "termade_task_update" => {
            let id = args["id"]
                .as_str()
                .context("missing required parameter `id`")?
                .to_string();
            let status = match args.get("status").and_then(|v| v.as_str()) {
                Some(s) => Some(TaskStatus::parse(s).with_context(|| {
                    format!("unknown task status: {s} (todo/doing/blocked/review/done)")
                })?),
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

        "termade_spawn" => {
            let kind_str = args["kind"].as_str().unwrap_or("opencode");
            let kind = AgentKind::parse(kind_str)
                .with_context(|| format!("unknown agent kind: {kind_str}"))?;
            let prompt = args["prompt"]
                .as_str()
                .context("missing required parameter `prompt`")?
                .to_string();
            let label = args["label"].as_str().map(|s| s.to_string());
            let role = args["role"]
                .as_str()
                .map(Role::parse)
                .unwrap_or(Role::Worker);
            let worktree = args["worktree"].as_bool().unwrap_or(false);
            let permission = args["permission"]
                .as_str()
                .and_then(PermissionPolicy::parse);

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
                model: args["model"].as_str().map(|s| s.to_string()),
                effort: args["effort"].as_str().map(|s| s.to_string()),
                attachments: vec![],
                inherit_from: None,
                args: vec![],
            }))?;
            let note = match frame {
                Frame::Ok { note } => note.unwrap_or_default(),
                _ => String::new(),
            };
            let id = note
                .split_whitespace()
                .find(|t| t.starts_with("task_"))
                .unwrap_or(&note);
            Ok(json!({ "ok": true, "id": id, "note": note }))
        }

        "termade_artifact_share" => {
            let path = args["path"]
                .as_str()
                .context("missing required parameter `path`")?
                .to_string();
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

        "termade_approve" => {
            let tool = args["tool_name"].as_str().unwrap_or("tool").to_string();
            let input = args["input"].clone();
            let frame = roundtrip(&Request::ApprovalAsk {
                session: session.to_string(),
                tool,
                input: input.to_string(),
            })?;
            let allowed = matches!(&frame, Frame::Ok { note: Some(n) } if n == "allow");
            Ok(approval_response(allowed, &input))
        }

        "termade_status" => {
            let text = args["text"]
                .as_str()
                .context("missing required parameter `text`")?
                .to_string();
            roundtrip(&Request::SessionNote {
                session: session.to_string(),
                note: text,
            })?;
            Ok(json!({ "ok": true }))
        }

        "termade_diff" => {
            let target = args["session"].as_str().unwrap_or(session).to_string();
            match roundtrip(&Request::WorktreeDiff { session: target })? {
                Frame::Diff {
                    session,
                    branch,
                    cwd,
                    diff,
                } => Ok(json!({
                    "session": session,
                    "branch": branch,
                    "cwd": cwd,
                    "diff": termade_core::redact_secrets(&diff),
                })),
                other => bail!("unexpected response: {other:?}"),
            }
        }

        "termade_merge" => {
            let target = args["session"]
                .as_str()
                .context("missing required parameter `session`")?
                .to_string();
            let dry_run = args["dry_run"].as_bool().unwrap_or(false);
            match roundtrip(&Request::WorktreeMerge {
                session: target,
                dry_run,
            })? {
                Frame::Ok { note } => Ok(json!({ "ok": true, "note": note })),
                other => bail!("unexpected response: {other:?}"),
            }
        }

        other => bail!("unknown MCP tool: {other}"),
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
        assert_eq!(init["result"]["serverInfo"]["name"], "termade");

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
            "termade_whoami",
            "termade_peers",
            "termade_send",
            "termade_inbox",
            "termade_wait",
            "termade_ask",
            "termade_task_list",
            "termade_task_create",
            "termade_task_update",
            "termade_spawn",
            "termade_artifact_share",
            "termade_status",
            "termade_approve",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
    }
}

/// `termade_task_list` 的回傳：每個任務加上 `waiting_on`（還沒完成的前置任務），
/// 讓 agent 看得出任務為什麼被擋住、該去問誰。
fn task_list_json(state: &termade_core::State) -> Value {
    let tasks: Vec<Value> = state
        .tasks
        .values()
        .map(|t| {
            let mut v = serde_json::to_value(t).unwrap_or_else(|_| json!({}));
            let waiting: Vec<Value> = state
                .unfinished_deps(t)
                .iter()
                .map(|d| {
                    json!({
                        "id": d.id,
                        "title": d.title,
                        "status": d.status.id(),
                        "assignee": d.assignee,
                    })
                })
                .collect();
            v["waiting_on"] = Value::Array(waiting);
            v
        })
        .collect();
    json!({ "tasks": tasks })
}

#[cfg(test)]
mod task_list_tests {
    use super::*;
    use termade_core::{State, Task};

    fn task(id: &str, status: TaskStatus, deps: &[&str]) -> Task {
        Task {
            id: id.into(),
            title: format!("title-{id}"),
            detail: String::new(),
            status,
            assignee: None,
            created_by: None,
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
            updated_ms: 0,
        }
    }

    #[test]
    fn task_list_reports_dependencies_and_unfinished_ones() {
        let mut state = State::default();
        for t in [
            task("task_a", TaskStatus::Done, &[]),
            task("task_b", TaskStatus::Doing, &[]),
            task("task_c", TaskStatus::Blocked, &["task_a", "task_b"]),
        ] {
            state.tasks.insert(t.id.clone(), t);
        }
        let out = task_list_json(&state);
        let c = out["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == "task_c")
            .unwrap();
        assert_eq!(c["depends_on"], json!(["task_a", "task_b"]));
        let waiting = c["waiting_on"].as_array().unwrap();
        assert_eq!(waiting.len(), 1, "只列還沒完成的");
        assert_eq!(waiting[0]["id"], "task_b");
        assert_eq!(waiting[0]["status"], "doing");
        let a = out["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == "task_a")
            .unwrap();
        assert_eq!(a["waiting_on"], json!([]));
    }
}

#[cfg(test)]
mod approval_tests {
    use super::*;

    #[test]
    fn approval_response_matches_claude_format() {
        let input = json!({"command": "ls"});
        let allow: Value =
            serde_json::from_str(approval_response(true, &input).as_str().unwrap()).unwrap();
        assert_eq!(allow["behavior"], "allow");
        assert_eq!(allow["updatedInput"], input);
        let deny: Value =
            serde_json::from_str(approval_response(false, &input).as_str().unwrap()).unwrap();
        assert_eq!(deny["behavior"], "deny");
        assert!(deny["message"].is_string());
    }
}
