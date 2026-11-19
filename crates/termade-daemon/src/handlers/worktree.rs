//! Worktree 與程式碼審查請求處理。

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use termade_core::{
    now_millis, BusMessage, Event, MsgKind, ReviewComment, Session, Target, WorktreeInfo,
};
use termade_protocol::{Frame, MergeStrategy};
use tr::tr;

use crate::git::{
    compute_worktree_diff, create_worktree_pr, discard_worktree_change, gc_block_reason,
    git_output, git_status_summary, merge_worktree_strategy, prepare_conflict_in_worktree,
    remove_worktree, session_repo, MergeConflict,
};
use crate::handlers::session::{resume_session, send_prompt};
use crate::{lock, new_id, ClientTx, Shared};

pub(crate) fn collect_worktrees(shared: &Shared) -> Vec<WorktreeInfo> {
    let sessions: Vec<Session> = lock(&shared.inner)
        .state
        .sessions_sorted()
        .into_iter()
        .filter(|s| s.branch.is_some())
        .cloned()
        .collect();
    let mut out = Vec::with_capacity(sessions.len());
    for s in sessions {
        let path = Path::new(&s.cwd);
        let (changed_files, summary) = if path.is_dir() {
            git_status_summary(path)
        } else {
            (0, tr!("(已移除)"))
        };
        out.push(WorktreeInfo {
            session: s.id,
            label: s.label,
            path: s.cwd,
            branch: s.branch.unwrap_or_default(),
            live: s.status.is_live(),
            changed_files,
            summary,
        });
    }
    out
}

pub(crate) fn handle_list(shared: &Arc<Shared>, reply: &ClientTx) {
    let worktrees = collect_worktrees(shared);
    let _ = reply.send(Frame::Worktrees { worktrees });
}

pub(crate) fn handle_diff(shared: &Arc<Shared>, reply: &ClientTx, session: String) {
    let sid = shared.resolve_session(&session);
    let sess = lock(&shared.inner).state.sessions.get(&sid).cloned();
    match sess {
        Some(s) => match compute_worktree_diff(Path::new(&s.cwd)) {
            Ok(diff) => {
                let _ = reply.send(Frame::Diff {
                    session: s.id,
                    branch: s.branch,
                    cwd: s.cwd,
                    diff,
                });
            }
            Err(e) => {
                let _ = reply.send(Frame::error(tr!("取得 diff 失敗：{e}", e = e)));
            }
        },
        None => {
            let _ = reply.send(Frame::error(tr!(
                "找不到 session {session}",
                session = session
            )));
        }
    }
}

pub(crate) fn handle_discard(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    file: String,
    patch: Option<String>,
) {
    let sid = shared.resolve_session(&session);
    let sess = lock(&shared.inner).state.sessions.get(&sid).cloned();
    match sess {
        Some(s) => {
            let wt_dir = Path::new(&s.cwd);
            match discard_worktree_change(wt_dir, &file, patch.as_deref()) {
                Ok(msg) => {
                    if let Ok(diff) = compute_worktree_diff(wt_dir) {
                        let _ = reply.send(Frame::Diff {
                            session: s.id,
                            branch: s.branch,
                            cwd: s.cwd,
                            diff,
                        });
                    }
                    let _ = reply.send(Frame::ok_note(msg));
                }
                Err(e) => {
                    let _ = reply.send(Frame::error(tr!("退回變更失敗：{e}", e = e)));
                }
            }
        }
        None => {
            let _ = reply.send(Frame::error(tr!(
                "找不到 session {session}",
                session = session
            )));
        }
    }
}

