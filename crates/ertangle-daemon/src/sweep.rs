//! Session cleanup: auto-archive merged workers, the periodic sweep, archive retention
//! and the manual `prune` request.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use ertangle_core::{now_millis, Event, Session, SessionId, SessionStatus, Task, TaskId};
use ertangle_protocol::Frame;
use tr::tr;

use crate::git::{gc_block_reason, remove_worktree, session_repo, worktree_gone};
use crate::{config, lock, ClientTx, Shared};

/// How often the daemon sweeps sessions.
const SWEEP_INTERVAL: Duration = Duration::from_secs(600);
/// Delay before the first sweep so daemon startup is not slowed by git calls.
const FIRST_SWEEP_DELAY: Duration = Duration::from_secs(5);
/// Ended sessions without a worktree stay in the list this long (counted from when they ended).
const KEEP_ENDED_MS: u64 = 3600 * 1000;

/// Whether an agent-spawned worker may be auto-archived right now.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WorkerCheck {
    /// Not an agent-spawned worker with a worktree: never auto-archived.
    Never,
    /// Not finished yet (busy, open tasks); look again on the next sweep.
    Wait,
    /// Finished. `explain` asks for a status note when the merge check blocks archival.
    Ready { explain: bool },
}

/// Pure eligibility rule for auto-archive (merge state is checked separately).
pub(crate) fn worker_check(s: &Session, tasks: &BTreeMap<TaskId, Task>) -> WorkerCheck {
    if s.parent.is_none() || s.branch.is_none() {
        return WorkerCheck::Never;
    }
    let mut has_tasks = false;
    for t in tasks
        .values()
        .filter(|t| t.assignee.as_deref() == Some(s.id.as_str()))
    {
        has_tasks = true;
        if t.status != ertangle_core::TaskStatus::Done {
            return WorkerCheck::Wait;
        }
    }
    let is_live = s.status.is_live();
    if is_live && !matches!(s.status, SessionStatus::Idle | SessionStatus::Sleeping) {
        return WorkerCheck::Wait;
    }
    WorkerCheck::Ready {
        explain: !is_live || has_tasks,
    }
}

/// Result of a cleanup pass.
#[derive(Debug, Default)]
pub(crate) struct SweepReport {
    pub archived: usize,
    /// `label (reason)` for every session that was kept.
    pub skipped: Vec<String>,
}

impl SweepReport {
    fn summary(&self) -> String {
        let mut note = tr!("已封存 {n} 個 session", n = self.archived);
        if !self.skipped.is_empty() {
            note.push_str(&tr!(
                "；略過 {count} 個：{list}",
                count = self.skipped.len(),
                list = self.skipped.join("、")
            ));
        }
        note
    }
}

impl Shared {
    /// Reason the session's worktree must be kept, or `None` when it is merged, clean or gone.
    fn merge_block(&self, s: &Session) -> Option<String> {
        let repo = session_repo(&self.default_cwd(), s);
        if worktree_gone(&repo, s) {
            return None;
        }
        gc_block_reason(&repo, s)
    }

    /// Stops the agent if alive, removes a merged worktree (when no other session shares
    /// it) and drops the session from the list (archiving it when resumable).
    fn retire_session(self: &Arc<Self>, s: &Session) -> Result<(), String> {
        if s.branch.is_some() {
            if let Some(user) = self.cwd_in_use_by_other(s) {
                return Err(tr!("worktree 仍被 {user} 使用", user = user));
            }
        }
        if s.status.is_live() {
            crate::handlers::session::stop_session(self, &s.id);
        }
        if s.branch.is_some() && self.cwd_shared_with(s).is_empty() {
            let repo = session_repo(&self.default_cwd(), s);
            if !worktree_gone(&repo, s) {
                remove_worktree(&repo, s, false).map_err(|e| tr!("移除失敗：{e}", e = e))?;
            }
        }
        self.emit(Event::SessionRemoved {
            session: s.id.clone(),
        });
        lock(&self.inner).pending_nudge.remove(&s.id);
        self.clear_queue_for_ended(&s.id, true);
        Ok(())
    }

    /// Records why a worker was kept, once (no event when the note is unchanged).
    fn note_kept(&self, s: &Session, reason: &str) {
        let note = tr!("未自動封存：{reason}", reason = reason);
        if s.status_note.as_deref() == Some(note.as_str()) {
            return;
        }
        self.emit(Event::SessionNote {
            session: s.id.clone(),
            note,
        });
    }

