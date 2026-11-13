//! 協作說明：告訴每個 agent 它是誰、向誰回報、怎麼使用 `termade_*` 工具。
//!
//! 注入管道有兩層（見 `docs/DESIGN.md` 的「協作說明」）：
//! 1. `termade mcp` 在 MCP `initialize` 回應的 `instructions` 帶出（`mcp.rs`）。
//! 2. 啟動 agent 時經 `SessionSpec::system_append` 交給 adapter（`handlers/session.rs`）。
//!
//! 不寫成 worktree 裡的 `AGENTS.md`：合併前的自動 commit 會把它提交進使用者的分支，
//! 也會干擾 repo 自己的 `AGENTS.md`。

use std::path::Path;

use termade_core::{Role, Session, State};

use crate::config::Config;

/// `.termade/instructions.md` 最多取用的位元組數，避免把整份文件塞進每個 agent 的 context。
const MAX_REPO_INSTRUCTIONS: usize = 8 * 1024;

const HEADER: &str = "[termade 協作說明]";

/// 對所有 session 都一樣的通則（查不到 session 時只給這一段）。
fn general_rules(out: &mut String) {
    out.push_str(
        "- 進度：工作進行中，用 termade_status 更新一句話的進度。\n\
         - 任務：被指派任務時，先用 termade_task_update 把狀態改為 doing，完成後改為 done。\n\
         - 提問：需要別人的答案才能繼續時用 termade_ask；termade_peers 可以查有哪些 session。\n",
    );
}

fn push_extra(out: &mut String, extra: &str) {
    let extra = extra.trim();
    if !extra.is_empty() {
        out.push('\n');
        out.push_str(extra);
        out.push('\n');
    }
}

/// 組出某個 session 的協作說明。`parent` 是產生它的 session（人產生的為 `None`）；
/// `extra` 原樣附加在最後。
pub fn collab_instructions(me: &Session, parent: Option<&Session>, extra: &str) -> String {
    let mut out = format!("{HEADER}\n");
    let spawned_by = match (parent, me.parent.as_deref()) {
        (Some(p), _) => format!("session「{}」（{}）", p.label, p.id),
        (None, Some(pid)) => format!("session {pid}"),
        (None, None) => "使用者".to_string(),
    };
    out.push_str(&format!(
        "你是 termade 管理的 agent session「{}」（id {}，角色 {}），由{spawned_by}產生。\n",
        me.label,
        me.id,
        me.role.id()
    ));
    match parent.map(|p| p.id.as_str()).or(me.parent.as_deref()) {
        Some(pid) => out.push_str(&format!(
            "- 回報：完成或卡住時，用 termade_send（to=\"{pid}\"）把結果告訴父 session；不要做完就停住不回報。\n"
        )),
        None => out.push_str(
            "- 回報：你是使用者直接產生的；完成或卡住時直接在回覆裡說明，不需要另外傳訊息。\n",
        ),
    }
    general_rules(&mut out);
    if let Some(branch) = &me.branch {
        out.push_str(&format!(
            "- worktree：你在獨立的 git worktree（分支 {branch}）工作。成果留在這個分支；不要自己合併回主分支，除非被明確要求。\n"
        ));
    }
    if me.role == Role::Coordinator {
        out.push_str(
            "- 協調：用 termade_spawn 產生 worker，用 termade_task_create 與 depends_on 安排順序；等回覆用 termade_wait，不要輪詢。\n",
        );
    }
    push_extra(&mut out, extra);
    out
}

/// 查不到 session（例如使用者手動執行 `termade mcp`）時的通則。
pub fn general_instructions(extra: &str) -> String {
    let mut out = format!("{HEADER}\n");
    general_rules(&mut out);
    push_extra(&mut out, extra);
    out
}

/// 設定與 repo 提供的附加文字：`[collab] extra`、`[roles.<role>] instructions`，
/// 最後是 `<project>/.termade/instructions.md`。
fn extra_text(cfg: &Config, role: Option<&Role>, project: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(e) = cfg.collab.extra.as_deref() {
        parts.push(e.trim().to_string());
    }
    if let Some(r) = role
        .and_then(|r| cfg.roles.get(r.id()))
        .and_then(|r| r.instructions.as_deref())
    {
        parts.push(r.trim().to_string());
    }
    if let Ok(mut md) = std::fs::read_to_string(project.join(".termade").join("instructions.md")) {
        if md.len() > MAX_REPO_INSTRUCTIONS {
            let mut cut = MAX_REPO_INSTRUCTIONS;
            while !md.is_char_boundary(cut) {
                cut -= 1;
            }
            md.truncate(cut);
            md.push_str("\n…（.termade/instructions.md 過長，已截斷）");
        }
        parts.push(md.trim().to_string());
    }
    parts.retain(|p| !p.is_empty());
    parts.join("\n\n")
}

