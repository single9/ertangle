//! 設定面板：用清單切換 config.toml 的預設值，不必記 `:set` 的語法。
//!
//! `:settings`、`:set` (不帶參數)或 `Space ,` 開啟。↑↓ 選項目，←→／Space／Enter 切換值，
//! 每次切換立即寫進全域 `config.toml` (只對之後新建立的 session 生效)。

use crossterm::event::{KeyCode, KeyEvent};
use termade_core::AgentKind;
use termade_daemon::config::Config;
use tr::tr;

use crate::app::{App, Mode};
use crate::hosts::HostRouter;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Item {
    DefaultAgent,
    DefaultRole,
    DefaultPermission,
    WorkerOrder1,
    WorkerOrder2,
    WorkerOrder3,
    WorkerOrder4,
    TargetAgent,
    Model,
    Effort,
    SpawnWorktree,
    SpawnConfirm,
    SpawnMaxEffort,
    AutoResume,
    SharedCargoTarget,
    MergeStrategy,
    CollabInstructions,
    InjectSkill,
    TokenHints,
    WebCompanion,
    WebPort,
    Language,
}

pub(crate) const ITEMS: [Item; 22] = [
    Item::DefaultAgent,
    Item::DefaultRole,
    Item::DefaultPermission,
    Item::WorkerOrder1,
    Item::WorkerOrder2,
    Item::WorkerOrder3,
    Item::WorkerOrder4,
    Item::TargetAgent,
    Item::Model,
    Item::Effort,
    Item::SpawnWorktree,
    Item::SpawnConfirm,
    Item::SpawnMaxEffort,
    Item::AutoResume,
    Item::SharedCargoTarget,
    Item::MergeStrategy,
    Item::CollabInstructions,
    Item::InjectSkill,
    Item::TokenHints,
    Item::WebCompanion,
    Item::WebPort,
    Item::Language,
];

const AGENTS: [&str; 4] = ["opencode", "claude", "codex", "antigravity"];

/// 已安裝的內建 agent (設定面板只列這些)。
fn installed_agents() -> Vec<&'static str> {
    AGENTS
        .iter()
        .copied()
        .filter(|a| AgentKind::parse(a).is_some_and(|k| termade_adapter::is_installed(&k)))
        .collect()
}

type LiveModels =
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<(String, String)>>>>;

/// 開著的值選擇清單：打字過濾、↑↓ 選、Enter 套用。
#[derive(Default)]
pub(crate) struct Sub {
    pub filter: String,
    pub cursor: usize,
}

pub(crate) struct Settings {
    pub sub: Option<Sub>,
    /// 向 agent CLI 查到的 model 清單 (背景執行緒填入，`opencode models` 等最多要等幾秒)。
    live: LiveModels,
    requested: std::collections::HashSet<String>,
    pub cursor: usize,
    /// 「預設 model／effort」這兩列套用的 agent。
    pub agent: String,
    pub cfg: Config,
}

pub(crate) fn config_path() -> std::path::PathBuf {
    termade_daemon::config_dir().join("config.toml")
}

pub(crate) fn open(app: &mut App) {
    let cfg = Config::load_file(&config_path());
    let agent = app
        .selected_kind_id()
        .or_else(|| cfg.defaults.agent.clone())
        .unwrap_or_else(|| "claude".into());
    let mut s = Settings {
        sub: None,
        live: Default::default(),
        requested: Default::default(),
        cursor: 0,
        agent,
        cfg,
    };
    s.fetch_models();
    app.settings = Some(s);
    app.mode = Mode::Settings;
}

impl Settings {
    /// 對目前的 agent 在背景查 model 清單 (每個 agent 只查一次)。
    fn fetch_models(&mut self) {
        let Some(kind) = AgentKind::parse(&self.agent) else {
            return;
        };
        if !LIVE_KINDS.contains(&self.agent.as_str()) || !self.requested.insert(self.agent.clone())
        {
            return;
        }
        let (live, name) = (self.live.clone(), self.agent.clone());
        std::thread::spawn(move || {
            let found = crate::menu::query_models(&kind);
            if let Ok(mut m) = live.lock() {
                m.insert(name, found);
            }
        });
    }

    pub(crate) fn loading_models_pub(&self) -> bool {
        self.loading_models()
    }

    fn loading_models(&self) -> bool {
        LIVE_KINDS.contains(&self.agent.as_str())
            && !self.live.lock().is_ok_and(|m| m.contains_key(&self.agent))
    }
}

