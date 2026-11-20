//! Shared interactive user question normalization across diverse agents.
//!
//! Different agents emit interactive question requests under different tool names
//! and JSON schemas:
//! - OpenCode: `question` (`opencode.tool.question`)
//! - Antigravity: `ask_question`
//! - Claude Code: `AskUserQuestion` or MCP `ertangle_question`
//! - Codex: `request_user_input` or `item/tool/requestUserInput`
//!
//! This module normalizes them into [`ertangle_core::UserQuestion`].

use ertangle_core::{new_id, QuestionOption, UserQuestion};
use serde_json::Value;

/// Checks if a tool name refers to an interactive user question tool.
pub fn is_question_tool(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let base = lower
        .rsplit_once(':')
        .map(|(_, b)| b)
        .or_else(|| lower.rsplit_once("__").map(|(_, b)| b))
        .unwrap_or(&lower);

    matches!(
        base,
        "question"
            | "ask_question"
            | "askuserquestion"
            | "request_user_input"
            | "ertangle_question"
            | "ertangle_ask_question"
    )
}

/// Parses options from either an array of objects `[{"label": "...", "description": "..."}]`
/// or an array of strings `["Option A", "Option B"]`.
pub fn parse_options(val: &Value) -> Vec<QuestionOption> {
    let Some(arr) = val.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            if let Some(s) = item.as_str() {
                Some(QuestionOption::plain(s))
            } else if let Some(obj) = item.as_object() {
                let label = obj
                    .get("label")
                    .or_else(|| obj.get("value"))
                    .or_else(|| obj.get("name"))
                    .and_then(|v| v.as_str())?;
                let desc = obj.get("description").and_then(|v| v.as_str());
                Some(QuestionOption::new(label, desc))
            } else {
                None
            }
        })
        .collect()
}

