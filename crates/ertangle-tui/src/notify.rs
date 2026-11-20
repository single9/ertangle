//! 通知：回合結束、等待核准、失敗、額度用完時提醒不在畫面前的使用者。
//!
//! 偵測 ([`notices_for`])是純函式；送出方式有終端機 bell、OSC 9 與使用者自訂命令。
//! 通知由 TUI 發出 (daemon 可能在遠端或沒有桌面環境)，TUI 沒開就沒有通知。

use std::collections::HashMap;
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tr::tr;

use ertangle_core::{Event, Record, SessionId, SessionStatus, State};
use ertangle_daemon::config::TuiCfg;

/// `ts` 比現在早超過這麼久的事件視為歷史 (重連補送)，不通知。
pub(crate) const STALE_MS: u64 = 5_000;
/// 同一個 session 同一類通知在這段時間內只發一次。
pub(crate) const DEBOUNCE: Duration = Duration::from_secs(3);
/// 「做完了」的回合長度門檻預設值 (秒)。
pub(crate) const DEFAULT_MIN_TURN_SECS: u64 = 10;
/// 通知文字上限 (字元)，避免工具名或標籤過長。
const TEXT_CAP: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum NoticeKind {
    /// 回合結束 (忙碌 → 閒置)。
    Done,
    Failed,
    /// 等待核准。
    Approval,
    /// 額度用完。
    Limited,
}

impl NoticeKind {
    /// `ERTANGLE_NOTIFY_KIND` 的值。
    pub(crate) fn id(self) -> &'static str {
        match self {
            NoticeKind::Done => "done",
            NoticeKind::Failed => "failed",
            NoticeKind::Approval => "approval",
            NoticeKind::Limited => "limited",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) kind: NoticeKind,
    pub(crate) session: SessionId,
    pub(crate) label: String,
    pub(crate) text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    Bell,
    Osc9,
    Command,
}

impl Method {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bell" => Some(Method::Bell),
            "osc9" => Some(Method::Osc9),
            "command" => Some(Method::Command),
            _ => None,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Method::Bell => "bell",
            Method::Osc9 => "osc9",
            Method::Command => "command",
        }
    }
}

/// [`notices_for`] 的過濾條件；全部由呼叫端填入，測試可以直接構造。
pub(crate) struct NotifyCtx<'a> {
    /// 現在的時間 (毫秒，與 `Record::ts` 同一個基準)。
    pub(crate) now_ms: u64,
    pub(crate) now: Instant,
    pub(crate) min_turn: Duration,
    /// 各 session 目前這個回合的開始時間；沒有紀錄的視為夠長。
    pub(crate) turn_started: &'a HashMap<SessionId, Instant>,
    /// 終端機是否有焦點 (終端機不回報時為 `false`)。
    pub(crate) focused: bool,
    /// 目前選取的 session。
    pub(crate) selected: Option<&'a str>,
    pub(crate) scope_here: bool,
    pub(crate) home_project: &'a str,
    /// 各 (session, 類別) 上次通知的時間。
    pub(crate) last_sent: &'a HashMap<(SessionId, NoticeKind), Instant>,
}

/// 「忙碌」：回合進行中。休眠、啟動中都不算，所以喚醒 (`Sleeping` → `Idle`)
/// 與啟動完成 (`Starting` → `Idle`)不會被當成回合結束。
pub(crate) fn is_busy(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Busy | SessionStatus::WaitingApproval)
}

