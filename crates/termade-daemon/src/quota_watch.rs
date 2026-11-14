//! 額度提醒與自動交接：額度快滿時在 session 上留下交接建議；額度用完且設定了
//! `[handoff] auto = true` 時，自動交接給 `fallback` 裡第一個可用的 agent。

use std::sync::Arc;
use tr::tr;

use termade_core::{now_millis, AgentKind, Event, Quota, SessionId};

use crate::config::Config;
use crate::paths::config_dir;
use crate::{client_channel, lock, Shared};

/// 預設的提醒門檻（百分比）。
pub(crate) const DEFAULT_WARN_PERCENT: f32 = 90.0;
/// 自動交接最多串接幾次（避免 A→B→A 無限循環）。
const MAX_HANDOFF_DEPTH: usize = 3;

/// 最吃緊的「尚未重置」視窗用量；全都已重置回傳 0。
pub(crate) fn live_pressure(q: &Quota, now_ms: u64) -> f32 {
    q.windows
        .iter()
        .filter(|w| w.resets_at_ms.is_none_or(|r| r > now_ms))
        .map(|w| w.used_percent)
        .fold(0.0, f32::max)
}

/// 從 `fallback`（`kind[:model]`）挑第一個可接手的：agent 不同於來源，且它的額度沒滿。
pub(crate) fn pick_fallback(
    fallback: &[String],
    source: &AgentKind,
    quotas: &std::collections::BTreeMap<String, Quota>,
    now_ms: u64,
) -> Option<(AgentKind, Option<String>)> {
    fallback.iter().find_map(|entry| {
        let (k, m) = match entry.split_once(':') {
            Some((k, m)) if !m.is_empty() => (k, Some(m.to_string())),
            _ => (entry.as_str(), None),
        };
        let kind = AgentKind::parse(k)?;
        // 同一個 agent 換 model 通常共用同一個帳號額度，沒有幫助。
        if kind.id() == source.id() {
            return None;
        }
        let blocked = quotas
            .get(kind.id())
            .is_some_and(|q| q.reached || live_pressure(q, now_ms) >= 100.0);
        (!blocked).then_some((kind, m))
    })
}

impl Shared {
    pub(crate) fn session_config(&self, s: &termade_core::Session) -> Config {
        let cwd = std::path::Path::new(&s.cwd);
        let root = std::path::Path::new(s.project_root());
        let def = self.default_cwd();
        let dir = if cwd.join(".termade").join("config.toml").is_file() {
            cwd
        } else if root.join(".termade").join("config.toml").is_file() {
            root
        } else if def.join(".termade").join("config.toml").is_file() {
            &def
        } else {
            cwd
        };
        Config::load(&config_dir(), dir)
    }

