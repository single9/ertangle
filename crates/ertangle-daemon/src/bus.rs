//! 訊息匯流排：防呆斷路器、waiter、訊息投遞與 mailbox 後備。

use super::*;
use tr::tr;

fn quota_exhausted(state: &ertangle_core::State, sid: &str) -> bool {
    let now = ertangle_core::now_millis();
    state
        .sessions
        .get(sid)
        .and_then(|session| state.quotas.get(session.kind.id()))
        .is_some_and(|quota| quota.is_reached(now) || quota.max_used_live(now) >= 100.0)
}

// ---------------------------------------------------------------------------
// 防呆：速率限制與迴圈偵測
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(crate) struct PairStats {
    last_body: String,
    repeat_count: usize,
    timestamps_ms: VecDeque<u64>,
}

/// 針對每對 `(from, to)` 檢查是否發生無限迴圈或訊息風暴。
#[derive(Default)]
pub struct BusGuard {
    pairs: HashMap<(String, String), PairStats>,
}

impl BusGuard {
    pub fn check(&mut self, from: &str, to: &Target, body: &str, now_ms: u64) -> Result<()> {
        let to_key = match to {
            Target::Direct(s) => format!("@{s}"),
            Target::Channel(c) => format!("#{c}"),
            Target::Broadcast => "*".to_string(),
        };
        let entry = self
            .pairs
            .entry((from.to_string(), to_key.clone()))
            .or_default();

        // 1) 速率限制：滑動 60 秒視窗。
        while let Some(&front) = entry.timestamps_ms.front() {
            if now_ms.saturating_sub(front) > 60_000 {
                entry.timestamps_ms.pop_front();
            } else {
                break;
            }
        }
        if entry.timestamps_ms.len() >= PAIR_RATE_CAP_PER_MIN {
            bail!(
                "{}",
                tr!(
                    "速率限制：{from} → {to_key} 在 1 分鐘內已傳送超過 {cap} 則訊息",
                    from = from,
                    to_key = to_key,
                    cap = PAIR_RATE_CAP_PER_MIN
                )
            );
        }

        // 2) 迴圈偵測：連續相同內容達 LOOP_REPEAT_CAP 次即斷路。
        if !body.is_empty() && entry.last_body == body {
            if entry.repeat_count >= LOOP_REPEAT_CAP {
                bail!(
                    "{}",
                    tr!(
                        "迴圈偵測：{from} → {to_key} 連續傳送相同內容超過 {cap} 次，已斷路",
                        from = from,
                        to_key = to_key,
                        cap = LOOP_REPEAT_CAP
                    )
                );
            }
            entry.repeat_count += 1;
        } else {
            entry.last_body = body.to_string();
            entry.repeat_count = 1;
        }

        entry.timestamps_ms.push_back(now_ms);
        Ok(())
    }
}

/// 哪些訊息種類值得主動叫醒閒置的收件者。
///
/// `Status` 與 `Artifact` 屬於被動資訊 (更新狀態、分享產出)，留在收件匣即可；
/// 其餘 (chat / handoff / question / answer)都代表對方可能需要動作。
pub(crate) fn kind_wakes_recipient(kind: MsgKind) -> bool {
    !matches!(kind, MsgKind::Status | MsgKind::Artifact)
}

/// 阻塞等待訊息的註冊項 (`BusWait` / `ertangle_wait` / `ertangle_ask`)。
pub(crate) struct Waiter {
    pub(crate) session: SessionId,
    pub(crate) from: Option<SessionId>,
    pub(crate) reply_to: Option<MsgId>,
    pub(crate) tx: Sender<BusMessage>,
}