/// 依設定組出 `session`（id 或 label）的協作說明；`[collab] instructions = false` 時回 `None`。
///
/// 設定與 `.termade/instructions.md` 取自 session 所屬的專案（worktree 看主 repo，
/// 因為 `.termade/` 不進版控，worktree 裡不會有）。查不到 session 時以 `fallback_cwd` 為專案，
/// 只給通則。
pub fn for_session(
    state: &State,
    session: &str,
    global_dir: &Path,
    fallback_cwd: &Path,
) -> Option<String> {
    let me = state
        .resolve_session_id(session)
        .and_then(|id| state.sessions.get(&id));
    let project = me.map_or(fallback_cwd, |s| Path::new(s.project_root()));
    let cfg = Config::load(global_dir, project);
    if cfg.collab.instructions == Some(false) {
        return None;
    }
    let extra = extra_text(&cfg, me.map(|s| &s.role), project);
    Some(match me {
        Some(s) => {
            let parent = s.parent.as_ref().and_then(|p| state.sessions.get(p));
            collab_instructions(s, parent, &extra)
        }
        None => general_instructions(&extra),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use termade_core::{AgentKind, SessionStatus};

    fn sess(id: &str, label: &str, role: Role) -> Session {
        Session {
            id: id.into(),
            kind: AgentKind::ClaudeCode,
            label: label.into(),
            cwd: "/tmp".into(),
            branch: None,
            role,
            status: SessionStatus::Idle,
            model: None,
            cost: Default::default(),
            parent: None,
            created_ms: 0,
            transport: String::new(),
            status_note: None,
            repo_root: None,
            permission: Default::default(),
            context: None,
            native_id: None,
        }
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(termade_core::new_id(tag));
        std::fs::create_dir_all(d.join(".termade")).unwrap();
        d
    }

    #[test]
    fn human_spawned_worker_without_worktree() {
        let me = sess("ses_w", "worker/a", Role::Worker);
        let t = collab_instructions(&me, None, "");
        assert!(t.contains("worker/a") && t.contains("ses_w") && t.contains("角色 worker"));
        assert!(t.contains("由使用者產生"));
        assert!(t.contains("直接在回覆裡說明"));
        assert!(t.contains("termade_status") && t.contains("termade_task_update"));
        assert!(t.contains("termade_ask"));
        // 不相關的段落不出現。
        assert!(!t.contains("termade_send"), "{t}");
        assert!(!t.contains("worktree"), "{t}");
        assert!(
            !t.contains("termade_task_create") && !t.contains("termade_wait"),
            "{t}"
        );
        assert!(t.lines().count() <= 30);
    }

    #[test]
    fn agent_spawned_worker_in_worktree_reports_to_parent() {
        let parent = sess("ses_p", "coord", Role::Coordinator);
        let mut me = sess("ses_w", "worker/a", Role::Worker);
        me.parent = Some("ses_p".into());
        me.branch = Some("termade/ses_w".into());
        let t = collab_instructions(&me, Some(&parent), "");
        assert!(t.contains("由session「coord」（ses_p）產生"), "{t}");
        assert!(t.contains("termade_send（to=\"ses_p\"）"), "{t}");
        assert!(t.contains("分支 termade/ses_w") && t.contains("不要自己合併"));
        assert!(!t.contains("直接在回覆裡說明"), "{t}");
        assert!(!t.contains("termade_task_create"), "{t}");
    }

    #[test]
    fn parent_gone_still_names_its_id() {
        let mut me = sess("ses_w", "worker/a", Role::Worker);
        me.parent = Some("ses_gone".into());
        let t = collab_instructions(&me, None, "");
        assert!(t.contains("termade_send（to=\"ses_gone\"）"), "{t}");
        assert!(!t.contains("直接在回覆裡說明"));
    }

    #[test]
    fn coordinator_gets_orchestration_section() {
        let me = sess("ses_c", "coord", Role::Coordinator);
        let t = collab_instructions(&me, None, "");
        assert!(t.contains("termade_task_create") && t.contains("depends_on"));
        assert!(t.contains("termade_wait") && t.contains("不要輪詢"));
        assert!(t.contains("termade_spawn"));
        assert!(!t.contains("worktree"), "{t}");
        // 協調者自己也在 worktree 且由 agent 產生：所有段落都在，仍在 30 行內。
        let top = sess("ses_top", "top", Role::Coordinator);
        let mut sub = sess("ses_c", "coord", Role::Coordinator);
        sub.parent = Some("ses_top".into());
        sub.branch = Some("termade/ses_c".into());
        let t = collab_instructions(&sub, Some(&top), "");
        assert!(t.contains("不要自己合併") && t.contains("termade_send") && t.contains("不要輪詢"));
        assert!(t.lines().count() <= 30);
    }

    #[test]
    fn extra_is_appended_last() {
        let me = sess("ses_w", "worker/a", Role::Worker);
        let t = collab_instructions(&me, None, "一律用繁體中文回報。");
        assert!(t.trim_end().ends_with("一律用繁體中文回報。"));
        assert!(general_instructions("x-extra")
            .trim_end()
            .ends_with("x-extra"));
    }

    #[test]
    fn for_session_layers_config_role_and_repo_file_in_order() {
        let global = temp_dir("collab-g");
        let repo = temp_dir("collab-r");
        std::fs::write(
            repo.join(".termade/config.toml"),
            "[collab]\nextra = \"EXTRA-CFG\"\n[roles.reviewer]\ninstructions = \"ROLE-REVIEWER\"\n",
        )
        .unwrap();
        std::fs::write(repo.join(".termade/instructions.md"), "REPO-FILE 最後\n").unwrap();
        let mut st = State::new();
        // worktree session：cwd 在 worktree，設定與 instructions.md 取自主 repo。
        let mut rv = sess("ses_r", "reviewer/x", Role::Reviewer);
        rv.cwd = repo.join(".termade/wt/ses_r").to_string_lossy().to_string();
        rv.repo_root = Some(repo.to_string_lossy().to_string());
        let mut wk = sess("ses_w", "worker/x", Role::Worker);
        wk.cwd = repo.to_string_lossy().to_string();
        st.sessions.insert(rv.id.clone(), rv);
        st.sessions.insert(wk.id.clone(), wk);

        let t = for_session(&st, "reviewer/x", &global, &global).unwrap();
        let (a, b, c) = (
            t.find("EXTRA-CFG").unwrap(),
            t.find("ROLE-REVIEWER").unwrap(),
            t.find("REPO-FILE").unwrap(),
        );
        assert!(t.find("termade_status").unwrap() < a && a < b && b < c);
        assert!(t.trim_end().ends_with("REPO-FILE 最後"), "{t}");

        // 其他角色拿不到 reviewer 的說明。
        let t = for_session(&st, "ses_w", &global, &global).unwrap();
        assert!(!t.contains("ROLE-REVIEWER") && t.contains("EXTRA-CFG"));
        assert!(t.trim_end().ends_with("REPO-FILE 最後"));
    }

    #[test]
    fn for_session_unknown_session_gets_general_rules_only() {
        let global = temp_dir("collab-g");
        let cwd = temp_dir("collab-c");
        let t = for_session(&State::new(), "human", &global, &cwd).unwrap();
        assert!(t.contains("termade_status") && t.contains("termade_ask"));
        assert!(!t.contains("你是 termade 管理的"), "{t}");
        assert!(!t.contains("回報："), "{t}");
    }

    #[test]
    fn for_session_disabled_by_config() {
        let global = temp_dir("collab-g");
        std::fs::write(
            global.join("config.toml"),
            "[collab]\ninstructions = false\n",
        )
        .unwrap();
        let cwd = temp_dir("collab-c");
        let mut st = State::new();
        let mut me = sess("ses_w", "worker/x", Role::Worker);
        me.cwd = cwd.to_string_lossy().to_string();
        st.sessions.insert(me.id.clone(), me);
        assert_eq!(for_session(&st, "ses_w", &global, &cwd), None);
        assert_eq!(for_session(&st, "nobody", &global, &cwd), None);
        // repo 設定可以再打開。
        std::fs::write(
            cwd.join(".termade/config.toml"),
            "[collab]\ninstructions = true\n",
        )
        .unwrap();
        assert!(for_session(&st, "ses_w", &global, &cwd).is_some());
    }

    #[test]
    fn oversized_repo_file_is_truncated() {
        let global = temp_dir("collab-g");
        let cwd = temp_dir("collab-c");
        std::fs::write(cwd.join(".termade/instructions.md"), "字".repeat(10_000)).unwrap();
        let t = for_session(&State::new(), "x", &global, &cwd).unwrap();
        assert!(t.len() < MAX_REPO_INSTRUCTIONS + 2048);
        assert!(t.contains("已截斷"));
    }
}
