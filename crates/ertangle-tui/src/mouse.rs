//! 滑鼠事件處理與逐字稿區塊選取。

use anyhow::Result;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tr::tr;
use unicode_width::UnicodeWidthChar;

use crate::app::{scroll_transcript_lines, App, CenterView, Mode};
use crate::clipboard::write_clipboard;
use crate::hosts::HostRouter;
use crate::request_selected_diff;

pub(crate) fn handle_mouse(
    app: &mut App,
    writer: &mut HostRouter,
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
            if app.quota_area.contains(pos) {
                crate::quota::open(app);
                return Ok(());
            }
            // 輸入框：`Normal` 模式點一下即進入輸入模式；其他輸入模式可拖曳選取複製。
            if app.input_area.contains(pos) {
                if app.mode == Mode::Normal {
                    app.start_prompt();
                    app.status = tr!("輸入模式：Enter 送出 · Esc 取消 · 可拖曳選取複製");
                    return Ok(());
                }
                if is_input_mode(app.mode) {
                    if let Some(idx) = input_char_index(app, mouse.column, mouse.row, false) {
                        app.input_cursor = idx;
                        app.input_sel = Some((idx, idx));
                        app.selection = None;
                    }
                    return Ok(());
                }
            }
            if let Some(&(_, idx)) = app.tab_areas.iter().find(|(r, _)| r.contains(pos)) {
                if idx < app.session_ids().len() {
                    app.selected = idx;
                    app.sync_active_host_from_selection();
                    writer.set_active(&app.active_host);
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
                // 每一筆 session 佔一行；`ListState.offset` 是筆數 (此處等於列數)。
                let rel_row = mouse.row.saturating_sub(area.y + 1);
                let idx = app.sessions_offset + (rel_row / crate::SESSION_ITEM_ROWS) as usize;
                let visible = app.session_ids().len();
                let has_fold = app.ended_count() > 0;
                let fold_row = app.ended_fold_row(visible);
                if has_fold && idx == fold_row {
                    crate::app::toggle_ended(app, None);
                    return Ok(());
                }
                // Rows below an expanded fold header map back to the session index minus one.
                let idx = if has_fold && app.show_ended && idx > fold_row {
                    idx - 1
                } else {
                    idx
                };
                if idx < visible {
                    app.selected = idx;
                    app.sidebar_focus = None;
                    app.sync_active_host_from_selection();
                    writer.set_active(&app.active_host);
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
            if app.view == CenterView::Transcript
                && mouse.row == app.transcript_area.bottom().saturating_sub(1)
                && mouse.column >= app.transcript_area.x
                && mouse.column < app.transcript_area.right()
            {
                let has_bg = app
                    .selected_id()
                    .and_then(|id| app.state.sessions.get(&id))
                    .is_some_and(|s| !s.bg_procs.is_empty());
                if has_bg {
                    crate::ps::open(app);
                    return Ok(());
                }
            }
            // 逐字稿拖曳選取：只涵蓋中央區塊，不會選到左右側欄。
            if app.view == CenterView::Transcript {
                if let Some(p) = transcript_pos(app, mouse.column, mouse.row, false) {
                    app.selection = Some((p, p));
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) if app.input_sel.is_some() => {
            if let Some(idx) = input_char_index(app, mouse.column, mouse.row, true) {
                if let Some((anchor, _)) = app.input_sel {
                    app.input_sel = Some((anchor, idx));
                    let n = crate::inputbox::selected_text(&app.input, app.input_sel)
                        .chars()
                        .count();
                    if n > 0 {
                        app.status = tr!("已選取 {n} 個字元 (放開滑鼠複製)", n = n);
                    }
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) if app.input_sel.is_some() => {
            let had_selection = app.input_sel.is_some_and(|(a, h)| a != h);
            let sel = app.input_sel.take();
            let text = crate::inputbox::selected_text(&app.input, sel);
            if !had_selection || text.is_empty() {
                return Ok(());
            }
            match write_clipboard(&text) {
                Ok(backend) => {
                    app.status = tr!(
                        "已複製輸入框選取內容 ({n} 個字元 · {backend})",
                        n = text.chars().count(),
                        backend = backend
                    );
                }
                Err(e) => app.status = tr!("複製失敗：{e}", e = e),
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
                    "選取第 {from}–{to} 行 (放開滑鼠複製)",
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
                    if open_url_at(app, l0, c0) {
                        return Ok(());
                    }
                } else {
                    let text = selection_text(app);
                    if text.is_empty() {
                        app.status = tr!("選取範圍沒有文字");
                    } else {
                        match write_clipboard(&text) {
                            Ok(backend) => {
                                let n = text.lines().count().max(1);
                                app.status =
                                    tr!("已複製選取 {n} 行 ({backend})", n = n, backend = backend);
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

/// 這些模式會在輸入框裡編輯文字 (點擊可移動游標、拖曳可選取複製)。
pub(crate) fn is_input_mode(mode: Mode) -> bool {
    matches!(
        mode,
        Mode::Prompt
            | Mode::Spawn
            | Mode::Message
            | Mode::Review
            | Mode::Command
            | Mode::Jump
            | Mode::Search
    )
}

/// 把輸入框上的滑鼠座標映射成字元索引；`clamp` 時允許拖到區域外。
pub(crate) fn input_char_index(app: &App, column: u16, row: u16, clamp: bool) -> Option<usize> {
    let area = app.input_area;
    if area.width < 3 || area.height < 3 {
        return None;
    }
    let x0 = area.x + 1;
    let y0 = area.y + 1;
    let w = area.width.saturating_sub(2);
    let h = area.height.saturating_sub(2);
    let inside = column >= x0 && column < x0 + w && row >= y0 && row < y0 + h;
    if !inside && !clamp {
        return None;
    }
    let col = column.saturating_sub(x0).min(w - 1) as usize;
    let rel = row.saturating_sub(y0).min(h - 1) as usize;
    let rows = crate::inputbox::rows(&app.input, w as usize);
    let (cur_row, _) = crate::inputbox::cursor_pos(&app.input, &rows, app.input_cursor);
    // 與 `draw_input` 相同的可見視窗起點。
    let first = (cur_row + 1)
        .saturating_sub(h as usize)
        .min(rows.len().saturating_sub(h as usize));
    let target = (first + rel).min(rows.len().saturating_sub(1));
    Some(crate::inputbox::index_at(&app.input, &rows, target, col))
}

/// 把滑鼠座標映射成逐字稿的 (絕對渲染行, 顯示欄)；`clamp` 時允許拖到區域外。
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

/// 讓選取範圍的起點在終點之前 (行優先)。
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

/// 依顯示欄位標亮一行中的選取區間 (保留原本樣式)。
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

/// 依顯示欄位切出字串片段 (中文等寬字元以 2 欄計)。
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

/// 目前滑鼠選取範圍的純文字 (只含中央逐字稿，不含側欄與邊框)。
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

fn open_url_at(app: &mut App, line: usize, col: usize) -> bool {
    let Some(cache) = &app.md_cache else {
        return false;
    };
    // 1. 優先精準比對 Markdown 連結 (標籤文字覆蓋範圍直接可點)
    if let Some(target) = cache
        .links
        .iter()
        .find(|l| l.line == line && col >= l.col_start && col < l.col_end)
    {
        return match open_external(&target.url) {
            Ok(()) => {
                app.status = tr!("已開啟連結：{url}", url = target.url);
                true
            }
            Err(e) => {
                app.status = tr!("無法開啟連結：{e}", e = e);
                true
            }
        };
    }

    // 2. 備用語法掃描純文字中的網址
    let Some(rendered) = cache.lines.get(line) else {
        return false;
    };
    let text: String = rendered.spans.iter().map(|s| s.content.as_ref()).collect();
    let Some(url) = url_at_display_col(&text, col) else {
        return false;
    };
    match open_external(&url) {
        Ok(()) => {
            app.status = tr!("已開啟連結：{url}", url = url);
            true
        }
        Err(e) => {
            app.status = tr!("無法開啟連結：{e}", e = e);
            true
        }
    }
}

/// 找出覆蓋 `col` (顯示欄位)的 http(s) 或 file URL。
fn url_at_display_col(text: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 7 < chars.len() {
        let s: String = chars[i..].iter().take(8).collect();
        if s.starts_with("https://") || s.starts_with("http://") || s.starts_with("file://") {
            starts.push(i);
        }
        i += 1;
    }
    for start in starts {
        let mut end = start;
        while end < chars.len() {
            let ch = chars[end];
            if ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | '`') {
                break;
            }
            end += 1;
        }
        while end > start {
            let ch = chars[end - 1];
            if matches!(
                ch,
                '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\'' | '"'
            ) {
                // 括號成對時保留右括號 (常見於 Wikipedia 連結)。
                if matches!(ch, ')' | ']' | '}') {
                    let open = match ch {
                        ')' => '(',
                        ']' => '[',
                        '}' => '{',
                        _ => ' ',
                    };
                    let closes = chars[start..end].iter().filter(|&&c| c == ch).count();
                    let opens = chars[start..end].iter().filter(|&&c| c == open).count();
                    if closes <= opens {
                        break;
                    }
                }
                end -= 1;
            } else {
                break;
            }
        }
        if start >= end {
            continue;
        }
        let mut dcol = 0usize;
        let mut from = 0usize;
        let mut to = 0usize;
        for (idx, &ch) in chars.iter().enumerate() {
            if idx == start {
                from = dcol;
            }
            dcol += ch.width().unwrap_or(0);
            if idx + 1 == end {
                to = dcol;
                break;
            }
        }
        if to == 0 {
            // URL 到行尾
            let mut dc = 0usize;
            for (idx, &ch) in chars.iter().enumerate() {
                if idx == start {
                    from = dc;
                }
                dc += ch.width().unwrap_or(0);
            }
            to = dc;
        }
        if col >= from && col < to {
            return Some(chars[start..end].iter().collect());
        }
    }
    None
}

fn open_external(url: &str) -> std::io::Result<()> {
    let clean_target = if let Some(stripped) = url.strip_prefix("file://") {
        let path = stripped.split('#').next().unwrap_or(stripped);
        path.to_string()
    } else {
        url.to_string()
    };

    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");

    cmd.arg(&clean_target);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd.spawn().map(|_| ())
}
