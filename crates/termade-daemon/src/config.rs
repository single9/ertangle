//! `config.toml`：預設 agent、每個 agent／角色的 model、額外參數與環境變數。
//!
//! 位置：`~/.config/termade/config.toml`，repo 的 `.termade/config.toml` 逐欄覆寫。
//! 解析順序：spawn 請求的明確值 → `roles.<role>` → `agents.<kind>` → `defaults`。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use termade_core::{AgentKind, PermissionPolicy, Role};
use termade_protocol::SpawnRequest;
pub use toml::Value as TomlValue;
use tr::tr;

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Defaults {
    pub agent: Option<String>,
    pub permission: Option<String>,
    /// agent 經 MCP／帶 `parent` spawn 時的權限上限。
    pub max_agent_permission: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgentCfg {
    pub bin: Option<String>,
    pub model: Option<String>,
    /// TUI 選擇 model 時額外列出的建議清單。
    pub models: Vec<String>,
    pub effort: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub sandbox: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RoleCfg {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub permission: Option<String>,
    pub read_only: Option<bool>,
    pub effort: Option<String>,
    /// 這個角色專屬的協作說明，附加在通則後面。
    pub instructions: Option<String>,
    pub session_usd: Option<f64>,
}

/// agent 自己 (經 MCP／帶 `parent`)建立 session 時的防呆。只認全域設定：
/// repo 內的設定檔不可信，不能放寬使用者的成本防線。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgentSpawnCfg {
    /// 一律用獨立 worktree (預設 true；不在 git repo 內時略過)。
    pub worktree: Option<bool>,
    /// 免確認的 effort 上限 (預設 `medium`)。
    pub max_effort: Option<String>,
    /// 免確認的 model 白名單 (子字串比對，如 `["haiku", "sonnet"]`)；空 = 不限制。
    pub allowed_models: Vec<String>,
    /// 何時先問使用者：`never` (超標直接調降／拒絕)、`costly` (超標才問，預設)、`always`。
    pub confirm: Option<String>,
    /// 單一父 session 同時最多幾個執行中的子 session (預設 3；`[limits] max_children` 有設時以它為準)。
    pub max_live_children: Option<usize>,
}

/// 一次 agent spawn 的檢查結果。
#[derive(Debug, Clone, PartialEq)]
pub enum SpawnCheck {
    /// 在範圍內。
    Fine,
    /// 超出免確認範圍，原因列表；呼叫端依 `confirm` 決定要問人、調降或拒絕。
    Exceeds {
        reasons: Vec<String>,
        effort_too_high: bool,
        model_not_allowed: bool,
    },
}

impl AgentSpawnCfg {
    pub fn worktree(&self) -> bool {
        self.worktree.unwrap_or(true)
    }
    pub fn max_live_children(&self) -> usize {
        self.max_live_children.unwrap_or(3)
    }
    pub fn max_effort(&self) -> &str {
        self.max_effort.as_deref().unwrap_or("medium")
    }
    pub fn confirm(&self) -> &str {
        match self.confirm.as_deref() {
            Some(c @ ("never" | "always")) => c,
            _ => "costly",
        }
    }
    pub fn check(&self, model: Option<&str>, effort: Option<&str>) -> SpawnCheck {
        let rank = |e: &str| termade_adapter::EFFORT_LEVELS.iter().position(|x| *x == e);
        let mut reasons = Vec::new();
        let effort_too_high = match (effort.and_then(rank), rank(self.max_effort())) {
            (Some(e), Some(max)) if e > max => {
                reasons.push(tr!(
                    "effort {effort} 高於免確認上限 {max}",
                    effort = effort.unwrap_or(""),
                    max = self.max_effort()
                ));
                true
            }
            _ => false,
        };
        let model_not_allowed = match model {
            Some(m) if !self.allowed_models.is_empty() => {
                let ok = self.allowed_models.iter().any(|a| m.contains(a.as_str()));
                if !ok {
                    reasons.push(tr!(
                        "model {m} 不在免確認清單 ({list})",
                        m = m,
                        list = self.allowed_models.join(", ")
                    ));
                }
                !ok
            }
            _ => false,
        };
        if reasons.is_empty() {
            SpawnCheck::Fine
        } else {
            SpawnCheck::Exceeds {
                reasons,
                effort_too_high,
                model_not_allowed,
            }
        }
    }
}

/// 注入每個 agent 的協作說明 (`collab.rs`)。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct CollabCfg {
    /// 是否注入 (預設開啟)。
    pub instructions: Option<bool>,
    /// 附加在通則後面的文字。
    pub extra: Option<String>,
    /// 全域的 ADE 系統提示檔 (Markdown／文字)，附加在每個 agent 的協作說明裡。只認全域設定。
    pub prompt_file: Option<String>,
    /// 在協作說明後附上 `termade-ade` skill 內文 (預設關閉；沒安裝 skill 的人也能拿到)。
    pub inject_skill: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct TuiCfg {
    pub layout: Option<String>,
    /// 介面語言：`auto` (依 `LC_ALL`／`LC_MESSAGES`／`LANG`)、`zh-TW`、`en`；省略 = `auto`。
    /// 只認全域設定，見 `termade_core::i18n`。
    pub language: Option<String>,
    /// 通知方式：`bell`、`osc9`、`command`；空陣列 = 關閉，省略 = `["bell"]`。
    pub notify: Option<Vec<String>>,
    /// `command` 方式執行的命令 (`sh -c`)，內容由 `TERMADE_NOTIFY_*` 環境變數帶入。
    pub notify_command: Option<String>,
    /// 回合短於這個秒數時，「做完了」不通知 (預設 10)。
    pub notify_min_turn_secs: Option<u64>,
}

/// 成本與子 agent 數量上限。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct LimitsCfg {
    /// 單一 session 的成本上限 (美元)。
    pub session_usd: Option<f64>,
    /// 沒有美金數字的 agent 用這個 (輸入＋輸出 token 數)。
    pub session_tokens: Option<u64>,
    /// 一個 session 加上它所有後代的總和上限 (美元)。
    pub tree_usd: Option<f64>,
    /// 一個 session 同時 live 的直接子 agent 數上限。
    pub max_children: Option<usize>,
    /// 子 agent 層數上限 (人產生的是第 0 層)。
    pub max_depth: Option<usize>,
    /// 成本警告門檻 (百分比，預設 80)。
    pub warn_percent: Option<f64>,
}

