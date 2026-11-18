//! 協作說明：告訴每個 agent 它是誰、向誰回報、怎麼使用 `termade_*` 工具。
//!
//! 注入管道有兩層 (見 `docs/DESIGN.md` 的「協作說明」)：
//! 1. `termade mcp` 在 MCP `initialize` 回應的 `instructions` 帶出 (`mcp.rs`)。
//! 2. 啟動 agent 時經 `SessionSpec::system_append` 交給 adapter (`handlers/session.rs`)。
//!
//! 不寫成 worktree 裡的 `AGENTS.md`：合併前的自動 commit 會把它提交進使用者的分支，
//! 也會干擾 repo 自己的 `AGENTS.md`。

use std::path::Path;

use termade_core::{Role, Session, State};

use crate::config::Config;

/// `.termade/instructions.md` 最多取用的位元組數，避免把整份文件塞進每個 agent 的 context。
const MAX_REPO_INSTRUCTIONS: usize = 8 * 1024;

const HEADER: &str = "[termade collaboration instructions]";

/// 對所有 session 都一樣的通則 (查不到 session 時只給這一段)。
fn general_rules(out: &mut String) {
    out.push_str(
        "- Progress: While working, keep a one-line progress update via termade_status.\n\
         - Tasks: When assigned a task, change its status to doing via termade_task_update, and to done upon completion.\n\
         - Questions: Use termade_ask when blocked waiting for peer answers; use termade_peers to query active sessions.\n\
         - Communication Protocol: All inter-agent messages MUST use concise English format (`TASK_DONE <id> <summary>`, `TASK_BLOCKED <id> <reason>`, `QUERY: <question>`, `ANSWER: <reply>`, `ACK`) to minimize token usage.\n\
         - Zero Pleasantries Policy: Strictly forbid pleasantries, courtesy ping-pong, and polite sign-offs (e.g. \"Thank you\", \"Goodbye\", \"辛苦了\", \"合作愉快\"). Once `TASK_DONE` is sent, terminate your turn immediately without further replies.\n",
    );
}

/// `[collab] token_hints = true` 時附加的工具使用建議：每多一個回合都要重讀整段上下文，
/// 所以重點是減少回合數與輸出量 (見 `docs/tickets/T47-agent-token-savings.md`)。
const TOKEN_HINTS: &str = "Token economy (tool use): every extra turn re-reads the whole context, so fewer turns and smaller outputs save the most.\n\
- Issue independent tool calls together in one turn instead of one per turn.\n\
- Combine read-only probes (git status, git log, ls, head) into a single shell command.\n\
- Limit output: use line ranges, `| head -n 50`, `git diff --stat`, `git log -n 20`; do not dump whole large files or logs.\n\
- Do not re-read a file you already read unless it has changed.\n\
- Prefer a one-line tool (rg, jq) over writing a throwaway script.";

fn push_extra(out: &mut String, extra: &str) {
    let extra = extra.trim();
    if !extra.is_empty() {
        out.push('\n');
        out.push_str(extra);
        out.push('\n');
    }
}

/// 組出某個 session 的協作說明。`parent` 是產生它的 session (人產生的為 `None`)；
/// `extra` 原樣附加在最後。
pub fn collab_instructions(me: &Session, parent: Option<&Session>, extra: &str) -> String {
    let mut out = format!("{HEADER}\n");
    let spawned_by = match (parent, me.parent.as_deref()) {
        (Some(p), _) => format!("session \"{}\" ({})", p.label, p.id),
        (None, Some(pid)) => format!("session {pid}"),
        (None, None) => "the user".to_string(),
    };
    out.push_str(&format!(
        "You are agent session \"{}\" (id {}, role {}), spawned by {spawned_by}.\n",
        me.label,
        me.id,
        me.role.id()
    ));
    match parent.map(|p| p.id.as_str()).or(me.parent.as_deref()) {
        Some(pid) => out.push_str(&format!(
            "- Reporting: When finished or blocked, use termade_send (to=\"{pid}\") with concise English protocol (`TASK_DONE <task_id> <summary>` or `TASK_BLOCKED <task_id> <reason>`) to report results back to parent session, then stop your turn immediately; do not stop silently without reporting.\n"
        )),
        None => out.push_str(
            "- Reporting: You were directly spawned by the user; answer directly in your response.\n",
        ),
    }
    general_rules(&mut out);
    if let Some(branch) = &me.branch {
        out.push_str(&format!(
            "- Worktree: You are working in an isolated git worktree (branch {branch}). Keep your commits on this branch; do not merge into the main branch unless explicitly requested. Never use `git stash`: the stash stack is shared with every other worktree, so a pop can apply another session's changes; set work aside with a WIP commit instead.\n"
        ));
    }
    let has_parent = parent.is_some() || me.parent.is_some();
    if me.role == Role::Coordinator {
        out.push_str(
            "- Coordination: Spawn workers using termade_spawn and schedule dependencies with termade_task_create (via depends_on); use termade_wait to await replies, do not poll.\n\
             - Budget & Models: Sessions spawned via termade_spawn get independent workspaces by default. Do not self-select expensive models or effort; use defaults unless needed. Ask the user before using high-tier models. If usage exceeds configured limits, the system will prompt for confirmation.\n\
             - Token economy: Spawn the fewest workers; do small tasks inline. Give self-contained prompts with exact files/line ranges so workers do not re-explore. Prefer cheaper default models for mechanical work. Check quota via termade_whoami before spawning; when `QUOTA_WARN` arrives, stop spawning and have workers commit and report. Reuse an idle worker via termade_task_create instead of spawning a new one.\n",
        );
    } else {
        let report_target = if has_parent {
            "via termade_send"
        } else {
            "directly in your response"
        };
        out.push_str(&format!(
            "- Scope: Focus strictly on your assigned task. Do not call termade_spawn to create new sessions unless explicitly instructed by the user or coordinator. If blocked or if a task requires decomposition, report back {report_target}.\n"
        ));
    }
    push_extra(&mut out, extra);
    out
}

