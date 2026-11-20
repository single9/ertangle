//! Helix 風格的 Space 選單 (which-key)與 spawn 選擇器 (agent → model → prompt)。

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ertangle_core::{AgentKind, Role};
use tr::tr;

use crate::app::{App, Mode};
use crate::commands::execute_command;
use crate::hosts::HostRouter;

/// 選單項目的動作。
#[derive(Clone, Copy)]
pub(crate) enum Act {
    /// 進入子選單。
    Menu,
    /// 執行命令 (等同在 `:` 輸入)。
    Cmd(&'static str),
    /// 開啟 spawn 選擇器。
    Spawn,
    /// 開啟交接選擇器 (選接手的 agent 與 model)。
    Handoff,
    /// 切到輸入模式並預填文字。
    Prefill(Mode, &'static str),
}

#[derive(Clone, Copy)]
pub(crate) struct Item {
    pub key: char,
    pub label: &'static str,
    pub act: Act,
}

const fn it(key: char, label: &'static str, act: Act) -> Item {
    Item { key, label, act }
}

/// 依目前路徑回傳選單標題與項目；路徑不存在回傳 `None`。
pub(crate) fn menu_items(path: &[char]) -> Option<(&'static str, Vec<Item>)> {
    Some(match path {
        [] => (
            "SPC",
            vec![
                it('s', "session ▸", Act::Menu),
                it('g', "git / worktree ▸", Act::Menu),
                it('v', "view ▸", Act::Menu),
                it('m', "message ▸", Act::Menu),
                it(
                    ',',
                    ertangle_core::tr_noop!("設定 (預設 model／effort／權限…)"),
                    Act::Cmd("settings"),
                ),
                it(
                    'w',
                    ertangle_core::tr_noop!("Web 版 (網址與 QR Code)"),
                    Act::Cmd("web qr"),
                ),
                it(
                    'y',
                    ertangle_core::tr_noop!("複製內容 (Transcript 預設最新訊息)"),
                    Act::Cmd("copy"),
                ),
                it(
                    '?',
                    ertangle_core::tr_noop!("說明 (所有按鍵與命令)"),
                    Act::Cmd("help"),
                ),
                it(
                    'q',
                    ertangle_core::tr_noop!("離開 (保留 daemon)"),
                    Act::Cmd("quit"),
                ),
                it(
                    'x',
                    ertangle_core::tr_noop!("結束本專案所有 session"),
                    Act::Cmd("stop"),
                ),
                it(
                    'Q',
                    ertangle_core::tr_noop!("關閉共用 daemon 並離開"),
                    Act::Cmd("shutdown"),
                ),
            ],
        ),
        ['s'] => (
            "SPC s · session",
            vec![
                it(
                    'n',
                    ertangle_core::tr_noop!("新增 agent… (選 agent 與 model)"),
                    Act::Spawn,
                ),
                it(
                    'b',
                    ertangle_core::tr_noop!("啟動多 Agent 團隊範本 (playbook)"),
                    Act::Cmd("playbook"),
                ),
                it(
                    'h',
                    ertangle_core::tr_noop!("交接給其他 agent (額度用完時)"),
                    Act::Handoff,
                ),
                it(
                    'r',
                    ertangle_core::tr_noop!("接續已結束的 session (接回原對話)"),
                    Act::Cmd("resume"),
                ),
                it(
                    'm',
                    ertangle_core::tr_noop!("切換 model (執行中)"),
                    Act::Prefill(Mode::Command, "model "),
                ),
                it(
                    'e',
                    ertangle_core::tr_noop!("切換 effort (執行中)"),
                    Act::Prefill(Mode::Command, "effort "),
                ),
                it(
                    'c',
                    ertangle_core::tr_noop!("中斷目前回合"),
                    Act::Cmd("cancel"),
                ),
                it(
                    'p',
                    ertangle_core::tr_noop!("處理程序與 Subagent"),
                    Act::Cmd("ps"),
                ),
                it(
                    'q',
                    ertangle_core::tr_noop!("各 agent 額度 (用量與重置時間)"),
                    Act::Cmd("quota"),
                ),
                it(
                    'k',
                    ertangle_core::tr_noop!("結束 session"),
                    Act::Cmd("kill"),
                ),
                it('z', ertangle_core::tr_noop!("休眠"), Act::Cmd("sleep")),
                it('w', ertangle_core::tr_noop!("喚醒"), Act::Cmd("wake")),
                it(
                    'x',
                    ertangle_core::tr_noop!("移除已結束的 session"),
                    Act::Cmd("rm"),
                ),
                it(
                    'X',
                    ertangle_core::tr_noop!("清除所有已結束的 session"),
                    Act::Cmd("clean"),
                ),
            ],
        ),
        ['g'] => (
            "SPC g · git / worktree",
            vec![
                it('d', "diff", Act::Cmd("diff")),
                it('w', "worktrees", Act::Cmd("worktrees")),
                it(
                    'm',
                    ertangle_core::tr_noop!("merge 回主工作區"),
                    Act::Cmd("merge"),
                ),
                it(
                    'n',
                    ertangle_core::tr_noop!("merge 乾跑 (dry-run)"),
                    Act::Cmd("merge --dry-run"),
                ),
                it(
                    'r',
                    ertangle_core::tr_noop!("合併並委派解衝突"),
                    Act::Cmd("merge --resolve"),
                ),
                it(
                    'c',
                    ertangle_core::tr_noop!("gc 已合併的 worktree"),
                    Act::Cmd("gc"),
                ),
            ],
        ),
        ['v'] => (
            "SPC v · view",
            vec![
                it('o', "overview", Act::Cmd("overview")),
                it('k', ertangle_core::tr_noop!("任務視圖"), Act::Cmd("tasks")),
                it('l', ertangle_core::tr_noop!("切換版面"), Act::Cmd("layout")),
                it(
                    'g',
                    ertangle_core::tr_noop!("網格檢視 (一次看 4 個 session)"),
                    Act::Cmd("grid"),
                ),
                it(
                    'r',
                    ertangle_core::tr_noop!("原始文字／Markdown"),
                    Act::Cmd("raw"),
                ),
                it(
                    't',
                    ertangle_core::tr_noop!("顯示／隱藏思考"),
                    Act::Cmd("thoughts"),
                ),
                it(
                    'x',
                    ertangle_core::tr_noop!("展開／摺疊工具呼叫"),
                    Act::Cmd("tools"),
                ),
                it(
                    'p',
                    ertangle_core::tr_noop!("只看本專案／全部專案"),
                    Act::Cmd("scope"),
                ),
                it(
                    'b',
                    ertangle_core::tr_noop!("顯示／隱藏 Bus"),
                    Act::Cmd("bus"),
                ),
            ],
        ),
        ['m'] => (
            "SPC m · message",
            vec![
                it(
                    'm',
                    ertangle_core::tr_noop!("訊息給選取的 session"),
                    Act::Prefill(Mode::Message, ""),
                ),
                it(
                    'b',
                    ertangle_core::tr_noop!("廣播給所有人"),
                    Act::Prefill(Mode::Command, "broadcast "),
                ),
                it(
                    't',
                    ertangle_core::tr_noop!("新增任務"),
                    Act::Prefill(Mode::Command, "task "),
                ),
                it(
                    'f',
                    ertangle_core::tr_noop!("附加檔案／圖片… (@路徑，Tab 自動完成)"),
                    Act::Prefill(Mode::Prompt, "@"),
                ),
            ],
        ),
        _ => return None,
    })
}

fn close(app: &mut App) {
    app.mode = Mode::Normal;
    app.menu_path.clear();
}

pub(crate) fn open_menu(app: &mut App) {
    app.mode = Mode::Menu;
    app.menu_path.clear();
}

pub(crate) fn handle_menu_key(app: &mut App, writer: &mut HostRouter, key: KeyEvent) -> Result<()> {
    match key.code {
        KeyCode::Esc => close(app),
        KeyCode::Backspace => {
            if app.menu_path.pop().is_none() {
                close(app);
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let Some((_, items)) = menu_items(&app.menu_path) else {
                close(app);
                return Ok(());
            };
            let Some(item) = items.iter().find(|i| i.key == c).copied() else {
                app.status = tr!("選單沒有 `{c}`(Backspace 返回、Esc 關閉)", c = c);
                return Ok(());
            };
            match item.act {
                Act::Menu => app.menu_path.push(c),
                Act::Cmd(cmd) => {
                    close(app);
                    execute_command(app, writer, cmd)?;
                }
                Act::Spawn => {
                    close(app);
                    start_spawn_picker(app);
                }
                Act::Handoff => {
                    close(app);
                    start_handoff_picker(app);
                }
                Act::Prefill(mode, text) => {
                    close(app);
                    app.mode = mode;
                    app.input = text.to_string();
                    app.input_cursor = text.chars().count();
                }
            }
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// spawn 選擇器
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ResumeScope {
    #[default]
    Here,
    All,
}

#[derive(Clone)]
pub(crate) enum PickStage {
    Agent,
    Model(AgentKind),
    HandoffRole {
        kind: AgentKind,
        model: Option<String>,
    },
    /// 回答互動問題。
    Question {
        id: String,
        session: ertangle_core::SessionId,
        multi_select: bool,
        selected: std::collections::HashSet<String>,
    },
    /// 選擇多 Agent 團隊範本。
    Playbook,
    /// 選擇要接續的已結束 session。
    Resume,
    /// Pick the project to switch to.
    Project,
}

pub(crate) struct Picker {
    pub stage: PickStage,
    pub resume_scope: ResumeScope,
    pub title: String,
    /// (顯示文字, 值)；值為空字串代表「預設」。
    pub items: Vec<(String, String)>,
    pub filter: String,
    pub cursor: usize,
    /// 背景查詢 agent 可用 model 的結果 (查到後併入 `items`)。
    pub pending: Option<std::sync::mpsc::Receiver<Vec<(String, String)>>>,
    pub loading: bool,
    /// Antigravity 的 model id 把 effort 寫在尾巴 (`-low`／`-medium`…)：清單只列基底名稱，
    /// 這裡記錄每個基底有哪些 effort 變體。
    pub group_effort: bool,
    pub variants: std::collections::HashMap<String, Vec<String>>,
}

/// 把 `base-medium` 這類 id 拆成 (base, effort)；沒有 effort 尾巴就回 `None`。
pub(crate) fn split_effort_suffix(id: &str) -> Option<(&str, &str)> {
    let (base, suffix) = id.rsplit_once('-')?;
    ertangle_adapter::EFFORT_LEVELS
        .contains(&suffix)
        .then_some((base, suffix))
}

impl Picker {
    /// 把背景查詢的結果併進清單 (每次繪製前呼叫)。
    pub(crate) fn poll_pending(&mut self) {
        let Some(rx) = &self.pending else { return };
        match rx.try_recv() {
            Ok(found) => {
                // 先依基底名稱分組，再決定要不要合併：只有出現兩個以上 effort 變體才視為
                // 「同一個 model 的不同 effort」(ACP 的 model 選項不含 effort 尾巴)。
                let mut groups: Vec<(String, Vec<(String, String)>)> = Vec::new();
                for (label, value) in found {
                    let key = match split_effort_suffix(&value) {
                        Some((base, _)) if self.group_effort => base.to_string(),
                        _ => value.clone(),
                    };
                    match groups.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, v)) => v.push((label, value)),
                        None => groups.push((key, vec![(label, value)])),
                    }
                }
                for (base, members) in groups {
                    let merge = self.group_effort
                        && members.len() >= 2
                        && members
                            .iter()
                            .all(|(_, v)| split_effort_suffix(v).is_some());
                    if merge {
                        for (_, v) in &members {
                            if let Some((_, eff)) = split_effort_suffix(v) {
                                self.variants
                                    .entry(base.clone())
                                    .or_default()
                                    .push(eff.to_string());
                            }
                        }
                        // 顯示名稱去掉尾端的 `(Medium)` 這類 effort 標記。
                        let name = members[0]
                            .0
                            .split_once("  (")
                            .map(|(_, n)| n.trim_end_matches(')'))
                            .and_then(|n| n.rsplit_once(" ("))
                            .map(|(n, _)| n.to_string());
                        let label = match name {
                            Some(n) => format!("{base}  ({n})"),
                            None => base.clone(),
                        };
                        if !self.items.iter().any(|(_, v)| *v == base) {
                            self.items.push((label, base));
                        }
                    } else {
                        for (label, value) in members {
                            if !self.items.iter().any(|(_, v)| *v == value) {
                                self.items.push((label, value));
                            }
                        }
                    }
                }
                self.loading = false;
                self.pending = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(_) => {
                self.loading = false;
                self.pending = None;
            }
        }
    }

    pub(crate) fn visible(&self) -> Vec<&(String, String)> {
        let f = self.filter.to_ascii_lowercase();
        self.items
            .iter()
            .filter(|(label, value)| {
                f.is_empty()
                    || label.to_ascii_lowercase().contains(&f)
                    || value.to_ascii_lowercase().contains(&f)
            })
            .collect()
    }
}

/// 某個 agent 可選的 model：預設 + 內建清單 + config.toml 的 `models`／`model`。
pub(crate) fn model_choices(kind: &AgentKind) -> Vec<(String, String)> {
    let mut out = vec![(tr!("預設 (依 config.toml)"), String::new())];
    if matches!(kind, AgentKind::ClaudeCode) {
        out.extend(
            ertangle_adapter::models::CLAUDE_MODELS
                .iter()
                .map(|(id, name)| {
                    let name = ertangle_core::tr_dyn!(name);
                    (format!("{id}  ({name})"), id.to_string())
                }),
        );
    }
    let mut values: Vec<String> = Vec::new();
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = ertangle_daemon::config::Config::load(&ertangle_daemon::config_dir(), &cwd);
    if let Some(a) = cfg.agents.get(kind.id()) {
        for m in a.model.iter().chain(a.models.iter()) {
            if !values.contains(m) && !out.iter().any(|(_, v)| v == m) {
                values.push(m.clone());
            }
        }
    }
    out.extend(values.into_iter().map(|v| (v.clone(), v)));
    out
}

pub(crate) use ertangle_adapter::models::query_models;

/// 交接：選接手的 agent 與 model，選完直接送出 (簡報由 daemon 產生)。
pub(crate) fn start_handoff_picker(app: &mut App) {
    let Some(src) = app.selected_id() else {
        app.status = tr!("沒有選取的 session");
        return;
    };
    start_spawn_picker(app);
    app.handoff_from = Some(src);
    if let Some(p) = app.picker.as_mut() {
        p.title = tr!("交接給哪個 agent");
    }
}

pub(crate) fn start_question_picker(app: &mut App, q: &ertangle_core::UserQuestion) {
    let mut items: Vec<(String, String)> = Vec::new();
    for opt in &q.options {
        let label = match &opt.description {
            Some(desc) if !desc.is_empty() => format!("{} ({})", opt.label, desc),
            _ => opt.label.clone(),
        };
        items.push((label, opt.label.clone()));
    }
    let title = match &q.header {
        Some(h) if !h.is_empty() => format!("{}: {}", h, q.question),
        _ => q.question.clone(),
    };
    let title_suffix = if q.multi_select {
        tr!(" (Space 勾選 · Enter 送出 · Esc 取消)")
    } else {
        tr!(" (Enter 選擇 · 可輸入自訂答案 · Esc 取消)")
    };
    app.mode = Mode::Pick;
    app.picker = Some(Picker {
        stage: PickStage::Question {
            id: q.id.clone(),
            session: q.session.clone(),
            multi_select: q.multi_select,
            selected: std::collections::HashSet::new(),
        },
        resume_scope: ResumeScope::default(),
        title: format!("{title}{title_suffix}"),
        items,
        filter: String::new(),
        cursor: 0,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
    });
}

pub(crate) fn start_spawn_picker(app: &mut App) {
    app.handoff_from = None;
    // 使用者設定的 worker 順序排前面，其餘維持內建順序。
    let order = ertangle_daemon::config::Config::load_file(&crate::settings::config_path())
        .defaults
        .worker_order;
    let rank = |id: &str| order.iter().position(|o| o == id).unwrap_or(usize::MAX);
    let mut kinds = AgentKind::builtins().to_vec();
    kinds.sort_by_key(|k| rank(k.id()));
    let items = kinds
        .iter()
        .map(|k| (k.id().to_string(), k.id().to_string()))
        .collect();
    app.picker = Some(Picker {
        stage: PickStage::Agent,
        resume_scope: ResumeScope::default(),
        title: tr!("選擇 agent"),
        items,
        filter: String::new(),
        cursor: 0,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
    });
    app.mode = Mode::Pick;
}

/// `Space s b`／`:playbook`：列出內建與自訂團隊範本，選中後再輸入目標需求。
pub(crate) fn start_playbook_picker(app: &mut App) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = ertangle_daemon::config::Config::load(&ertangle_daemon::config_dir(), &cwd);
    let items: Vec<(String, String)> = cfg
        .playbooks()
        .into_iter()
        .map(|(name, pb)| (format!("{name}  ({})", pb.description), name))
        .collect();
    if items.is_empty() {
        app.status = tr!("沒有可用的 playbook");
        return;
    }
    app.picker = Some(Picker {
        stage: PickStage::Playbook,
        resume_scope: ResumeScope::default(),
        title: tr!("選擇團隊範本 (選完輸入目標需求)"),
        items,
        filter: String::new(),
        cursor: 0,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
    });
    app.mode = Mode::Pick;
}

pub(crate) fn format_compact_age(now_ms: u64, created_ms: u64) -> String {
    let secs = now_ms.saturating_sub(created_ms) / 1000;
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3600 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}m", secs / 60)
    }
}

/// Rebuild resume picker items: filter by scope, add parent annotation and project prefix.
pub(crate) fn rebuild_resume_items(app: &App, scope: ResumeScope) -> Vec<(String, String)> {
    let mut ended: Vec<(&ertangle_core::Session, bool)> = Vec::new();
    // 收集本機與多主機的已結束及封存歷史 session
    for (host_name, h) in &app.hosts {
        if app.host_filter.as_ref().is_some_and(|f| f != host_name) {
            continue;
        }
        for s in h.state.sessions.values().filter(|s| !s.status.is_live()) {
            if !ended.iter().any(|(existing, _)| existing.id == s.id) {
                ended.push((s, false));
            }
        }
        for s in h
            .state
            .archived_sessions
            .values()
            .filter(|s| s.native_id.is_some())
        {
            if !ended.iter().any(|(existing, _)| existing.id == s.id) {
                ended.push((s, true));
            }
        }
    }
    for s in app.state.sessions.values().filter(|s| !s.status.is_live()) {
        if !ended.iter().any(|(existing, _)| existing.id == s.id) {
            ended.push((s, false));
        }
    }
    for s in app
        .state
        .archived_sessions
        .values()
        .filter(|s| s.native_id.is_some())
    {
        if !ended.iter().any(|(existing, _)| existing.id == s.id) {
            ended.push((s, true));
        }
    }

    // Build an id -> label map covering live, ended, and archived sessions on all hosts.
    let mut parent_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for h in app.hosts.values() {
        for s in h
            .state
            .sessions
            .values()
            .chain(h.state.archived_sessions.values())
        {
            parent_map.insert(s.id.clone(), s.label.clone());
        }
    }
    for s in app
        .state
        .sessions
        .values()
        .chain(app.state.archived_sessions.values())
    {
        parent_map.insert(s.id.clone(), s.label.clone());
    }

    // Collect all project roots so project_name can disambiguate identical names.
    let mut all_roots: Vec<String> = Vec::new();
    for (s, _) in &ended {
        let root = s.project_root().to_string();
        if !all_roots.contains(&root) {
            all_roots.push(root);
        }
    }
    let all_roots_refs: Vec<&str> = all_roots.iter().map(|s| s.as_str()).collect();

    if scope == ResumeScope::Here {
        ended.retain(|(s, _)| s.project_root() == app.home_project);
    }

    ended.sort_by_key(|(a, _)| std::cmp::Reverse(a.created_ms));
    let now_ms = ertangle_core::now_millis();
    ended
        .into_iter()
        .map(|(s, is_archived)| {
            let kind_model = match &s.model {
                Some(m) if !m.is_empty() => format!("{}:{}", s.kind.id(), m),
                _ => s.kind.id().to_string(),
            };
            let age = format_compact_age(now_ms, s.created_ms);
            let resumable = if s.native_id.is_some() { "↻" } else { "–" };
            let parent_suffix = match &s.parent {
                Some(pid) => {
                    let plabel = parent_map
                        .get(pid)
                        .cloned()
                        .unwrap_or_else(|| crate::app::short(pid));
                    format!("  ↳ {plabel}")
                }
                None => String::new(),
            };
            let proj_prefix = if scope == ResumeScope::All {
                let pname = crate::app::project_name(s.project_root(), &all_roots_refs);
                format!("[{pname}] ")
            } else {
                String::new()
            };
            let tag_part = if is_archived {
                let tag = tr!("封存");
                format!(" [{tag}]")
            } else {
                String::new()
            };
            let label = format!(
                "{proj_prefix}{} {}{tag_part}  {}  {}  {}{parent_suffix}",
                s.status.glyph(),
                s.label,
                kind_model,
                age,
                resumable
            );
            (label, s.id.clone())
        })
        .collect()
}

pub(crate) fn start_resume_picker(app: &mut App) {
    let all_items = rebuild_resume_items(app, ResumeScope::All);
    if all_items.is_empty() {
        app.status = tr!("沒有可接續的 session (已結束且具備對話 id)");
        return;
    }

    let resume_scope = if app.scope_here {
        ResumeScope::Here
    } else {
        ResumeScope::All
    };
    let items = if resume_scope == ResumeScope::All {
        all_items
    } else {
        rebuild_resume_items(app, ResumeScope::Here)
    };

    let cursor = app
        .selected_id()
        .and_then(|sel| items.iter().position(|(_, id)| id == &sel))
        .unwrap_or(0);

    app.picker = Some(Picker {
        stage: PickStage::Resume,
        resume_scope,
        title: tr!("接續 session (Tab 切換專案範圍 · Enter 確認 · Esc 取消)"),
        items,
        filter: String::new(),
        cursor,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
    });
    app.mode = Mode::Pick;
}

/// `:project` / `:project open`: list configured and observed project roots, then switch to the chosen one.
pub(crate) fn start_project_picker(app: &mut App) {
    let global_dir = ertangle_daemon::config_dir();
    let cfg = ertangle_daemon::config::Config::load(&global_dir, &app.active_project);

    let mut roots: Vec<String> = Vec::new();
    let mut add_root = |r: &str| {
        let trimmed = r.trim();
        if !trimmed.is_empty() && !roots.iter().any(|existing| existing == trimmed) {
            roots.push(trimmed.to_string());
        }
    };

    add_root(&app.home_project);
    for r in &cfg.tui.projects {
        add_root(r);
    }
    for s in app.state.sessions.values() {
        add_root(s.project_root());
    }
    for s in app.state.archived_sessions.values() {
        add_root(s.project_root());
    }
    for h in app.hosts.values() {
        for s in h.state.sessions.values() {
            add_root(s.project_root());
        }
        for s in h.state.archived_sessions.values() {
            add_root(s.project_root());
        }
    }

    if roots.is_empty() {
        app.status = tr!("沒有可用的專案清單");
        return;
    }

    let all_roots_refs: Vec<&str> = roots.iter().map(|s| s.as_str()).collect();
    let items: Vec<(String, String)> = roots
        .iter()
        .map(|r| {
            let pname = crate::app::project_name(r, &all_roots_refs);
            let is_curr = r == &app.home_project;
            let mark = if is_curr { "* " } else { "  " };
            let label = format!("{mark}{:<16} {r}", pname);
            (label, r.clone())
        })
        .collect();

    let cursor = items
        .iter()
        .position(|(_, path)| path == &app.home_project)
        .unwrap_or(0);

    app.picker = Some(Picker {
        stage: PickStage::Project,
        resume_scope: ResumeScope::default(),
        title: tr!("選擇專案 (Enter 確認 · Esc 取消)"),
        items,
        filter: String::new(),
        cursor,
        pending: None,
        loading: false,
        group_effort: false,
        variants: Default::default(),
    });
    app.mode = Mode::Pick;
}

fn finish_picker(
    app: &mut App,
    writer: &mut HostRouter,
    kind: AgentKind,
    model: Option<String>,
    effort: Option<String>,
) -> Result<()> {
    app.picker = None;
    app.mode = Mode::Normal;
    if let Some(src) = app.handoff_from.take() {
        crate::send(
            writer,
            &ertangle_protocol::Request::Handoff {
                session: src,
                kind: kind.clone(),
                model: model.clone(),
                effort,
                note: None,
                role: None,
                parent: None,
                force: false,
            },
        )?;
        app.status = tr!(
            "已要求交接給 {target} (舊 session 會停止，新 agent 沿用同一個工作目錄)",
            target = format!(
                "{}{}",
                kind.id(),
                model.map(|m| format!(":{m}")).unwrap_or_default()
            )
        );
        app.spawn_variants.clear();
        return Ok(());
    }
    app.status = tr!(
        "spawn → {target}：輸入 prompt 後 Enter (可留空)，Ctrl-R 切換角色，Ctrl-F 切換 effort",
        target = format!(
            "{}{}{}",
            kind.id(),
            model
                .as_deref()
                .map(|m| format!(":{m}"))
                .unwrap_or_default(),
            effort
                .as_deref()
                .map(|e| format!(" effort={e}"))
                .unwrap_or_default(),
        )
    );
    app.spawn_target = Some((kind, model));
    app.spawn_effort = effort;
    app.mode = Mode::Spawn;
    app.input.clear();
    app.input_cursor = 0;
    app.spawn_worktree = false;
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = ertangle_daemon::config::Config::load(&ertangle_daemon::config_dir(), &cwd);
    app.spawn_role = cfg
        .defaults
        .role
        .as_deref()
        .map(Role::parse)
        .unwrap_or(Role::Coordinator);
    app.spawn_permission = None;
    app.spawn_bypass_armed = false;
    Ok(())
}

pub(crate) fn handle_pick_key(app: &mut App, writer: &mut HostRouter, key: KeyEvent) -> Result<()> {
    let Some(p) = app.picker.as_mut() else {
        app.mode = Mode::Normal;
        return Ok(());
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let n = p.visible().len();
    match key.code {
        KeyCode::Esc => {
            app.picker = None;
            app.mode = Mode::Normal;
        }
        KeyCode::Tab | KeyCode::BackTab => {
            if matches!(p.stage, PickStage::Resume) {
                let new_scope = match p.resume_scope {
                    ResumeScope::Here => ResumeScope::All,
                    ResumeScope::All => ResumeScope::Here,
                };
                let old_cursor = p.cursor;
                let items = rebuild_resume_items(app, new_scope);
                if let Some(p) = app.picker.as_mut() {
                    p.resume_scope = new_scope;
                    p.items = items;
                    let n = p.visible().len();
                    p.cursor = if n > 0 { old_cursor.min(n - 1) } else { 0 };
                }
                return Ok(());
            }
            if n > 0 {
                if key.code == KeyCode::Tab {
                    p.cursor = (p.cursor + 1) % n;
                } else {
                    p.cursor = (p.cursor + n - 1) % n;
                }
            }
        }
        KeyCode::Down => {
            if n > 0 {
                p.cursor = (p.cursor + 1) % n;
            }
        }
        KeyCode::Up => {
            if n > 0 {
                p.cursor = (p.cursor + n - 1) % n;
            }
        }
        KeyCode::Char('t') if ctrl => {
            if matches!(p.stage, PickStage::Resume) {
                let new_scope = match p.resume_scope {
                    ResumeScope::Here => ResumeScope::All,
                    ResumeScope::All => ResumeScope::Here,
                };
                let old_cursor = p.cursor;
                let items = rebuild_resume_items(app, new_scope);
                if let Some(p) = app.picker.as_mut() {
                    p.resume_scope = new_scope;
                    p.items = items;
                    let n = p.visible().len();
                    p.cursor = if n > 0 { old_cursor.min(n - 1) } else { 0 };
                }
                return Ok(());
            }
        }
        KeyCode::Char('n') if ctrl => {
            if n > 0 {
                p.cursor = (p.cursor + 1) % n;
            }
        }
        KeyCode::Char('p') if ctrl => {
            if n > 0 {
                p.cursor = (p.cursor + n - 1) % n;
            }
        }
        KeyCode::Backspace => {
            p.filter.pop();
            p.cursor = 0;
        }
        KeyCode::Char(' ') if !ctrl => {
            let is_multi_qst = matches!(
                p.stage,
                PickStage::Question {
                    multi_select: true,
                    ..
                }
            );
            if is_multi_qst {
                let chosen_val = p.visible().get(p.cursor).map(|(_, val)| val.clone());
                if let Some(val) = chosen_val {
                    if let PickStage::Question {
                        ref mut selected, ..
                    } = p.stage
                    {
                        if selected.contains(&val) {
                            selected.remove(&val);
                        } else {
                            selected.insert(val);
                        }
                    }
                    return Ok(());
                }
            }
            p.filter.push(' ');
            p.cursor = 0;
        }
        KeyCode::Char(c) if !ctrl => {
            p.filter.push(c);
            p.cursor = 0;
        }
        KeyCode::Enter => {
            let chosen = p.visible().get(p.cursor).map(|(_, v)| v.clone());
            let custom = p.filter.trim().to_string();
            match p.stage.clone() {
                PickStage::Agent => {
                    // 沒有符合的項目時，也接受別名 (例如 agy、oc)。
                    let chosen =
                        chosen.or_else(|| AgentKind::parse(&custom).map(|k| k.id().to_string()));
                    let Some(kind) = chosen.and_then(|v| AgentKind::parse(&v)) else {
                        app.status = tr!("沒有符合的 agent");
                        return Ok(());
                    };
                    p.title = tr!(
                        "{agent} 的 model (輸入可過濾；無符合時 Enter 使用輸入值)",
                        agent = kind.id()
                    );
                    p.items = model_choices(&kind);
                    p.filter.clear();
                    p.cursor = 0;
                    // 向 CLI 查詢可用 model (不阻塞介面)。
                    let (tx, rx) = std::sync::mpsc::channel();
                    let k = kind.clone();
                    if matches!(
                        k,
                        AgentKind::OpenCode | AgentKind::Antigravity | AgentKind::Codex
                    ) {
                        std::thread::spawn(move || {
                            let _ = tx.send(query_models(&k));
                        });
                        p.pending = Some(rx);
                        p.loading = true;
                        p.group_effort = matches!(kind, AgentKind::Antigravity);
                    }
                    p.stage = PickStage::Model(kind);
                }
                PickStage::Model(kind) => {
                    // 有符合的項目就用它；清單是空的但有輸入，就當成自訂 model。
                    let model = match chosen {
                        Some(v) if !v.is_empty() => Some(v),
                        Some(_) => None,
                        None if !custom.is_empty() => Some(custom),
                        None => None,
                    };
                    if let Some(src_id) = app.handoff_from.clone() {
                        let cur_role = app
                            .state
                            .sessions
                            .get(&src_id)
                            .map(|s| s.role.clone())
                            .unwrap_or(Role::Worker);
                        let is_coord = cur_role == Role::Coordinator;
                        let running_children_count = app
                            .state
                            .sessions
                            .values()
                            .filter(|s| s.parent.as_deref() == Some(&src_id) && s.status.is_live())
                            .count();
                        let role_choices = [Role::Coordinator, Role::Worker, Role::Reviewer];
                        let mut items = Vec::new();
                        let mut default_idx = 0;
                        for (i, r) in role_choices.iter().enumerate() {
                            if *r == cur_role {
                                default_idx = i;
                            }
                            let label = if is_coord
                                && *r != Role::Coordinator
                                && running_children_count > 0
                            {
                                tr!(
                                    "{role} (警告：仍有 {count} 個執行中子 session)",
                                    role = r.id(),
                                    count = running_children_count
                                )
                            } else {
                                r.id().to_string()
                            };
                            items.push((label, r.id().to_string()));
                        }
                        p.title = tr!("交接後的角色 (Enter 確認 · Esc 取消)");
                        p.items = items;
                        p.filter.clear();
                        p.cursor = default_idx;
                        p.pending = None;
                        p.loading = false;
                        p.group_effort = false;
                        p.stage = PickStage::HandoffRole { kind, model };
                        return Ok(());
                    }
                    app.spawn_variants = model
                        .as_deref()
                        .and_then(|m| p.variants.get(m).cloned())
                        .unwrap_or_default();
                    finish_picker(app, writer, kind, model, None)?;
                }
                PickStage::HandoffRole { kind, model } => {
                    let chosen_role_str = chosen.unwrap_or_else(|| {
                        if custom.is_empty() {
                            "worker".into()
                        } else {
                            custom
                        }
                    });
                    let new_role = Role::parse(&chosen_role_str);
                    app.picker = None;
                    app.mode = Mode::Normal;
                    if let Some(src) = app.handoff_from.take() {
                        let is_coord = app
                            .state
                            .sessions
                            .get(&src)
                            .is_some_and(|s| s.role == Role::Coordinator);
                        let has_running_children = app
                            .state
                            .sessions
                            .values()
                            .any(|s| s.parent.as_deref() == Some(&src) && s.status.is_live());
                        let force =
                            is_coord && new_role != Role::Coordinator && has_running_children;
                        crate::send(
                            writer,
                            &ertangle_protocol::Request::Handoff {
                                session: src,
                                kind: kind.clone(),
                                model: model.clone(),
                                effort: None,
                                note: None,
                                role: Some(new_role.clone()),
                                parent: None,
                                force,
                            },
                        )?;
                        app.status = tr!(
                            "已要求交接給 {target} (角色：{role})",
                            target = format!(
                                "{}{}",
                                kind.id(),
                                model.map(|m| format!(":{m}")).unwrap_or_default()
                            ),
                            role = new_role.id()
                        );
                        app.spawn_variants.clear();
                    }
                }
                PickStage::Playbook => {
                    let Some(name) = chosen.filter(|n| !n.is_empty()) else {
                        app.status = tr!("沒有符合的 playbook");
                        return Ok(());
                    };
                    app.picker = None;
                    app.mode = Mode::Command;
                    app.input = format!("playbook {name} ");
                    app.input_cursor = app.input.chars().count();
                    app.status = tr!("輸入目標需求後按 Enter 啟動 playbook {name}", name = name);
                }
                PickStage::Resume => {
                    let Some(id) = chosen else {
                        return Ok(());
                    };
                    let found = app
                        .state
                        .sessions
                        .get(&id)
                        .or_else(|| app.state.archived_sessions.get(&id))
                        .or_else(|| {
                            app.hosts.values().find_map(|h| {
                                h.state
                                    .sessions
                                    .get(&id)
                                    .or_else(|| h.state.archived_sessions.get(&id))
                            })
                        })
                        .cloned();
                    if let Some(s) = found {
                        let label = s.label.clone();
                        if !s.status.is_live() && s.native_id.is_some() {
                            app.select_or_pend(&id);
                            crate::send(
                                writer,
                                &ertangle_protocol::Request::Resume {
                                    session: id,
                                    prompt: None,
                                },
                            )?;
                            app.picker = None;
                            app.mode = Mode::Normal;
                            app.status = tr!("已要求接續 {label}", label = label);
                        } else if !s.status.is_live() {
                            app.status = tr!(
                                "{label} 沒有可接續的對話 id (PTY、舊版 log，或 agent 沒回報就結束)",
                                label = label
                            );
                        }
                    }
                }
                PickStage::Question {
                    id,
                    session,
                    multi_select,
                    selected,
                } => {
                    let answers: Vec<String> = if multi_select {
                        let mut ans: Vec<String> = selected.into_iter().collect();
                        if ans.is_empty() {
                            if let Some(v) = chosen {
                                if !v.is_empty() {
                                    ans.push(v);
                                }
                            } else if !custom.is_empty() {
                                ans.push(custom);
                            }
                        }
                        ans
                    } else if !custom.is_empty()
                        && chosen.as_deref() != Some(&custom)
                        && p.visible().is_empty()
                    {
                        vec![custom]
                    } else if let Some(v) = chosen {
                        vec![v]
                    } else if !custom.is_empty() {
                        vec![custom]
                    } else {
                        Vec::new()
                    };
                    app.picker = None;
                    app.mode = Mode::Normal;
                    crate::send(
                        writer,
                        &ertangle_protocol::Request::AnswerQuestion {
                            session,
                            id,
                            answers,
                        },
                    )?;
                    app.status = tr!("已送出問題回覆");
                }
                PickStage::Project => {
                    let chosen_path = if chosen.is_some() {
                        chosen
                    } else if !custom.is_empty() {
                        Some(custom)
                    } else {
                        None
                    };
                    app.picker = None;
                    app.mode = Mode::Normal;
                    if let Some(target) = chosen_path {
                        if let Err(err) = app.set_active_project(std::path::Path::new(&target)) {
                            app.status = err;
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}