/// 這個事件 (套用到 `before` 之前)該產生哪些通知。
pub(crate) fn notices_for(before: &State, record: &Record, ctx: &NotifyCtx) -> Vec<Notice> {
    if record.ts.saturating_add(STALE_MS) < ctx.now_ms {
        return Vec::new();
    }
    let (session, kind, extra) = match &record.event {
        Event::SessionStatus { session, status } => {
            let Some(prev) = before.sessions.get(session).map(|s| s.status) else {
                return Vec::new();
            };
            match status {
                SessionStatus::Idle if is_busy(prev) => (session, NoticeKind::Done, None),
                // `SessionLimited` 已把狀態設成 Failed，緊接著的 Failed 不再重複通知。
                SessionStatus::Failed if prev != SessionStatus::Failed => {
                    (session, NoticeKind::Failed, None)
                }
                _ => return Vec::new(),
            }
        }
        Event::PermissionRequested { request } => (
            &request.session,
            NoticeKind::Approval,
            Some(request.title.as_str()),
        ),
        Event::SessionLimited { session, .. } => (session, NoticeKind::Limited, None),
        _ => return Vec::new(),
    };
    let Some(s) = before.sessions.get(session) else {
        return Vec::new();
    };
    if ctx.scope_here && s.project_root() != ctx.home_project {
        return Vec::new();
    }
    if ctx.focused && ctx.selected == Some(session.as_str()) {
        return Vec::new();
    }
    if kind == NoticeKind::Done {
        let short = ctx
            .turn_started
            .get(session)
            .is_some_and(|t| ctx.now.saturating_duration_since(*t) < ctx.min_turn);
        if short {
            return Vec::new();
        }
    }
    let recent = ctx
        .last_sent
        .get(&(session.clone(), kind))
        .is_some_and(|t| ctx.now.saturating_duration_since(*t) < DEBOUNCE);
    if recent {
        return Vec::new();
    }
    let label = sanitize(&s.label);
    let what = match (kind, extra) {
        (NoticeKind::Done, _) => tr!("做完了"),
        (NoticeKind::Failed, _) => tr!("失敗"),
        (NoticeKind::Approval, Some(tool)) if !tool.trim().is_empty() => {
            tr!("等待核准 {tool}", tool = tool)
        }
        (NoticeKind::Approval, _) => tr!("等待核准"),
        (NoticeKind::Limited, _) => tr!("額度用完"),
    };
    vec![Notice {
        kind,
        session: session.clone(),
        text: sanitize(&tr!("{label}：{what}", label = label, what = what)),
        label,
    }]
}

/// 去掉控制字元 (含 ESC、BEL 與 C1)，並限制長度；寫進 OSC 序列前必須經過這裡。
pub(crate) fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(TEXT_CAP)
        .collect()
}

/// 把 bell／OSC 9 寫到終端機。`text` 會再清一次控制字元。
pub(crate) fn write_escape(out: &mut impl Write, method: Method, text: &str) -> io::Result<()> {
    match method {
        Method::Bell => out.write_all(b"\x07"),
        Method::Osc9 => write!(out, "\x1b]9;{}\x07", sanitize(text)),
        Method::Command => Ok(()),
    }
}

/// 以 `sh -c` 執行使用者的通知命令；內容只經環境變數傳入，不拼進命令字串。
pub(crate) fn notify_command(command: &str, notice: &Notice) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .env("ERTANGLE_NOTIFY_KIND", notice.kind.id())
        .env("ERTANGLE_NOTIFY_SESSION", &notice.session)
        .env("ERTANGLE_NOTIFY_LABEL", &notice.label)
        .env("ERTANGLE_NOTIFY_TEXT", &notice.text)
        // 子處理程序不能碰 TUI 佔用的終端機。
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

/// 通知的設定與執行期狀態 (`App` 持有一份)。
pub(crate) struct Notifier {
    /// `:notify on|off` 的執行期開關。
    pub(crate) enabled: bool,
    pub(crate) methods: Vec<Method>,
    /// 設定裡無法辨識的方式名稱 (`:notify` 會列出)。
    pub(crate) unknown: Vec<String>,
    pub(crate) command: Option<String>,
    pub(crate) min_turn: Duration,
    /// 終端機是否有焦點；收到第一個 focus 事件前當作沒有。
    pub(crate) focused: bool,
    pub(crate) turn_started: HashMap<SessionId, Instant>,
    pub(crate) last_sent: HashMap<(SessionId, NoticeKind), Instant>,
    /// 已偵測、等事件迴圈送出的通知。
    pub(crate) pending: Vec<Notice>,
}