/// 查不到 session (例如使用者手動執行 `termade mcp`)時的通則。
pub fn general_instructions(extra: &str) -> String {
    let mut out = format!("{HEADER}\n");
    general_rules(&mut out);
    push_extra(&mut out, extra);
    out
}

fn truncate_text(mut t: String, name: &str) -> String {
    if t.len() > MAX_REPO_INSTRUCTIONS {
        let mut cut = MAX_REPO_INSTRUCTIONS;
        while !t.is_char_boundary(cut) {
            cut -= 1;
        }
        t.truncate(cut);
        t.push_str(&format!("\n… ({name} truncated because it was too long)"));
    }
    t.trim().to_string()
}

/// 設定與 repo 提供的附加文字：`[collab] extra`、`[roles.<role>] instructions`，
/// 最後是 `<project>/.termade/instructions.md`。
fn extra_text(cfg: &Config, role: Option<&Role>, project: &Path, global_dir: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    // ADE 整體系統提示：全域設定檔指定的檔案 (只認全域，repo 內的設定不能指向任意檔案)。
    if let Some(f) = cfg.collab.prompt_file.as_deref() {
        let path = std::path::PathBuf::from(f);
        let path = if path.is_absolute() {
            path
        } else {
            global_dir.join(path)
        };
        match std::fs::read_to_string(&path) {
            Ok(t) => parts.push(truncate_text(t, "collab.prompt_file")),
            Err(e) => tracing::warn!("讀不到 collab.prompt_file {path:?}：{e}"),
        }
    }
    if let Some(e) = cfg.collab.extra.as_deref() {
        parts.push(e.trim().to_string());
    }
    if let Some(r) = role
        .and_then(|r| cfg.roles.get(r.id()))
        .and_then(|r| r.instructions.as_deref())
    {
        parts.push(r.trim().to_string());
    }
    if let Ok(md) = std::fs::read_to_string(project.join(".termade").join("instructions.md")) {
        parts.push(truncate_text(md, ".termade/instructions.md"));
    }
    parts.retain(|p| !p.is_empty());
    parts.join("\n\n")
}

