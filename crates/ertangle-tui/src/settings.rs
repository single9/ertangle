//! 設定面板：用清單切換 config.toml 的預設值，不必記 `:set` 的語法。
//!
//! `:settings`、`:set` (不帶參數)或 `Space ,` 開啟。↑↓ 選項目，←→／Space／Enter 切換值，
//! 每次切換立即寫進全域 `config.toml` (只對之後新建立的 session 生效)。

use crossterm::event::{KeyCode, KeyEvent};
use ertangle_core::AgentKind;
use ertangle_daemon::config::Config;
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
    CollabDelegation,
    InjectSkill,
    TokenHints,
    AutoCompact,
    AutoCompactThreshold,
    WebCompanion,
    WebPort,
    KeepAwake,
    Language,
}

pub(crate) const ITEMS: [Item; 26] = [
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
    Item::CollabDelegation,
    Item::InjectSkill,
    Item::TokenHints,
    Item::AutoCompact,
    Item::AutoCompactThreshold,
    Item::WebCompanion,
    Item::WebPort,
    Item::KeepAwake,
    Item::Language,
];

const AGENTS: [&str; 4] = ["opencode", "claude", "codex", "antigravity"];

/// 已安裝的內建 agent (設定面板只列這些)。
fn installed_agents() -> Vec<&'static str> {
    AGENTS
        .iter()
        .copied()
        .filter(|a| AgentKind::parse(a).is_some_and(|k| ertangle_adapter::is_installed(&k)))
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
    ertangle_daemon::config_dir().join("config.toml")
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

pub(crate) const TAB_COUNT: usize = 4;

pub(crate) fn tab_title(tab: usize) -> String {
    match tab {
        0 => tr!("1: Agent"),
        1 => tr!("2: Spawn & Git"),
        2 => tr!("3: 協作 & 壓縮"),
        _ => tr!("4: Web & 系統"),
    }
}