/// Parses tool call JSON input into a normalized [`UserQuestion`].
///
/// If input is invalid JSON or unknown format, gracefully falls back to a text question
/// rather than failing or panicking.
pub fn parse_tool_call_question(
    session: &str,
    name: &str,
    input_json: &str,
) -> Option<UserQuestion> {
    if !is_question_tool(name) {
        return None;
    }
    let id = new_id("qst");
    let val: Value = match serde_json::from_str(input_json) {
        Ok(v) => v,
        Err(_) => {
            // Malformed JSON fallback: degrade to text question.
            return Some(UserQuestion {
                id,
                session: session.to_string(),
                header: Some(name.to_string()),
                question: input_json.trim().to_string(),
                options: Vec::new(),
                multi_select: false,
                custom: true,
            });
        }
    };

    // Case 1: questions array `{"questions": [...]}`
    if let Some(questions) = val.get("questions").and_then(|q| q.as_array()) {
        if let Some(first) = questions.first() {
            let question = first
                .get("question")
                .or_else(|| first.get("prompt"))
                .or_else(|| first.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let header = first
                .get("header")
                .or_else(|| first.get("title"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let options = first.get("options").map(parse_options).unwrap_or_default();
            let multi_select = first
                .get("multiple")
                .or_else(|| first.get("multi_select"))
                .or_else(|| first.get("is_multi_select"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let custom = first
                .get("custom")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            return Some(UserQuestion {
                id,
                session: session.to_string(),
                header,
                question: if question.is_empty() {
                    input_json.to_string()
                } else {
                    question
                },
                options,
                multi_select,
                custom,
            });
        }
    }

    // Case 2: Flat single question `{"question": "...", "options": [...]}`
    if let Some(question) = val.get("question").and_then(|v| v.as_str()) {
        let header = val
            .get("header")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let options = val.get("options").map(parse_options).unwrap_or_default();
        let multi_select = val
            .get("multi_select")
            .or_else(|| val.get("multiple"))
            .or_else(|| val.get("is_multi_select"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let custom = val.get("custom").and_then(|v| v.as_bool()).unwrap_or(true);

        return Some(UserQuestion {
            id,
            session: session.to_string(),
            header,
            question: question.to_string(),
            options,
            multi_select,
            custom,
        });
    }

    // Degrade to text question
    Some(UserQuestion {
        id,
        session: session.to_string(),
        header: Some(name.to_string()),
        question: input_json.to_string(),
        options: Vec::new(),
        multi_select: false,
        custom: true,
    })
}

/// Parses an ACP `elicitation/create` request into [`UserQuestion`] and property keys.
pub fn parse_acp_elicitation(session: &str, params: &Value) -> Option<(UserQuestion, Vec<String>)> {
    let id = new_id("qst");
    let message = params
        .get("message")
        .or_else(|| params.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut keys = Vec::new();
    let mut options = Vec::new();
    let mut header = None;
    let mut question_text = message.clone();
    let mut multi_select = false;

    // Check requestedSchema or form fields
    let schema = params.get("requestedSchema").unwrap_or(params);
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        for (k, v) in props {
            keys.push(k.clone());
            if header.is_none() {
                header = v.get("title").and_then(|t| t.as_str()).map(str::to_string);
            }
            if let Some(desc) = v.get("description").and_then(|d| d.as_str()) {
                if question_text.is_empty() {
                    question_text = desc.to_string();
                }
            }
            if v.get("type").and_then(|t| t.as_str()) == Some("array") {
                multi_select = true;
            }
            // Enum or options in schema
            if let Some(enum_vals) = v.get("enum").and_then(|e| e.as_array()) {
                for ev in enum_vals {
                    if let Some(s) = ev.as_str() {
                        options.push(QuestionOption::plain(s));
                    }
                }
            } else if let Some(items) = v
                .get("items")
                .and_then(|i| i.get("enum"))
                .and_then(|e| e.as_array())
            {
                for ev in items {
                    if let Some(s) = ev.as_str() {
                        options.push(QuestionOption::plain(s));
                    }
                }
            } else if let Some(opts) = v.get("options") {
                options.extend(parse_options(opts));
            }
        }
    }

    if let Some(fields) = params.get("fields").and_then(|f| f.as_array()) {
        for f in fields {
            if let Some(k) = f.get("key").and_then(|k| k.as_str()) {
                keys.push(k.to_string());
            }
            if header.is_none() {
                header = f.get("title").and_then(|t| t.as_str()).map(str::to_string);
            }
            if let Some(desc) = f.get("description").and_then(|d| d.as_str()) {
                if question_text.is_empty() {
                    question_text = desc.to_string();
                }
            }
            if f.get("type").and_then(|t| t.as_str()) == Some("multiselect") {
                multi_select = true;
            }
            if let Some(opts) = f.get("options") {
                options.extend(parse_options(opts));
            }
        }
    }

    if question_text.is_empty() {
        question_text = header
            .clone()
            .unwrap_or_else(|| "Interactive Question".to_string());
    }

    let q = UserQuestion {
        id,
        session: session.to_string(),
        header,
        question: question_text,
        options,
        multi_select,
        custom: true,
    };
    Some((q, keys))
}

/// Parses Codex `item/tool/requestUserInput` parameters into [`UserQuestion`] and question key.
pub fn parse_codex_user_input(
    session: &str,
    params: &Value,
) -> Option<(UserQuestion, Option<String>)> {
    let id = new_id("qst");
    let questions = params.get("questions").and_then(|q| q.as_array())?;
    let first = questions.first()?;

    let q_key = first.get("id").and_then(|v| v.as_str()).map(str::to_string);
    let header = first
        .get("header")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let question = first
        .get("question")
        .or_else(|| first.get("prompt"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let options = first.get("options").map(parse_options).unwrap_or_default();
    let multi_select = first
        .get("multiple")
        .or_else(|| first.get("multi_select"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let q = UserQuestion {
        id,
        session: session.to_string(),
        header,
        question,
        options,
        multi_select,
        custom: true,
    };
    Some((q, q_key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_tool_name_recognition() {
        assert!(is_question_tool("question"));
        assert!(is_question_tool("ask_question"));
        assert!(is_question_tool("AskUserQuestion"));
        assert!(is_question_tool("request_user_input"));
        assert!(is_question_tool("mcp__ertangle__ertangle_question"));
        assert!(is_question_tool("server:ask_question"));
        assert!(!is_question_tool("Bash"));
        assert!(!is_question_tool("Edit"));
        assert!(!is_question_tool("read_file"));
    }

    #[test]
    fn parse_opencode_question_format() {
        let json = r#"{
            "questions": [{
                "question": "Which framework would you like to use?",
                "header": "Framework",
                "options": [
                    {"label": "React", "description": "Popular UI library"},
                    {"label": "Vue", "description": "Progressive framework"}
                ],
                "multiple": false,
                "custom": true
            }]
        }"#;
        let q = parse_tool_call_question("ses_1", "question", json).expect("parse opencode");
        assert_eq!(q.header.as_deref(), Some("Framework"));
        assert_eq!(q.question, "Which framework would you like to use?");
        assert_eq!(q.options.len(), 2);
        assert_eq!(q.options[0].label, "React");
        assert_eq!(
            q.options[0].description.as_deref(),
            Some("Popular UI library")
        );
        assert!(!q.multi_select);
        assert!(q.custom);
    }

    #[test]
    fn parse_antigravity_question_format() {
        let json = r#"{
            "questions": [{
                "question": "Select target architectures",
                "options": ["x86_64", "aarch64", "wasm32"],
                "is_multi_select": true
            }]
        }"#;
        let q = parse_tool_call_question("ses_2", "ask_question", json).expect("parse agy");
        assert_eq!(q.question, "Select target architectures");
        assert_eq!(q.options.len(), 3);
        assert_eq!(q.options[1].label, "aarch64");
        assert!(q.multi_select);
    }

    #[test]
    fn parse_codex_app_server_format() {
        let params = serde_json::json!({
            "threadId": "th_1",
            "questions": [{
                "id": "db_choice",
                "header": "Database Selection",
                "question": "Select your primary database",
                "options": [
                    {"label": "Postgres", "description": "Relational"},
                    {"label": "Redis", "description": "Key-value"}
                ]
            }]
        });
        let (q, key) = parse_codex_user_input("ses_3", &params).expect("parse codex");
        assert_eq!(key.as_deref(), Some("db_choice"));
        assert_eq!(q.header.as_deref(), Some("Database Selection"));
        assert_eq!(q.options[0].label, "Postgres");
    }

    #[test]
    fn parse_acp_elicitation_format() {
        let params = serde_json::json!({
            "mode": "form",
            "message": "Deployment Options",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "env": {
                        "type": "string",
                        "title": "Environment",
                        "description": "Target environment",
                        "enum": ["production", "staging", "dev"]
                    }
                }
            }
        });
        let (q, keys) = parse_acp_elicitation("ses_4", &params).expect("parse acp");
        assert_eq!(keys, vec!["env"]);
        assert_eq!(q.header.as_deref(), Some("Environment"));
        assert_eq!(q.options.len(), 3);
        assert_eq!(q.options[0].label, "production");
    }

    #[test]
    fn malformed_input_graceful_fallback() {
        let q = parse_tool_call_question("ses_5", "question", "not a valid json { broken")
            .expect("fallback");
        assert_eq!(q.question, "not a valid json { broken");
        assert!(q.options.is_empty());
        assert!(q.custom);
    }
}