impl Waiter {
    pub(crate) fn matches(&self, state: &State, msg: &BusMessage) -> bool {
        let label = state.sessions.get(&self.session).map(|s| s.label.as_str());
        if !msg.matches_recipient(&self.session, label) {
            return false;
        }
        if let Some(expected_from) = &self.from {
            let resolved_expected = state
                .resolve_session_id(expected_from)
                .unwrap_or_else(|| expected_from.clone());
            let resolved_actual = state
                .resolve_session_id(&msg.from)
                .unwrap_or_else(|| msg.from.clone());
            if resolved_expected != resolved_actual {
                return false;
            }
        }
        if let Some(expected_reply) = &self.reply_to {
            if let Some(actual_reply) = &msg.reply_to {
                if actual_reply != expected_reply {
                    return false;
                }
            } else if self.from.is_none() {
                return false;
            }
        }
        true
    }
}

impl Shared {
    pub(crate) fn handle_bus_send(
        self: &Arc<Self>,
        reply: &ClientTx,
        from: Option<SessionId>,
        to: Target,
        kind: MsgKind,
        body: String,
        reply_to: Option<MsgId>,
    ) {
        // 已交接的收件者會改投給接手者 (T24)，超限判斷要看實際收件者 (T25)。
        let (over_budget_target, quota_blocked_target) = match &to {
            Target::Direct(t) => {
                let inner = lock(&self.inner);
                let sid = inner
                    .state
                    .resolve_session_id(t)
                    .unwrap_or_else(|| t.clone());
                let usable =
                    |id: &str| {
                        inner.state.sessions.get(id).is_some_and(|s| {
                            s.status.is_live() && !quota_exhausted(&inner.state, id)
                        })
                    };
                let actual = if usable(&sid) {
                    sid.clone()
                } else {
                    let successor = inner.state.successor_of(&sid);
                    if successor != sid && usable(&successor) {
                        successor
                    } else {
                        sid.clone()
                    }
                };
                let over_budget = inner
                    .state
                    .sessions
                    .get(&actual)
                    .is_some_and(|s| s.over_budget);
                let quota_blocked = quota_exhausted(&inner.state, &actual);
                (over_budget, quota_blocked)
            }
            _ => (false, false),
        };
        match self.deliver_message_noted(from, to, kind, body, reply_to) {
            Ok((msg_id, route_notes)) => {
                let mut note = if over_budget_target && kind_wakes_recipient(kind) {
                    tr!(
                        "已送出訊息 {msg_id} (收件者已超過成本上限 over_budget，拒絕自動喚醒)",
                        msg_id = msg_id
                    )
                } else {
                    tr!("已送出訊息 {msg_id}", msg_id = msg_id)
                };
                for n in route_notes {
                    note.push_str(&tr!("({n})", n = n));
                }
                if quota_blocked_target && kind_wakes_recipient(kind) {
                    note.push_str(&format!(
                        "\n{}",
                        tr!("{prefix}target quota is exhausted; message queued but the peer will not be woken", prefix = crate::handlers::task::WARNING_PREFIX)
                    ));
                }
                let _ = reply.send(Frame::ok_note(note));
            }
            Err(e) => {
                let _ = reply.send(Frame::error(e));
            }
        }
    }

    /// 把訊息送進匯流排：防呆檢查、喚醒阻塞中的 waiter、寫入各 session 的
    /// mailbox，並主動叫醒閒置 (`Idle`)或休眠 (`Sleeping`)的收件者。
    ///
    /// 喚醒規則：
    /// * 直接訊息 (`Direct`)與廣播／頻道 (`Broadcast` / `Channel`)都會喚醒收件者。
    /// * 只喚醒狀態為 `Idle` / `Sleeping` 的 session；`Busy` 代表正在工作，
    ///   訊息留在收件匣，並記入 `pending_nudge`：該 session 回到 `Idle`、
    ///   且排隊的 prompt (T19)送完後，由 [`Self::nudge_unread_if_idle`] 把
    ///   仍未讀的訊息合併成一則提醒 (每則訊息只提醒一次；已被 waiter 取走或
    ///   `ertangle_inbox` 讀過的、`over_budget` 的 session 不提醒)。
    /// * 已經透過 `ertangle_wait` / `ertangle_ask` 取得訊息的 session 不重複打擾。
    /// * `Status` / `Artifact` 屬於被動資訊，不主動喚醒任何人。
    pub(crate) fn deliver_message(
        self: &Arc<Self>,
        from: Option<SessionId>,
        to: Target,
        kind: MsgKind,
        body: String,
        reply_to: Option<MsgId>,
    ) -> std::result::Result<MsgId, String> {
        self.deliver_message_noted(from, to, kind, body, reply_to)
            .map(|(id, _)| id)
    }