/// 預設的成本警告門檻 (百分比)。
pub const DEFAULT_BUDGET_WARN_PERCENT: f64 = 80.0;

/// 額度提醒與自動交接。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct HandoffCfg {
    /// 帳號額度用量達到這個百分比就提醒 (預設 90)。
    pub warn_percent: Option<f32>,
    /// 額度用完時自動交接給 `fallback` 裡第一個可用的 agent (預設關閉，因為會動用另一邊的額度)。
    pub auto: Option<bool>,
    /// 候選接手者，例如 `["codex", "claude:sonnet"]`。
    pub fallback: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// 一個 Playbook 成員：展開後就是一個 `SpawnRequest`。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PlaybookMember {
    /// 成員標籤；省略時用 `playbook 名稱/角色`。
    pub label: String,
    /// agent id：`opencode` / `claude` / `codex` / `antigravity`。
    pub kind: String,
    /// `coordinator` / `worker` / `reviewer`。
    pub role: String,
    /// 是否用獨立 worktree。
    pub worktree: bool,
    /// 開場 prompt；`{{goal}}` 會被替換成使用者輸入的目標。
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// 一個多 Agent 團隊範本 ([playbooks.<name>])。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PlaybookConfig {
    pub description: String,
    /// TOML 裡寫成 `[[playbooks.<name>.sessions]]`。
    pub sessions: Vec<PlaybookMember>,
    /// `false` 時從可用清單中移除 (可用來隱藏內建範本)。
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// 開箱即用的 3 個團隊範本。使用者可用同名 `[playbooks.<name>]` 覆寫。
pub fn builtin_playbooks() -> BTreeMap<String, PlaybookConfig> {
    let member = |label: &str, kind: &str, role: &str, prompt: &str| PlaybookMember {
        label: label.to_string(),
        kind: kind.to_string(),
        role: role.to_string(),
        worktree: false,
        prompt: Some(prompt.to_string()),
        model: None,
        effort: None,
    };
    BTreeMap::from([
        (
            "coord-review".to_string(),
            PlaybookConfig {
                description: tr!("協調者拆解任務，完成後由審查者把關"),
                enabled: true,
                sessions: vec![
                    member(
                        "coord",
                        "claude",
                        "coordinator",
                        "請協調團隊完成以下目標：{{goal}}",
                    ),
                    member(
                        "reviewer",
                        "codex",
                        "reviewer",
                        "請審查以下目標的實作成果並列出問題：{{goal}}",
                    ),
                ],
            },
        ),
        (
            "tdd-pair".to_string(),
            PlaybookConfig {
                description: tr!("測試與實作配對協作：先寫失敗測試，再讓它通過"),
                enabled: true,
                sessions: vec![
                    member(
                        "coord",
                        "claude",
                        "coordinator",
                        "請協調測試與實作工程師完成以下目標：{{goal}}",
                    ),
                    member(
                        "test",
                        "opencode",
                        "worker",
                        "先針對以下目標撰寫會失敗的測試：{{goal}}",
                    ),
                    member(
                        "impl",
                        "claude",
                        "worker",
                        "請實作以下目標並讓所有測試通過：{{goal}}",
                    ),
                ],
            },
        ),
        (
            "dual-review".to_string(),
            PlaybookConfig {
                description: tr!("兩個不同 agent 平行審查同一份變更"),
                enabled: true,
                sessions: vec![
                    member(
                        "reviewer-a",
                        "claude",
                        "reviewer",
                        "請審查以下變更的正確性與邊界情況：{{goal}}",
                    ),
                    member(
                        "reviewer-b",
                        "codex",
                        "reviewer",
                        "請從安全、效能與可維護性角度審查以下變更：{{goal}}",
                    ),
                ],
            },
        ),
    ])
}

/// 遠端主機連線設定 ([remotes.<name>])。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteHostCfg {
    pub addr: String,
    #[serde(default)]
    pub token_file: Option<String>,
    #[serde(default)]
    pub token_env: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl RemoteHostCfg {
    pub fn resolve_token(&self) -> Option<String> {
        if let Some(env) = &self.token_env {
            if let Ok(t) = std::env::var(env) {
                let trimmed = t.trim().to_string();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
        }
        if let Some(file) = &self.token_file {
            let p = if let Some(rest) = file.strip_prefix("~/") {
                if let Some(home) = std::env::var_os("HOME") {
                    PathBuf::from(home).join(rest)
                } else {
                    PathBuf::from(file)
                }
            } else {
                PathBuf::from(file)
            };
            if let Ok(t) = crate::read_token_file(&p) {
                return Some(t);
            }
        }
        crate::get_client_token()
    }
}

/// Web 版支援設定 ([web])。
/// 為安全起見僅讀取全域設定檔，不接受 repo 設定檔覆寫。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WebCfg {
    pub enabled: Option<bool>,
    pub addr: Option<String>,
    pub port: Option<u16>,
}