impl Notifier {
    pub(crate) fn from_cfg(cfg: &TuiCfg) -> Self {
        let mut methods = Vec::new();
        let mut unknown = Vec::new();
        match &cfg.notify {
            None => methods.push(Method::Bell),
            Some(list) => {
                for name in list {
                    match Method::parse(name) {
                        Some(m) if !methods.contains(&m) => methods.push(m),
                        Some(_) => {}
                        None => unknown.push(name.clone()),
                    }
                }
            }
        }
        Notifier {
            enabled: true,
            methods,
            unknown,
            command: cfg.notify_command.clone().filter(|c| !c.trim().is_empty()),
            min_turn: Duration::from_secs(
                cfg.notify_min_turn_secs.unwrap_or(DEFAULT_MIN_TURN_SECS),
            ),
            focused: false,
            turn_started: HashMap::new(),
            last_sent: HashMap::new(),
            pending: Vec::new(),
        }
    }

    /// 目前狀態的一行說明 (`:notify` 與切換後顯示在狀態列)。
    pub(crate) fn describe(&self) -> String {
        let mut out = if self.methods.is_empty() {
            tr!("通知：關閉 (config.toml 的 [tui] notify 是空的)")
        } else {
            let names: Vec<&str> = self.methods.iter().map(|m| m.id()).collect();
            let methods = names.join(&tr!("列表分隔" => "、"));
            if self.enabled {
                tr!("通知：開 ({methods})", methods = methods)
            } else {
                tr!("通知：關 ({methods})", methods = methods)
            }
        };
        if self.methods.contains(&Method::Command) && self.command.is_none() {
            out.push_str(&tr!("；notify_command 未設定"));
        }
        if !self.unknown.is_empty() {
            out.push_str(&tr!(
                "；忽略未知方式：{methods}",
                methods = self.unknown.join(&tr!("列表分隔" => "、"))
            ));
        }
        out
    }

    /// 在事件套用到 `before` **之前**呼叫：偵測通知並維護回合開始時間。
    pub(crate) fn observe(
        &mut self,
        before: &State,
        record: &Record,
        selected: Option<&str>,
        scope_here: bool,
        home_project: &str,
        now_ms: u64,
    ) {
        let now = Instant::now();
        if self.enabled && !self.methods.is_empty() {
            let ctx = NotifyCtx {
                now_ms,
                now,
                min_turn: self.min_turn,
                turn_started: &self.turn_started,
                focused: self.focused,
                selected,
                scope_here,
                home_project,
                last_sent: &self.last_sent,
            };
            let found = notices_for(before, record, &ctx);
            for n in &found {
                self.last_sent.insert((n.session.clone(), n.kind), now);
            }
            self.pending.extend(found);
        }
        match &record.event {
            Event::SessionStatus { session, status } => {
                let prev = before.sessions.get(session).map(|s| s.status);
                if *status == SessionStatus::Busy {
                    // 核准後回到 Busy 仍是同一個回合，不重設起點。
                    if !prev.is_some_and(is_busy) {
                        self.turn_started.insert(session.clone(), now);
                    }
                } else if !is_busy(*status) {
                    self.turn_started.remove(session);
                }
            }
            Event::SessionRemoved { session } => {
                self.turn_started.remove(session);
                self.last_sent.retain(|(s, _), _| s != session);
            }
            _ => {}
        }
    }

    /// 收到完整快照：正在忙的 session 從現在起算回合時間。
    pub(crate) fn observe_snapshot(&mut self, state: &State) {
        let now = Instant::now();
        self.turn_started
            .retain(|id, _| state.sessions.get(id).is_some_and(|s| is_busy(s.status)));
        for s in state.sessions.values().filter(|s| is_busy(s.status)) {
            self.turn_started.entry(s.id.clone()).or_insert(now);
        }
    }

    /// 送出累積的通知。回傳值是要顯示在狀態列的錯誤 (沒有就是 `None`)。
    pub(crate) fn flush(&mut self, out: &mut impl Write) -> Option<String> {
        let mut error = None;
        for notice in std::mem::take(&mut self.pending) {
            if let Err(e) = self.deliver(out, &notice) {
                error = Some(e);
            }
        }
        error
    }

