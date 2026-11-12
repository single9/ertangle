//! `config.toml`：預設 agent、每個 agent／角色的 model、額外參數與環境變數。
//!
//! 位置：`~/.config/ade/config.toml`，repo 的 `.ade/config.toml` 逐欄覆寫。
//! 解析順序：spawn 請求的明確值 → `roles.<role>` → `agents.<kind>` → `defaults`。

use ade_core::{AgentKind, PermissionPolicy, Role};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

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
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub defaults: Defaults,
    pub agents: BTreeMap<String, AgentCfg>,
    pub roles: BTreeMap<String, RoleCfg>,
}

/// 解析後、可直接餵給 `SessionSpec` 的結果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resolved {
    pub model: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub bin: Option<String>,
    pub permission: PermissionPolicy,
    /// 有 parent 時的上限；`None` 表示不額外限制（仍受父 session 限制）。
    pub max_agent_permission: Option<PermissionPolicy>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub read_only: bool,
    pub sandbox: Option<String>,
}

fn kind_key(kind: &AgentKind) -> String {
    kind.id().to_string()
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
        }
        out
    }

    /// 載入全域（`config_dir`）與 repo（`<cwd>/.ade/config.toml`）設定。
    pub fn load(global_dir: &Path, cwd: &Path) -> Config {
        Config::layered(
            Config::load_file(&global_dir.join("config.toml")),
            Config::load_file(&cwd.join(".ade").join("config.toml")),
        )
    }

    /// 套用四層覆寫順序，決定一次 spawn 實際的 model／參數／環境。
    pub fn resolve(
        &self,
        kind: &AgentKind,
        role: &Role,
        req_model: Option<&str>,
        req_args: &[String],
        req_permission: Option<PermissionPolicy>,
    ) -> Resolved {
        let agent = self.agents.get(&kind_key(kind));
        let role_cfg = self.roles.get(role.id());
        let model = req_model
            .map(str::to_string)
            .or_else(|| role_cfg.and_then(|r| r.model.clone()))
            .or_else(|| agent.and_then(|a| a.model.clone()));
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
            model,
            args,
            env,
            bin: agent.and_then(|a| a.bin.clone()),
        }
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
        let r = c.resolve(&k, &Role::Coordinator, None, &[], None);
        assert_eq!(r.model.as_deref(), Some("sonnet")); // agent 層
        let r = c.resolve(&k, &Role::Worker, None, &[], None);
        assert_eq!(r.model.as_deref(), Some("haiku")); // role 蓋過 agent
        let r = c.resolve(&k, &Role::Worker, Some("opus"), &["--bar".into()], None);
        assert_eq!(r.model.as_deref(), Some("opus")); // 請求最優先
        assert_eq!(r.args, vec!["--foo", "--bar"]);
        assert_eq!(r.env, vec![("A".to_string(), "1".to_string())]);
        let r = c.resolve(&AgentKind::Codex, &Role::Coordinator, None, &[], None);
        assert_eq!(r.model, None);
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
        let r = c.resolve(&k, &Role::Reviewer, None, &[], None);
        assert_eq!(r.permission, PermissionPolicy::Ask);
        assert!(r.read_only);
        assert_eq!(r.max_agent_permission, Some(PermissionPolicy::Auto));
        let r = c.resolve(
            &k,
            &Role::Reviewer,
            None,
            &[],
            Some(PermissionPolicy::Bypass),
        );
        assert_eq!(r.permission, PermissionPolicy::Bypass);
        let r = c.resolve(&k, &Role::Worker, None, &[], None);
        assert_eq!(r.permission, PermissionPolicy::Auto);
    }

    #[test]
    fn bad_file_is_ignored() {
        assert!(Config::parse("[[[").is_err());
    }
}