/// 要向 CLI 查 model 清單的 agent。
const LIVE_KINDS: [&str; 3] = ["opencode", "antigravity", "codex"];

impl Item {
    /// 這個項目是分區的第一列時，回傳分區標題 (設定面板在它上方畫標題列)。
    pub(crate) fn section(self) -> Option<String> {
        Some(match self {
            Item::DefaultAgent => tr!("新 session 預設值"),
            Item::WorkerOrder1 => {
                tr!("Worker 啟動順序 (agent 建立子 session 時依序挑第一個已安裝的)")
            }
            Item::TargetAgent => tr!("各 agent 的預設值"),
            Item::SpawnWorktree => tr!("agent 自行建立 session 的防呆"),
            Item::MergeStrategy => tr!("Git 與 worktree"),
            Item::CollabInstructions => tr!("協作說明"),
            Item::WebCompanion => tr!("Web 版"),
            Item::Language => tr!("介面"),
            _ => return None,
        })
    }

    /// Worker 順位欄位的索引 (0 起算)。
    fn worker_slot(self) -> Option<usize> {
        match self {
            Item::WorkerOrder1 => Some(0),
            Item::WorkerOrder2 => Some(1),
            Item::WorkerOrder3 => Some(2),
            Item::WorkerOrder4 => Some(3),
            _ => None,
        }
    }

    pub(crate) fn label(self, s: &Settings) -> String {
        match self {
            Item::DefaultAgent => tr!("預設 agent"),
            Item::DefaultRole => tr!("預設角色"),
            Item::DefaultPermission => tr!("預設權限"),
            Item::WorkerOrder1 => tr!("第 1 順位"),
            Item::WorkerOrder2 => tr!("第 2 順位"),
            Item::WorkerOrder3 => tr!("第 3 順位"),
            Item::WorkerOrder4 => tr!("第 4 順位"),
            Item::TargetAgent => tr!("下面兩項設定的對象 agent"),
            Item::Model if s.loading_models() => {
                tr!("{agent} 的預設 model (清單載入中…)", agent = s.agent)
            }
            Item::Model => tr!("{agent} 的預設 model", agent = s.agent),
            Item::Effort => tr!("{agent} 的預設 effort", agent = s.agent),
            Item::SpawnWorktree => tr!("agent 自己建立的 session 用獨立 worktree"),
            Item::SpawnConfirm => tr!("agent 建立高消耗 session 時"),
            Item::SpawnMaxEffort => tr!("agent 建立 session 免確認的 effort 上限"),
            Item::AutoResume => tr!("daemon 重啟時自動接續執行中的 session"),
            Item::SharedCargoTarget => tr!("worktree 共用 cargo target (編譯排隊、不重複)"),
            Item::MergeStrategy => tr!("worktree 合併策略"),
            Item::CollabInstructions => tr!("把協作說明注入每個 agent"),
            Item::InjectSkill => tr!("把 termade-ade skill 注入協作說明"),
            Item::TokenHints => tr!("把省 token 提示注入協作說明"),
            Item::WebCompanion => tr!("Web 版支援 (Web UI)"),
            Item::WebPort => tr!("Web 版連接埠 (Web Port)"),
            Item::Language => tr!("介面語言"),
        }
    }

    /// 目前儲存的值 (空字串＝未設定)。
    fn current(self, s: &Settings) -> String {
        let c = &s.cfg;
        let agent = c.agents.get(&s.agent);
        match self {
            Item::DefaultAgent => c.defaults.agent.clone().unwrap_or_default(),
            Item::DefaultRole => c.defaults.role.clone().unwrap_or_default(),
            Item::DefaultPermission => c.defaults.permission.clone().unwrap_or_default(),
            Item::WorkerOrder1 | Item::WorkerOrder2 | Item::WorkerOrder3 | Item::WorkerOrder4 => {
                let n = self.worker_slot().unwrap_or(0);
                c.defaults.worker_order.get(n).cloned().unwrap_or_default()
            }
            Item::TargetAgent => s.agent.clone(),
            Item::Model => agent.and_then(|a| a.model.clone()).unwrap_or_default(),
            Item::Effort => agent.and_then(|a| a.effort.clone()).unwrap_or_default(),
            Item::SpawnWorktree => on_off(c.agent_spawn.worktree()),
            Item::SpawnConfirm => c.agent_spawn.confirm().to_string(),
            Item::SpawnMaxEffort => c.agent_spawn.max_effort().to_string(),
            Item::AutoResume => on_off(c.agent_spawn.auto_resume()),
            Item::SharedCargoTarget => on_off(c.agent_spawn.shared_cargo_target()),
            Item::MergeStrategy => c
                .git
                .merge_strategy
                .map(|s| s.to_string())
                .unwrap_or_default(),
            Item::CollabInstructions => on_off(c.collab.instructions != Some(false)),
            Item::InjectSkill => on_off(c.collab.inject_skill == Some(true)),
            Item::TokenHints => on_off(c.collab.token_hints == Some(true)),
            Item::WebCompanion => {
                if c.web.enabled == Some(false) {
                    "".into()
                } else {
                    c.web.addr.clone().unwrap_or_else(|| {
                        if c.web.enabled == Some(true) {
                            "tailscale".into()
                        } else {
                            "".into()
                        }
                    })
                }
            }
            Item::WebPort => c.web.port.map(|p| p.to_string()).unwrap_or_default(),
            Item::Language => c.tui.language.clone().unwrap_or_default(),
        }
    }

