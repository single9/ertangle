//! 額度提醒與自動交接：額度快滿時在 session 上留下交接建議；額度用完且設定了
//! `[handoff] auto = true` 時，自動交接給 `fallback` 裡第一個可用的 agent。

use std::sync::Arc;
use tr::tr;

use ertangle_core::{now_millis, AgentKind, Event, Quota, SessionId};

use crate::config::Config;
use crate::paths::config_dir;
use crate::{client_channel, lock, Shared};

/// 預設的提醒門檻 (百分比)。
pub(crate) const DEFAULT_WARN_PERCENT: f32 = 90.0;
/// 自動交接最多串接幾次 (避免 A→B→A 無限循環)。
const MAX_HANDOFF_DEPTH: usize = 3;

/// 最吃緊的「尚未重置」視窗用量；全都已重置回傳 0。
pub(crate) fn live_pressure(q: &Quota, now_ms: u64) -> f32 {
    q.max_used_live(now_ms)
}

fn is_inactive_window(q: &Quota, name: &str) -> bool {
    q.agent == "antigravity" && name.starts_with("3p-")
}

/// 送給 agent 的額度警告 (固定英文協定格式)。
/// agent 自己回報「接近上限」(Claude `allowed_warning`) 時的提早提醒：比 `QUOTA_WARN` 溫和。
pub(crate) fn quota_notice_body(agent: &str, used: f32) -> String {
    format!(
        "QUOTA_NOTICE {agent} {used:.0}% (provider warning): avoid spawning new {agent} workers unless necessary (prefer other agents with available quota); finish current {agent} tasks and commit progress."
    )
}

pub(crate) fn quota_warn_body(agent: &str, used: f32) -> String {
    format!(
        "QUOTA_WARN {agent} {used:.0}%: stop spawning new {agent} workers (other agents with available quota may still be used); if you or your active workers run on {agent}, checkpoint/commit work now and report status."
    )
}

/// 判斷某個 session 是否需要收到 `agent` 的額度警告 / 提醒：
/// - 必須是 live 狀態。
/// - `only_coordinators = true` (用於 `QUOTA_NOTICE`) 時，僅通知角色為 Coordinator 且 (自身是該 `agent`，或子樹內有 live 的該 `agent` session)。
/// - `only_coordinators = false` (用於 `QUOTA_WARN`) 時，通知所有自身為該 `agent` 的 live session，以及子樹內有 live 的該 `agent` session 的 Coordinator。
pub(crate) fn should_notify_quota(
    s: &ertangle_core::Session,
    all_sessions: &std::collections::BTreeMap<SessionId, ertangle_core::Session>,
    agent: &str,
    only_coordinators: bool,
) -> bool {
    if !s.status.is_live() {
        return false;
    }
    if only_coordinators && s.role != ertangle_core::Role::Coordinator {
        return false;
    }
    if s.kind.id() == agent {
        return true;
    }
    if s.role == ertangle_core::Role::Coordinator {
        return all_sessions.values().any(|other| {
            if !other.status.is_live() || other.kind.id() != agent {
                return false;
            }
            // 沿著 parent 往上找是否屬於此 coordinator 的子樹
            let mut cur = other.parent.as_deref();
            let mut depth = 0;
            while let Some(pid) = cur {
                if pid == s.id {
                    return true;
                }
                cur = all_sessions.get(pid).and_then(|p| p.parent.as_deref());
                depth += 1;
                if depth > 32 {
                    break;
                }
            }
            false
        });
    }
    false
}

/// 子 agent spawn 前的額度檢查結果。
#[derive(Debug, PartialEq)]
pub(crate) enum SpawnQuota {
    /// 沒問題；附簡短摘要 (查不到額度時為 `None`)。
    Ok(Option<String>),
    /// 改用其他 agent。
    Fallback(AgentKind, Option<String>, String),
    /// 拒絕，附英文原因。
    Reject(String),
}