impl WebCfg {
    pub fn is_enabled(&self) -> bool {
        match (self.enabled, self.addr.as_deref()) {
            (Some(false), _) => false,
            (Some(true), _) => true,
            (None, Some(a)) => !a.is_empty() && a != "off",
            (None, None) => false,
        }
    }

    /// 取得設定的 port，若未指定則依序從 addr 解析或使用預設 7421。
    pub fn resolved_port(&self) -> u16 {
        if let Some(p) = self.port {
            return p;
        }
        if let Some(a) = self.addr.as_deref() {
            if let Some((_, p_str)) = a.rsplit_once(':') {
                if let Ok(p) = p_str.parse::<u16>() {
                    return p;
                }
            } else if let Ok(p) = a.parse::<u16>() {
                return p;
            }
        }
        7421
    }

    pub fn resolved_addr(&self) -> String {
        let port = self.resolved_port();
        match self.addr.as_deref() {
            Some(a) if !a.is_empty() && a != "off" => {
                if a == "tailscale" {
                    if self.port.is_some() {
                        format!("tailscale:{port}")
                    } else {
                        "tailscale".to_string()
                    }
                } else if a.starts_with("tailscale:") {
                    if self.port.is_some() {
                        format!("tailscale:{port}")
                    } else {
                        a.to_string()
                    }
                } else if a.chars().all(|c| c.is_ascii_digit()) || a.starts_with(':') {
                    format!("tailscale:{port}")
                } else if let Some((host, _)) = a.rsplit_once(':') {
                    if self.port.is_some() {
                        format!("{host}:{port}")
                    } else {
                        a.to_string()
                    }
                } else {
                    format!("{a}:{port}")
                }
            }
            _ => {
                if self.port.is_some() {
                    format!("tailscale:{port}")
                } else {
                    "tailscale".to_string()
                }
            }
        }
    }
}

/// Git 與 worktree 相關設定 ([git])。
/// 為安全起見僅讀取全域設定檔，不接受 repo 設定檔覆寫任意 shell 指令。
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GitCfg {
    /// 合併前執行的驗證指令 (如 "cargo test")。非 0 退出則中止合併。
    pub pre_merge_command: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub limits: LimitsCfg,
    pub handoff: HandoffCfg,
    pub collab: CollabCfg,
    pub agent_spawn: AgentSpawnCfg,
    pub defaults: Defaults,
    pub agents: BTreeMap<String, AgentCfg>,
    pub roles: BTreeMap<String, RoleCfg>,
    pub tui: TuiCfg,
    /// Git 與 worktree 設定；為安全起見僅讀取全域設定檔。
    pub git: GitCfg,
    /// Web 版支援設定；為安全起見僅讀取全域設定檔。
    pub web: WebCfg,
    /// 遠端主機清單；為安全起見僅讀取全域設定檔，不接受 repo 設定檔覆寫。
    pub remotes: BTreeMap<String, RemoteHostCfg>,
    /// 使用者自訂的多 Agent 團隊範本；與內建範本合併，同名者覆寫。
    pub playbooks: BTreeMap<String, PlaybookConfig>,
}

/// 解析後、可直接餵給 `SessionSpec` 的結果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resolved {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub bin: Option<String>,
    pub permission: PermissionPolicy,
    /// 有 parent 時的上限；`None` 表示不額外限制 (仍受父 session 限制)。
    pub max_agent_permission: Option<PermissionPolicy>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub read_only: bool,
    pub sandbox: Option<String>,
    /// 設定檔解析出的單一 session 成本上限 (`roles.<role>.session_usd` > `limits.session_usd`)。
    pub session_usd: Option<f64>,
}

fn kind_key(kind: &AgentKind) -> String {
    kind.id().to_string()
}

/// 疊加時只允許調低、不允許調高 (用於 `limits` 與 repo 的 `roles.<role>.session_usd`)。
fn min_opt<T: PartialOrd + Copy>(global: Option<T>, local: Option<T>) -> Option<T> {
    match (global, local) {
        (Some(g), Some(l)) => Some(if l < g { l } else { g }),
        (Some(g), None) => Some(g),
        (None, Some(l)) => Some(l),
        (None, None) => None,
    }
}

impl Config {
    pub fn parse(src: &str) -> Result<Config, toml::de::Error> {
        toml::from_str(src)
    }