    /// 可選的值：` (儲存值, 顯示文字)`。
    fn choices(self, s: &Settings) -> Vec<(String, String)> {
        let pair = |v: &str, l: &str| (v.to_string(), l.to_string());
        let mut out = match self {
            Item::WorkerOrder1 | Item::WorkerOrder2 | Item::WorkerOrder3 | Item::WorkerOrder4 => {
                // 只列已安裝的；已存的值就算沒裝，下方的「清單外的值」邏輯仍會補上。
                let mut v = vec![pair("", &tr!("未設定"))];
                v.extend(installed_agents().into_iter().map(|a| pair(a, a)));
                v
            }
            Item::DefaultAgent => {
                let mut v = vec![pair("", &tr!("未設定"))];
                v.extend(installed_agents().into_iter().map(|a| pair(a, a)));
                v
            }
            Item::DefaultRole => vec![
                pair("", &tr!("未設定 (coordinator)")),
                pair("coordinator", "coordinator"),
                pair("worker", "worker"),
                pair("reviewer", "reviewer"),
            ],
            Item::DefaultPermission => vec![
                pair("", &tr!("未設定 (auto)")),
                pair("ask", &tr!("ask：每個動作都問")),
                pair("auto", &tr!("auto：自動核准一般動作")),
                pair("bypass", &tr!("bypass：完全不問")),
            ],
            Item::TargetAgent => installed_agents().into_iter().map(|a| pair(a, a)).collect(),
            Item::Model => {
                let kind = AgentKind::parse(&s.agent);
                let mut v = vec![pair("", &tr!("未設定 (agent 自己的預設)"))];
                if let Some(k) = kind {
                    v.extend(
                        crate::menu::model_choices(&k)
                            .into_iter()
                            .filter(|(_, val)| !val.is_empty())
                            .map(|(label, val)| (val, label)),
                    );
                }
                let live = s
                    .live
                    .lock()
                    .ok()
                    .and_then(|m| m.get(&s.agent).cloned())
                    .unwrap_or_default();
                for (label, val) in live {
                    if !v.iter().any(|(x, _)| *x == val) {
                        v.push((val, label));
                    }
                }
                v
            }
            Item::Effort => {
                let mut v = vec![pair("", &tr!("未設定"))];
                v.extend(termade_adapter::EFFORT_LEVELS.iter().map(|l| pair(l, l)));
                v
            }
            Item::SpawnWorktree
            | Item::CollabInstructions
            | Item::InjectSkill
            | Item::TokenHints
            | Item::AutoResume
            | Item::SharedCargoTarget => {
                vec![pair("on", &tr!("開")), pair("off", &tr!("關"))]
            }
            Item::SpawnConfirm => vec![
                pair("costly", &tr!("超出範圍才問使用者")),
                pair("always", &tr!("每次都問使用者")),
                pair("never", &tr!("不問 (effort 調降、model 拒絕)")),
            ],
            Item::SpawnMaxEffort => termade_adapter::EFFORT_LEVELS
                .iter()
                .map(|l| pair(l, l))
                .collect(),
            Item::MergeStrategy => vec![
                pair("", &tr!("未設定")),
                pair("merge", "merge"),
                pair("squash", "squash"),
                pair("rebase", "rebase"),
            ],
            Item::WebCompanion => {
                let port = s.cfg.web.resolved_port();
                vec![
                    pair("", &tr!("關閉")),
                    pair("tailscale", &tr!("Tailscale (自動 IP:{port})", port = port)),
                    pair(
                        &format!("127.0.0.1:{port}"),
                        &tr!("本機 (127.0.0.1:{port})", port = port),
                    ),
                    pair(
                        &format!("0.0.0.0:{port}"),
                        &tr!("全網 (0.0.0.0:{port})", port = port),
                    ),
                ]
            }
            Item::WebPort => vec![
                pair("", &tr!("預設 (7421)")),
                pair("7421", "7421"),
                pair("8080", "8080"),
                pair("8000", "8000"),
                pair("3000", "3000"),
                pair("9090", "9090"),
            ],
            Item::Language => {
                let mut v = vec![pair("", &tr!("自動 (依 LANG 環境變數)"))];
                v.extend(
                    termade_core::i18n::LANGUAGES
                        .iter()
                        .map(|(_, code, name)| pair(code, name)),
                );
                v
            }
        };
        // 設定檔裡手寫了清單以外的值時也要列出來，才不會一切換就把它蓋掉而回不去。
        let cur = self.current(s);
        if !out.iter().any(|(v, _)| *v == cur) {
            out.push((cur.clone(), cur));
        }
        out
    }

