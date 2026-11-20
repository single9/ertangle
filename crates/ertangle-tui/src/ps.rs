//! 互動式處理程序檢視浮層 (`:ps` / `Space s p`)。

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ertangle_core::SessionId;
use ertangle_protocol::Request;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use std::collections::HashSet;
use tr::tr;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode};
use crate::hosts::HostRouter;
use crate::send;

#[derive(Clone, Debug)]
pub(crate) struct PsModal {
    pub(crate) session: SessionId,
    pub(crate) cursor: usize,
    pub(crate) killed_pids: HashSet<u32>,
}

/// 開啟 `:ps` 互動處理程序浮層。
pub(crate) fn open(app: &mut App) {
    let Some(sid) = app.selected_id() else {
        app.status = tr!("沒有選取的 session");
        return;
    };
    app.mode = Mode::Ps;
    app.ps_modal = Some(PsModal {
        session: sid,
        cursor: 0,
        killed_pids: HashSet::new(),
    });
}

/// 處理 `:ps` 浮層的鍵盤操作。
pub(crate) fn handle_key(
    app: &mut App,
    writer: Option<&mut HostRouter>,
    key: KeyEvent,
) -> Result<()> {
    let Some(modal) = app.ps_modal.as_mut() else {
        app.mode = Mode::Normal;
        return Ok(());
    };

    let sid = modal.session.clone();
    let bg_len = app
        .state
        .sessions
        .get(&sid)
        .map(|s| s.bg_procs.len())
        .unwrap_or(0);

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.mode = Mode::Normal;
            app.ps_modal = None;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            modal.cursor = modal.cursor.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if bg_len > 0 {
                modal.cursor = (modal.cursor + 1).min(bg_len - 1);
            }
        }
        KeyCode::Char('x') | KeyCode::Char('d') => {
            let proc_opt = app
                .state
                .sessions
                .get(&sid)
                .and_then(|s| s.bg_procs.get(modal.cursor).cloned());

            if let Some(proc) = proc_opt {
                let pid = proc.pid;
                let force = modal.killed_pids.contains(&pid);
                if !force {
                    modal.killed_pids.insert(pid);
                }
                if let Some(w) = writer {
                    send(
                        w,
                        &Request::KillBgProc {
                            session: sid.clone(),
                            pid: Some(pid),
                            force,
                        },
                    )?;
                }
                app.status = if force {
                    tr!("已強制終止處理程序 PID {pid} (SIGKILL)", pid = pid)
                } else {
                    tr!(
                        "已請求終止處理程序 PID {pid} (SIGTERM，再按一次強制終止)",
                        pid = pid
                    )
                };
            } else {
                app.status = tr!("沒有可終止的背景處理程序");
            }
        }
        KeyCode::Char('X') => {
            if let Some(w) = writer {
                send(
                    w,
                    &Request::KillBgProc {
                        session: sid.clone(),
                        pid: None,
                        force: false,
                    },
                )?;
            }
            app.status = tr!("已要求清理 {session} 底下所有背景處理程序", session = sid);
        }
        _ => {}
    }

    Ok(())
}

/// 繪製置中的處理程序檢視浮層。
pub(crate) fn draw(f: &mut ratatui::Frame, app: &App) {
    let Some(modal) = &app.ps_modal else {
        return;
    };
    let s = match app.state.sessions.get(&modal.session) {
        Some(s) => s,
        None => return,
    };

    let full = f.area();
    let w = full.width.saturating_sub(6).clamp(50, 110);
    let mut total_rows = 6;
    if !s.active_tools.is_empty() {
        total_rows += s.active_tools.len() as u16 + 2;
    }
    if !s.bg_procs.is_empty() {
        total_rows += s.bg_procs.len() as u16 + 2;
    } else {
        total_rows += 2;
    }

    let h = total_rows.min(full.height.saturating_sub(2));
    let area = Rect::new(
        full.x + full.width.saturating_sub(w) / 2,
        full.y + full.height.saturating_sub(h) / 2,
        w.min(full.width),
        h,
    );

    let mut lines: Vec<Line> = Vec::new();
    let now = ertangle_core::now_millis();

    // [執行中工具 / Subagent] 區塊 (有工具時才顯示)
    if !s.active_tools.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            tr!(" ─── 執行中工具／Subagent ───"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]));
        for t in &s.active_tools {
            let elapsed_secs = (now.saturating_sub(t.started_ms)) / 1000;
            let tag = if t.is_subagent { "[subagent]" } else { "[run]" };
            let tag_color = if t.is_subagent {
                Color::Magenta
            } else {
                Color::Cyan
            };
            let sum_str = if t.summary.is_empty() {
                String::new()
            } else {
                format!(": {}", t.summary)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {tag} "), Style::default().fg(tag_color)),
                Span::styled(
                    format!("{}{sum_str} ", t.name),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("({elapsed_secs}s)"),
                    Style::default().fg(Color::Yellow),
                ),
            ]));
        }
        lines.push(Line::raw(""));
    }

    // [背景子處理程序] 區塊
    lines.push(Line::from(vec![Span::styled(
        tr!(" ─── 背景子處理程序 (PID / 執行時間 / 命令列) ───"),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));

    if s.bg_procs.is_empty() {
        lines.push(Line::styled(
            tr!("  (目前沒有殘留的背景子處理程序)"),
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        lines.push(Line::from(vec![Span::styled(
            format!(
                "  {:<8} {:<10} {}",
                "PID",
                tr!("執行時間"),
                tr!("命令列摘要")
            ),
            Style::default().fg(Color::Gray),
        )]));

        for (idx, proc) in s.bg_procs.iter().enumerate() {
            let sel = idx == modal.cursor;
            let etime = format_elapsed(proc.elapsed_secs);
            let base_style = if sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            let prefix = if sel { "▶ " } else { "  " };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{prefix}{:<8} {:<10} ", proc.pid, etime),
                    base_style,
                ),
                Span::styled(
                    truncate_str(&proc.cmdline, w.saturating_sub(26) as usize),
                    base_style.fg(Color::White),
                ),
            ]));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        tr!(" ↑↓/j/k 移動 · x 單獨終止 (連按強制) · X 清理全部 · Esc/q 關閉"),
        Style::default().fg(Color::Gray),
    )));

    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(tr!(" 處理程序與 Subagent 監控 ({label}) ", label = s.label)),
        ),
        area,
    );
}

fn format_elapsed(secs: u64) -> String {
    if secs >= 3600 {
        format!(
            "{:02}:{:02}:{:02}",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

fn truncate_str(s: &str, max_w: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw + 1 > max_w {
            out.push('…');
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}
