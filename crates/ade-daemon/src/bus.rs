//! 訊息匯流排：防呆斷路器、waiter、訊息投遞與 mailbox 後備。

use super::*;

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
                "速率限制：{from} → {to_key} 在 1 分鐘內已傳送超過 {PAIR_RATE_CAP_PER_MIN} 則訊息"
            );
        }

        // 2) 迴圈偵測：連續相同內容達 LOOP_REPEAT_CAP 次即斷路。
        if !body.is_empty() && entry.last_body == body {
            if entry.repeat_count >= LOOP_REPEAT_CAP {
                bail!(
                    "迴圈偵測：{from} → {to_key} 連續傳送相同內容超過 {LOOP_REPEAT_CAP} 次，已斷路"
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
/// `Status` 與 `Artifact` 屬於被動資訊（更新狀態、分享產出），留在收件匣即可；
/// 其餘（chat / handoff / question / answer）都代表對方可能需要動作。
pub(crate) fn kind_wakes_recipient(kind: MsgKind) -> bool {
    !matches!(kind, MsgKind::Status | MsgKind::Artifact)
}

/// 阻塞等待訊息的註冊項（`BusWait` / `ade_wait` / `ade_ask`）。
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
        match self.deliver_message(from, to, kind, body, reply_to) {
            Ok(msg_id) => {
                let _ = reply.send(Frame::ok_note(format!("已送出訊息 {msg_id}")));
            }
            Err(e) => {
                let _ = reply.send(Frame::error(e));
            }
        }
    }

    /// 把訊息送進匯流排：防呆檢查、喚醒阻塞中的 waiter、寫入各 session 的
    /// mailbox，並主動叫醒閒置（`Idle`）或休眠（`Sleeping`）的收件者。
    ///
    /// 喚醒規則：
    /// * 直接訊息（`Direct`）與廣播／頻道（`Broadcast` / `Channel`）都會喚醒收件者。
    /// * 只喚醒狀態為 `Idle` / `Sleeping` 的 session；`Busy` 代表正在工作，
    ///   訊息留在收件匣即可。
    /// * 已經透過 `ade_wait` / `ade_ask` 取得訊息的 session 不重複打擾。
    /// * `Status` / `Artifact` 屬於被動資訊，不主動喚醒任何人。
    pub(crate) fn deliver_message(
        self: &Arc<Self>,
        from: Option<SessionId>,
        to: Target,
        kind: MsgKind,
        body: String,
        reply_to: Option<MsgId>,
    ) -> std::result::Result<MsgId, String> {
        let now = now_millis();
        let (from_sid, resolved_to) = {
            let mut inner = lock(&self.inner);
            let from_sid = from
                .map(|f| inner.state.resolve_session_id(&f).unwrap_or(f))
                .unwrap_or_else(|| "human".into());
            let resolved_to = match to {
                Target::Direct(t) => {
                    Target::Direct(inner.state.resolve_session_id(&t).unwrap_or(t))
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
                    inner
                        .inbox_cursors
                        .insert(w.session.clone(), message.id.clone());
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
                    if s.id == from_sid
                        || woken_by_waiter.contains(&s.id)
                        || !matches!(s.status, SessionStatus::Idle | SessionStatus::Sleeping)
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
                    "請處理並使用 ade_send（或 `ade send --to {}`）回覆。",
                    message.from
                ),
                _ => "請處理；可先呼叫 ade_inbox 查看完整收件匣，再用 ade_send 回覆。".to_string(),
            };
            let nudge = format!(
                "[ade 匯流排訊息 id={} 來自 {} ({})]: {}\n{reply_hint}",
                message.id,
                message.from,
                message.kind.id(),
                message.body,
            );
            let _ = ctl.send_input(&nudge);
        }

        Ok(msg_id)
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
            let after = inner.inbox_cursors.get(&sid).cloned();
            let unread = inner.state.inbox_for(&sid, after.as_deref());
            if let Some(found) = unread.into_iter().find(|m| waiter.matches(&inner.state, m)) {
                inner.inbox_cursors.insert(sid, found.id.clone());
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
            inner.inbox_cursors.get(&sid).cloned()
        } else {
            None
        };
        let messages = inner.state.inbox_for(&sid, after.as_deref());
        if unread_only {
            if let Some(last) = messages.last() {
                inner.inbox_cursors.insert(sid, last.id.clone());
            }
        }
        let _ = reply.send(Frame::Messages {
            messages,
            timed_out: false,
        });
    }
}

/// 機制 C 後備：把收到的訊息附加到 `<cwd>/.ade/mailbox/<session>.jsonl`。
pub(crate) fn append_mailbox(cwd: &Path, session: &str, msg: &BusMessage) -> Result<()> {
    let dir = cwd.join(".ade").join("mailbox");
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