/// 決定 agent 自己 spawn 時該用哪個 agent：額度吃緊就換 `fallback`，沒得換就拒絕。
pub(crate) fn check_spawn_quota(
    kind: &AgentKind,
    quotas: &std::collections::BTreeMap<String, Quota>,
    fallback: &[String],
    warn_percent: f32,
    now_ms: u64,
) -> SpawnQuota {
    let Some(q) = quotas.get(kind.id()) else {
        return SpawnQuota::Ok(None);
    };
    let used = live_pressure(q, now_ms);
    let window = q
        .windows
        .iter()
        .filter(|w| !is_inactive_window(q, &w.name))
        .filter(|w| w.resets_at_ms.is_none_or(|r| r > now_ms))
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
        .map(|w| w.name.clone());
    let summary = match &window {
        Some(w) => format!("quota {} {used:.0}% ({w} window)", q.agent),
        None => format!("quota {} {used:.0}%", q.agent),
    };
    if !q.is_reached(now_ms) && used < warn_percent {
        return SpawnQuota::Ok(Some(summary));
    }
    if let Some((k, m)) = pick_fallback(fallback, kind, quotas, now_ms) {
        let note = format!("{summary}; switched to {}", k.id());
        return SpawnQuota::Fallback(k, m, note);
    }
    let reset = q
        .windows
        .iter()
        .filter(|w| !is_inactive_window(q, &w.name))
        .filter_map(|w| w.resets_at_ms.filter(|r| *r > now_ms))
        .min()
        .map(|r| {
            format!(
                " Resets in {}.",
                ertangle_core::format_remaining(r - now_ms)
            )
        })
        .unwrap_or_default();
    SpawnQuota::Reject(format!(
        "{summary} is at/over the warn threshold ({warn_percent:.0}%) and no fallback agent is available. Do not spawn more workers: finish the work inline or wait for the quota reset.{reset}"
    ))
}

/// 從 `fallback`(`kind[:model]`)挑第一個可接手的：agent 不同於來源，且它的額度沒滿。
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
            .is_some_and(|q| q.is_reached(now_ms) || live_pressure(q, now_ms) >= 100.0);
        (!blocked).then_some((kind, m))
    })
}