pub(crate) fn handle_merge(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    dry_run: bool,
    resolve: bool,
    strategy: MergeStrategy,
) {
    let sid = shared.resolve_session(&session);
    let sess = lock(&shared.inner).state.sessions.get(&sid).cloned();
    let default_cwd = shared.default_cwd();
    match sess {
        Some(s) => {
            match merge_worktree_strategy(&session_repo(&default_cwd, &s), &s, strategy, dry_run) {
                Ok(msg) => {
                    let _ = reply.send(Frame::ok_note(msg));
                }
                Err(e) => {
                    if let Some(conflict) = e.downcast_ref::<MergeConflict>() {
                        if resolve {
                            let wt_dir = Path::new(&s.cwd);
                            let prep_res = if strategy == MergeStrategy::Rebase {
                                let _ = git_output(wt_dir, &["rebase", &conflict.target]);
                                let conflicts = if !conflict.files.is_empty() {
                                    conflict.files.clone()
                                } else {
                                    git_output(wt_dir, &["diff", "--name-only", "--diff-filter=U"])
                                        .map(|o| {
                                            String::from_utf8_lossy(&o.stdout)
                                                .lines()
                                                .map(str::trim)
                                                .filter(|s| !s.is_empty())
                                                .map(str::to_string)
                                                .collect::<Vec<_>>()
                                        })
                                        .unwrap_or_default()
                                };
                                Ok(conflicts)
                            } else {
                                prepare_conflict_in_worktree(wt_dir, &conflict.target)
                            };

                            match prep_res {
                                Ok(conflicts) => {
                                    let prompt = match strategy {
                                    MergeStrategy::Rebase => format!(
                                        "[termade 衝突解決通知]\n在嘗試將分支「{branch}」rebase 至主分支「{target}」時發生 Git 衝突。\n已在你的工作目錄保留合併衝突標記。\n\n衝突檔案：\n{conflict_list}\n\n請執行以下步驟解決衝突：\n1. 檢視衝突檔案中的衝突標記 (<<<<<<<, =======, >>>>>>>)，根據上下文保留正確修改。\n2. 解決衝突後，使用 git add 標記已解決，並執行專案測試確保功能正常。\n3. 確認完成後執行 git rebase --continue 完成 rebase，並透過 termade_status 回報進度。",
                                        branch = conflict.branch,
                                        target = conflict.target,
                                        conflict_list = conflicts
                                            .iter()
                                            .map(|f| format!("- {f}"))
                                            .collect::<Vec<_>>()
                                            .join("\n"),
                                    ),
                                    _ => format!(
                                        "[termade 衝突解決通知]\n在嘗試將分支「{branch}」合併至主分支「{target}」時發生 Git 合併衝突。\n已在你的工作目錄保留合併衝突標記。\n\n衝突檔案：\n{conflict_list}\n\n請執行以下步驟解決衝突：\n1. 檢視衝突檔案中的衝突標記 (<<<<<<<, =======, >>>>>>>)，根據上下文保留正確修改。\n2. 解決衝突後，執行專案的測試與檢查以確保功能正常。\n3. 確認完成後進行 commit，並透過 termade_status 回報進度。",
                                        branch = conflict.branch,
                                        target = conflict.target,
                                        conflict_list = conflicts
                                            .iter()
                                            .map(|f| format!("- {f}"))
                                            .collect::<Vec<_>>()
                                            .join("\n"),
                                    ),
                                };

                                    let send_res = if shared.control(&s.id).is_some() {
                                        send_prompt(
                                            shared,
                                            &s.id,
                                            &prompt,
                                            &[],
                                            Some(termade_core::PROMPT_FROM_TERMADE),
                                        )
                                    } else if s.native_id.is_some() {
                                        resume_session(
                                            shared,
                                            &s.id,
                                            Some(prompt),
                                            Some(termade_core::PROMPT_FROM_TERMADE.to_string()),
                                        )
                                        .map(|_| ())
                                    } else {
                                        Err(anyhow::anyhow!(tr!(
                                            "session {label} 已結束且無對話 id，無法自動接續解衝突",
                                            label = s.label
                                        )))
                                    };

                                    match send_res {
                                        Ok(()) => {
                                            let note = tr!(
                                                "已將衝突委派給 {label}，衝突檔案：{files}",
                                                label = s.label,
                                                files = conflicts.join(", ")
                                            );
                                            let _ = reply.send(Frame::ok_note(note));
                                        }
                                        Err(send_err) => {
                                            let abort_cmd = if strategy == MergeStrategy::Rebase {
                                                "rebase"
                                            } else {
                                                "merge"
                                            };
                                            let _ = Command::new("git")
                                                .arg("-C")
                                                .arg(wt_dir)
                                                .args([abort_cmd, "--abort"])
                                                .output();
                                            let _ = reply.send(Frame::error(tr!(
                                                "委派解衝突失敗，已還原 worktree：{send_err}",
                                                send_err = send_err
                                            )));
                                        }
                                    }
                                }
                                Err(prep_err) => {
                                    let abort_cmd = if strategy == MergeStrategy::Rebase {
                                        "rebase"
                                    } else {
                                        "merge"
                                    };
                                    let _ = Command::new("git")
                                        .arg("-C")
                                        .arg(wt_dir)
                                        .args([abort_cmd, "--abort"])
                                        .output();
                                    let _ = reply.send(Frame::error(tr!(
                                        "在 worktree 建立衝突標記失敗：{prep_err}",
                                        prep_err = prep_err
                                    )));
                                }
                            }
                        } else {
                            let msg = tr!(
                                "合併衝突 ({files})；輸入 :merge --resolve 交由 Agent 自動解衝突",
                                files = conflict.files.join(", ")
                            );
                            let _ = reply.send(Frame::error(msg));
                        }
                    } else {
                        let _ = reply.send(Frame::error(tr!("合併失敗：{e}", e = e)));
                    }
                }
            }
        }
        None => {
            let _ = reply.send(Frame::error(tr!(
                "找不到 session {session}",
                session = session
            )));
        }
    }
}