    /// 同 [`Self::deliver_message`]，另回傳投遞路徑的說明 (改投給接手者、收件者已結束等)。
    ///
    /// 直接訊息的收件者若已結束且已被交接，改投給 [`State::successor_of`] 的結果，
    /// body 開頭註明原收件者；已結束又沒有接手者時仍照常投遞 (留作紀錄)，但說明不會被讀取。
    pub(crate) fn deliver_message_noted(
        self: &Arc<Self>,
        from: Option<SessionId>,
        to: Target,
        kind: MsgKind,
        mut body: String,
        reply_to: Option<MsgId>,
    ) -> std::result::Result<(MsgId, Vec<String>), String> {
        let now = now_millis();
        let mut route_notes: Vec<String> = Vec::new();
        let (from_sid, resolved_to) = {
            let mut inner = lock(&self.inner);
            let from_sid = from
                .map(|f| inner.state.resolve_session_id(&f).unwrap_or(f))
                .unwrap_or_else(|| "human".into());
            let resolved_to = match to {
                Target::Direct(t) => {
                    let mut sid = inner
                        .state
                        .resolve_session_id(&t)
                        .unwrap_or_else(|| t.clone());
                    let unusable = |st: &ertangle_core::State, id: &str| {
                        st.sessions.get(id).is_some_and(|s| !s.status.is_live())
                            || quota_exhausted(st, id)
                    };
                    if unusable(&inner.state, &sid) {
                        let succ = inner.state.successor_of(&sid);
                        if succ != sid && !unusable(&inner.state, &succ) {
                            let succ_label = inner
                                .state
                                .sessions
                                .get(&succ)
                                .map(|s| s.label.clone())
                                .unwrap_or_else(|| succ.clone());
                            body = format!("(原收件者 {t}，已交接)\n{body}");
                            route_notes.push(tr!(
                                "收件者 {t} 已交接，訊息改投給接手的 {succ_label} ({succ})",
                                t = t,
                                succ_label = succ_label,
                                succ = succ
                            ));
                            sid = succ;
                        }
                        if unusable(&inner.state, &sid) {
                            if quota_exhausted(&inner.state, &sid) {
                                route_notes.push(tr!(
                                    "目標 agent 額度已用完，訊息留在 mailbox，額度恢復前不會被讀取",
                                    sid = sid
                                ));
                            } else {
                                route_notes
                                    .push(tr!("對方 {sid} 已結束，訊息不會被讀取", sid = sid));
                            }
                        }
                    }
                    Target::Direct(sid)
                }
                other => other,
            };
            inner
                .bus_guard
                .check(&from_sid, &resolved_to, &body, now)
                .map_err(|e| e.to_string())?;
            (from_sid, resolved_to)
        };

        let msg_id = new_id("msg");
        let message = BusMessage {
            id: msg_id.clone(),
            from: from_sid.clone(),
            to: resolved_to.clone(),
            kind,
            body: body.clone(),
            reply_to,
            ts: now,
        };
        self.emit(Event::BusMessage {
            message: message.clone(),
        });

        let mut woken_by_waiter: HashSet<SessionId> = HashSet::new();
        let mut idle_targets: Vec<(SessionId, Arc<dyn AgentControl>)> = Vec::new();
        let mut mailbox_dirs: Vec<(PathBuf, String)> = Vec::new();
        {
            let mut inner = lock(&self.inner);
            let state_clone = inner.state.clone();
            let waiters = std::mem::take(&mut inner.waiters);
            let mut remaining = Vec::with_capacity(waiters.len());
            for w in waiters {
                if w.matches(&state_clone, &message) && w.tx.send(message.clone()).is_ok() {
                    self.advance_inbox_cursor_if_oldest(&mut inner, &w.session, &message.id);
                    woken_by_waiter.insert(w.session.clone());
                } else {
                    remaining.push(w);
                }
            }
            inner.waiters = remaining;

            for s in state_clone.sessions.values() {
                if message.matches_recipient(&s.id, Some(&s.label)) {
                    mailbox_dirs.push((PathBuf::from(&s.cwd), s.id.clone()));
                }
            }

            if kind_wakes_recipient(kind) {
                for s in state_clone.sessions.values() {
                    if quota_exhausted(&state_clone, &s.id) {
                        continue;
                    }
                    if s.status == SessionStatus::Busy
                        && s.id != from_sid
                        && !s.over_budget
                        && !woken_by_waiter.contains(&s.id)
                        && message.matches_recipient(&s.id, Some(&s.label))
                    {
                        inner
                            .pending_nudge
                            .entry(s.id.clone())
                            .or_default()
                            .push(message.id.clone());
                        continue;
                    }
                    if s.id == from_sid
                        || s.over_budget
                        || woken_by_waiter.contains(&s.id)
                        || !matches!(s.status, SessionStatus::Idle | SessionStatus::Sleeping)
                        || inner.queues.get(&s.id).is_some_and(|q| !q.is_empty())
                        || !message.matches_recipient(&s.id, Some(&s.label))
                    {
                        continue;
                    }
                    if let Some(entry) = inner.sessions.get(&s.id) {
                        idle_targets.push((s.id.clone(), entry.ctl.clone()));
                    }
                }
            }
        }

        for (cwd, sid) in mailbox_dirs {
            let _ = append_mailbox(&cwd, &sid, &message);
        }

        for (target_sid, ctl) in idle_targets {
            self.wake_if_sleeping(&target_sid, &ctl);
            let reply_hint = match &message.to {
                Target::Direct(_) => format!(
                    "請處理並使用 ertangle_send (或 `ertangle send --to {}`)回覆。",
                    message.from
                ),
                _ => "請處理；可先呼叫 ertangle_inbox 查看完整收件匣，再用 ertangle_send 回覆。"
                    .to_string(),
            };
            let nudge = format!(
                "[ertangle 匯流排訊息 id={} 來自 {} ({})]: {}\n{reply_hint}",
                message.id,
                message.from,
                message.kind.id(),
                message.body,
            );
            let _ = ctl.send_input(&nudge);
        }

        Ok((msg_id, route_notes))
    }