impl Shared {
    pub(crate) fn session_config(&self, s: &ertangle_core::Session) -> Config {
        let cwd = std::path::Path::new(&s.cwd);
        let root = std::path::Path::new(s.project_root());
        let def = self.default_cwd();
        let dir = if cwd.join(".ertangle").join("config.toml").is_file() {
            cwd
        } else if root.join(".ertangle").join("config.toml").is_file() {
            root
        } else if def.join(".ertangle").join("config.toml").is_file() {
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

        // 1) 單一 session 上限：沒有美金數字 (usd == 0 且有 token)時改看 session_tokens，否則看 USD。
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
                            "token 用量已達 {used}/{limit} ({pct}%)，接近上限",
                            used = ertangle_core::human_tokens(total_tokens),
                            limit = ertangle_core::human_tokens(tok_limit),
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
                        "成本已達 ${spent}/{limit} ({pct}%)，接近上限",
                        spent = format!("{spent:.2}"),
                        limit = ertangle_core::format_usd_limit(limit),
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

        // 2) 整棵樹的上限 (tree_usd)：從這個 session 往上找到根，加總整棵樹，超過時中斷樹裡所有 live 的 session。
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

    /// 用量越過門檻時，對該 agent 還在跑的 session 留下交接建議 (每次越線只提醒一次)。
    fn warn_if_quota_high(self: &Arc<Self>, quota: &Quota) {
        let now = now_millis();
        let cwd = self.default_cwd();
        let threshold = self
            .handoff_cfg(&cwd)
            .warn_percent
            .unwrap_or(DEFAULT_WARN_PERCENT);
        let used = live_pressure(quota, now);
        let notice_key = format!("{}:notice", quota.agent);
        if used < threshold && !quota.is_reached(now) {
            lock_set(&self.quota_warned).remove(&quota.agent);
            if !quota.warning {
                lock_set(&self.quota_warned).remove(&notice_key);
                return;
            }
            // 還沒到門檻，但 agent 自己說快到上限了：只提醒 coordinator 一次，不動其他 session。
            if !lock_set(&self.quota_warned).insert(notice_key) {
                return;
            }
            let coordinators: Vec<SessionId> = {
                let inner = lock(&self.inner);
                inner
                    .state
                    .sessions
                    .values()
                    .filter(|s| should_notify_quota(s, &inner.state.sessions, &quota.agent, true))
                    .map(|s| s.id.clone())
                    .collect()
            };
            let body = quota_notice_body(&quota.agent, used);
            for rid in coordinators {
                let _ = self.deliver_message(
                    Some("ertangle".into()),
                    ertangle_core::Target::Direct(rid),
                    ertangle_core::MsgKind::Chat,
                    body.clone(),
                    None,
                );
            }
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
        // 即時通知：該 agent 的 session 與相關 coordinator (透過匯流排，能喚醒 ertangle_wait)。
        let recipients: Vec<SessionId> = {
            let inner = lock(&self.inner);
            inner
                .state
                .sessions
                .values()
                .filter(|s| should_notify_quota(s, &inner.state.sessions, &quota.agent, false))
                .map(|s| s.id.clone())
                .collect()
        };
        let body = quota_warn_body(&quota.agent, used);
        for rid in recipients {
            let _ = self.deliver_message(
                Some("ertangle".into()),
                ertangle_core::Target::Direct(rid),
                ertangle_core::MsgKind::Chat,
                body.clone(),
                None,
            );
        }
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
                None,
                None,
                false,
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
    use ertangle_core::QuotaWindow;
    use std::collections::BTreeMap;

    fn q(agent: &str, pct: f32, reached: bool, resets: Option<u64>) -> Quota {
        Quota {
            agent: agent.into(),
            windows: vec![QuotaWindow {
                name: "5h".into(),
                used_percent: pct,
                resets_at_ms: resets,
            }],
            reached,
            warning: false,
            updated_ms: 0,
        }
    }

    #[test]
    fn pressure_ignores_reset_windows() {
        assert_eq!(live_pressure(&q("claude", 95.0, false, Some(5)), 10), 0.0);
        assert_eq!(live_pressure(&q("claude", 95.0, false, Some(50)), 10), 95.0);
    }

    #[test]
    fn antigravity_third_party_pressure_does_not_block_spawn() {
        let mut quota = q("antigravity", 61.0, false, None);
        quota.windows.push(QuotaWindow {
            name: "3p-7d".into(),
            used_percent: 90.33,
            resets_at_ms: Some(100),
        });
        let quotas = BTreeMap::from([("antigravity".into(), quota)]);

        assert_eq!(
            check_spawn_quota(&AgentKind::Antigravity, &quotas, &[], 90.0, 0),
            SpawnQuota::Ok(Some("quota antigravity 61% (5h window)".into()))
        );
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

    #[test]
    fn spawn_and_fallback_allow_agent_after_exhausted_window_resets() {
        let claude = AgentKind::ClaudeCode;
        let mut quotas = BTreeMap::new();
        let quota = Quota {
            agent: "claude".into(),
            windows: vec![
                QuotaWindow {
                    name: "5h".into(),
                    used_percent: 100.0,
                    resets_at_ms: Some(100),
                },
                QuotaWindow {
                    name: "7d".into(),
                    used_percent: 51.0,
                    resets_at_ms: Some(500),
                },
            ],
            reached: true,
            warning: false,
            updated_ms: 0,
        };
        quotas.insert("claude".into(), quota.clone());

        // At now_ms = 50: 5h not reset -> rejected or fallback triggered.
        assert!(matches!(
            check_spawn_quota(&claude, &quotas, &[], 90.0, 50),
            SpawnQuota::Reject(_)
        ));
        assert!(pick_fallback(&["claude".to_string()], &AgentKind::Codex, &quotas, 50).is_none());

        // At now_ms = 150: 5h has reset, 7d is 51% < 90% -> allowed.
        assert!(matches!(
            check_spawn_quota(&claude, &quotas, &[], 90.0, 150),
            SpawnQuota::Ok(_)
        ));
        assert!(pick_fallback(&["claude".to_string()], &AgentKind::Codex, &quotas, 150).is_some());

        // If 7d is 100%: still blocked even after 5h reset.
        let mut quota_full = quota;
        quota_full.windows[1].used_percent = 100.0;
        quotas.insert("claude".into(), quota_full);
        assert!(matches!(
            check_spawn_quota(&claude, &quotas, &[], 90.0, 150),
            SpawnQuota::Reject(_)
        ));
        assert!(pick_fallback(&["claude".to_string()], &AgentKind::Codex, &quotas, 150).is_none());
    }

    #[test]
    fn spawn_quota_ok_fallback_reject() {
        let claude = AgentKind::ClaudeCode;
        let mut quotas = BTreeMap::new();
        // 沒有額度資料：放行、無摘要。
        assert_eq!(
            check_spawn_quota(&claude, &quotas, &[], 90.0, 0),
            SpawnQuota::Ok(None)
        );
        quotas.insert("claude".into(), q("claude", 72.0, false, None));
        match check_spawn_quota(&claude, &quotas, &[], 90.0, 0) {
            SpawnQuota::Ok(Some(n)) => assert_eq!(n, "quota claude 72% (5h window)"),
            o => panic!("{o:?}"),
        }
        // 吃緊且有 fallback：換 agent。
        quotas.insert("claude".into(), q("claude", 95.0, false, None));
        let fb = vec!["codex".to_string()];
        match check_spawn_quota(&claude, &quotas, &fb, 90.0, 0) {
            SpawnQuota::Fallback(k, _, n) => {
                assert_eq!(k.id(), "codex");
                assert!(n.contains("switched to codex"), "{n}");
            }
            o => panic!("{o:?}"),
        }
        // 沒有 fallback：拒絕並附重置時間。
        quotas.insert("claude".into(), q("claude", 100.0, true, Some(3_600_000)));
        match check_spawn_quota(&claude, &quotas, &[], 90.0, 0) {
            SpawnQuota::Reject(e) => {
                assert!(e.contains("inline") && e.contains("Resets in 1h00m"), "{e}")
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn quota_notice_body_is_softer_than_warn() {
        let b = quota_notice_body("claude", 77.0);
        assert!(b.starts_with("QUOTA_NOTICE claude 77%"), "{b}");
        assert!(!b.contains("stop spawning"));
    }

    #[test]
    fn quota_warn_body_format() {
        assert_eq!(
            quota_warn_body("claude", 91.4),
            "QUOTA_WARN claude 91%: stop spawning new claude workers (other agents with available quota may still be used); if you or your active workers run on claude, checkpoint/commit work now and report status."
        );
    }

    #[test]
    fn should_notify_quota_filters_unrelated_coordinators() {
        use ertangle_core::{AgentKind, Role, Session, SessionStatus};

        fn make_sess(
            id: &str,
            kind: AgentKind,
            role: Role,
            parent: Option<&str>,
            live: bool,
        ) -> Session {
            Session {
                id: id.into(),
                kind,
                label: id.into(),
                cwd: "/tmp".into(),
                branch: None,
                role,
                status: if live {
                    SessionStatus::Idle
                } else {
                    SessionStatus::Exited
                },
                model: None,
                cost: Default::default(),
                parent: parent.map(|s| s.into()),
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
                ended_ms: None,
            }
        }

        let mut all_sessions = BTreeMap::new();

        // 1. A claude worker and a claude coordinator
        let claude_worker = make_sess("ses_cw", AgentKind::ClaudeCode, Role::Worker, None, true);
        let claude_coord = make_sess(
            "ses_cc",
            AgentKind::ClaudeCode,
            Role::Coordinator,
            None,
            true,
        );
        all_sessions.insert(claude_worker.id.clone(), claude_worker.clone());
        all_sessions.insert(claude_coord.id.clone(), claude_coord.clone());

        // Both match QUOTA_WARN (only_coordinators = false)
        assert!(should_notify_quota(
            &claude_worker,
            &all_sessions,
            "claude",
            false
        ));
        assert!(should_notify_quota(
            &claude_coord,
            &all_sessions,
            "claude",
            false
        ));
        // QUOTA_NOTICE (only_coordinators = true): only coordinator matches
        assert!(!should_notify_quota(
            &claude_worker,
            &all_sessions,
            "claude",
            true
        ));
        assert!(should_notify_quota(
            &claude_coord,
            &all_sessions,
            "claude",
            true
        ));

        // 2. An antigravity coordinator with a live claude child worker
        let agy_coord_with_claude = make_sess(
            "ses_ac1",
            AgentKind::Antigravity,
            Role::Coordinator,
            None,
            true,
        );
        let child_claude_worker = make_sess(
            "ses_ccw",
            AgentKind::ClaudeCode,
            Role::Worker,
            Some("ses_ac1"),
            true,
        );
        all_sessions.insert(
            agy_coord_with_claude.id.clone(),
            agy_coord_with_claude.clone(),
        );
        all_sessions.insert(child_claude_worker.id.clone(), child_claude_worker.clone());

        assert!(should_notify_quota(
            &agy_coord_with_claude,
            &all_sessions,
            "claude",
            false
        ));
        assert!(should_notify_quota(
            &agy_coord_with_claude,
            &all_sessions,
            "claude",
            true
        ));

        // 3. An antigravity coordinator with only antigravity child workers, or no workers, or exited claude worker
        let agy_coord_solo = make_sess(
            "ses_ac2",
            AgentKind::Antigravity,
            Role::Coordinator,
            None,
            true,
        );
        all_sessions.insert(agy_coord_solo.id.clone(), agy_coord_solo.clone());
        assert!(!should_notify_quota(
            &agy_coord_solo,
            &all_sessions,
            "claude",
            false
        ));
        assert!(!should_notify_quota(
            &agy_coord_solo,
            &all_sessions,
            "claude",
            true
        ));

        let agy_child_worker = make_sess(
            "ses_aw",
            AgentKind::Antigravity,
            Role::Worker,
            Some("ses_ac2"),
            true,
        );
        all_sessions.insert(agy_child_worker.id.clone(), agy_child_worker.clone());
        assert!(!should_notify_quota(
            &agy_coord_solo,
            &all_sessions,
            "claude",
            false
        ));
        assert!(!should_notify_quota(
            &agy_coord_solo,
            &all_sessions,
            "claude",
            true
        ));

        // Exited claude worker under agy_coord_solo
        let exited_claude = make_sess(
            "ses_ex_c",
            AgentKind::ClaudeCode,
            Role::Worker,
            Some("ses_ac2"),
            false,
        );
        all_sessions.insert(exited_claude.id.clone(), exited_claude.clone());
        assert!(!should_notify_quota(
            &agy_coord_solo,
            &all_sessions,
            "claude",
            false
        ));
        assert!(!should_notify_quota(
            &agy_coord_solo,
            &all_sessions,
            "claude",
            true
        ));
    }
}