/// 依設定組出 `session`(id 或 label)的協作說明；`[collab] instructions = false` 時回 `None`。
///
/// 設定與 `.termade/instructions.md` 取自 session 所屬的專案 (worktree 看主 repo，
/// 因為 `.termade/` 不進版控，worktree 裡不會有)。查不到 session 時以 `fallback_cwd` 為專案，
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
    let mut extra = extra_text(&cfg, me.map(|s| &s.role), project, global_dir);
    if cfg.collab.token_hints == Some(true) {
        if !extra.is_empty() {
            extra.push_str("\n\n");
        }
        extra.push_str(TOKEN_HINTS);
    }
    if cfg.collab.inject_skill == Some(true) {
        if !extra.is_empty() {
            extra.push_str("\n\n");
        }
        extra.push_str(crate::skill::skill_body());
    }
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

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(termade_core::new_id(tag));
        std::fs::create_dir_all(d.join(".termade")).unwrap();
        d
    }

    #[test]
    fn human_spawned_worker_without_worktree() {
        let me = sess("ses_w", "worker/a", Role::Worker);
        let t = collab_instructions(&me, None, "");
        assert!(t.contains("worker/a") && t.contains("ses_w") && t.contains("role worker"));
        assert!(t.contains("spawned by the user"));
        assert!(t.contains("answer directly in your response"));
        assert!(t.contains("termade_status") && t.contains("termade_task_update"));
        assert!(t.contains("termade_ask"));
        assert!(t.contains("Communication Protocol") && t.contains("TASK_DONE"));
        assert!(
            t.contains("Zero Pleasantries Policy") && t.contains("terminate your turn immediately")
        );
        assert!(t.contains("Do not call termade_spawn"));
        // 不相關的段落不出現。
        assert!(!t.contains("termade_send"), "{t}");
        assert!(!t.contains("Worktree"), "{t}");
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
        assert!(t.contains("spawned by session \"coord\" (ses_p)"), "{t}");
        assert!(t.contains("termade_send (to=\"ses_p\")"), "{t}");
        assert!(t.contains("TASK_DONE") && t.contains("TASK_BLOCKED"), "{t}");
        assert!(t.contains("Zero Pleasantries Policy"), "{t}");
        assert!(t.contains("branch termade/ses_w") && t.contains("do not merge"));
        assert!(!t.contains("answer directly in your response"), "{t}");
        assert!(!t.contains("termade_task_create"), "{t}");
        assert!(t.contains("Do not call termade_spawn"), "{t}");
    }

    #[test]
    fn parent_gone_still_names_its_id() {
        let mut me = sess("ses_w", "worker/a", Role::Worker);
        me.parent = Some("ses_gone".into());
        let t = collab_instructions(&me, None, "");
        assert!(t.contains("termade_send (to=\"ses_gone\")"), "{t}");
        assert!(!t.contains("answer directly in your response"));
    }

    #[test]
    fn coordinator_gets_orchestration_section() {
        let me = sess("ses_c", "coord", Role::Coordinator);
        let t = collab_instructions(&me, None, "");
        assert!(t.contains("termade_task_create") && t.contains("depends_on"));
        assert!(t.contains("termade_wait") && t.contains("do not poll"));
        assert!(t.contains("termade_spawn"));
        assert!(!t.contains("Do not call termade_spawn"), "{t}");
        assert!(!t.contains("Worktree"), "{t}");
        // 協調者自己也在 worktree 且由 agent 產生：所有段落都在，仍在 30 行內。
        let top = sess("ses_top", "top", Role::Coordinator);
        let mut sub = sess("ses_c", "coord", Role::Coordinator);
        sub.parent = Some("ses_top".into());
        sub.branch = Some("termade/ses_c".into());
        let t = collab_instructions(&sub, Some(&top), "");
        assert!(
            t.contains("do not merge") && t.contains("termade_send") && t.contains("do not poll")
        );
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
        assert!(!t.contains("You are agent session"), "{t}");
        assert!(!t.contains("Reporting:"), "{t}");
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
        assert!(t.contains("truncated"));
    }

    #[test]
    fn global_prompt_file_is_included_before_extra() {
        let dir = temp_dir("pf");
        std::fs::write(dir.join("ade.md"), "全域規範：先寫測試").unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[collab]\nprompt_file = \"ade.md\"\nextra = \"附註\"\n",
        )
        .unwrap();
        let project = temp_dir("proj");
        let t = for_session(&State::default(), "nope", &dir, &project).unwrap();
        let (a, b) = (t.find("全域規範").unwrap(), t.find("附註").unwrap());
        assert!(a < b, "{t}");
    }

    #[test]
    fn coordinator_gets_token_economy_rules() {
        let me = sess("ses_c", "coordinator/x", Role::Coordinator);
        let t = collab_instructions(&me, None, "");
        assert!(
            t.contains("Token economy") && t.contains("QUOTA_WARN"),
            "{t}"
        );
        let w = collab_instructions(&sess("ses_w", "w", Role::Worker), None, "");
        assert!(!w.contains("Token economy"));
    }

    #[test]
    fn token_hints_are_added_only_when_enabled() {
        let global = temp_dir("collab-hints-g");
        let repo = temp_dir("collab-hints-r");
        let st = State::new();
        let off = for_session(&st, "nobody", &global, &repo).unwrap();
        assert!(!off.contains("Token economy (tool use)"));
        std::fs::write(global.join("config.toml"), "[collab]\ntoken_hints = true\n").unwrap();
        let on = for_session(&st, "nobody", &global, &repo).unwrap();
        assert!(on.contains("Token economy (tool use)"), "{on}");
        // 專案設定可以覆寫全域設定。
        std::fs::write(
            repo.join(".termade").join("config.toml"),
            "[collab]\ntoken_hints = false\n",
        )
        .unwrap();
        let local_off = for_session(&st, "nobody", &global, &repo).unwrap();
        assert!(
            !local_off.contains("Token economy (tool use)"),
            "{local_off}"
        );
    }

    #[test]
    fn inject_skill_appends_body_only_when_enabled() {
        let global = temp_dir("collab-skill-g");
        let repo = temp_dir("collab-skill-r");
        let st = State::new();
        let off = for_session(&st, "nobody", &global, &repo).unwrap();
        assert!(!off.contains("# termade ADE collaboration"));
        std::fs::write(
            global.join("config.toml"),
            "[collab]\ninject_skill = true\n",
        )
        .unwrap();
        let on = for_session(&st, "nobody", &global, &repo).unwrap();
        assert!(on.contains("# termade ADE collaboration"), "{on}");
    }
}