    /// 目前值的顯示文字。
    pub(crate) fn display(self, s: &Settings) -> String {
        let cur = self.current(s);
        self.choices(s)
            .into_iter()
            .find(|(v, _)| *v == cur)
            .map(|(_, l)| l)
            .unwrap_or(cur)
    }

    pub(crate) fn is_toggle(self) -> bool {
        matches!(
            self,
            Item::SpawnWorktree
                | Item::CollabInstructions
                | Item::InjectSkill
                | Item::TokenHints
                | Item::AutoResume
                | Item::SharedCargoTarget
        )
    }
}

fn on_off(b: bool) -> String {
    if b { "on" } else { "off" }.to_string()
}

/// 把目前列切到下一個 (`dir = 1`)或上一個 (`-1`)值並寫入設定檔。
fn cycle(app: &mut App, writer: Option<&mut HostRouter>, dir: isize) {
    let Some(s) = app.settings.as_mut() else {
        return;
    };
    let item = ITEMS[s.cursor];
    let choices = item.choices(s);
    let cur = item.current(s);
    let idx = choices.iter().position(|(v, _)| *v == cur).unwrap_or(0) as isize;
    let next = (idx + dir).rem_euclid(choices.len() as isize) as usize;
    let value = choices[next].0.clone();
    apply_value(app, writer, value);
}

