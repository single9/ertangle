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

/// Parses tool call JSON input into a list of normalized [`UserQuestion`]s.
///
/// If input is invalid JSON or unknown format, gracefully falls back to a text question
/// rather than failing or panicking.
pub fn parse_tool_call_questions(session: &str, name: &str, input_json: &str) -> Vec<UserQuestion> {
    if !is_question_tool(name) {
        return Vec::new();
    }
    let val: Value = match serde_json::from_str(input_json) {
        Ok(v) => v,
        Err(_) => {
            // Malformed JSON fallback: degrade to text question.
            return vec![UserQuestion {
                id: new_id("qst"),
                session: session.to_string(),
                header: Some(name.to_string()),
                question: input_json.trim().to_string(),
                options: Vec::new(),
                multi_select: false,
                custom: true,
                group: None,
            }];
        }
    };

    // Case 1: questions array `{"questions": [...]}`
    if let Some(questions) = val.get("questions").and_then(|q| q.as_array()) {
        let group = (questions.len() > 1).then(|| new_id("qgrp"));
        let mut list = Vec::new();
        for (i, item) in questions.iter().enumerate() {
            let question = item
                .get("question")
                .or_else(|| item.get("prompt"))
                .or_else(|| item.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let header = item
                .get("header")
                .or_else(|| item.get("title"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let options = item.get("options").map(parse_options).unwrap_or_default();
            let multi_select = item
                .get("multiple")
                .or_else(|| item.get("multi_select"))
                .or_else(|| item.get("multiSelect"))
                .or_else(|| item.get("is_multi_select"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let custom = item.get("custom").and_then(|v| v.as_bool()).unwrap_or(true);

            list.push(UserQuestion {
                id: new_id("qst"),
                session: session.to_string(),
                header,
                question: if question.is_empty() {
                    if questions.len() > 1 {
                        format!("Question {}", i + 1)
                    } else {
                        input_json.to_string()
                    }
                } else {
                    question
                },
                options,
                multi_select,
                custom,
                group: group.clone(),
            });
        }
        if !list.is_empty() {
            return list;
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
            .or_else(|| val.get("multiSelect"))
            .or_else(|| val.get("multiple"))
            .or_else(|| val.get("is_multi_select"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let custom = val.get("custom").and_then(|v| v.as_bool()).unwrap_or(true);

        return vec![UserQuestion {
            id: new_id("qst"),
            session: session.to_string(),
            header,
            question: question.to_string(),
            options,
            multi_select,
            custom,
            group: None,
        }];
    }

    // Degrade to text question
    vec![UserQuestion {
        id: new_id("qst"),
        session: session.to_string(),
        header: Some(name.to_string()),
        question: input_json.to_string(),
        options: Vec::new(),
        multi_select: false,
        custom: true,
        group: None,
    }]
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
    parse_tool_call_questions(session, name, input_json)
        .into_iter()
        .next()
}

/// Parses an ACP `elicitation/create` request into multiple [`UserQuestion`]s with property keys.
pub fn parse_acp_elicitations(session: &str, params: &Value) -> Vec<(UserQuestion, String)> {
    let message = params
        .get("message")
        .or_else(|| params.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut out = Vec::new();
    let schema = params.get("requestedSchema").unwrap_or(params);
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        let group = (props.len() > 1).then(|| new_id("qgrp"));
        for (k, v) in props {
            let header = v.get("title").and_then(|t| t.as_str()).map(str::to_string);
            let mut desc = v
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            if desc.is_empty() {
                desc = if !message.is_empty() {
                    message.clone()
                } else {
                    header.clone().unwrap_or_else(|| k.clone())
                };
            }
            let multi_select = v.get("type").and_then(|t| t.as_str()) == Some("array");
            let mut options = Vec::new();
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
            } else if let Some(variants) = v
                .get("oneOf")
                .or_else(|| v.get("anyOf"))
                .and_then(|value| value.as_array())
            {
                for variant in variants {
                    let label = variant
                        .get("title")
                        .or_else(|| variant.get("const"))
                        .and_then(|value| value.as_str());
                    if let Some(label) = label {
                        let description = variant
                            .get("description")
                            .and_then(|value| value.as_str())
                            .map(str::to_string);
                        options.push(QuestionOption::new(label, description.as_deref()));
                    }
                }
            } else if let Some(opts) = v.get("options") {
                options.extend(parse_options(opts));
            }

            out.push((
                UserQuestion {
                    id: new_id("qst"),
                    session: session.to_string(),
                    header,
                    question: desc,
                    options,
                    multi_select,
                    custom: true,
                    group: group.clone(),
                },
                k.clone(),
            ));
        }
    }

    if let Some(fields) = params.get("fields").and_then(|f| f.as_array()) {
        let group = (fields.len() > 1).then(|| new_id("qgrp"));
        for f in fields {
            let key = f
                .get("key")
                .and_then(|k| k.as_str())
                .unwrap_or("field")
                .to_string();
            let header = f.get("title").and_then(|t| t.as_str()).map(str::to_string);
            let mut desc = f
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            if desc.is_empty() {
                desc = if !message.is_empty() {
                    message.clone()
                } else {
                    header.clone().unwrap_or_else(|| key.clone())
                };
            }
            let multi_select = f.get("type").and_then(|t| t.as_str()) == Some("multiselect");
            let options = f.get("options").map(parse_options).unwrap_or_default();

            out.push((
                UserQuestion {
                    id: new_id("qst"),
                    session: session.to_string(),
                    header,
                    question: desc,
                    options,
                    multi_select,
                    custom: true,
                    group: group.clone(),
                },
                key,
            ));
        }
    }

    if out.is_empty() {
        let question_text = if !message.is_empty() {
            message
        } else {
            "Interactive Question".to_string()
        };
        out.push((
            UserQuestion {
                id: new_id("qst"),
                session: session.to_string(),
                header: None,
                question: question_text,
                options: Vec::new(),
                multi_select: false,
                custom: true,
                group: None,
            },
            "answer".to_string(),
        ));
    }

    out
}

/// Parses an ACP `elicitation/create` request into [`UserQuestion`] and property keys.
pub fn parse_acp_elicitation(session: &str, params: &Value) -> Option<(UserQuestion, Vec<String>)> {
    let list = parse_acp_elicitations(session, params);
    let keys: Vec<String> = list.iter().map(|(_, k)| k.clone()).collect();
    list.into_iter().next().map(|(q, _)| (q, keys))
}

/// Parses Codex `item/tool/requestUserInput` parameters into multiple [`UserQuestion`]s with question keys.
pub fn parse_codex_user_inputs(session: &str, params: &Value) -> Vec<(UserQuestion, String)> {
    let Some(questions) = params.get("questions").and_then(|q| q.as_array()) else {
        return Vec::new();
    };

    let group = (questions.len() > 1).then(|| new_id("qgrp"));
    let mut out = Vec::new();
    for (i, item) in questions.iter().enumerate() {
        let q_key = item
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("q{i}"));
        let header = item
            .get("header")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let question = item
            .get("question")
            .or_else(|| item.get("prompt"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let options = item.get("options").map(parse_options).unwrap_or_default();
        let multi_select = item
            .get("multiple")
            .or_else(|| item.get("multi_select"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        out.push((
            UserQuestion {
                id: new_id("qst"),
                session: session.to_string(),
                header,
                question: if question.is_empty() {
                    format!("Question {}", i + 1)
                } else {
                    question
                },
                options,
                multi_select,
                custom: true,
                group: group.clone(),
            },
            q_key,
        ));
    }
    out
}

/// Parses Codex `item/tool/requestUserInput` parameters into [`UserQuestion`] and question key.
pub fn parse_codex_user_input(
    session: &str,
    params: &Value,
) -> Option<(UserQuestion, Option<String>)> {
    parse_codex_user_inputs(session, params)
        .into_iter()
        .next()
        .map(|(q, k)| (q, Some(k)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn multi_question_requests_share_a_group() {
        let json = r#"{
            "questions": [
                {"question": "Database?", "options": [{"label": "Postgres"}]},
                {"question": "Framework?", "options": [{"label": "React"}]}
            ]
        }"#;
        let list = parse_tool_call_questions("ses_1", "question", json);
        assert_eq!(list.len(), 2);
        let group = list[0].group.clone().expect("group");
        assert_eq!(list[1].group.as_deref(), Some(group.as_str()));
    }

    #[test]
    fn parses_acp_one_of_options() {
        let params = json!({
            "message": "Questions",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "q0": {
                        "type": "string",
                        "title": "UI framework",
                        "description": "Which UI framework?",
                        "oneOf": [
                            {"const": "React", "title": "React", "description": "Mature"},
                            {"const": "Vue", "title": "Vue", "description": "Simple"}
                        ]
                    }
                }
            }
        });
        let list = parse_acp_elicitations("ses_1", &params);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0.question, "Which UI framework?");
        let labels: Vec<_> = list[0].0.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, vec!["React", "Vue"]);
        assert_eq!(list[0].0.options[0].description.as_deref(), Some("Mature"));
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
    fn parse_multiple_tool_call_questions() {
        let json = r#"{
            "questions": [
                {
                    "question": "Choose database",
                    "header": "Database",
                    "options": ["PostgreSQL", "SQLite"]
                },
                {
                    "question": "Choose cache",
                    "header": "Cache",
                    "options": ["Redis", "Memcached"],
                    "multi_select": true
                }
            ]
        }"#;
        let list = parse_tool_call_questions("ses_1", "AskUserQuestion", json);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].header.as_deref(), Some("Database"));
        assert_eq!(list[0].question, "Choose database");
        assert_eq!(list[0].options.len(), 2);
        assert!(!list[0].multi_select);
        assert_eq!(list[1].header.as_deref(), Some("Cache"));
        assert_eq!(list[1].question, "Choose cache");
        assert!(list[1].multi_select);
    }

    #[test]
    fn parse_claude_camel_case_multi_select() {
        let json = r#"{
            "questions": [{
                "question": "Choose features",
                "header": "Features",
                "multiSelect": true,
                "options": ["Auth", "Search"]
            }]
        }"#;
        let q = parse_tool_call_question("ses_1", "AskUserQuestion", json).expect("parse claude");
        assert!(q.multi_select);
        assert_eq!(q.options.len(), 2);
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
    fn parse_multiple_codex_questions() {
        let params = serde_json::json!({
            "threadId": "th_1",
            "questions": [
                {
                    "id": "q1",
                    "question": "First question?",
                    "options": ["Yes", "No"]
                },
                {
                    "id": "q2",
                    "question": "Second question?",
                    "options": ["Option A", "Option B"]
                }
            ]
        });
        let list = parse_codex_user_inputs("ses_3", &params);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].1, "q1");
        assert_eq!(list[0].0.question, "First question?");
        assert_eq!(list[1].1, "q2");
        assert_eq!(list[1].0.question, "Second question?");
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
    fn parse_multiple_acp_elicitations() {
        let params = serde_json::json!({
            "mode": "form",
            "message": "Project Config",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "lang": {
                        "type": "string",
                        "title": "Language",
                        "enum": ["Rust", "TypeScript"]
                    },
                    "framework": {
                        "type": "string",
                        "title": "Framework",
                        "enum": ["Actix", "Axum"]
                    }
                }
            }
        });
        let list = parse_acp_elicitations("ses_4", &params);
        assert_eq!(list.len(), 2);
        let keys: Vec<String> = list.iter().map(|(_, k)| k.clone()).collect();
        assert!(keys.contains(&"lang".to_string()));
        assert!(keys.contains(&"framework".to_string()));
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