    fn deliver(&self, out: &mut impl Write, notice: &Notice) -> Result<(), String> {
        let mut result = Ok(());
        for method in &self.methods {
            match method {
                Method::Bell | Method::Osc9 => {
                    if let Err(e) =
                        write_escape(out, *method, &notice.text).and_then(|()| out.flush())
                    {
                        result = Err(tr!("通知寫入終端機失敗：{e}", e = e));
                    }
                }
                Method::Command => match &self.command {
                    None => result = Err(tr!("通知：notify_command 未設定")),
                    Some(command) => match notify_command(command, notice).spawn() {
                        // 不等待結果；另開執行緒回收子處理程序，避免留下殭屍。
                        Ok(mut child) => {
                            std::thread::spawn(move || {
                                let _ = child.wait();
                            });
                        }
                        Err(e) => result = Err(tr!("通知命令執行失敗：{e}", e = e)),
                    },
                },
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ertangle_core::{AgentKind, Cost, PermissionRequest, Role, Session};

    const NOW_MS: u64 = 1_000_000;

    fn state_with(status: SessionStatus) -> State {
        let mut st = State::new();
        for (id, label, cwd) in [
            ("s1", "worker/a", "/work/alpha"),
            ("s2", "w2", "/work/beta"),
        ] {
            st.sessions.insert(
                id.into(),
                Session {
                    id: id.into(),
                    kind: AgentKind::ClaudeCode,
                    label: label.into(),
                    cwd: cwd.into(),
                    branch: None,
                    role: Role::Worker,
                    status,
                    model: None,
                    cost: Cost::default(),
                    parent: None,
                    created_ms: 1,
                    transport: "stream-json".into(),
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
                },
            );
        }
        st
    }

    fn rec(event: Event) -> Record {
        Record {
            seq: 1,
            ts: NOW_MS,
            event,
        }
    }

    fn status(session: &str, status: SessionStatus) -> Record {
        rec(Event::SessionStatus {
            session: session.into(),
            status,
        })
    }

    fn approval(session: &str, title: &str) -> Record {
        rec(Event::PermissionRequested {
            request: PermissionRequest {
                req: "r1".into(),
                session: session.into(),
                title: title.into(),
                detail: "rm -rf x".into(),
            },
        })
    }

    /// 可變的測試情境；`ctx()` 借出 [`NotifyCtx`]。
    struct Env {
        now: Instant,
        turn_started: HashMap<SessionId, Instant>,
        last_sent: HashMap<(SessionId, NoticeKind), Instant>,
        focused: bool,
        selected: Option<&'static str>,
        scope_here: bool,
    }

    impl Env {
        fn new() -> Self {
            Env {
                // 往後推一小時，才能安全地減出「過去」的時間點。
                now: Instant::now() + Duration::from_secs(3600),
                turn_started: HashMap::new(),
                last_sent: HashMap::new(),
                focused: false,
                selected: None,
                scope_here: false,
            }
        }
        fn ago(&self, secs: u64) -> Instant {
            self.now - Duration::from_secs(secs)
        }
        fn ctx(&self) -> NotifyCtx<'_> {
            NotifyCtx {
                now_ms: NOW_MS,
                now: self.now,
                min_turn: Duration::from_secs(10),
                turn_started: &self.turn_started,
                focused: self.focused,
                selected: self.selected,
                scope_here: self.scope_here,
                home_project: "/work/alpha",
                last_sent: &self.last_sent,
            }
        }
    }

    #[test]
    fn busy_to_idle_notifies_once_and_idle_to_idle_does_not() {
        let env = Env::new();
        let got = notices_for(
            &state_with(SessionStatus::Busy),
            &status("s1", SessionStatus::Idle),
            &env.ctx(),
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, NoticeKind::Done);
        assert_eq!(got[0].session, "s1");
        assert_eq!(got[0].text, "worker/a：做完了");

        for prev in [
            SessionStatus::Idle,
            // 喚醒與啟動完成不是回合結束。
            SessionStatus::Sleeping,
            SessionStatus::Starting,
        ] {
            let got = notices_for(
                &state_with(prev),
                &status("s1", SessionStatus::Idle),
                &env.ctx(),
            );
            assert!(got.is_empty(), "{prev:?} → Idle 不該通知");
        }
        // 核准完直接結束也算回合結束。
        let got = notices_for(
            &state_with(SessionStatus::WaitingApproval),
            &status("s1", SessionStatus::Idle),
            &env.ctx(),
        );
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn permission_failed_and_limited_notify() {
        let env = Env::new();
        let busy = state_with(SessionStatus::Busy);
        let got = notices_for(&busy, &approval("s1", "Bash"), &env.ctx());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, NoticeKind::Approval);
        assert_eq!(got[0].text, "worker/a：等待核准 Bash");

        let got = notices_for(&busy, &status("s1", SessionStatus::Failed), &env.ctx());
        assert_eq!(got[0].kind, NoticeKind::Failed);

        let limited = rec(Event::SessionLimited {
            session: "s1".into(),
            detail: "usage limit".into(),
        });
        let got = notices_for(&busy, &limited, &env.ctx());
        assert_eq!(got[0].text, "worker/a：額度用完");

        // Claude 在 Limited 之後還會送 Failed：套用 Limited 後狀態已是 Failed，不重複。
        let mut after = busy.clone();
        after.apply(&limited);
        let got = notices_for(&after, &status("s1", SessionStatus::Failed), &env.ctx());
        assert!(got.is_empty());
    }

    #[test]
    fn stale_records_are_history() {
        let env = Env::new();
        let busy = state_with(SessionStatus::Busy);
        let mut old = status("s1", SessionStatus::Idle);
        old.ts = NOW_MS - STALE_MS - 1;
        assert!(notices_for(&busy, &old, &env.ctx()).is_empty());
        let mut old = approval("s1", "Bash");
        old.ts = NOW_MS - 60_000;
        assert!(notices_for(&busy, &old, &env.ctx()).is_empty());
        // 剛好在界線內仍通知。
        let mut edge = status("s1", SessionStatus::Idle);
        edge.ts = NOW_MS - STALE_MS;
        assert_eq!(notices_for(&busy, &edge, &env.ctx()).len(), 1);
    }

    #[test]
    fn short_turns_are_quiet_but_approval_and_failure_are_not() {
        let mut env = Env::new();
        let busy = state_with(SessionStatus::Busy);
        env.turn_started.insert("s1".into(), env.ago(3));
        let idle = status("s1", SessionStatus::Idle);
        assert!(notices_for(&busy, &idle, &env.ctx()).is_empty());
        assert_eq!(
            notices_for(&busy, &approval("s1", "Edit"), &env.ctx()).len(),
            1
        );
        assert_eq!(
            notices_for(&busy, &status("s1", SessionStatus::Failed), &env.ctx()).len(),
            1
        );
        // 夠長的回合會通知。
        env.turn_started.insert("s1".into(), env.ago(30));
        assert_eq!(notices_for(&busy, &idle, &env.ctx()).len(), 1);
    }

    #[test]
    fn focused_selected_session_is_quiet_until_focus_is_lost() {
        let mut env = Env::new();
        let busy = state_with(SessionStatus::Busy);
        let idle = status("s1", SessionStatus::Idle);
        env.focused = true;
        env.selected = Some("s1");
        assert!(notices_for(&busy, &idle, &env.ctx()).is_empty());
        assert!(notices_for(&busy, &approval("s1", "Bash"), &env.ctx()).is_empty());
        // 有焦點但選的是別的 session：仍通知。
        assert_eq!(
            notices_for(&busy, &status("s2", SessionStatus::Idle), &env.ctx()).len(),
            1
        );
        env.focused = false;
        assert_eq!(notices_for(&busy, &idle, &env.ctx()).len(), 1);
    }

    #[test]
    fn other_projects_are_quiet_when_scoped_here() {
        let mut env = Env::new();
        let busy = state_with(SessionStatus::Busy);
        let other = status("s2", SessionStatus::Idle);
        env.scope_here = true;
        assert!(notices_for(&busy, &other, &env.ctx()).is_empty());
        assert_eq!(
            notices_for(&busy, &status("s1", SessionStatus::Idle), &env.ctx()).len(),
            1
        );
        env.scope_here = false;
        assert_eq!(notices_for(&busy, &other, &env.ctx()).len(), 1);
    }

    #[test]
    fn repeats_within_three_seconds_are_debounced() {
        let mut env = Env::new();
        let busy = state_with(SessionStatus::Busy);
        let idle = status("s1", SessionStatus::Idle);
        env.last_sent
            .insert(("s1".into(), NoticeKind::Done), env.ago(1));
        assert!(notices_for(&busy, &idle, &env.ctx()).is_empty());
        // 不同類別、不同 session 不受影響。
        assert_eq!(
            notices_for(&busy, &approval("s1", "Bash"), &env.ctx()).len(),
            1
        );
        assert_eq!(
            notices_for(&busy, &status("s2", SessionStatus::Idle), &env.ctx()).len(),
            1
        );
        env.last_sent
            .insert(("s1".into(), NoticeKind::Done), env.ago(4));
        assert_eq!(notices_for(&busy, &idle, &env.ctx()).len(), 1);
    }

    fn bell_notifier() -> Notifier {
        let mut n = Notifier::from_cfg(&TuiCfg::default());
        n.min_turn = Duration::ZERO;
        n
    }

    #[test]
    fn notifier_debounces_and_tracks_turns() {
        let mut n = bell_notifier();
        let mut st = state_with(SessionStatus::Idle);
        let feed = |n: &mut Notifier, st: &mut State, r: Record| {
            n.observe(st, &r, None, false, "/work/alpha", NOW_MS);
            st.apply(&r);
        };
        feed(&mut n, &mut st, status("s1", SessionStatus::Busy));
        assert!(n.turn_started.contains_key("s1"));
        feed(&mut n, &mut st, status("s1", SessionStatus::Idle));
        assert!(!n.turn_started.contains_key("s1"));
        // 3 秒內又忙完一次：只有一則。
        feed(&mut n, &mut st, status("s1", SessionStatus::Busy));
        feed(&mut n, &mut st, status("s1", SessionStatus::Idle));
        assert_eq!(n.pending.len(), 1);

        let mut out = Vec::new();
        assert_eq!(n.flush(&mut out), None);
        assert_eq!(out, b"\x07");
        assert!(n.pending.is_empty());
    }

    #[test]
    fn notifier_respects_min_turn_and_runtime_switch() {
        let mut n = Notifier::from_cfg(&TuiCfg::default());
        assert_eq!(n.min_turn, Duration::from_secs(DEFAULT_MIN_TURN_SECS));
        let mut st = state_with(SessionStatus::Idle);
        for s in [SessionStatus::Busy, SessionStatus::Idle] {
            let r = status("s1", s);
            n.observe(&st, &r, None, false, "/work/alpha", NOW_MS);
            st.apply(&r);
        }
        assert!(n.pending.is_empty(), "剛開始就結束的回合不通知");

        n.enabled = false;
        let r = approval("s1", "Bash");
        n.observe(&st, &r, None, false, "/work/alpha", NOW_MS);
        assert!(n.pending.is_empty());
        n.enabled = true;
        n.observe(&st, &r, None, false, "/work/alpha", NOW_MS);
        assert_eq!(n.pending.len(), 1);
    }

    #[test]
    fn config_selects_methods() {
        let n = Notifier::from_cfg(&TuiCfg::default());
        assert_eq!(n.methods, vec![Method::Bell]);
        let cfg = TuiCfg {
            notify: Some(vec!["osc9".into(), "Command".into(), "toast".into()]),
            notify_command: Some("true".into()),
            notify_min_turn_secs: Some(2),
            ..Default::default()
        };
        let n = Notifier::from_cfg(&cfg);
        assert_eq!(n.methods, vec![Method::Osc9, Method::Command]);
        assert_eq!(n.unknown, vec!["toast".to_string()]);
        assert_eq!(n.min_turn, Duration::from_secs(2));
        assert!(n.describe().contains("toast"));
        // 空陣列 = 關閉。
        let off = Notifier::from_cfg(&TuiCfg {
            notify: Some(vec![]),
            ..Default::default()
        });
        assert!(off.methods.is_empty());
        let mut off = off;
        let st = state_with(SessionStatus::Busy);
        off.observe(&st, &approval("s1", "Bash"), None, false, "/", NOW_MS);
        assert!(off.pending.is_empty());
    }

    #[test]
    fn osc_sequence_has_no_control_characters_from_the_message() {
        let mut st = state_with(SessionStatus::Busy);
        st.sessions.get_mut("s1").unwrap().label = "wor\x1b]52;c;AAAA\x07ker\n".into();
        let env = Env::new();
        let got = notices_for(&st, &approval("s1", "Ba\x07sh\x1b[31m\u{9b}\r"), &env.ctx());
        let mut out = Vec::new();
        write_escape(&mut out, Method::Osc9, &got[0].text).unwrap();
        let s = String::from_utf8(out).unwrap();
        let body = s
            .strip_prefix("\x1b]9;")
            .and_then(|r| r.strip_suffix('\x07'))
            .expect("OSC 9 框架");
        assert!(!body.chars().any(char::is_control), "{body:?}");
        assert_eq!(body, "wor]52;c;AAAAker：等待核准 Bash[31m");

        // 直接給未清理的文字也一樣。
        let mut out = Vec::new();
        write_escape(&mut out, Method::Osc9, "a\x07b\x1bc").unwrap();
        assert_eq!(out, b"\x1b]9;abc\x07");
    }

    #[test]
    fn command_gets_notice_via_environment_only() {
        let notice = Notice {
            kind: NoticeKind::Approval,
            session: "s1".into(),
            label: "worker/a".into(),
            text: "worker/a：等待核准 $(touch /tmp/x)".into(),
        };
        let cmd = notify_command("notify-send ertangle \"$ERTANGLE_NOTIFY_TEXT\"", &notice);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_owned()).collect();
        assert_eq!(args[0], "-c");
        // 命令字串原封不動，通知內容沒有被拼進去。
        assert_eq!(args[1], "notify-send ertangle \"$ERTANGLE_NOTIFY_TEXT\"");
        let env: HashMap<_, _> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .collect();
        assert_eq!(env["ERTANGLE_NOTIFY_KIND"], "approval");
        assert_eq!(env["ERTANGLE_NOTIFY_SESSION"], "s1");
        assert_eq!(env["ERTANGLE_NOTIFY_LABEL"], "worker/a");
        assert_eq!(env["ERTANGLE_NOTIFY_TEXT"], notice.text);
    }

    #[test]
    fn command_runs_and_missing_command_is_reported() {
        let dir = std::env::temp_dir().join(format!("ertangle-notify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("out");
        let cfg = TuiCfg {
            notify: Some(vec!["command".into()]),
            notify_command: Some(format!(
                "printf '%s|%s' \"$ERTANGLE_NOTIFY_KIND\" \"$ERTANGLE_NOTIFY_TEXT\" > '{}'",
                file.display()
            )),
            ..Default::default()
        };
        let mut n = Notifier::from_cfg(&cfg);
        let st = state_with(SessionStatus::Busy);
        n.observe(&st, &approval("s1", "Bash"), None, false, "/", NOW_MS);
        let mut out = Vec::new();
        assert_eq!(n.flush(&mut out), None);
        assert!(out.is_empty(), "command 不寫終端機");
        let mut got = String::new();
        for _ in 0..100 {
            got = std::fs::read_to_string(&file).unwrap_or_default();
            if got.ends_with("Bash") {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(got, "approval|worker/a：等待核准 Bash");
        let _ = std::fs::remove_dir_all(&dir);

        let mut n = Notifier::from_cfg(&TuiCfg {
            notify: Some(vec!["command".into()]),
            ..Default::default()
        });
        n.observe(&st, &approval("s1", "Bash"), None, false, "/", NOW_MS);
        assert!(n.flush(&mut out).unwrap().contains("notify_command"));
    }
}