/// 把目前選取的項目設成 `value` 並寫入設定檔。
fn apply_value(app: &mut App, writer: Option<&mut HostRouter>, value: String) {
    let Some(s) = app.settings.as_mut() else {
        return;
    };
    let item = ITEMS[s.cursor];
    if item == Item::TargetAgent {
        s.agent = value;
        s.fetch_models();
        return;
    }
    let path = config_path();
    if let Some(slot) = item.worker_slot() {
        let mut order = s.cfg.defaults.worker_order.clone();
        order.resize(order.len().max(slot + 1), String::new());
        order[slot] = value;
        // 空位與重複的 agent 去掉，保留相對順序。
        let mut seen = std::collections::HashSet::new();
        order.retain(|a| !a.is_empty() && seen.insert(a.clone()));
        let toml_value = (!order.is_empty()).then(|| {
            termade_daemon::config::TomlValue::Array(
                order
                    .into_iter()
                    .map(termade_daemon::config::TomlValue::String)
                    .collect(),
            )
        });
        app.status = match Config::set_toml(&path, &["defaults"], "worker_order", toml_value) {
            Ok(()) => {
                s.cfg = Config::load_file(&path);
                tr!(
                    "已儲存到 {path} (新建立的 session 起生效)",
                    path = path.display()
                )
            }
            Err(e) => tr!("寫入設定失敗：{e}", e = e),
        };
        return;
    }
    if item == Item::WebCompanion {
        let (enabled_val, addr_val) = if value.is_empty() {
            (
                Some(termade_daemon::config::TomlValue::Boolean(false)),
                None,
            )
        } else {
            (
                Some(termade_daemon::config::TomlValue::Boolean(true)),
                Some(termade_daemon::config::TomlValue::String(value.clone())),
            )
        };
        let _ = Config::set_toml(&path, &["web"], "enabled", enabled_val);
        let _ = Config::set_toml(&path, &["web"], "addr", addr_val);
        s.cfg = Config::load_file(&path);
        if let Some(w) = writer {
            if value.is_empty() {
                let _ = crate::send(
                    w,
                    &termade_protocol::Request::WebControl {
                        enabled: Some(false),
                        addr: None,
                    },
                );
                app.status = tr!("已關閉 Web 版並儲存至 {path}", path = path.display());
            } else {
                let _ = crate::send(
                    w,
                    &termade_protocol::Request::WebControl {
                        enabled: Some(true),
                        addr: Some(value.clone()),
                    },
                );
                app.status = tr!("已啟動 Web 版並儲存至 {path}", path = path.display());
            }
        } else {
            app.status = tr!("已儲存到 {path}", path = path.display());
        }
        return;
    }
    if item == Item::WebPort {
        let port_val = if value.is_empty() {
            None
        } else {
            value
                .parse::<i64>()
                .ok()
                .map(termade_daemon::config::TomlValue::Integer)
        };
        let _ = Config::set_toml(&path, &["web"], "port", port_val);
        s.cfg = Config::load_file(&path);
        if s.cfg.web.is_enabled() {
            if let Some(w) = writer {
                let addr = s.cfg.web.resolved_addr();
                let _ = crate::send(
                    w,
                    &termade_protocol::Request::WebControl {
                        enabled: Some(true),
                        addr: Some(addr),
                    },
                );
            }
        }
        app.status = tr!("已更新 Web 版連接埠並儲存至 {path}", path = path.display());
        return;
    }
    let (table, key): (&[&str], &str) = match item {
        Item::DefaultAgent => (&["defaults"], "agent"),
        Item::DefaultRole => (&["defaults"], "role"),
        Item::DefaultPermission => (&["defaults"], "permission"),
        Item::SpawnWorktree => (&["agent_spawn"], "worktree"),
        Item::SpawnConfirm => (&["agent_spawn"], "confirm"),
        Item::SpawnMaxEffort => (&["agent_spawn"], "max_effort"),
        Item::CollabInstructions => (&["collab"], "instructions"),
        Item::InjectSkill => (&["collab"], "inject_skill"),
        Item::TokenHints => (&["collab"], "token_hints"),
        Item::AutoResume => (&["agent_spawn"], "auto_resume"),
        Item::SharedCargoTarget => (&["agent_spawn"], "shared_cargo_target"),
        Item::MergeStrategy => (&["git"], "merge_strategy"),
        Item::WebCompanion | Item::WebPort => return,
        Item::Language => (&["tui"], "language"),
        Item::Model => (&["agents", ""], "model"),
        Item::Effort => (&["agents", ""], "effort"),
        Item::TargetAgent => return,
        Item::WorkerOrder1 | Item::WorkerOrder2 | Item::WorkerOrder3 | Item::WorkerOrder4 => return,
    };
    let agent = s.agent.clone();
    let mut tbl: Vec<&str> = table.to_vec();
    if tbl.last() == Some(&"") {
        tbl.pop();
        tbl.push(&agent);
    }
    let toml_value = if item.is_toggle() {
        Some(termade_daemon::config::TomlValue::Boolean(value == "on"))
    } else if value.is_empty() {
        None
    } else {
        Some(termade_daemon::config::TomlValue::String(value.clone()))
    };
    match Config::set_toml(&path, &tbl, key, toml_value) {
        Ok(()) => {
            s.cfg = Config::load_file(&path);
            if item == Item::MergeStrategy {
                app.status = tr!("已更新合併策略並儲存至 {path}", path = path.display());
                return;
            }
            if item == Item::Language {
                // 立即生效：之後畫的每個字串都走新語言。
                let lang = termade_core::i18n::resolve(Some(&value));
                app.status = match termade_core::i18n::set_language(lang) {
                    Ok(()) => tr!("已儲存到 {path}", path = path.display()),
                    Err(e) => tr!("載入翻譯失敗：{e}", e = e),
                };
                return;
            }
            app.status = tr!(
                "已儲存到 {path} (新建立的 session 起生效)",
                path = path.display()
            );
        }
        Err(e) => app.status = tr!("寫入設定失敗：{e}", e = e),
    }
}