    /// Archives merged, finished agent-spawned workers (never human-spawned sessions).
    pub(crate) fn sweep_workers(self: &Arc<Self>) -> SweepReport {
        let (sessions, tasks) = {
            let inner = lock(&self.inner);
            (
                inner.state.sessions.values().cloned().collect::<Vec<_>>(),
                inner.state.tasks.clone(),
            )
        };
        let mut report = SweepReport::default();
        for s in sessions {
            let WorkerCheck::Ready { explain } = worker_check(&s, &tasks) else {
                continue;
            };
            let outcome = match self.merge_block(&s) {
                Some(reason) => Err(reason),
                None => self.retire_session(&s),
            };
            match outcome {
                Ok(()) => report.archived += 1,
                Err(reason) => {
                    if explain {
                        self.note_kept(&s, &reason);
                    }
                    report.skipped.push(format!("{} ({reason})", s.label));
                }
            }
        }
        report
    }

    /// Archives ended sessions without a worktree once they have been ended for an hour.
    /// Orphans interrupted by this restart (`orphan_ids`) stay for auto or manual resume.
    pub(crate) fn prune_ended_sessions(&self, now_ms: u64, orphan_ids: &HashSet<SessionId>) {
        let mut inner = lock(&self.inner);
        let stale: Vec<SessionId> = inner
            .state
            .sessions
            .values()
            .filter(|s| {
                !orphan_ids.contains(&s.id)
                    && !s.status.is_live()
                    && s.branch.is_none()
                    && now_ms.saturating_sub(s.ended_or_created_ms()) > KEEP_ENDED_MS
            })
            .map(|s| s.id.clone())
            .collect();
        for session in stale {
            self.emit_locked(&mut inner, Event::SessionRemoved { session });
        }
    }

    /// Drops archived sessions beyond the configured count or age limits.
    pub(crate) fn prune_archive(&self, now_ms: u64) {
        let cfg = config::Config::load_file(&crate::paths::config_dir().join("config.toml"));
        let mut inner = lock(&self.inner);
        let sessions = inner.state.archived_to_prune(
            now_ms,
            cfg.sessions.archive_max(),
            cfg.sessions.archive_max_days(),
        );
        if !sessions.is_empty() {
            self.emit_locked(&mut inner, Event::ArchivePruned { sessions });
        }
    }

    /// One full cleanup pass: workers, ended sessions, archive retention.
    pub(crate) fn run_sweep(self: &Arc<Self>) {
        self.sweep_workers();
        let now = now_millis();
        self.prune_ended_sessions(now, &HashSet::new());
        self.prune_archive(now);
    }

    /// Starts the background sweep thread (first pass shortly after startup).
    pub(crate) fn spawn_sweeper(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        std::thread::spawn(move || {
            std::thread::sleep(FIRST_SWEEP_DELAY);
            while let Some(sh) = weak.upgrade() {
                sh.run_sweep();
                drop(sh);
                std::thread::sleep(SWEEP_INTERVAL);
            }
        });
    }

    /// Manual prune: archives ended sessions that are merged or have no worktree.
    pub(crate) fn prune_sessions(self: &Arc<Self>, project: Option<&str>) -> SweepReport {
        let ended: Vec<Session> = lock(&self.inner)
            .state
            .sessions
            .values()
            .filter(|s| !s.status.is_live() && project.is_none_or(|p| s.project_root() == p))
            .cloned()
            .collect();
        let mut report = SweepReport::default();
        for s in ended {
            let blocked = if s.branch.is_some() {
                self.merge_block(&s)
            } else {
                None
            };
            let outcome = match blocked {
                Some(reason) => Err(reason),
                None => self.retire_session(&s),
            };
            match outcome {
                Ok(()) => report.archived += 1,
                Err(reason) => report.skipped.push(format!("{} ({reason})", s.label)),
            }
        }
        report
    }
}

pub(crate) fn handle_prune(shared: &Arc<Shared>, reply: &ClientTx, project: Option<String>) {
    let report = shared.prune_sessions(project.as_deref());
    let _ = reply.send(Frame::ok_note(report.summary()));
}
