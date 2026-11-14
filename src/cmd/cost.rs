//! 成本統計與事件重播子命令（cost、replay）。

use std::path::PathBuf;

use anyhow::{bail, Result};
use termade_protocol::{Frame, Request};
use tr::tr;

use super::roundtrip;

pub(crate) fn cmd_quota(json: bool) -> Result<()> {
    let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
        bail!("{}", tr!("未取得快照"));
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&state.quotas)?);
        return Ok(());
    }
    if state.quotas.is_empty() {
        println!("{}", tr!("(尚無額度資料：agent 執行過一個回合後才會回報)"));
        return Ok(());
    }
    let now = termade_core::now_millis();
    for q in state.quotas.values() {
        println!(
            "{}",
            tr!(
                "{summary}   （{age} 前更新）",
                summary = q.summary(now),
                age = termade_core::format_remaining(now.saturating_sub(q.updated_ms))
            )
        );
    }
    Ok(())
}

pub(crate) fn cmd_cost(json: bool) -> Result<()> {
    let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
        bail!("{}", tr!("未取得快照"));
    };
    let total = state.total_cost();
    if json {
        let per_session: Vec<_> = state
            .sessions_sorted()
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "label": s.label,
                    "kind": s.kind.id(),
                    "usd": s.cost.usd,
                    "budget_usd": s.budget_usd,
                    "over_budget": s.over_budget,
                    "input_tokens": s.cost.input_tokens,
                    "output_tokens": s.cost.output_tokens,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "total": total,
                "sessions": per_session,
            }))?
        );
        return Ok(());
    }
    for s in state.sessions_sorted() {
        let cost_col = match (s.budget_usd, s.format_budget()) {
            (Some(limit), Some(bstr)) => {
                if s.over_budget || s.cost.usd >= limit {
                    format!("\x1b[31m{bstr} {}\x1b[0m", tr!("[超過上限]"))
                } else if limit > 0.0 && s.cost.usd * 100.0 / limit >= 80.0 {
                    format!("\x1b[33m{bstr}\x1b[0m")
                } else {
                    bstr
                }
            }
            _ => format!("${:.4}", s.cost.usd),
        };
        println!(
            "{}  {:<20} {:<10} {}  ({} in / {} out)",
            s.id,
            s.label,
            s.kind.id(),
            cost_col,
            s.cost.input_tokens,
            s.cost.output_tokens
        );
    }
    println!(
        "total: ${:.4}  ({} input tokens / {} output tokens)",
        total.usd, total.input_tokens, total.output_tokens
    );
    Ok(())
}

pub(crate) fn cmd_replay(since: u64, file: Option<PathBuf>, all: bool, json: bool) -> Result<()> {
    let path = file.unwrap_or_else(termade_daemon::log_path);
    let (state, records) = if all {
        termade_daemon::replay_log_all(&path)?
    } else {
        let older = termade_daemon::rotated_logs(&path);
        if !older.is_empty() {
            eprintln!(
                "{}",
                tr!(
                    "提示：另有 {count} 份較舊的輪替檔，加 --all 可一併讀取",
                    count = older.len()
                )
            );
        }
        termade_daemon::replay_log(&path)?
    };
    let filtered: Vec<_> = records
        .into_iter()
        .filter(|r| r.seq > since && !matches!(r.event, termade_core::Event::Snapshot { .. }))
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "log_path": path,
                "records_count": filtered.len(),
                "last_seq": state.seq,
                "records": filtered,
            }))?
        );
        return Ok(());
    }
    println!(
        "{}",
        tr!(
            "重播 {path}：共 {count} 筆事件（最新 seq={seq}，sessions={sessions}，tasks={tasks}，messages={messages}）",
            path = format!("{path:?}"),
            count = filtered.len(),
            seq = state.seq,
            sessions = state.sessions.len(),
            tasks = state.tasks.len(),
            messages = state.messages.len()
        )
    );
    for r in filtered.iter().rev().take(30).rev() {
        println!("#{:>5}  ts={}  {:?}", r.seq, r.ts, r.event);
    }
    Ok(())
}
