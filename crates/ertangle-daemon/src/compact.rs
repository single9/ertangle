//! 自動 compact：session 回到閒置、上下文用量超過門檻時，由 daemon 代送 `/compact`。
//!
//! 只對 agent 自己宣告支援 `compact` 斜線指令的 session 生效 (Claude stream-json 已實測可用)；
//! 其他 agent 不動作，不會把 `/compact` 當成一般文字送進對話。

use super::*;

/// 判斷這個 session 此刻是否該自動 compact (純函式，方便測試)。
pub(crate) fn should_compact(
    s: &ertangle_core::Session,
    cfg: &config::AutoCompactCfg,
    already_compacting: bool,
) -> bool {
    if !cfg.enabled() || already_compacting {
        return false;
    }
    if s.status != SessionStatus::Idle || s.over_budget {
        return false;
    }
    if !s.slash_commands.iter().any(|c| c == "compact") {
        return false;
    }
    let Some(ctx) = s.context else {
        return false;
    };
    let Some(window) = ctx.window.filter(|w| *w > 0) else {
        return false;
    };
    ctx.used.saturating_mul(100) >= window.saturating_mul(u64::from(cfg.threshold_percent()))
}

impl Shared {
    /// 回到 `Idle` 時呼叫：剛跑完的是自動 compact 回合就只清旗標；否則用量超標就送 `/compact`。
    /// 回傳是否送出了 `/compact` (呼叫端據此略過佇列與未讀提醒)。
    pub(crate) fn auto_compact_if_needed(self: &Arc<Self>, sid: &SessionId) -> bool {
        if lock(&self.inner).auto_compacting.remove(sid) {
            return false;
        }
        let Some(cwd) = lock(&self.inner)
            .state
            .sessions
            .get(sid)
            .map(|s| PathBuf::from(&s.cwd))
        else {
            return false;
        };
        let cfg = config::Config::load(&config_dir(), &cwd).auto_compact;
        {
            let mut inner = lock(&self.inner);
            let due = inner
                .state
                .sessions
                .get(sid)
                .is_some_and(|s| should_compact(s, &cfg, false));
            if !due {
                return false;
            }
            inner.auto_compacting.insert(sid.clone());
        }
        match handlers::session::send_prompt(
            self,
            sid,
            "/compact",
            &[],
            Some(ertangle_core::PROMPT_FROM_ERTANGLE),
        ) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("自動 compact {sid} 失敗：{e}");
                lock(&self.inner).auto_compacting.remove(sid);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ertangle_core::{ContextUsage, Session};

    fn session(used: u64, window: Option<u64>, cmds: &[&str], status: SessionStatus) -> Session {
        let mut s = crate::tests::restart_session("s", status);
        s.context = Some(ContextUsage { used, window });
        s.slash_commands = cmds.iter().map(|c| c.to_string()).collect();
        s
    }

    #[test]
    fn threshold_defaults_and_clamps() {
        let cfg = config::AutoCompactCfg::default();
        assert!(cfg.enabled());
        assert_eq!(cfg.threshold_percent(), 80);
        let lo = config::AutoCompactCfg {
            threshold_percent: Some(0),
            ..Default::default()
        };
        assert_eq!(lo.threshold_percent(), 10);
        let hi = config::AutoCompactCfg {
            threshold_percent: Some(100),
            ..Default::default()
        };
        assert_eq!(hi.threshold_percent(), 95);
    }

    #[test]
    fn compacts_only_when_over_threshold_idle_and_supported() {
        let cfg = config::AutoCompactCfg::default();
        let idle = SessionStatus::Idle;
        assert!(should_compact(
            &session(80, Some(100), &["compact"], idle),
            &cfg,
            false
        ));
        assert!(!should_compact(
            &session(79, Some(100), &["compact"], idle),
            &cfg,
            false
        ));
        assert!(!should_compact(
            &session(90, None, &["compact"], idle),
            &cfg,
            false
        ));
        assert!(!should_compact(
            &session(90, Some(100), &["model"], idle),
            &cfg,
            false
        ));
        assert!(!should_compact(
            &session(90, Some(100), &["compact"], SessionStatus::Busy),
            &cfg,
            false
        ));
        assert!(!should_compact(
            &session(90, Some(100), &["compact"], idle),
            &cfg,
            true
        ));
    }

    #[test]
    fn custom_threshold_and_disable() {
        let s = session(50, Some(100), &["compact"], SessionStatus::Idle);
        let at50 = config::AutoCompactCfg {
            threshold_percent: Some(50),
            ..Default::default()
        };
        assert!(should_compact(&s, &at50, false));
        let off = config::AutoCompactCfg {
            enabled: Some(false),
            threshold_percent: Some(50),
        };
        assert!(!should_compact(&s, &off, false));
    }

    #[test]
    fn config_parses_auto_compact_section() {
        let c = config::Config::parse("[auto_compact]\nenabled = false\nthreshold_percent = 65\n")
            .unwrap();
        assert!(!c.auto_compact.enabled());
        assert_eq!(c.auto_compact.threshold_percent(), 65);
    }
}