impl Item {
    /// Tab index (`0..TAB_COUNT`) that this setting belongs to.
    pub(crate) fn tab(self) -> usize {
        match self {
            Item::DefaultAgent
            | Item::DefaultRole
            | Item::DefaultPermission
            | Item::WorkerOrder1
            | Item::WorkerOrder2
            | Item::WorkerOrder3
            | Item::WorkerOrder4
            | Item::TargetAgent
            | Item::Model
            | Item::Effort => 0,
            Item::SpawnWorktree
            | Item::SpawnConfirm
            | Item::SpawnMaxEffort
            | Item::AutoResume
            | Item::SharedCargoTarget
            | Item::MergeStrategy => 1,
            Item::CollabInstructions
            | Item::CollabDelegation
            | Item::InjectSkill
            | Item::TokenHints
            | Item::AutoCompact
            | Item::AutoCompactThreshold => 2,
            Item::WebCompanion | Item::WebPort | Item::KeepAwake | Item::Language => 3,
        }
    }

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
            Item::AutoCompact => tr!("上下文"),
            Item::WebCompanion => tr!("Web 版"),
            Item::KeepAwake => tr!("電源"),
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
            Item::Model => tr!("{agent} 的預設 model", agent = s.agent),
            Item::Effort => tr!("{agent} 的預設 effort", agent = s.agent),
            Item::SpawnWorktree => tr!("agent 自己建立的 session 用獨立 worktree"),
            Item::SpawnConfirm => tr!("agent 建立高消耗 session 時"),
            Item::SpawnMaxEffort => tr!("agent 建立 session 免確認的 effort 上限"),
            Item::AutoResume => tr!("daemon 重啟時自動接續執行中的 session"),
            Item::SharedCargoTarget => tr!("worktree 共用 cargo target (編譯排隊、不重複)"),
            Item::MergeStrategy => tr!("worktree 合併策略"),
            Item::CollabInstructions => tr!("把協作說明注入每個 agent"),
            Item::CollabDelegation => tr!("Coordinator 主動分派 worker 策略"),
            Item::InjectSkill => tr!("把 ertangle-ade skill 注入協作說明"),
            Item::TokenHints => tr!("把省 token 提示注入協作說明"),
            Item::AutoCompact => tr!("上下文快滿時自動 compact"),
            Item::AutoCompactThreshold => tr!("自動 compact 的觸發門檻 (上下文用量 %)"),
            Item::WebCompanion => tr!("Web 版支援 (Web UI)"),
            Item::WebPort => tr!("Web 版連接埠 (Web Port)"),
            Item::KeepAwake => tr!("daemon 執行期間防止系統休眠"),
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
            Item::AutoCompact => on_off(c.auto_compact.enabled()),
            Item::AutoCompactThreshold => c.auto_compact.threshold_percent().to_string(),
            Item::SharedCargoTarget => on_off(c.agent_spawn.shared_cargo_target()),
            Item::MergeStrategy => c
                .git
                .merge_strategy
                .map(|s| s.to_string())
                .unwrap_or_default(),
            Item::CollabInstructions => on_off(c.collab.instructions != Some(false)),
            Item::CollabDelegation => c
                .collab
                .delegation
                .map(|d| d.to_string())
                .unwrap_or_default(),
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
            Item::KeepAwake => on_off(c.power.keep_awake()),
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
            Item::CollabDelegation => vec![
                pair("", &tr!("未設定 (balanced)")),
                pair("proactive", "proactive"),
                pair("balanced", "balanced"),
                pair("manual", "manual"),
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
                v.extend(ertangle_adapter::EFFORT_LEVELS.iter().map(|l| pair(l, l)));
                v
            }
            Item::SpawnWorktree
            | Item::CollabInstructions
            | Item::InjectSkill
            | Item::TokenHints
            | Item::AutoResume
            | Item::AutoCompact
            | Item::SharedCargoTarget
            | Item::KeepAwake => {
                vec![pair("on", &tr!("開")), pair("off", &tr!("關"))]
            }
            Item::AutoCompactThreshold => ["50", "60", "70", "80", "90"]
                .iter()
                .map(|p| pair(p, &format!("{p}%")))
                .collect(),
            Item::SpawnConfirm => vec![
                pair("costly", &tr!("超出範圍才問使用者")),
                pair("always", &tr!("每次都問使用者")),
                pair("never", &tr!("不問 (effort 調降、model 拒絕)")),
            ],
            Item::SpawnMaxEffort => ertangle_adapter::EFFORT_LEVELS
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
                    ertangle_core::i18n::LANGUAGES
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
        let val = self
            .choices(s)
            .into_iter()
            .find(|(v, _)| *v == cur)
            .map(|(_, l)| l)
            .unwrap_or(cur);
        if self == Item::Model && s.loading_models() {
            format!("{val} ({})", tr!("載入中…"))
        } else {
            val
        }
    }

    /// 顯示在設定面板下方說明區的簡短解釋。
    pub(crate) fn description(self) -> String {
        match self {
            Item::DefaultAgent => tr!("新建立 session 時預設使用的 agent CLI (未設定時依環境偵測)。"),
            Item::DefaultRole => tr!("人工直接開啟新 session 的預設角色 (coordinator / worker / reviewer)。"),
            Item::DefaultPermission => tr!("工具執行權限：ask 每次詢問、auto 自動核准一般操作、bypass 全免問。"),
            Item::WorkerOrder1 | Item::WorkerOrder2 | Item::WorkerOrder3 | Item::WorkerOrder4 => {
                tr!("Coordinator 呼叫 ertangle_spawn 未指定 agent 時，依此順序挑第一個已安裝且額度充足的 agent。")
            }
            Item::TargetAgent => tr!("切換下方兩列「預設 model / effort」所設定的目標 agent。"),
            Item::Model => tr!("該 agent 未指定模型時的預設 model；Enter 可搜尋即時清單。"),
            Item::Effort => tr!("該 agent 的預設推理深度 (僅 claude / codex 等支援 effort 的 agent 生效)。"),
            Item::SpawnWorktree => tr!("開啟時，agent 透過 ertangle_spawn 建立的子 session 會自動取得獨立 git worktree。"),
            Item::SpawnConfirm => tr!("當子 session 使用高階模型或超過 effort 上限時的確認策略。"),
            Item::SpawnMaxEffort => tr!("超過此 effort 等級的 spawn 請求會依上方策略要求確認。"),
            Item::AutoResume => tr!("daemon 重啟或斷線重連後，自動接續先前執行中的 session。"),
            Item::SharedCargoTarget => tr!("多個 worktree 共用同一個 CARGO_TARGET_DIR，省空間並避免重複編譯。"),
            Item::MergeStrategy => tr!("Coordinator 執行 ertangle_merge 時預設使用的 git 合併策略 (merge / squash / rebase)。"),
            Item::CollabInstructions => tr!("啟動或連線時自動注入 [ertangle collaboration instructions] 協作規範。"),
            Item::CollabDelegation => tr!(
                "proactive：實作優先派給 worker；balanced：小改 inline、多檔或測試迴圈派給 worker；manual：除非明確要求否則不主動派工。"
            ),
            Item::InjectSkill => tr!("將內嵌的 ertangle-ade 協作技能全文直接附在協作說明後方。"),
            Item::TokenHints => tr!("在協作說明附上減少回合數與輸出長度的省 token 守則。"),
            Item::AutoCompact => tr!("當對話上下文用量達到門檻且 session 閒置時，自動觸發 compact。"),
            Item::AutoCompactThreshold => tr!("觸發自動 compact 的上下文使用率百分比 (預設 85%)。"),
            Item::WebCompanion => tr!("啟用或切換 Web UI 監聽位址 (localhost / tailscale / 0.0.0.0)，可立即套用。"),
            Item::WebPort => tr!("Web UI HTTP/WebSocket 服務監聽的連接埠 (預設 7421)。"),
            Item::KeepAwake => tr!("有執行中 session 時防止系統進入休眠，確保長任務不中斷。"),
            Item::Language => tr!("切換 TUI 顯示語言 (繁體中文 / English / 自動偵測)，變更立即生效。"),
        }
    }

    pub(crate) fn is_toggle(self) -> bool {
        matches!(
            self,
            Item::SpawnWorktree
                | Item::CollabInstructions
                | Item::InjectSkill
                | Item::TokenHints
                | Item::AutoResume
                | Item::AutoCompact
                | Item::SharedCargoTarget
                | Item::KeepAwake
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
    let Some(&item) = ITEMS.get(s.cursor) else {
        return;
    };
    let choices = item.choices(s);
    let cur = item.current(s);
    let idx = choices.iter().position(|(v, _)| *v == cur).unwrap_or(0) as isize;
    let next = (idx + dir).rem_euclid(choices.len() as isize) as usize;
    let value = choices
        .get(next)
        .map_or_else(String::new, |(v, _)| v.clone());
    apply_value(app, writer, value);
}

/// 把目前選取的項目設成 `value` 並寫入設定檔。
fn apply_value(app: &mut App, writer: Option<&mut HostRouter>, value: String) {
    let Some(s) = app.settings.as_mut() else {
        return;
    };
    let Some(&item) = ITEMS.get(s.cursor) else {
        return;
    };
    if item == Item::TargetAgent {
        s.agent = value;
        s.fetch_models();
        return;
    }
    let path = config_path();
    if let Some(slot) = item.worker_slot() {
        let mut order = s.cfg.defaults.worker_order.clone();
        order.resize(order.len().max(slot + 1), String::new());
        if let Some(elem) = order.get_mut(slot) {
            *elem = value;
        }
        // 空位與重複的 agent 去掉，保留相對順序。
        let mut seen = std::collections::HashSet::new();
        order.retain(|a| !a.is_empty() && seen.insert(a.clone()));
        let toml_value = (!order.is_empty()).then(|| {
            ertangle_daemon::config::TomlValue::Array(
                order
                    .into_iter()
                    .map(ertangle_daemon::config::TomlValue::String)
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
                Some(ertangle_daemon::config::TomlValue::Boolean(false)),
                None,
            )
        } else {
            (
                Some(ertangle_daemon::config::TomlValue::Boolean(true)),
                Some(ertangle_daemon::config::TomlValue::String(value.clone())),
            )
        };
        // 寫入設定檔與通知 daemon，若失敗則保留目前狀態
        let _ = Config::set_toml(&path, &["web"], "enabled", enabled_val);
        let _ = Config::set_toml(&path, &["web"], "addr", addr_val);
        s.cfg = Config::load_file(&path);
        if let Some(w) = writer {
            if value.is_empty() {
                // 傳送控制請求至 daemon，連線中斷時忽略
                let _ = crate::send(
                    w,
                    &ertangle_protocol::Request::WebControl {
                        enabled: Some(false),
                        addr: None,
                    },
                );
                app.status = tr!("已關閉 Web 版並儲存至 {path}", path = path.display());
            } else {
                // 傳送控制請求至 daemon，連線中斷時忽略
                let _ = crate::send(
                    w,
                    &ertangle_protocol::Request::WebControl {
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
    if item == Item::AutoCompactThreshold {
        let val = value
            .parse::<i64>()
            .ok()
            .map(ertangle_daemon::config::TomlValue::Integer);
        app.status = match Config::set_toml(&path, &["auto_compact"], "threshold_percent", val) {
            Ok(()) => {
                s.cfg = Config::load_file(&path);
                tr!("已儲存到 {path}", path = path.display())
            }
            Err(e) => tr!("寫入設定失敗：{e}", e = e),
        };
        return;
    }
    if item == Item::WebPort {
        let port_val = if value.is_empty() {
            None
        } else {
            value
                .parse::<i64>()
                .ok()
                .map(ertangle_daemon::config::TomlValue::Integer)
        };
        // 寫入設定檔，失敗時保留現狀
        let _ = Config::set_toml(&path, &["web"], "port", port_val);
        s.cfg = Config::load_file(&path);
        if s.cfg.web.is_enabled() {
            if let Some(w) = writer {
                let addr = s.cfg.web.resolved_addr();
                // 傳送控制請求至 daemon，連線中斷時忽略
                let _ = crate::send(
                    w,
                    &ertangle_protocol::Request::WebControl {
                        enabled: Some(true),
                        addr: Some(addr),
                    },
                );
            }
        }
        app.status = tr!("已更新 Web 版連接埠並儲存至 {path}", path = path.display());
        return;
    }
    if item == Item::KeepAwake {
        // 電源設定只寫全域檔，並立即通知 daemon 取得或釋放 inhibitor。
        let enabled = value == "on";
        let toml_value = Some(ertangle_daemon::config::TomlValue::Boolean(enabled));
        match Config::set_toml(&path, &["power"], "keep_awake", toml_value) {
            Ok(()) => {
                s.cfg = Config::load_file(&path);
                if let Some(w) = writer {
                    let _ = crate::send(w, &ertangle_protocol::Request::KeepAwake { enabled });
                }
                app.status = tr!("已更新防休眠並儲存至 {path}", path = path.display());
            }
            Err(e) => app.status = tr!("寫入設定失敗：{e}", e = e),
        }
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
        Item::CollabDelegation => (&["collab"], "delegation"),
        Item::InjectSkill => (&["collab"], "inject_skill"),
        Item::TokenHints => (&["collab"], "token_hints"),
        Item::AutoResume => (&["agent_spawn"], "auto_resume"),
        Item::AutoCompact => (&["auto_compact"], "enabled"),
        Item::AutoCompactThreshold => return,
        Item::SharedCargoTarget => (&["agent_spawn"], "shared_cargo_target"),
        Item::MergeStrategy => (&["git"], "merge_strategy"),
        Item::WebCompanion | Item::WebPort | Item::KeepAwake => return,
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
        Some(ertangle_daemon::config::TomlValue::Boolean(value == "on"))
    } else if value.is_empty() {
        None
    } else {
        Some(ertangle_daemon::config::TomlValue::String(value.clone()))
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
                let lang = ertangle_core::i18n::resolve(Some(&value));
                app.status = match ertangle_core::i18n::set_language(lang) {
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
        let choices = ITEMS
            .get(self.cursor)
            .map_or_else(Vec::new, |item| item.choices(self));
        choices
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
    let Some(&item) = ITEMS.get(s.cursor) else {
        return;
    };
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

fn active_tab(cursor: usize) -> usize {
    ITEMS.get(cursor).map_or(0, |item| item.tab())
}

fn tab_indices(tab: usize) -> Vec<usize> {
    ITEMS
        .iter()
        .enumerate()
        .filter_map(|(i, item)| (item.tab() == tab).then_some(i))
        .collect()
}

fn move_in_tab(cursor: &mut usize, dir: isize) {
    let tab = active_tab(*cursor);
    let indices = tab_indices(tab);
    if indices.is_empty() {
        return;
    }
    let pos = indices.iter().position(|&i| i == *cursor).unwrap_or(0);
    let next = if dir > 0 {
        (pos + 1) % indices.len()
    } else {
        (pos + indices.len() - 1) % indices.len()
    };
    if let Some(&idx) = indices.get(next) {
        *cursor = idx;
    }
}

fn switch_tab(cursor: &mut usize, target_tab: usize) {
    let tab = target_tab % TAB_COUNT;
    if let Some(first) = tab_indices(tab).first().copied() {
        *cursor = first;
    }
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
                move_in_tab(&mut s.cursor, 1);
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(s) = app.settings.as_mut() {
                move_in_tab(&mut s.cursor, -1);
            }
        }
        // 開關直接切換；其他項目開清單選 (清單長，不適合左右輪替)。
        KeyCode::Char(' ') | KeyCode::Enter => {
            let toggle = app
                .settings
                .as_ref()
                .and_then(|s| ITEMS.get(s.cursor))
                .is_some_and(|item| item.is_toggle());
            if toggle {
                cycle(app, writer, 1)
            } else {
                open_sub(app)
            }
        }
        KeyCode::Tab => {
            if let Some(s) = app.settings.as_mut() {
                let next = (active_tab(s.cursor) + 1) % TAB_COUNT;
                switch_tab(&mut s.cursor, next);
            }
        }
        KeyCode::BackTab => {
            if let Some(s) = app.settings.as_mut() {
                let prev = (active_tab(s.cursor) + TAB_COUNT - 1) % TAB_COUNT;
                switch_tab(&mut s.cursor, prev);
            }
        }
        KeyCode::Char(c @ '1'..='4') => {
            if let Some(s) = app.settings.as_mut() {
                let tab = (c as usize).saturating_sub('1' as usize);
                switch_tab(&mut s.cursor, tab);
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
    use ertangle_protocol::MergeStrategy;

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

    #[test]
    fn keep_awake_setting_is_a_toggle_backed_by_power_config() {
        let mut s = settings_with(None);
        assert!(ITEMS.contains(&Item::KeepAwake));
        assert!(Item::KeepAwake.is_toggle());
        assert_eq!(Item::KeepAwake.current(&s), "off");
        let values: Vec<_> = Item::KeepAwake
            .choices(&s)
            .into_iter()
            .map(|(v, _)| v)
            .collect();
        assert_eq!(values, ["on", "off"]);

        s.cfg = Config::parse("[power]\nkeep_awake = true\n").unwrap_or_default();
        assert_eq!(Item::KeepAwake.current(&s), "on");
    }
}
