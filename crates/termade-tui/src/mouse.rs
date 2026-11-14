//! 滑鼠事件處理與逐字稿區塊選取。

use anyhow::Result;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use termade_daemon::ConnStream;
use tr::tr;
use unicode_width::UnicodeWidthChar;

use crate::app::{scroll_transcript_lines, App, CenterView};
use crate::clipboard::write_clipboard;
use crate::request_selected_diff;

pub(crate) fn handle_mouse(
    app: &mut App,
    writer: &mut ConnStream,
    mouse: MouseEvent,
) -> Result<()> {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            if app.view == CenterView::Diff {
                app.diff_cursor = app.diff_cursor.saturating_sub(3);
            } else if app.view == CenterView::Tasks {
                crate::tasks::move_task_cursor(app, -1);
            } else {
                scroll_transcript_lines(app, -3);
            }
        }
        MouseEventKind::ScrollDown => {
            if app.view == CenterView::Diff {
                let max = app.diff_lines.len().saturating_sub(1);
                app.diff_cursor = (app.diff_cursor + 3).min(max);
            } else if app.view == CenterView::Tasks {
                crate::tasks::move_task_cursor(app, 1);
            } else {
                scroll_transcript_lines(app, 3);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let area = app.sessions_area;
            let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
            if let Some(&(_, idx)) = app.tab_areas.iter().find(|(r, _)| r.contains(pos)) {
                if idx < app.session_ids().len() {
                    app.selected = idx;
                    app.selection = None;
                    if app.view == CenterView::Diff {
                        request_selected_diff(app, writer)?;
                    }
                }
                return Ok(());
            }
            if area.width > 0
                && area.height > 2
                && mouse.row < area.y + area.height - 1
                && area.contains(pos)
            {
                let idx = app.sessions_offset + mouse.row.saturating_sub(area.y + 1) as usize;
                if idx < app.session_ids().len() {
                    app.selected = idx;
                    app.selection = None;
                    if app.view == CenterView::Diff {
                        request_selected_diff(app, writer)?;
                    }
                }
                return Ok(());
            }
            if crate::tasks::click_task(app, mouse.column, mouse.row) {
                return Ok(());
            }
            // 逐字稿拖曳選取：只涵蓋中央區塊，不會選到左右側欄。
            if app.view == CenterView::Transcript {
                if let Some(p) = transcript_pos(app, mouse.column, mouse.row, false) {
                    app.selection = Some((p, p));
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) if app.view == CenterView::Transcript => {
            if let (Some(sel), Some(p)) = (
                app.selection,
                transcript_pos(app, mouse.column, mouse.row, true),
            ) {
                app.selection = Some((sel.0, p));
                let ((l0, _), (l1, _)) = normalize_selection((sel.0, p));
                app.status = tr!(
                    "選取第 {from}–{to} 行（放開滑鼠複製）",
                    from = l0 + 1,
                    to = l1 + 1
                );
            }
        }
        MouseEventKind::Up(MouseButton::Left) if app.view == CenterView::Transcript => {
            if let Some(sel) = app.selection {
                let ((l0, c0), (l1, c1)) = normalize_selection(sel);
                if (l0, c0) == (l1, c1) {
                    app.selection = None;
                } else {
                    let text = selection_text(app);
                    if text.is_empty() {
                        app.status = tr!("選取範圍沒有文字");
                    } else {
                        match write_clipboard(&text) {
                            Ok(()) => {
                                let n = text.lines().count().max(1);
                                app.status =
                                    tr!("已複製選取 {n} 行（OSC 52；Shift+拖曳可原生選取）", n = n);
                            }
                            Err(e) => app.status = tr!("複製失敗：{e}", e = e),
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// 把滑鼠座標映射成逐字稿的（絕對渲染行, 顯示欄）；`clamp` 時允許拖到區域外。
pub(crate) fn transcript_pos(
    app: &App,
    column: u16,
    row: u16,
    clamp: bool,
) -> Option<(usize, usize)> {
    let area = app.transcript_area;
    if area.width < 4 || area.height < 4 || app.transcript_total == 0 {
        return None;
    }
    let x0 = area.x + 1;
    let y0 = area.y + 1;
    let w = app.transcript_inner_w.max(1) as u16;
    let h = area.height.saturating_sub(2);
    let inside = column >= x0 && column < x0 + w && row >= y0 && row < y0 + h;
    if !inside && !clamp {
        return None;
    }
    let col = column.saturating_sub(x0) as usize;
    let rel_row = row.saturating_sub(y0) as usize;
    let col = col.min(w.saturating_sub(1) as usize);
    let rel_row = rel_row.min(h.saturating_sub(1) as usize);
    let line = app.transcript_offset.saturating_add(rel_row);
    Some((line.min(app.transcript_total - 1), col))
}

/// 讓選取範圍的起點在終點之前（行優先）。
pub(crate) fn normalize_selection(
    sel: ((usize, usize), (usize, usize)),
) -> ((usize, usize), (usize, usize)) {
    let (a, b) = sel;
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// 指定行在選取範圍內要標亮的顯示欄區間 `[from, to)`。
pub(crate) fn selection_columns(
    sel: ((usize, usize), (usize, usize)),
    line: usize,
    inner_w: usize,
) -> Option<(usize, usize)> {
    let ((l0, c0), (l1, c1)) = normalize_selection(sel);
    if line < l0 || line > l1 {
        return None;
    }
    let from = if line == l0 { c0 } else { 0 };
    let to = if line == l1 {
        (c1 + 1).min(inner_w)
    } else {
        inner_w
    };
    (to > from).then_some((from, to))
}

/// 依顯示欄位標亮一行中的選取區間（保留原本樣式）。
pub(crate) fn highlight_columns(line: &Line<'static>, from: usize, to: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut col = 0usize;
    for span in &line.spans {
        let mut buf = String::new();
        let mut state: Option<bool> = None;
        for ch in span.content.chars() {
            let w = ch.width().unwrap_or(0);
            let selected = if w == 0 {
                state.unwrap_or(false)
            } else {
                col < to && col + w > from
            };
            if let Some(prev) = state {
                if prev != selected {
                    let style = select_style(span.style, prev);
                    spans.push(Span::styled(std::mem::take(&mut buf), style));
                    state = Some(selected);
                }
            } else {
                state = Some(selected);
            }
            buf.push(ch);
            col += w;
        }
        if let Some(selected) = state {
            spans.push(Span::styled(buf, select_style(span.style, selected)));
        }
    }
    Line::from(spans)
}

fn select_style(base: Style, selected: bool) -> Style {
    if selected {
        base.add_modifier(Modifier::REVERSED)
    } else {
        base
    }
}

/// 依顯示欄位切出字串片段（中文等寬字元以 2 欄計）。
pub(crate) fn slice_display(s: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    for ch in s.chars() {
        if col >= to {
            break;
        }
        let w = ch.width().unwrap_or(0);
        if col + w > from {
            out.push(ch);
        }
        col += w;
    }
    out
}

/// 目前滑鼠選取範圍的純文字（只含中央逐字稿，不含側欄與邊框）。
pub(crate) fn selection_text(app: &App) -> String {
    let Some(sel) = app.selection else {
        return String::new();
    };
    let Some(cache) = &app.md_cache else {
        return String::new();
    };
    if cache.lines.is_empty() {
        return String::new();
    }
    let ((l0, c0), (l1, c1)) = normalize_selection(sel);
    let last = l1.min(cache.lines.len() - 1);
    let mut out: Vec<String> = Vec::new();
    for (i, line) in cache.lines.iter().enumerate().take(last + 1).skip(l0) {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let from = if i == l0 { c0 } else { 0 };
        let to = if i == last { c1 + 1 } else { usize::MAX };
        out.push(slice_display(&text, from, to).trim_end().to_string());
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}
