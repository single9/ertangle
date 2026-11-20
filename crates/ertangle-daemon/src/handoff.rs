//! 交接：把一個 session 的工作 (通常是額度用完)交給另一個 agent。
//!
//! 做法參考 Orca 的「full handoff」：新 agent 以簡報當初始 prompt 啟動，舊的停止，不等結果。
//! 差別是額度用完時舊 agent 已無法自己寫簡報，所以由 daemon 從事件狀態與 git 組出簡報。

use ertangle_core::{BusMessage, Role, Session, State, Target, Task};

/// 簡報裡最多帶幾行最近的 transcript。
const TAIL_LINES: usize = 40;

/// 組出交接簡報。`git_summary` 是工作目錄的 status／diff stat／最近 commit。
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_handoff_brief(
    state: &State,
    s: &Session,
    git_summary: &str,
    note: Option<&str>,
    unread: &[BusMessage],
    new_role: &Role,
    new_parent: Option<&str>,
    live_children: &[Session],
) -> String {
    let lines: Vec<&String> = state
        .transcripts
        .get(&s.id)
        .map(|t| t.iter().collect())
        .unwrap_or_default();

    // prompt 在 transcript 裡以 `❯ ` (使用者) 或 `» [來源] ` (其他 agent／ertangle) 開頭，
    // 多行的後續行縮排兩格。agent 建立的 session 原始任務來自父 session，也要算進來。
    let mut prompts: Vec<String> = Vec::new();
    let mut skipping = false;
    for l in &lines {
        if let Some((from, first)) = ertangle_core::parse_prompt_line(l) {
            // ertangle 自己的提醒／通知不是任務，略過 (後續行也一併略過)。
            if from == Some(ertangle_core::PROMPT_FROM_ERTANGLE) {
                skipping = true;
                continue;
            }
            skipping = false;
            prompts.push(first.to_string());
        } else if !l.starts_with("  ") {
            skipping = false;
        } else if !skipping
            && !prompts.is_empty()
            && !l.starts_with("  ✓")
            && !l.starts_with("  ✗")
            && !l.starts_with("  📎")
        {
            if let Some(last) = prompts.last_mut() {
                last.push('\n');
                last.push_str(l.trim_start());
            }
        }
    }
    let tail: Vec<&str> = lines
        .iter()
        .rev()
        .take(TAIL_LINES)
        .rev()
        .map(|l| l.as_str())
        .collect();
    let tasks: Vec<&Task> = state
        .tasks
        .values()
        .filter(|t| t.assignee.as_deref() == Some(s.id.as_str()))
        .collect();

    let role_changed = &s.role != new_role;
    let mut b = String::new();
    if role_changed {
        let (report_en, report_zh) = match new_parent {
            None => (
                "you now report to the user".to_string(),
                "你現在向使用者回報".to_string(),
            ),
            Some(pid) => (
                format!("you now report to parent session {pid}"),
                format!("你現在向父 session {pid} 回報"),
            ),
        };
        b.push_str(&format!(
            "Your role changed from {old_role} to {new_role}; {report_en}.\n\
             你的角色已由 {old_role} 變更為 {new_role}；{report_zh}。\n\
             你正在接手另一個 agent ({label}，{kind})的工作目錄，但你的身分與職責已調整。\n\
             以下原任務、最後輸出、未讀訊息與 diff 僅為背景資訊供參考，而非你要直接繼續執行的工作 (background context, not work to resume)；請依新角色的職責出發。\n\n",
            old_role = s.role.id(),
            new_role = new_role.id(),
            label = s.label,
            kind = s.kind.id(),
        ));
    } else {
        b.push_str(&format!(
            "你正在接手另一個 agent ({}，{})未完成的工作。它因為額度用完或中斷而停止，無法再回答問題，\n\
             請依下列資訊與工作目錄中的實際內容繼續；不要重做已完成的部分，先檢查目前的 git 狀態與附上的 diff 再動手。\n\n",
            s.label,
            s.kind.id()
        ));
    }

    if !live_children.is_empty() {
        b.push_str("## 警告：執行中的子 session (Warning: running children)\n");
        b.push_str("原 coordinator 降級時，下列子 session 仍在執行中：\n");
        for c in live_children {
            b.push_str(&format!("- {} ({})\n", c.id, c.label));
        }
        b.push('\n');
    }

    let bg_prefix = if role_changed { "背景資訊：" } else { "" };
    let bg_suffix = if role_changed { " (Background)" } else { "" };

    if let Some(first) = prompts.first() {
        b.push_str(&format!("## {bg_prefix}原始任務{bg_suffix}\n{first}\n\n"));
    }
    if prompts.len() > 1 {
        b.push_str(&format!("## {bg_prefix}之後的追加指示{bg_suffix}\n"));
        for p in prompts.iter().skip(1).rev().take(5).rev() {
            b.push_str(&format!("- {}\n", p.replace('\n', " ")));
        }
        b.push('\n');
    }
    if !tasks.is_empty() {
        b.push_str(&format!("## {bg_prefix}指派給它的任務{bg_suffix}\n"));
        for t in tasks {
            b.push_str(&format!(
                "- [{:?}] {}{}\n",
                t.status,
                t.title,
                if t.detail.is_empty() {
                    String::new()
                } else {
                    format!("：{}", t.detail)
                }
            ));
        }
        b.push('\n');
    }
    if let Some(n) = &s.status_note {
        b.push_str(&format!(
            "## {bg_prefix}它最後回報的狀態{bg_suffix}\n{n}\n\n"
        ));
    }
    if !unread.is_empty() {
        b.push_str(&format!(
            "## {bg_prefix}它收件匣裡尚未讀取的訊息{bg_suffix}\n"
        ));
        for m in unread {
            let to = match &m.to {
                Target::Direct(_) => "直接",
                Target::Broadcast => "廣播",
                Target::Channel(_) => "頻道",
            };
            b.push_str(&format!(
                "- 來自 {} ({}，{})：{}\n",
                m.from,
                to,
                m.kind.id(),
                m.body.replace('\n', "\n  ")
            ));
        }
        b.push_str("需要回覆時用 ertangle_send；之後送給舊 session 的訊息會自動轉給你。\n\n");
    }
    if let Some(n) = note.filter(|n| !n.trim().is_empty()) {
        b.push_str(&format!("## 補充說明\n{n}\n\n"));
    }
    b.push_str(&format!(
        "## {bg_prefix}工作目錄現況{bg_suffix}\n路徑：{}{}\n{git_summary}\n注意：未 commit 的變更已由 ertangle 自動 commit 到這個分支，可能是做到一半的狀態。\n\n",
        s.cwd,
        s.branch.as_deref().map(|b| format!("(分支 {b})")).unwrap_or_default()
    ));
    if !tail.is_empty() {
        b.push_str(&format!(
            "## {bg_prefix}它最後的輸出 (最近幾十行){bg_suffix}\n```\n"
        ));
        b.push_str(&tail.join("\n"));
        b.push_str("\n```\n");
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use ertangle_core::{AgentKind, Cost, PermissionPolicy, Role, SessionStatus};

    fn sess() -> Session {
        Session {
            id: "s1".into(),
            kind: AgentKind::ClaudeCode,
            label: "worker/a".into(),
            cwd: "/tmp/wt".into(),
            branch: Some("ertangle/s1".into()),
            role: Role::Worker,
            status: SessionStatus::Failed,
            model: None,
            cost: Cost::default(),
            parent: None,
            created_ms: 0,
            transport: String::new(),
            status_note: Some("額度用完：limit".into()),
            repo_root: None,
            context: None,
            native_id: None,
            permission: PermissionPolicy::Auto,
            budget_usd: None,
            over_budget: false,
            handed_off_to: None,
            effort: None,
            slash_commands: Vec::new(),
            active_tools: Vec::new(),
            bg_procs: Vec::new(),
            tool_calls: 0,
        }
    }

    #[test]
    fn brief_has_task_followups_note_and_tail() {
        let mut st = State::new();
        let t = st.transcripts.entry("s1".into()).or_default();
        t.push_back("❯ 實作 EU 郵遞區號驗證".into());
        t.push_back("  含單元測試".into());
        t.push_back("▸ Bash: cargo test".into());
        t.push_back("  ✗ 3 failed".into());
        t.push_back("❯ 順便更新文件".into());
        let s = sess();
        let b = build_handoff_brief(
            &st,
            &s,
            "M src/zip.rs",
            Some("先修失敗的測試"),
            &[],
            &s.role,
            s.parent.as_deref(),
            &[],
        );
        assert!(
            b.contains("## 原始任務\n實作 EU 郵遞區號驗證\n含單元測試"),
            "{b}"
        );
        assert!(b.contains("順便更新文件"));
        assert!(b.contains("先修失敗的測試"));
        assert!(b.contains("M src/zip.rs") && b.contains("ertangle/s1"));
        assert!(b.contains("✗ 3 failed"));
    }

    #[test]
    fn brief_takes_task_from_parent_and_skips_ertangle_notices() {
        let mut st = State::new();
        let t = st.transcripts.entry("s1".into()).or_default();
        t.push_back("» [coord/a] 實作 T49".into());
        t.push_back("  照票上的步驟".into());
        t.push_back("» [ertangle] [ertangle 未讀訊息提醒] 你忙碌時收到 1 則訊息".into());
        t.push_back("  請呼叫 ertangle_inbox 查看".into());
        let s = sess();
        let b = build_handoff_brief(&st, &s, "", None, &[], &s.role, s.parent.as_deref(), &[]);
        assert!(b.contains("## 原始任務\n實作 T49\n照票上的步驟"), "{b}");
        assert!(!b.contains("照票上的步驟\n請呼叫"), "{b}");
        assert!(!b.contains("## 後續指示"), "{b}");
    }

    #[test]
    fn brief_tail_is_capped() {
        let mut st = State::new();
        let t = st.transcripts.entry("s1".into()).or_default();
        for i in 0..200 {
            t.push_back(format!("line {i}"));
        }
        let s = sess();
        let b = build_handoff_brief(&st, &s, "", None, &[], &s.role, s.parent.as_deref(), &[]);
        assert!(b.contains("line 199") && !b.contains("line 100\n"));
    }

    #[test]
    fn brief_includes_unread_messages() {
        use ertangle_core::{MsgKind, Target};
        let msg = BusMessage {
            id: "m1".into(),
            from: "ses_other".into(),
            to: Target::Direct("worker/a".into()),
            kind: MsgKind::Chat,
            body: "超限時佇列怎麼處理？\n請回覆".into(),
            reply_to: None,
            ts: 0,
        };
        let st = State::new();
        let s = sess();
        let b = build_handoff_brief(&st, &s, "", None, &[msg], &s.role, s.parent.as_deref(), &[]);
        assert!(b.contains("尚未讀取的訊息"), "{b}");
        assert!(
            b.contains("ses_other") && b.contains("超限時佇列怎麼處理？"),
            "{b}"
        );
        let none = build_handoff_brief(&st, &s, "", None, &[], &s.role, s.parent.as_deref(), &[]);
        assert!(!none.contains("尚未讀取的訊息"));
    }

    #[test]
    fn brief_reflects_role_change() {
        let mut st = State::new();
        let t = st.transcripts.entry("s1".into()).or_default();
        t.push_back("❯ 舊任務".into());
        let s = sess(); // Worker
        let b = build_handoff_brief(
            &st,
            &s,
            "git status",
            None,
            &[],
            &Role::Coordinator,
            None,
            &[],
        );
        assert!(
            b.contains("Your role changed from worker to coordinator; you now report to the user")
        );
        assert!(b.contains("你的角色已由 worker 變更為 coordinator；你現在向使用者回報"));
        assert!(b.contains("## 背景資訊：原始任務 (Background)"));
        assert!(b.contains("## 背景資訊：工作目錄現況 (Background)"));
    }
}