impl Settings {
    /// 值選擇清單目前顯示的項目 (依過濾字)。
    pub(crate) fn sub_visible(&self) -> Vec<(String, String)> {
        let f = self.sub.as_ref().map(|s| s.filter.to_lowercase());
        ITEMS[self.cursor]
            .choices(self)
            .into_iter()
            .filter(|(v, l)| {
                f.as_deref().is_none_or(|f| {
                    f.is_empty() || v.to_lowercase().contains(f) || l.to_lowercase().contains(f)
                })
            })
            .collect()
    }
}

fn handle_sub_key(app: &mut App, writer: Option<&mut HostRouter>, key: KeyEvent) {
    let Some(s) = app.settings.as_mut() else {
        return;
    };
    let n = s.sub_visible().len();
    let Some(sub) = s.sub.as_mut() else { return };
    match key.code {
        KeyCode::Esc => s.sub = None,
        KeyCode::Down => sub.cursor = (sub.cursor + 1).min(n.saturating_sub(1)),
        KeyCode::Up => sub.cursor = sub.cursor.saturating_sub(1),
        KeyCode::PageDown => sub.cursor = (sub.cursor + 10).min(n.saturating_sub(1)),
        KeyCode::PageUp => sub.cursor = sub.cursor.saturating_sub(10),
        KeyCode::Backspace => {
            sub.filter.pop();
            sub.cursor = 0;
        }
        KeyCode::Char(c) => {
            sub.filter.push(c);
            sub.cursor = 0;
        }
        KeyCode::Enter => {
            let cursor = sub.cursor;
            let pick = s.sub_visible().get(cursor).map(|(v, _)| v.clone());
            s.sub = None;
            if let Some(v) = pick {
                apply_value(app, writer, v);
            }
        }
        _ => {}
    }
}

/// 開啟值選擇清單，游標停在目前的值上。
fn open_sub(app: &mut App) {
    let Some(s) = app.settings.as_mut() else {
        return;
    };
    let item = ITEMS[s.cursor];
    let cur = item.current(s);
    let cursor = item
        .choices(s)
        .iter()
        .position(|(v, _)| *v == cur)
        .unwrap_or(0);
    s.sub = Some(Sub {
        filter: String::new(),
        cursor,
    });
}

pub(crate) fn handle_key(app: &mut App, writer: Option<&mut HostRouter>, key: KeyEvent) {
    if app.settings.as_ref().is_some_and(|s| s.sub.is_some()) {
        return handle_sub_key(app, writer, key);
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.mode = Mode::Normal;
            app.settings = None;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(s) = app.settings.as_mut() {
                s.cursor = (s.cursor + 1) % ITEMS.len();
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(s) = app.settings.as_mut() {
                s.cursor = (s.cursor + ITEMS.len() - 1) % ITEMS.len();
            }
        }
        // 開關直接切換；其他項目開清單選 (清單長，不適合左右輪替)。
        KeyCode::Char(' ') | KeyCode::Enter => {
            let toggle = app
                .settings
                .as_ref()
                .is_some_and(|s| ITEMS[s.cursor].is_toggle());
            if toggle {
                cycle(app, writer, 1)
            } else {
                open_sub(app)
            }
        }
        KeyCode::Right | KeyCode::Char('l') => cycle(app, writer, 1),
        KeyCode::Left | KeyCode::Char('h') => cycle(app, writer, -1),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termade_protocol::MergeStrategy;

    fn settings_with(merge_strategy: Option<MergeStrategy>) -> Settings {
        let mut cfg = Config::default();
        cfg.git.merge_strategy = merge_strategy;
        Settings {
            sub: None,
            live: Default::default(),
            requested: Default::default(),
            cursor: 0,
            agent: "claude".into(),
            cfg,
        }
    }

    #[test]
    fn merge_strategy_setting_offers_all_strategies() {
        let s = settings_with(Some(MergeStrategy::Squash));
        assert!(ITEMS.contains(&Item::MergeStrategy));
        assert_eq!(Item::MergeStrategy.current(&s), "squash");
        let values: Vec<_> = Item::MergeStrategy
            .choices(&s)
            .into_iter()
            .map(|(v, _)| v)
            .collect();
        assert_eq!(values, ["", "merge", "squash", "rebase"]);
    }

    #[test]
    fn merge_strategy_setting_defaults_to_unset() {
        let s = settings_with(None);
        assert_eq!(Item::MergeStrategy.current(&s), "");
        assert!(Item::MergeStrategy
            .choices(&s)
            .first()
            .is_some_and(|(v, _)| v.is_empty()));
    }
}