    /// 讀單一檔案；不存在回空設定，格式錯誤回報警告後當作空的。
    pub fn load_file(path: &Path) -> Config {
        let Ok(src) = std::fs::read_to_string(path) else {
            return Config::default();
        };
        match Config::parse(&src) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("設定檔 {path:?} 格式錯誤，已忽略：{e}");
                Config::default()
            }
        }
    }

    /// 全域設定被 repo 設定逐欄覆寫。
    pub fn layered(global: Config, local: Config) -> Config {
        let mut out = global;
        if local.defaults.agent.is_some() {
            out.defaults.agent = local.defaults.agent;
        }
        if local.defaults.permission.is_some() {
            out.defaults.permission = local.defaults.permission;
        }
        if local.defaults.max_agent_permission.is_some() {
            out.defaults.max_agent_permission = local.defaults.max_agent_permission;
        }
        for (k, v) in local.agents {
            let e = out.agents.entry(k).or_default();
            if v.bin.is_some() {
                e.bin = v.bin;
            }
            if v.model.is_some() {
                e.model = v.model;
            }
            if !v.args.is_empty() {
                e.args = v.args;
            }
            e.env.extend(v.env);
            if v.effort.is_some() {
                e.effort = v.effort;
            }
            if !v.models.is_empty() {
                e.models = v.models;
            }
            if !v.allowed_tools.is_empty() {
                e.allowed_tools = v.allowed_tools;
            }
            if !v.disallowed_tools.is_empty() {
                e.disallowed_tools = v.disallowed_tools;
            }
            if v.sandbox.is_some() {
                e.sandbox = v.sandbox;
            }
        }
        let global_session_usd = out.limits.session_usd;
        out.limits.session_usd = min_opt(out.limits.session_usd, local.limits.session_usd);
        out.limits.session_tokens = min_opt(out.limits.session_tokens, local.limits.session_tokens);
        out.limits.tree_usd = min_opt(out.limits.tree_usd, local.limits.tree_usd);
        out.limits.max_children = min_opt(out.limits.max_children, local.limits.max_children);
        out.limits.max_depth = min_opt(out.limits.max_depth, local.limits.max_depth);
        out.limits.warn_percent = min_opt(out.limits.warn_percent, local.limits.warn_percent);

        for (k, v) in local.roles {
            let e = out.roles.entry(k).or_default();
            if v.agent.is_some() {
                e.agent = v.agent;
            }
            if v.model.is_some() {
                e.model = v.model;
            }
            if v.permission.is_some() {
                e.permission = v.permission;
            }
            if v.read_only.is_some() {
                e.read_only = v.read_only;
            }
            if v.effort.is_some() {
                e.effort = v.effort;
            }
            if v.instructions.is_some() {
                e.instructions = v.instructions;
            }
            if v.session_usd.is_some() {
                let cap = e.session_usd.or(global_session_usd);
                e.session_usd = min_opt(cap, v.session_usd);
            }
        }
        if local.limits.session_usd.is_some() {
            for r in out.roles.values_mut() {
                if r.session_usd.is_some() {
                    r.session_usd = min_opt(r.session_usd, local.limits.session_usd);
                }
            }
        }
        if local.collab.instructions.is_some() {
            out.collab.instructions = local.collab.instructions;
        }
        if local.collab.inject_skill.is_some() {
            out.collab.inject_skill = local.collab.inject_skill;
        }
        if local.collab.extra.is_some() {
            out.collab.extra = local.collab.extra;
        }
        if local.handoff.warn_percent.is_some() {
            out.handoff.warn_percent = local.handoff.warn_percent;
        }
        if local.handoff.auto.is_some() {
            out.handoff.auto = local.handoff.auto;
        }
        if !local.handoff.fallback.is_empty() {
            out.handoff.fallback = local.handoff.fallback;
        }
        if local.tui.layout.is_some() {
            out.tui.layout = local.tui.layout;
        }
        if local.tui.notify.is_some() {
            out.tui.notify = local.tui.notify;
        }
        // notify_command 只認全域設定：repo 內的設定檔不可信，不能讓它決定要執行的命令。
        if local.tui.notify_min_turn_secs.is_some() {
            out.tui.notify_min_turn_secs = local.tui.notify_min_turn_secs;
        }
        for (name, pb) in local.playbooks {
            out.playbooks.insert(name, pb);
        }
        out
    }

    /// 內建範本與使用者自訂範本的合併結果；`enabled = false` 的範本會被移除。
    pub fn playbooks(&self) -> BTreeMap<String, PlaybookConfig> {
        let mut out = builtin_playbooks();
        for (name, pb) in &self.playbooks {
            if pb.enabled {
                out.insert(name.clone(), pb.clone());
            } else {
                out.remove(name);
            }
        }
        out
    }

    /// 把 `name` 範本展開成一串 `SpawnRequest`：第一個是主 Session (coordinator，
    /// 或清單中第一個 `role == coordinator` 的成員)，呼叫端需把其餘成員的 `parent`
    /// 指向主 Session。`{{goal}}` 會代入 `goal`。
    pub fn playbook_spawns(&self, name: &str, goal: &str) -> Result<Vec<SpawnRequest>, String> {
        let goal = goal.trim();
        if goal.is_empty() {
            return Err(tr!("目標需求不可為空"));
        }
        let pbs = self.playbooks();
        let pb = pbs.get(name).ok_or_else(|| {
            let list = pbs.keys().cloned().collect::<Vec<_>>().join(", ");
            tr!(
                "未知的 playbook：{name} (可用：{list})",
                name = name,
                list = list
            )
        })?;
        if pb.sessions.is_empty() {
            return Err(tr!("playbook {name} 沒有成員", name = name));
        }
        let coordinator = pb
            .sessions
            .iter()
            .position(|m| Role::parse(&m.role) == Role::Coordinator)
            .unwrap_or(0);
        let mut ordered: Vec<&PlaybookMember> = vec![&pb.sessions[coordinator]];
        ordered.extend(
            pb.sessions
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != coordinator)
                .map(|(_, m)| m),
        );

        ordered
            .into_iter()
            .map(|m| {
                let kind = AgentKind::parse(&m.kind)
                    .ok_or_else(|| tr!("未知的 agent：{k}", k = m.kind))?;
                let role = Role::parse(&m.role);
                let label = if m.label.is_empty() {
                    format!("{name}/{}", role.id())
                } else {
                    format!("{name}/{}", m.label)
                };
                let prompt = m
                    .prompt
                    .as_deref()
                    .unwrap_or("{{goal}}")
                    .replace("{{goal}}", goal);
                Ok(SpawnRequest {
                    kind,
                    cwd: None,
                    label: Some(label),
                    role,
                    prompt: Some(prompt),
                    worktree: m.worktree,
                    transport: Default::default(),
                    permission: None,
                    parent: None,
                    model: m.model.clone(),
                    effort: m.effort.clone(),
                    attachments: Vec::new(),
                    inherit_from: None,
                    args: Vec::new(),
                    budget_usd: None,
                })
            })
            .collect()
    }

    /// 把單一設定值寫進 `path` (不存在就建立)：`table` 是表格路徑 (如 `["agents","claude"]`)，
    /// 值為空字串代表刪除該鍵。會以 TOML 重新序列化，檔案內的註解不會保留。
    pub fn set_value(path: &Path, table: &[&str], key: &str, value: &str) -> anyhow::Result<()> {
        let v = (!value.is_empty()).then(|| toml::Value::String(value.to_string()));
        Config::set_toml(path, table, key, v)
    }

    /// 同 `set_value`，但可寫任意 TOML 值 (布林、數字…)；`None` 代表刪除該鍵。
    pub fn set_toml(
        path: &Path,
        table: &[&str],
        key: &str,
        value: Option<toml::Value>,
    ) -> anyhow::Result<()> {
        let mut root: toml::Table = match std::fs::read_to_string(path) {
            Ok(src) => src.parse()?,
            Err(_) => toml::Table::new(),
        };
        let mut cur = &mut root;
        for t in table {
            cur = cur
                .entry(t.to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!(tr!("`{t}` 不是表格", t = t)))?;
        }
        match value {
            Some(v) => {
                cur.insert(key.to_string(), v);
            }
            None => {
                cur.remove(key);
            }
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // 先寫暫存檔再改名，避免寫到一半被讀到。
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(&root)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// 載入全域 (`config_dir`)與 repo (`<cwd>/.termade/config.toml`)設定。
    pub fn load(global_dir: &Path, cwd: &Path) -> Config {
        Config::layered(
            Config::load_file(&global_dir.join("config.toml")),
            Config::load_file(&cwd.join(".termade").join("config.toml")),
        )
    }

    /// 套用四層覆寫順序，決定一次 spawn 實際的 model／參數／環境。
    pub fn resolve(
        &self,
        kind: &AgentKind,
        role: &Role,
        req_model: Option<&str>,
        req_effort: Option<&str>,
        req_args: &[String],
        req_permission: Option<PermissionPolicy>,
    ) -> Resolved {
        let agent = self.agents.get(&kind_key(kind));
        let role_cfg = self.roles.get(role.id());
        let model = req_model
            .map(str::to_string)
            .or_else(|| role_cfg.and_then(|r| r.model.clone()))
            .or_else(|| agent.and_then(|a| a.model.clone()));
        let effort = req_effort
            .map(str::to_string)
            .or_else(|| role_cfg.and_then(|r| r.effort.clone()))
            .or_else(|| agent.and_then(|a| a.effort.clone()));
        let mut args = agent.map(|a| a.args.clone()).unwrap_or_default();
        args.extend(req_args.iter().cloned());
        let env = agent
            .map(|a| a.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let permission = req_permission
            .or_else(|| {
                role_cfg
                    .and_then(|r| r.permission.as_deref())
                    .and_then(PermissionPolicy::parse)
            })
            .or_else(|| {
                self.defaults
                    .permission
                    .as_deref()
                    .and_then(PermissionPolicy::parse)
            })
            .unwrap_or_default();
        Resolved {
            permission,
            max_agent_permission: self
                .defaults
                .max_agent_permission
                .as_deref()
                .and_then(PermissionPolicy::parse),
            allowed_tools: agent.map(|a| a.allowed_tools.clone()).unwrap_or_default(),
            disallowed_tools: agent
                .map(|a| a.disallowed_tools.clone())
                .unwrap_or_default(),
            read_only: role_cfg.and_then(|r| r.read_only).unwrap_or(false),
            sandbox: agent.and_then(|a| a.sandbox.clone()),
            session_usd: role_cfg
                .and_then(|r| r.session_usd)
                .or(self.limits.session_usd),
            model,
            effort,
            args,
            env,
            bin: agent.and_then(|a| a.bin.clone()),
        }
    }

    /// 解析單一 session 的實際成本上限 (優先序：請求覆寫 > `roles.<role>.session_usd` > `limits.session_usd`)。
    pub fn resolve_budget_usd(&self, role: &Role, req_budget: Option<f64>) -> Option<f64> {
        req_budget
            .or_else(|| self.roles.get(role.id()).and_then(|r| r.session_usd))
            .or(self.limits.session_usd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
[defaults]
agent = "claude"

[agents.claude]
model = "sonnet"
args = ["--foo"]
env = { A = "1" }

[roles.worker]
model = "haiku"
"#;

    #[test]
    fn precedence_request_role_agent() {
        let c = Config::parse(SRC).unwrap();
        let k = AgentKind::ClaudeCode;
        let r = c.resolve(&k, &Role::Coordinator, None, None, &[], None);
        assert_eq!(r.model.as_deref(), Some("sonnet")); // agent 層
        let r = c.resolve(&k, &Role::Worker, None, None, &[], None);
        assert_eq!(r.model.as_deref(), Some("haiku")); // role 蓋過 agent
        let r = c.resolve(
            &k,
            &Role::Worker,
            Some("opus"),
            None,
            &["--bar".into()],
            None,
        );
        assert_eq!(r.model.as_deref(), Some("opus")); // 請求最優先
        assert_eq!(r.args, vec!["--foo", "--bar"]);
        assert_eq!(r.env, vec![("A".to_string(), "1".to_string())]);
        let r = c.resolve(&AgentKind::Codex, &Role::Coordinator, None, None, &[], None);
        assert_eq!(r.model, None);
    }

    #[test]
    fn web_cfg_parsing_and_defaults() {
        let toml_src = r#"
[web]
enabled = true
addr = "127.0.0.1:7421"
"#;
        let c = Config::parse(toml_src).unwrap();
        assert!(c.web.is_enabled());
        assert_eq!(c.web.resolved_addr(), "127.0.0.1:7421");

        let toml_off = r#"
[web]
enabled = false
addr = "tailscale"
"#;
        let c2 = Config::parse(toml_off).unwrap();
        assert!(!c2.web.is_enabled());

        let toml_default = "";
        let c3 = Config::parse(toml_default).unwrap();
        assert!(!c3.web.is_enabled());
        assert_eq!(c3.web.resolved_addr(), "tailscale");

        let toml_implicit = r#"
[web]
addr = "tailscale"
"#;
        let c4 = Config::parse(toml_implicit).unwrap();
        assert!(c4.web.is_enabled());

        let toml_port = r#"
[web]
enabled = true
port = 8080
"#;
        let c5 = Config::parse(toml_port).unwrap();
        assert!(c5.web.is_enabled());
        assert_eq!(c5.web.resolved_port(), 8080);
        assert_eq!(c5.web.resolved_addr(), "tailscale:8080");

        let toml_port_override = r#"
[web]
enabled = true
addr = "127.0.0.1:7421"
port = 9090
"#;
        let c6 = Config::parse(toml_port_override).unwrap();
        assert_eq!(c6.web.resolved_port(), 9090);
        assert_eq!(c6.web.resolved_addr(), "127.0.0.1:9090");
    }

    #[test]
    fn local_overrides_global_per_field() {
        let g = Config::parse(SRC).unwrap();
        let l = Config::parse("[agents.claude]\nmodel = \"opus\"\n").unwrap();
        let c = Config::layered(g, l);
        let a = &c.agents["claude"];
        assert_eq!(a.model.as_deref(), Some("opus"));
        assert_eq!(a.args, vec!["--foo"]);
        assert_eq!(c.defaults.agent.as_deref(), Some("claude"));
    }

    #[test]
    fn role_permission_applies_and_request_overrides() {
        let c = Config::parse(
            "[defaults]\npermission = \"auto\"\nmax_agent_permission = \"auto\"\n[roles.reviewer]\npermission = \"ask\"\nread_only = true\n",
        )
        .unwrap();
        let k = AgentKind::ClaudeCode;
        let r = c.resolve(&k, &Role::Reviewer, None, None, &[], None);
        assert_eq!(r.permission, PermissionPolicy::Ask);
        assert!(r.read_only);
        assert_eq!(r.max_agent_permission, Some(PermissionPolicy::Auto));
        let r = c.resolve(
            &k,
            &Role::Reviewer,
            None,
            None,
            &[],
            Some(PermissionPolicy::Bypass),
        );
        assert_eq!(r.permission, PermissionPolicy::Bypass);
        let r = c.resolve(&k, &Role::Worker, None, None, &[], None);
        assert_eq!(r.permission, PermissionPolicy::Auto);
    }

    #[test]
    fn effort_precedence_request_role_agent() {
        let c =
            Config::parse("[agents.claude]\neffort=\"low\"\n[roles.reviewer]\neffort=\"high\"\n")
                .unwrap();
        let k = AgentKind::ClaudeCode;
        assert_eq!(
            c.resolve(&k, &Role::Worker, None, None, &[], None)
                .effort
                .as_deref(),
            Some("low")
        );
        assert_eq!(
            c.resolve(&k, &Role::Reviewer, None, None, &[], None)
                .effort
                .as_deref(),
            Some("high")
        );
        assert_eq!(
            c.resolve(&k, &Role::Reviewer, None, Some("max"), &[], None)
                .effort
                .as_deref(),
            Some("max")
        );
    }

    #[test]
    fn bad_file_is_ignored() {
        assert!(Config::parse("[[[").is_err());
    }

    #[test]
    fn custom_bin_in_agent_cfg() {
        let c = Config::parse(
            r#"
[agents.claude]
bin = "/opt/custom/claude"
"#,
        )
        .unwrap();
        let r = c.resolve(&AgentKind::ClaudeCode, &Role::Worker, None, None, &[], None);
        assert_eq!(r.bin.as_deref(), Some("/opt/custom/claude"));
    }

    #[test]
    fn tui_layout_config_parsing_and_layered() {
        let g = Config::parse(
            r#"
[tui]
layout = "two"
"#,
        )
        .unwrap();
        assert_eq!(g.tui.layout.as_deref(), Some("two"));
        let l = Config::parse(
            r#"
[tui]
layout = "tabs"
"#,
        )
        .unwrap();
        let layered = Config::layered(g, l);
        assert_eq!(layered.tui.layout.as_deref(), Some("tabs"));
    }

    #[test]
    fn tui_notify_config_parsing_and_layered() {
        assert_eq!(Config::parse("").unwrap().tui.notify, None);
        let g = Config::parse(
            r#"
[tui]
layout = "two"
notify = ["bell", "command"]
notify_command = "notify-send termade \"$TERMADE_NOTIFY_TEXT\""
notify_min_turn_secs = 30
"#,
        )
        .unwrap();
        assert_eq!(
            g.tui.notify,
            Some(vec!["bell".to_string(), "command".to_string()])
        );
        assert_eq!(
            g.tui.notify_command.as_deref(),
            Some("notify-send termade \"$TERMADE_NOTIFY_TEXT\"")
        );
        assert_eq!(g.tui.notify_min_turn_secs, Some(30));

        // repo 只覆寫有寫的欄位；空陣列也算有寫 (用來關閉通知)。
        let l = Config::parse("[tui]\nnotify = []\nnotify_min_turn_secs = 5\n").unwrap();
        let c = Config::layered(g.clone(), l);
        assert_eq!(c.tui.notify, Some(vec![]));
        assert_eq!(c.tui.notify_min_turn_secs, Some(5));
        assert!(c.tui.notify_command.is_some());
        assert_eq!(c.tui.layout.as_deref(), Some("two"));

        let l = Config::parse("[tui]\nnotify_command = \"true\"\n").unwrap();
        let c = Config::layered(g, l);
        assert_eq!(
            c.tui.notify_command.as_deref(),
            Some("notify-send termade \"$TERMADE_NOTIFY_TEXT\""),
            "repo 不能覆寫 notify_command"
        );
        assert_eq!(c.tui.notify.map(|v| v.len()), Some(2));
    }

    #[test]
    fn collab_config_parsing_and_layered() {
        let g = Config::parse(
            "[collab]\nextra = \"全域\"\n[roles.reviewer]\ninstructions = \"只讀\"\nmodel = \"m\"\n",
        )
        .unwrap();
        assert_eq!(g.collab.instructions, None); // 沒寫＝預設開啟
        assert_eq!(g.roles["reviewer"].instructions.as_deref(), Some("只讀"));
        let l = Config::parse(
            "[collab]\ninstructions = false\n[roles.reviewer]\ninstructions = \"repo\"\n",
        )
        .unwrap();
        let c = Config::layered(g, l);
        assert_eq!(c.collab.instructions, Some(false));
        assert_eq!(c.collab.extra.as_deref(), Some("全域")); // repo 沒寫的欄位保留全域
        assert_eq!(c.roles["reviewer"].instructions.as_deref(), Some("repo"));
        assert_eq!(c.roles["reviewer"].model.as_deref(), Some("m"));
    }

    #[test]
    fn limits_config_layered_only_lowers_never_raises() {
        let global = Config::parse(
            r#"
[limits]
session_usd = 5.0
session_tokens = 200000
tree_usd = 20.0
max_children = 4
max_depth = 2
warn_percent = 80

[roles.coordinator]
session_usd = 10.0
"#,
        )
        .unwrap();

        // repo 試圖調高各項上限：全部被忽略，維持全域上限
        let local_higher = Config::parse(
            r#"
[limits]
session_usd = 50.0
session_tokens = 900000
tree_usd = 100.0
max_children = 10
max_depth = 5
warn_percent = 95

[roles.coordinator]
session_usd = 50.0
[roles.worker]
session_usd = 20.0
"#,
        )
        .unwrap();
        let c1 = Config::layered(global.clone(), local_higher);
        assert_eq!(c1.limits.session_usd, Some(5.0));
        assert_eq!(c1.limits.session_tokens, Some(200_000));
        assert_eq!(c1.limits.tree_usd, Some(20.0));
        assert_eq!(c1.limits.max_children, Some(4));
        assert_eq!(c1.limits.max_depth, Some(2));
        assert_eq!(c1.limits.warn_percent, Some(80.0));
        assert_eq!(c1.roles["coordinator"].session_usd, Some(10.0));
        // worker 原本沒有 role session_usd，受 global.limits.session_usd (5.0) 限制
        assert_eq!(c1.roles["worker"].session_usd, Some(5.0));

        // repo 調低各項上限：採用較低值
        let local_lower = Config::parse(
            r#"
[limits]
session_usd = 2.0
tree_usd = 8.0
max_children = 2
max_depth = 1

[roles.coordinator]
session_usd = 1.5
"#,
        )
        .unwrap();
        let c2 = Config::layered(global, local_lower);
        assert_eq!(c2.limits.session_usd, Some(2.0));
        assert_eq!(c2.limits.tree_usd, Some(8.0));
        assert_eq!(c2.limits.max_children, Some(2));
        assert_eq!(c2.limits.max_depth, Some(1));
        assert_eq!(c2.roles["coordinator"].session_usd, Some(1.5));
        assert_eq!(c2.resolve_budget_usd(&Role::Worker, None), Some(2.0));
        assert_eq!(c2.resolve_budget_usd(&Role::Coordinator, None), Some(1.5));
        assert_eq!(c2.resolve_budget_usd(&Role::Worker, Some(0.5)), Some(0.5));
    }

    #[test]
    fn set_value_creates_updates_and_removes() {
        let dir = std::env::temp_dir().join(format!("termade-cfg-{}", std::process::id()));
        let path = dir.join("sub").join("config.toml");
        Config::set_value(&path, &["agents", "claude"], "model", "opus").unwrap();
        Config::set_value(&path, &["agents", "claude"], "effort", "high").unwrap();
        Config::set_value(&path, &["defaults"], "permission", "ask").unwrap();
        let c = Config::load_file(&path);
        assert_eq!(c.agents["claude"].model.as_deref(), Some("opus"));
        assert_eq!(c.agents["claude"].effort.as_deref(), Some("high"));
        assert_eq!(c.defaults.permission.as_deref(), Some("ask"));
        // 覆寫與刪除
        Config::set_value(&path, &["agents", "claude"], "model", "haiku").unwrap();
        Config::set_value(&path, &["agents", "claude"], "effort", "").unwrap();
        let c = Config::load_file(&path);
        assert_eq!(c.agents["claude"].model.as_deref(), Some("haiku"));
        assert_eq!(c.agents["claude"].effort, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_spawn_check_flags_costly_requests() {
        let c = Config::parse(
            r#"
[agent_spawn]
max_effort = "medium"
allowed_models = ["haiku", "sonnet"]
"#,
        )
        .unwrap();
        let a = &c.agent_spawn;
        assert!(a.worktree());
        assert_eq!(a.confirm(), "costly");
        assert_eq!(
            a.check(Some("claude-haiku-5-5"), Some("low")),
            SpawnCheck::Fine
        );
        assert_eq!(a.check(None, None), SpawnCheck::Fine);
        let SpawnCheck::Exceeds {
            effort_too_high,
            model_not_allowed,
            ..
        } = a.check(Some("opus"), Some("max"))
        else {
            panic!()
        };
        assert!(effort_too_high && model_not_allowed);
        // repo 設定不能放寬
        let local =
            Config::parse("[agent_spawn]\nconfirm = \"never\"\nworktree = false\n").unwrap();
        let merged = Config::layered(c, local);
        assert_eq!(merged.agent_spawn.confirm(), "costly");
        assert!(merged.agent_spawn.worktree());
    }

    #[test]
    fn remotes_parsing_and_security_ignored_in_local() {
        let global_src = r#"
[remotes.workstation]
addr = "100.64.0.12:7420"
token_file = "~/.config/termade/workstation.token"
enabled = true

[remotes.gpu]
addr = "100.64.0.50:7420"
token_env = "GPU_TOKEN"
enabled = false
"#;
        let global = Config::parse(global_src).unwrap();
        assert_eq!(global.remotes.len(), 2);
        let ws = &global.remotes["workstation"];
        assert_eq!(ws.addr, "100.64.0.12:7420");
        assert_eq!(
            ws.token_file.as_deref(),
            Some("~/.config/termade/workstation.token")
        );
        assert!(ws.enabled);
        let gpu = &global.remotes["gpu"];
        assert_eq!(gpu.token_env.as_deref(), Some("GPU_TOKEN"));
        assert!(!gpu.enabled);

        // repo 設定中定義的 remotes 必須被忽略，不得覆蓋全域 remotes
        let local_src = r#"
[remotes.evil]
addr = "6.6.6.6:7420"
enabled = true
"#;
        let local = Config::parse(local_src).unwrap();
        let merged = Config::layered(global, local);
        assert_eq!(merged.remotes.len(), 2);
        assert!(!merged.remotes.contains_key("evil"));
        assert!(merged.remotes.contains_key("workstation"));
    }

    #[test]
    fn git_pre_merge_command_parsing_and_security_ignored_in_local() {
        let global_src = r#"
[git]
pre_merge_command = "cargo test"
"#;
        let global = Config::parse(global_src).unwrap();
        assert_eq!(global.git.pre_merge_command.as_deref(), Some("cargo test"));

        // repo 設定不得覆寫 pre_merge_command
        let local_src = r#"
[git]
pre_merge_command = "rm -rf /"
"#;
        let local = Config::parse(local_src).unwrap();
        let merged = Config::layered(global, local);
        assert_eq!(merged.git.pre_merge_command.as_deref(), Some("cargo test"));
    }

    #[test]
    fn playbooks_merge_builtins_and_expand_goal_into_spawn_requests() {
        let src = r#"
[playbooks.tdd]
description = "自訂 TDD 團隊"
[[playbooks.tdd.sessions]]
label = "coord"
kind = "claude"
role = "coordinator"
prompt = "協調：{{goal}}"
[[playbooks.tdd.sessions]]
label = "reviewer"
kind = "opencode"
role = "reviewer"
worktree = true

[playbooks.coord-review]
enabled = false
"#;
        let cfg = Config::parse(src).unwrap();
        let merged = Config::layered(Config::default(), cfg);
        let pbs = merged.playbooks();
        assert_eq!(pbs.len(), 3); // 3 個內建，其中 coord-review 被停用，tdd 為新增
        assert!(pbs.contains_key("tdd"));
        assert!(!pbs.contains_key("coord-review"));
        assert!(pbs.contains_key("dual-review"));
        assert_eq!(pbs["tdd"].description, "自訂 TDD 團隊");

        let spawns = merged.playbook_spawns("tdd", "  修好登入功能  ").unwrap();
        assert_eq!(spawns.len(), 2);
        assert_eq!(spawns[0].role, Role::Coordinator);
        assert_eq!(spawns[0].label.as_deref(), Some("tdd/coord"));
        assert_eq!(spawns[0].prompt.as_deref(), Some("協調：修好登入功能"));
        assert_eq!(spawns[1].role, Role::Reviewer);
        assert_eq!(spawns[1].label.as_deref(), Some("tdd/reviewer"));
        assert!(spawns[1].worktree);
        // 沒有 prompt 的成員直接以目標需求開場。
        assert_eq!(spawns[1].prompt.as_deref(), Some("修好登入功能"));

        // 內建範本依角色排序：coordinator 永遠是第一個。
        let builtin = Config::default()
            .playbook_spawns("tdd-pair", "寫功能")
            .unwrap();
        assert_eq!(builtin.len(), 3);
        assert_eq!(builtin[0].role, Role::Coordinator);
        assert!(builtin[0].prompt.as_deref().unwrap().contains("寫功能"));
        assert!(builtin
            .iter()
            .all(|s| !s.prompt.as_deref().unwrap().contains("{{goal}}")));

        assert!(merged.playbook_spawns("nope", "x").is_err());
        assert!(merged.playbook_spawns("tdd", "  ").is_err());
    }
}
