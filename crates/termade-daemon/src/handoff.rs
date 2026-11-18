//! 交接：把一個 session 的工作 (通常是額度用完)交給另一個 agent。
//!
//! 做法參考 Orca 的「full handoff」：新 agent 以簡報當初始 prompt 啟動，舊的停止，不等結果。
//! 差別是額度用完時舊 agent 已無法自己寫簡報，所以由 daemon 從事件狀態與 git 組出簡報。

use termade_core::{BusMessage, Session, State, Target, Task};

/// 簡報裡最多帶幾行最近的 transcript。
const TAIL_LINES: usize = 40;

/// 組出交接簡報。`git_summary` 是工作目錄的 status／diff stat／最近 commit。
pub(crate) fn build_handoff_brief(
    state: &State,
    s: &Session,
    git_summary: &str,
    note: Option<&str>,
    unread: &[BusMessage],
) -> String {
    let lines: Vec<&String> = state
        .transcripts
        .get(&s.id)
        .map(|t| t.iter().collect())
        .unwrap_or_default();

    // 使用者 prompt 在 transcript 裡以 `❯ ` 開頭 (多行的後續行縮排兩格)。
    let mut prompts: Vec<String> = Vec::new();
    for l in &lines {
        if let Some(first) = l.strip_prefix("❯ ") {
            prompts.push(first.to_string());
        } else if l.starts_with("  ")
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

    let mut b = String::new();
    b.push_str(&format!(
        "你正在接手另一個 agent ({}，{})未完成的工作。它因為額度用完或中斷而停止，無法再回答問題，\n\
         請依下列資訊與工作目錄中的實際內容繼續；不要重做已完成的部分，先檢查目前的 git 狀態與附上的 diff 再動手。\n\n",
        s.label,
        s.kind.id()
    ));
    if let Some(first) = prompts.first() {
        b.push_str(&format!("## 原始任務\n{first}\n\n"));
    }
    if prompts.len() > 1 {
        b.push_str("## 之後的追加指示\n");
        for p in prompts.iter().skip(1).rev().take(5).rev() {
            b.push_str(&format!("- {}\n", p.replace('\n', " ")));
        }
        b.push('\n');
    }
    if !tasks.is_empty() {
        b.push_str("## 指派給它的任務\n");
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
        b.push_str(&format!("## 它最後回報的狀態\n{n}\n\n"));
    }
    if !unread.is_empty() {
        b.push_str("## 它收件匣裡尚未讀取的訊息\n");
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
        b.push_str("需要回覆時用 termade_send；之後送給舊 session 的訊息會自動轉給你。\n\n");
    }
    if let Some(n) = note.filter(|n| !n.trim().is_empty()) {
        b.push_str(&format!("## 補充說明\n{n}\n\n"));
    }
    b.push_str(&format!(
        "## 工作目錄現況\n路徑：{}{}\n{git_summary}\n注意：未 commit 的變更已由 termade 自動 commit 到這個分支，可能是做到一半的狀態。\n\n",
        s.cwd,
        s.branch.as_deref().map(|b| format!("(分支 {b})")).unwrap_or_default()
    ));
    if !tail.is_empty() {
        b.push_str("## 它最後的輸出 (最近幾十行)\n```\n");
        b.push_str(&tail.join("\n"));
        b.push_str("\n```\n");
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use termade_core::{AgentKind, Cost, PermissionPolicy, Role, SessionStatus};

    fn sess() -> Session {
        Session {
            id: "s1".into(),
            kind: AgentKind::ClaudeCode,
            label: "worker/a".into(),
            cwd: "/tmp/wt".into(),
            branch: Some("termade/s1".into()),
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
        let b = build_handoff_brief(&st, &sess(), "M src/zip.rs", Some("先修失敗的測試"), &[]);
        assert!(
            b.contains("## 原始任務\n實作 EU 郵遞區號驗證\n含單元測試"),
            "{b}"
        );
        assert!(b.contains("順便更新文件"));
        assert!(b.contains("先修失敗的測試"));
        assert!(b.contains("M src/zip.rs") && b.contains("termade/s1"));
        assert!(b.contains("✗ 3 failed"));
    }

    #[test]
    fn brief_tail_is_capped() {
        let mut st = State::new();
        let t = st.transcripts.entry("s1".into()).or_default();
        for i in 0..200 {
            t.push_back(format!("line {i}"));
        }
        let b = build_handoff_brief(&st, &sess(), "", None, &[]);
        assert!(b.contains("line 199") && !b.contains("line 100\n"));
    }

    #[test]
    fn brief_includes_unread_messages() {
        use termade_core::{MsgKind, Target};
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
        let b = build_handoff_brief(&st, &sess(), "", None, &[msg]);
        assert!(b.contains("尚未讀取的訊息"), "{b}");
        assert!(
            b.contains("ses_other") && b.contains("超限時佇列怎麼處理？"),
            "{b}"
        );
        let none = build_handoff_brief(&st, &sess(), "", None, &[]);
        assert!(!none.contains("尚未讀取的訊息"));
    }
}
