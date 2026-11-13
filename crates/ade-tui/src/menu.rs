//! Helix 風格的 Space 選單（which-key）與 spawn 選擇器（agent → model → prompt）。

use ade_core::AgentKind;
use ade_daemon::ConnStream;
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
                it('q', "離開（保留 daemon）", Act::Cmd("quit")),
                it('Q', "關閉 daemon 並離開", Act::Cmd("shutdown")),
            ],
        ),
        ['s'] => (
            "SPC s · session",
            vec![
                it('n', "新增 agent…（選 agent 與 model）", Act::Spawn),
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

#[derive(Clone)]
pub(crate) struct Picker {
    pub stage: PickStage,
    pub title: String,
    /// (顯示文字, 值)；值為空字串代表「預設」。
    pub items: Vec<(String, String)>,
    pub filter: String,
    pub cursor: usize,
}

impl Picker {
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
    let cfg = ade_daemon::config::Config::load(&ade_daemon::config_dir(), &cwd);
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

pub(crate) fn start_spawn_picker(app: &mut App) {
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
    });
    app.mode = Mode::Pick;
}

pub(crate) fn handle_pick_key(app: &mut App, key: KeyEvent) -> Result<()> {
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
                    app.picker = None;
                    app.status = format!(
                        "spawn → {}{}：輸入 prompt 後 Enter（可留空）",
                        kind.id(),
                        model
                            .as_deref()
                            .map(|m| format!(":{m}"))
                            .unwrap_or_default()
                    );
                    app.spawn_target = Some((kind, model));
                    app.mode = Mode::Spawn;
                    app.input.clear();
                    app.input_cursor = 0;
                    app.spawn_worktree = false;
                    app.spawn_permission = None;
                    app.spawn_bypass_armed = false;
                }
            }
        }
        _ => {}
    }
    Ok(())
}
