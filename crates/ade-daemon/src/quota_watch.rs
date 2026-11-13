//! 額度提醒與自動交接：額度快滿時在 session 上留下交接建議；額度用完且設定了
//! `[handoff] auto = true` 時，自動交接給 `fallback` 裡第一個可用的 agent。

use std::sync::Arc;

use ade_core::{now_millis, AgentKind, Event, Quota, SessionId};

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
                note: format!(
                    "{} 額度已用 {used:.0}%，快用完了；需要時用 :handoff 交接給其他 agent",
                    quota.agent
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
                note: "額度用完，但 [handoff] fallback 沒有可用的 agent；請手動 :handoff".into(),
            });
            return;
        };
        let shared = self.clone();
        let sid = sid.clone();
        // 交接會啟動新程序，不要卡住 ingest 執行緒。
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
    use ade_core::QuotaWindow;
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