    /// 回到 `Idle` 時，把忙碌期間收到、至今仍未讀的訊息合併成一則提醒 prompt (T25)。
    ///
    /// 已被 `inbox_cursors` 涵蓋 (waiter 取走或 `ertangle_inbox` 讀過)的不提醒；
    /// `over_budget` 的 session 保留記錄不送，調高上限後下一次回到 `Idle` 再補。
    pub(crate) fn nudge_unread_if_idle(self: &Arc<Self>, sid: &SessionId) {
        let (ids, text) = {
            let mut inner = lock(&self.inner);
            let idle = inner
                .state
                .sessions
                .get(sid)
                .is_some_and(|s| s.status == SessionStatus::Idle && !s.over_budget);
            if !idle {
                return;
            }
            let Some(ids) = inner.pending_nudge.remove(sid) else {
                return;
            };
            let pos = |id: &str| inner.state.messages.iter().position(|m| m.id == id);
            let cursor = inner.state.inbox_cursors.get(sid).and_then(|c| pos(c));
            let unread: Vec<&BusMessage> = ids
                .iter()
                .filter(|id| match (pos(id), cursor) {
                    (Some(i), Some(c)) => i > c,
                    (Some(_), None) => true,
                    (None, _) => false,
                })
                .filter_map(|id| inner.state.messages.iter().find(|m| &m.id == id))
                .collect();
            if unread.is_empty() {
                return;
            }
            let list: Vec<String> = unread
                .iter()
                .map(|m| format!("id={} 來自 {} ({})", m.id, m.from, m.kind.id()))
                .collect();
            let text = format!(
                "[ertangle 未讀訊息提醒] 你忙碌時收到 {} 則訊息：{}。\n請呼叫 ertangle_inbox 查看 (僅在需要時以 ertangle_send 回覆；收到 ACK / ANSWER / TASK_DONE 不需回覆，亦不需向使用者重複說明規則)。",
                unread.len(),
                list.join("；"),
            );
            (ids, text)
        };
        if handlers::session::send_prompt(
            self,
            sid,
            &text,
            &[],
            Some(ertangle_core::PROMPT_FROM_ERTANGLE),
        )
        .is_err()
        {
            lock(&self.inner).pending_nudge.insert(sid.clone(), ids);
        }
    }

