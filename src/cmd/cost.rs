//! 成本統計與事件重播子命令（cost、replay）。

use std::path::PathBuf;

use ade_protocol::{Frame, Request};
use anyhow::{bail, Result};

use super::roundtrip;

pub(crate) fn cmd_quota(json: bool) -> Result<()> {
    let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
        bail!("未取得快照");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&state.quotas)?);
        return Ok(());
    }
    if state.quotas.is_empty() {
        println!("(尚無額度資料：agent 執行過一個回合後才會回報)");
        return Ok(());
    }
    let now = ade_core::now_millis();
    for q in state.quotas.values() {
        println!(
            "{}   （{} 前更新）",
            q.summary(now),
            ade_core::format_remaining(now.saturating_sub(q.updated_ms))
        );
    }
    Ok(())
}

pub(crate) fn cmd_cost(json: bool) -> Result<()> {
    let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
        bail!("未取得快照");
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
        println!(
            "{}  {:<20} {:<10} ${:.4}  ({} in / {} out)",
            s.id,
            s.label,
            s.kind.id(),
            s.cost.usd,
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
    let path = file.unwrap_or_else(ade_daemon::log_path);
    let (state, records) = if all {
        ade_daemon::replay_log_all(&path)?
    } else {
        let older = ade_daemon::rotated_logs(&path);
        if !older.is_empty() {
            eprintln!(
                "提示：另有 {} 份較舊的輪替檔，加 --all 可一併讀取",
                older.len()
            );
        }
        ade_daemon::replay_log(&path)?
    };
    let filtered: Vec<_> = records
        .into_iter()
        .filter(|r| r.seq > since && !matches!(r.event, ade_core::Event::Snapshot { .. }))
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
        "重播 {:?}：共 {} 筆事件（最新 seq={}，sessions={}，tasks={}，messages={}）",
        path,
        filtered.len(),
        state.seq,
        state.sessions.len(),
        state.tasks.len(),
        state.messages.len()
    );
    for r in filtered.iter().rev().take(30).rev() {
        println!("#{:>5}  ts={}  {:?}", r.seq, r.ts, r.event);
    }
    Ok(())
}