    /// `ingest` 在送出 `Event::Cost` 之後呼叫：檢查單一 session 與整棵樹的成本上限。
    pub(crate) fn check_budget(self: &Arc<Self>, sid: &SessionId) {
        let Some(s) = lock(&self.inner).state.sessions.get(sid).cloned() else {
            return;
        };
        let cfg = self.session_config(&s);
        let warn_pct = cfg
            .limits
            .warn_percent
            .unwrap_or(crate::config::DEFAULT_BUDGET_WARN_PERCENT);

        // 1) 單一 session 上限：沒有美金數字（usd == 0 且有 token）時改看 session_tokens，否則看 USD。
        let total_tokens = s.cost.input_tokens + s.cost.output_tokens;
        let usd_limit = s
            .budget_usd
            .or_else(|| cfg.resolve_budget_usd(&s.role, None));
        if s.cost.usd == 0.0 && total_tokens > 0 && cfg.limits.session_tokens.is_some() {
            if let Some(tok_limit) = cfg.limits.session_tokens.filter(|&l| l > 0) {
                let pct = total_tokens as f64 * 100.0 / tok_limit as f64;
                if pct < warn_pct {
                    lock_set(&self.budget_warned).remove(sid);
                } else if lock_set(&self.budget_warned).insert(sid.clone()) {
                    self.emit(Event::SessionNote {
                        session: sid.clone(),
                        note: tr!(
                            "token 用量已達 {used}/{limit}（{pct}%），接近上限",
                            used = termade_core::human_tokens(total_tokens),
                            limit = termade_core::human_tokens(tok_limit),
                            pct = format!("{pct:.0}")
                        ),
                    });
                }
                if total_tokens >= tok_limit {
                    if let Some((_, ctl)) = self.control(sid) {
                        let _ = ctl.cancel();
                    }
                    self.emit(Event::SessionOverBudget {
                        session: sid.clone(),
                        spent: total_tokens as f64,
                        limit: tok_limit as f64,
                    });
                }
            }
        } else if let Some(limit) = usd_limit.filter(|&l| l > 0.0) {
            let spent = s.cost.usd;
            let pct = spent * 100.0 / limit;
            if pct < warn_pct {
                lock_set(&self.budget_warned).remove(sid);
            } else if lock_set(&self.budget_warned).insert(sid.clone()) {
                self.emit(Event::SessionNote {
                    session: sid.clone(),
                    note: tr!(
                        "成本已達 ${spent}/{limit}（{pct}%），接近上限",
                        spent = format!("{spent:.2}"),
                        limit = termade_core::format_usd_limit(limit),
                        pct = format!("{pct:.0}")
                    ),
                });
            }
            if spent >= limit {
                if let Some((_, ctl)) = self.control(sid) {
                    let _ = ctl.cancel();
                }
                self.emit(Event::SessionOverBudget {
                    session: sid.clone(),
                    spent,
                    limit,
                });
            }
        }

        // 2) 整棵樹的上限（tree_usd）：從這個 session 往上找到根，加總整棵樹，超過時中斷樹裡所有 live 的 session。
        if let Some(tree_limit) = cfg.limits.tree_usd.filter(|&l| l > 0.0) {
            let (tree_spent, to_stop) = {
                let inner = lock(&self.inner);
                let spent = inner.state.tree_cost_usd(sid);
                let mut targets = Vec::new();
                if spent >= tree_limit {
                    for tid in inner.state.tree_sessions(sid) {
                        if let Some(ts) = inner.state.sessions.get(&tid) {
                            if ts.status.is_live() {
                                let ctl = inner.sessions.get(&tid).map(|e| e.ctl.clone());
                                targets.push((tid, ts.over_budget, ctl));
                            }
                        }
                    }
                }
                (spent, targets)
            };
            for (tid, already_over, ctl) in to_stop {
                if let Some(ctl) = ctl {
                    let _ = ctl.cancel();
                }
                if !already_over {
                    self.emit(Event::SessionOverBudget {
                        session: tid,
                        spent: tree_spent,
                        limit: tree_limit,
                    });
                }
            }
        }
    }

    /// `ingest` 在額度事件之後呼叫。
    pub(crate) fn react_to_quota(self: &Arc<Self>, event: &Event) {
        match event {
            Event::QuotaUpdated { quota } => self.warn_if_quota_high(quota),
            Event::SessionLimited { session, .. } => self.maybe_auto_handoff(session),
            _ => {}
        }
    }

    fn handoff_cfg(&self, cwd: &std::path::Path) -> crate::config::HandoffCfg {
        Config::load(&config_dir(), cwd).handoff
    }