    pub(crate) fn handle_bus_wait(
        self: &Arc<Self>,
        reply: &ClientTx,
        session: SessionId,
        from: Option<SessionId>,
        reply_to: Option<MsgId>,
        timeout_ms: u64,
    ) {
        let sid = self.resolve_session(&session);
        let (tx, rx) = mpsc::channel();
        let waiter = Waiter {
            session: sid.clone(),
            from,
            reply_to,
            tx,
        };

        {
            let mut inner = lock(&self.inner);
            let after = inner.state.inbox_cursors.get(&sid).cloned();
            let unread = inner.state.inbox_for(&sid, after.as_deref());
            if let Some(found) = unread.into_iter().find(|m| waiter.matches(&inner.state, m)) {
                self.advance_inbox_cursor_if_oldest(&mut inner, &sid, &found.id);
                let _ = reply.send(Frame::Messages {
                    messages: vec![found],
                    timed_out: false,
                });
                return;
            }
            inner.waiters.push(waiter);
        }

        let reply = reply.clone();
        let clamped_ms = timeout_ms.clamp(1, MAX_WAIT_MS);
        std::thread::spawn(
            move || match rx.recv_timeout(Duration::from_millis(clamped_ms)) {
                Ok(msg) => {
                    let _ = reply.send(Frame::Messages {
                        messages: vec![msg],
                        timed_out: false,
                    });
                }
                Err(_) => {
                    let _ = reply.send(Frame::Messages {
                        messages: vec![],
                        timed_out: true,
                    });
                }
            },
        );
    }

    pub(crate) fn handle_bus_inbox(
        self: &Arc<Self>,
        reply: &ClientTx,
        session: String,
        unread_only: bool,
    ) {
        let sid = self.resolve_session(&session);
        let mut inner = lock(&self.inner);
        let after = if unread_only {
            inner.state.inbox_cursors.get(&sid).cloned()
        } else {
            None
        };
        let messages = inner.state.inbox_for(&sid, after.as_deref());
        if unread_only {
            if let Some(last) = messages.last() {
                let last_id = last.id.clone();
                self.advance_inbox_cursor(&mut inner, &sid, &last_id);
            }
        }
        let _ = reply.send(Frame::Messages {
            messages,
            timed_out: false,
        });
    }
}

/// 機制 C 後備：把收到的訊息附加到 `<cwd>/.ertangle/mailbox/<session>.jsonl`。
pub(crate) fn append_mailbox(cwd: &Path, session: &str, msg: &BusMessage) -> Result<()> {
    let dir = cwd.join(".ertangle").join("mailbox");
    fs::create_dir_all(&dir)?;
    let file_path = dir.join(format!("{session}.jsonl"));
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)?;
    let mut line = serde_json::to_string(msg)?;
    line.push('\n');
    f.write_all(line.as_bytes())?;
    Ok(())
}