pub(crate) fn handle_pr(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    title: String,
    body: String,
) {
    let sid = shared.resolve_session(&session);
    let sess = lock(&shared.inner).state.sessions.get(&sid).cloned();
    match sess {
        Some(s) => match create_worktree_pr(&s, &title, &body) {
            Ok(url) => {
                let _ = reply.send(Frame::ok_note(url));
            }
            Err(e) => {
                let _ = reply.send(Frame::error(tr!("建立 PR 失敗：{e}", e = e)));
            }
        },
        None => {
            let _ = reply.send(Frame::error(tr!(
                "找不到 session {session}",
                session = session
            )));
        }
    }
}

pub(crate) fn handle_gc(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: Option<String>,
    force: bool,
) {
    let target_sid = session.map(|s| shared.resolve_session(&s));
    let default_cwd = shared.default_cwd();
    let candidates: Vec<Session> = {
        let inner = lock(&shared.inner);
        inner
            .state
            .sessions
            .values()
            .filter(|s| {
                s.branch.is_some()
                    && match &target_sid {
                        Some(id) => &s.id == id,
                        None => !s.status.is_live(),
                    }
            })
            .cloned()
            .collect()
    };
    let mut removed = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    // 共用 worktree (handoff)的多個 session 只清一次。
    let mut seen_cwd = std::collections::HashSet::new();
    for s in candidates {
        let repo = session_repo(&default_cwd, &s);
        if !seen_cwd.insert(s.cwd.clone()) || crate::git::worktree_gone(&repo, &s) {
            continue;
        }
        if let Some(user) = shared.cwd_in_use_by_other(&s) {
            skipped.push(tr!(
                "{label} (worktree 仍被 {user} 使用)",
                label = s.label,
                user = user
            ));
            continue;
        }
        if !force {
            if let Some(reason) = gc_block_reason(&repo, &s) {
                skipped.push(tr!("{label} ({reason})", label = s.label, reason = reason));
                continue;
            }
        }
        match remove_worktree(&repo, &s, force) {
            Ok(()) => removed += 1,
            Err(e) => skipped.push(tr!("{label} (移除失敗：{e})", label = s.label, e = e)),
        }
    }
    let mut note = tr!("已清理 {removed} 個 worktree", removed = removed);
    if !skipped.is_empty() {
        note.push_str(&tr!(
            "；略過 {count} 個：{list} (可用 --force 強制移除)",
            count = skipped.len(),
            list = skipped.join("、")
        ));
    }
    let _ = reply.send(Frame::ok_note(note));
}

pub(crate) fn handle_comment(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    location: String,
    comment: String,
) {
    let sid = shared.resolve_session(&session);
    let review = ReviewComment {
        id: new_id("rev"),
        session: sid.clone(),
        location: location.clone(),
        comment: comment.clone(),
        ts: now_millis(),
    };
    shared.emit(Event::ReviewCommented { review });
    let message = BusMessage {
        id: new_id("msg"),
        from: "reviewer".into(),
        to: Target::Direct(sid.clone()),
        kind: MsgKind::Chat,
        body: format!("[review @ {location}] {comment}"),
        reply_to: None,
        ts: now_millis(),
    };
    shared.emit(Event::BusMessage { message });

    if let Some((target_id, ctl)) = shared.control(&sid) {
        shared.wake_if_sleeping(&target_id, &ctl);
        let prompt = format!(
            "[termade 程式碼審查註解]\n位置：{location}\n意見：{comment}\n請針對上述位置與審查意見修正程式碼。"
        );
        let _ = ctl.send_input(&prompt);
    }
    let _ = reply.send(Frame::ok_note(tr!("已送出審查註解")));
}
