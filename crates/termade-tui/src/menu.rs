//! Helix 風格的 Space 選單（which-key）與 spawn 選擇器（agent → model → prompt）。

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use termade_core::AgentKind;
use termade_daemon::ConnStream;

use crate::app::{App, Mode};
use crate::commands::execute_command;

/// 選單項目的動作。
#[derive(Clone, Copy)]
pub(crate) enum Act {
    /// 進入子選單。
    Menu,
    /// 執行命令（等同在 `:` 輸入）。
    Cmd(&'static str),
    /// 開啟 spawn 選擇器。
    Spawn,
    /// 開啟交接選擇器（選接手的 agent 與 model）。
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
                it('y', "複製目前畫面", Act::Cmd("copy")),
                it('?', "說明（所有按鍵與命令）", Act::Cmd("help")),
                it('q', "離開（保留 daemon）", Act::Cmd("quit")),
                it('Q', "關閉 daemon 並離開", Act::Cmd("shutdown")),
            ],
        ),
        ['s'] => (
            "SPC s · session",
            vec![
                it('n', "新增 agent…（選 agent 與 model）", Act::Spawn),
                it('h', "交接給其他 agent（額度用完時）", Act::Handoff),
                it('c', "中斷目前回合", Act::Cmd("cancel")),
                it('k', "結束 session", Act::Cmd("kill")),
                it('z', "休眠", Act::Cmd("sleep")),
                it('w', "喚醒", Act::Cmd("wake")),
                it('x', "移除已結束的 session", Act::Cmd("rm")),
                it('X', "清除所有已結束的 session", Act::Cmd("clean")),
            ],
        ),
        ['g'] => (
            "SPC g · git / worktree",
            vec![
                it('d', "diff", Act::Cmd("diff")),
                it('w', "worktrees", Act::Cmd("worktrees")),
                it('m', "merge 回主工作區", Act::Cmd("merge")),
                it('n', "merge 乾跑（dry-run）", Act::Cmd("merge --dry-run")),
                it('c', "gc 已合併的 worktree", Act::Cmd("gc")),
            ],
        ),
        ['v'] => (
            "SPC v · view",
            vec![
                it('o', "overview", Act::Cmd("overview")),
                it('l', "切換版面", Act::Cmd("layout")),
                it('r', "原始文字／Markdown", Act::Cmd("raw")),
                it('t', "顯示／隱藏思考", Act::Cmd("thoughts")),
                it('x', "展開／摺疊工具呼叫", Act::Cmd("tools")),
                it('p', "只看本專案／全部專案", Act::Cmd("scope")),
                it('b', "顯示／隱藏 Bus", Act::Cmd("bus")),
            ],
        ),
        ['m'] => (
            "SPC m · message",
            vec![
                it('m', "訊息給選取的 session", Act::Prefill(Mode::Message, "")),
                it(
                    'b',
                    "廣播給所有人",
                    Act::Prefill(Mode::Command, "broadcast "),
                ),
                it('t', "新增任務", Act::Prefill(Mode::Command, "task ")),
                it(
                    'f',
                    "附加檔案／圖片…（@路徑，Tab 補全）",
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

pub(crate) fn handle_menu_key(app: &mut App, writer: &mut ConnStream, key: KeyEvent) -> Result<()> {
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
                app.status = format!("選單沒有 `{c}`（Backspace 返回、Esc 關閉）");
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

#[derive(Clone)]
pub(crate) enum PickStage {
    Agent,
    Model(AgentKind),
}

pub(crate) struct Picker {
    pub stage: PickStage,
    pub title: String,
    /// (顯示文字, 值)；值為空字串代表「預設」。
    pub items: Vec<(String, String)>,
    pub filter: String,
    pub cursor: usize,
    /// 背景查詢 agent 可用 model 的結果（查到後併入 `items`）。
    pub pending: Option<std::sync::mpsc::Receiver<Vec<(String, String)>>>,
    pub loading: bool,
    /// Antigravity 的 model id 把 effort 寫在尾巴（`-low`／`-medium`…）：清單只列基底名稱，
    /// 這裡記錄每個基底有哪些 effort 變體。
    pub group_effort: bool,
    pub variants: std::collections::HashMap<String, Vec<String>>,
}

/// 把 `base-medium` 這類 id 拆成 (base, effort)；沒有 effort 尾巴就回 `None`。
pub(crate) fn split_effort_suffix(id: &str) -> Option<(&str, &str)> {
    let (base, suffix) = id.rsplit_once('-')?;
    termade_adapter::EFFORT_LEVELS
        .contains(&suffix)
        .then_some((base, suffix))
}

impl Picker {
    /// 把背景查詢的結果併進清單（每次繪製前呼叫）。
    pub(crate) fn poll_pending(&mut self) {
        let Some(rx) = &self.pending else { return };
        match rx.try_recv() {
            Ok(found) => {
                // 先依基底名稱分組，再決定要不要合併：只有出現兩個以上 effort 變體才視為
                // 「同一個 model 的不同 effort」（ACP 的 model 選項不含 effort 尾巴）。
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

/// 某個 agent 可選的 model：預設 + 內建建議 + config.toml 的 `models`／`model`。
pub(crate) fn model_choices(kind: &AgentKind) -> Vec<(String, String)> {
    let mut values: Vec<String> = match kind {
        AgentKind::ClaudeCode => ["sonnet", "opus", "haiku"].map(String::from).to_vec(),
        _ => Vec::new(),
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = termade_daemon::config::Config::load(&termade_daemon::config_dir(), &cwd);
    if let Some(a) = cfg.agents.get(kind.id()) {
        for m in a.model.iter().chain(a.models.iter()) {
            if !values.contains(m) {
                values.push(m.clone());
            }
        }
    }
    let mut out = vec![("預設（依 config.toml）".to_string(), String::new())];
    out.extend(values.into_iter().map(|v| (v.clone(), v)));
    out
}

/// 向 agent 的 CLI 查詢可用 model（`opencode models`、`agy models`），最多等 8 秒。
/// 輸出每行第一欄是 model id，後面（以 tab 分隔）是顯示名稱。
fn query_models(kind: &AgentKind) -> Vec<(String, String)> {
    let (bin, args): (&str, &[&str]) = match kind {
        AgentKind::OpenCode => ("opencode", &["models"]),
        AgentKind::Antigravity => ("agy", &["models"]),
        AgentKind::Codex => ("codex", &["debug", "models"]),
        _ => return Vec::new(),
    };
    let Ok(mut child) = std::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                return Vec::new();
            }
        }
    }
    let mut out = String::new();
    if let Some(mut so) = child.stdout.take() {
        use std::io::Read;
        let _ = so.read_to_string(&mut out);
    }
    if matches!(kind, AgentKind::Codex) {
        parse_codex_models(&out)
    } else {
        parse_models(&out)
    }
}

/// `codex debug models` 輸出 `{"models":[{slug,display_name,visibility}]}`；只取 `visibility == list`。
pub(crate) fn parse_codex_models(out: &str) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(out) else {
        return Vec::new();
    };
    v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"] == "list")
        .filter_map(|m| {
            let slug = m["slug"].as_str()?;
            let label = match m["display_name"].as_str() {
                Some(n) if !n.is_empty() => format!("{slug}  ({n})"),
                _ => slug.to_string(),
            };
            Some((label, slug.to_string()))
        })
        .collect()
}

/// 解析 `models` 子命令的輸出：忽略空行與沒有 id 樣子的提示行。
pub(crate) fn parse_models(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let id = cols.next()?.trim();
            if id.is_empty() || id.contains(char::is_whitespace) || id.ends_with("...") {
                return None;
            }
            let label = match cols.next().map(str::trim) {
                Some(name) if !name.is_empty() => format!("{id}  ({name})"),
                _ => id.to_string(),
            };
            Some((label, id.to_string()))
        })
        .collect()
}

/// 交接：選接手的 agent 與 model，選完直接送出（簡報由 daemon 產生）。
pub(crate) fn start_handoff_picker(app: &mut App) {
    let Some(src) = app.selected_id() else {
        app.status = "沒有選取的 session".into();
        return;
    };
    start_spawn_picker(app);
    app.handoff_from = Some(src);
    if let Some(p) = app.picker.as_mut() {
        p.title = "交接給哪個 agent".into();
    }
}

pub(crate) fn start_spawn_picker(app: &mut App) {
    app.handoff_from = None;
    let items = AgentKind::builtins()
        .iter()
        .map(|k| (k.id().to_string(), k.id().to_string()))
        .collect();
    app.picker = Some(Picker {
        stage: PickStage::Agent,
        title: "選擇 agent".into(),
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

fn finish_picker(
    app: &mut App,
    writer: &mut ConnStream,
    kind: AgentKind,
    model: Option<String>,
    effort: Option<String>,
) -> Result<()> {
    app.picker = None;
    app.mode = Mode::Normal;
    if let Some(src) = app.handoff_from.take() {
        crate::send(
            writer,
            &termade_protocol::Request::Handoff {
                session: src,
                kind: kind.clone(),
                model: model.clone(),
                effort,
                note: None,
            },
        )?;
        app.status = format!(
            "已要求交接給 {}{}（舊 session 會停止，新 agent 沿用同一個工作目錄）",
            kind.id(),
            model.map(|m| format!(":{m}")).unwrap_or_default()
        );
        app.spawn_variants.clear();
        return Ok(());
    }
    app.status = format!(
        "spawn → {}{}{}：輸入 prompt 後 Enter（可留空）",
        kind.id(),
        model
            .as_deref()
            .map(|m| format!(":{m}"))
            .unwrap_or_default(),
        effort
            .as_deref()
            .map(|e| format!(" effort={e}"))
            .unwrap_or_default(),
    );
    app.spawn_target = Some((kind, model));
    app.spawn_effort = effort;
    app.mode = Mode::Spawn;
    app.input.clear();
    app.input_cursor = 0;
    app.spawn_worktree = false;
    app.spawn_permission = None;
    app.spawn_bypass_armed = false;
    Ok(())
}

pub(crate) fn handle_pick_key(app: &mut App, writer: &mut ConnStream, key: KeyEvent) -> Result<()> {
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
        KeyCode::Down | KeyCode::Tab => {
            if n > 0 {
                p.cursor = (p.cursor + 1) % n;
            }
        }
        KeyCode::Up | KeyCode::BackTab => {
            if n > 0 {
                p.cursor = (p.cursor + n - 1) % n;
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
        KeyCode::Char(c) if !ctrl => {
            p.filter.push(c);
            p.cursor = 0;
        }
        KeyCode::Enter => {
            let chosen = p.visible().get(p.cursor).map(|(_, v)| v.clone());
            let custom = p.filter.trim().to_string();
            match p.stage.clone() {
                PickStage::Agent => {
                    // 沒有符合的項目時，也接受別名（例如 agy、oc）。
                    let chosen =
                        chosen.or_else(|| AgentKind::parse(&custom).map(|k| k.id().to_string()));
                    let Some(kind) = chosen.and_then(|v| AgentKind::parse(&v)) else {
                        app.status = "沒有符合的 agent".into();
                        return Ok(());
                    };
                    p.title = format!(
                        "{} 的 model（輸入可過濾；無符合時 Enter 使用輸入值）",
                        kind.id()
                    );
                    p.items = model_choices(&kind);
                    p.filter.clear();
                    p.cursor = 0;
                    // 向 CLI 查詢可用 model（不阻塞介面）。
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
                    app.spawn_variants = model
                        .as_deref()
                        .and_then(|m| p.variants.get(m).cloned())
                        .unwrap_or_default();
                    finish_picker(app, writer, kind, model, None)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}