    /// 用量越過門檻時，對該 agent 還在跑的 session 留下交接建議（每次越線只提醒一次）。
    fn warn_if_quota_high(self: &Arc<Self>, quota: &Quota) {
        let now = now_millis();
        let cwd = self.default_cwd();
        let threshold = self
            .handoff_cfg(&cwd)
            .warn_percent
            .unwrap_or(DEFAULT_WARN_PERCENT);
        let used = live_pressure(quota, now);
        if used < threshold && !quota.reached {
            lock_set(&self.quota_warned).remove(&quota.agent);
            return;
        }
        if !lock_set(&self.quota_warned).insert(quota.agent.clone()) {
            return;
        }
        let live: Vec<SessionId> = lock(&self.inner)
            .state
            .sessions
            .values()
            .filter(|s| s.kind.id() == quota.agent && s.status.is_live())
            .map(|s| s.id.clone())
            .collect();
        for sid in live {
            self.emit(Event::SessionNote {
                session: sid,
                note: tr!(
                    "{agent} 額度已用 {used}%，快用完了；需要時用 :handoff 交接給其他 agent",
                    agent = quota.agent,
                    used = format!("{used:.0}")
                ),
            });
        }
    }

    /// 額度用完時，若設定 `[handoff] auto = true` 就自動交接。
    fn maybe_auto_handoff(self: &Arc<Self>, sid: &SessionId) {
        let Some(src) = lock(&self.inner).state.sessions.get(sid).cloned() else {
            return;
        };
        let cfg = self.handoff_cfg(std::path::Path::new(&src.cwd));
        if cfg.auto != Some(true) {
            return;
        }
        if src.label.matches('→').count() >= MAX_HANDOFF_DEPTH {
            tracing::warn!(
                "{} 已連續交接 {MAX_HANDOFF_DEPTH} 次，不再自動交接",
                src.label
            );
            return;
        }
        let quotas = lock(&self.inner).state.quotas.clone();
        let Some((kind, model)) = pick_fallback(&cfg.fallback, &src.kind, &quotas, now_millis())
        else {
            self.emit(Event::SessionNote {
                session: sid.clone(),
                note: tr!("額度用完，但 [handoff] fallback 沒有可用的 agent；請手動 :handoff"),
            });
            return;
        };
        let shared = self.clone();
        let sid = sid.clone();
        // 交接會啟動新處理程序，不要卡住 ingest 執行緒。
        std::thread::spawn(move || {
            let (tx, _rx) = client_channel(None);
            crate::handlers::session::handle_handoff(
                &shared,
                &tx,
                sid,
                kind,
                model,
                None,
                Some("自動交接：原 agent 額度用完".into()),
            );
        });
    }
}

fn lock_set(
    m: &std::sync::Mutex<std::collections::HashSet<String>>,
) -> std::sync::MutexGuard<'_, std::collections::HashSet<String>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use termade_core::QuotaWindow;

    fn q(agent: &str, pct: f32, reached: bool, resets: Option<u64>) -> Quota {
        Quota {
            agent: agent.into(),
            windows: vec![QuotaWindow {
                name: "5h".into(),
                used_percent: pct,
                resets_at_ms: resets,
            }],
            reached,
            updated_ms: 0,
        }
    }

    #[test]
    fn pressure_ignores_reset_windows() {
        assert_eq!(live_pressure(&q("claude", 95.0, false, Some(5)), 10), 0.0);
        assert_eq!(live_pressure(&q("claude", 95.0, false, Some(50)), 10), 95.0);
    }

    #[test]
    fn fallback_skips_same_agent_and_exhausted_ones() {
        let fb = vec![
            "claude:sonnet".to_string(),
            "codex".to_string(),
            "opencode".to_string(),
        ];
        let mut quotas = BTreeMap::new();
        let src = AgentKind::ClaudeCode;
        assert_eq!(
            pick_fallback(&fb, &src, &quotas, 0).unwrap().0.id(),
            "codex"
        );
        quotas.insert("codex".into(), q("codex", 100.0, true, None));
        assert_eq!(
            pick_fallback(&fb, &src, &quotas, 0).unwrap().0.id(),
            "opencode"
        );
        assert!(pick_fallback(&["claude".to_string()], &src, &quotas, 0).is_none());
        assert!(pick_fallback(&["bogus".to_string()], &src, &quotas, 0).is_none());
    }
}
