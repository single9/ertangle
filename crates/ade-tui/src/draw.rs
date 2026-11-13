//! 所有繪製函式（版面、標題、sessions、transcript、diff、bus、輸入框）。

use super::*;

pub(crate) fn draw(f: &mut TuiFrame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // 標題、頁籤與成本
            Constraint::Min(6),    // 主區
            Constraint::Length(3), // 輸入
            Constraint::Length(1), // 狀態
        ])
        .split(f.area());

    // 依終端寬度調整三欄比例：寬螢幕給側欄多一點，窄螢幕留給中央。
    let w = f.area().width;
    let layout = effective_layout(app.layout, w);
    app.bus_visible = layout == LayoutMode::Three || app.bus_overlay;
    if app.bus_visible {
        app.bus_seen = app.state.messages.len();
    }
    // 隱藏的欄位要清掉區域，避免滑鼠命中上一次的位置。
    app.sessions_area = Rect::default();
    app.tab_areas.clear();

    draw_header(f, chunks[0], app);

    let (side_l, side_r) = if w >= 140 {
        (34, 42)
    } else if w >= 110 {
        (30, 36)
    } else if w >= 84 {
        (26, 30)
    } else {
        (22, 24)
    };
    let main = chunks[1];
    let mut bus_area = None;
    let center = match layout {
        LayoutMode::Three => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(side_l),
                    Constraint::Min(20),
                    Constraint::Length(side_r),
                ])
                .split(main);
            draw_sessions(f, cols[0], app);
            bus_area = Some(cols[2]);
            cols[1]
        }
        LayoutMode::Two => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(side_l), Constraint::Min(20)])
                .split(main);
            draw_sessions(f, cols[0], app);
            cols[1]
        }
        LayoutMode::Tabs => {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
                .split(main);
            draw_session_tabs(f, rows[0], app);
            rows[1]
        }
    };

    // Bus 被隱藏時，待審批請求必須在別處看得到。
    let center = if layout != LayoutMode::Three && !app.state.pending_permissions.is_empty() {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(center);
        draw_pending_banner(f, rows[0], app);
        rows[1]
    } else {
        center
    };

    match app.view {
        CenterView::Transcript => draw_transcript(f, center, app),
        CenterView::Diff => draw_diff(f, center, app),
        CenterView::Worktrees => draw_worktrees(f, center, app),
    }
    if let Some(a) = bus_area {
        draw_bus(f, a, app);
    } else if app.bus_overlay {
        let ow = (main.width * 2 / 5).max(30).min(main.width);
        let r = Rect::new(main.x + main.width - ow, main.y, ow, main.height);
        f.render_widget(Clear, r);
        draw_bus(f, r, app);
    }
    draw_input(f, chunks[2], app);
    draw_status(f, chunks[3], app);
    match app.mode {
        Mode::Menu => draw_menu(f, chunks[2], app),
        Mode::Command => draw_command_hints(f, chunks[2], app),
        Mode::Pick => draw_picker(f, app),
        _ => {}
    }
}

/// Helix 風格 which-key 浮層：貼在輸入框上方右側，列出下一個可按的鍵。
pub(crate) fn draw_menu(f: &mut TuiFrame, input_area: Rect, app: &App) {
    let Some((title, items)) = crate::menu::menu_items(&app.menu_path) else {
        return;
    };
    let label_w = items
        .iter()
        .map(|i| UnicodeWidthStr::width(i.label))
        .max()
        .unwrap_or(10);
    let w = (label_w + 8)
        .max(title.len() + 6)
        .min(f.area().width as usize) as u16;
    let h = (items.len() as u16 + 2).min(input_area.y);
    let x = f.area().right().saturating_sub(w);
    let area = Rect::new(x, input_area.y.saturating_sub(h), w, h);
    let lines: Vec<Line> = items
        .iter()
        .map(|i| {
            Line::from(vec![
                Span::styled(
                    format!(" {} ", i.key),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(i.label),
            ])
        })
        .collect();
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(format!(" {title} · Backspace 返回 ")),
        ),
        area,
    );
}

/// `:` 命令列的提示浮層：貼在輸入框上方左側，列出符合的命令與用法。
pub(crate) fn draw_command_hints(f: &mut TuiFrame, input_area: Rect, app: &App) {
    let hints = crate::commands::command_hints(&app.input);
    if hints.is_empty() {
        return;
    }
    let shown = hints
        .len()
        .min(input_area.y.saturating_sub(2) as usize)
        .min(12);
    let name_w = hints.iter().map(|(n, _)| n.len()).max().unwrap_or(4);
    let text_w = hints
        .iter()
        .map(|(n, d)| name_w + 2 + UnicodeWidthStr::width(*d) + n.len().saturating_sub(n.len()))
        .max()
        .unwrap_or(20);
    let w = (text_w + 4).min(f.area().width as usize) as u16;
    let h = shown as u16 + 2;
    let area = Rect::new(f.area().x, input_area.y.saturating_sub(h), w, h);
    let lines: Vec<Line> = hints
        .iter()
        .take(shown)
        .map(|(n, d)| {
            Line::from(vec![
                Span::styled(
                    format!(" {n:<name_w$}  "),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(d.to_string(), Style::default().fg(Color::Gray)),
            ])
        })
        .collect();
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" 命令（Tab 補全） "),
        ),
        area,
    );
}

/// 置中的清單選擇器（agent／model），第一列是過濾輸入。
pub(crate) fn draw_picker(f: &mut TuiFrame, app: &mut App) {
    let Some(p) = app.picker.as_mut() else { return };
    p.poll_pending();
    let loading = p.loading;
    let p = &*p;
    let vis = p.visible();
    let w = 56.min(f.area().width);
    let h = (vis.len() as u16 + 4).clamp(6, 18).min(f.area().height);
    let area = Rect::new(
        f.area().x + (f.area().width.saturating_sub(w)) / 2,
        f.area().y + (f.area().height.saturating_sub(h)) / 3,
        w,
        h,
    );
    let mut lines = vec![Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Cyan)),
        Span::raw(p.filter.clone()),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ])];
    let room = h.saturating_sub(3) as usize;
    let start = p.cursor.saturating_sub(room.saturating_sub(1));
    for (i, (label, _)) in vis.iter().enumerate().skip(start).take(room) {
        let sel = i == p.cursor;
        lines.push(Line::from(Span::styled(
            format!("{} {label}", if sel { "▸" } else { " " }),
            if sel {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            },
        )));
    }
    if loading {
        lines.push(Line::from(Span::styled(
            "  載入可用 model 中…",
            Style::default().fg(Color::DarkGray),
        )));
    }
    if vis.is_empty() && !loading {
        lines.push(Line::from(Span::styled(
            if p.filter.is_empty() {
                "（沒有選項）".to_string()
            } else {
                format!("Enter 使用自訂值：{}", p.filter)
            },
            Style::default().fg(Color::DarkGray),
        )));
    }
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(format!(" {} ", p.title)),
        ),
        area,
    );
}

/// 一行待審批提示（Bus 隱藏時使用）：優先顯示目前選取 session 的請求。
pub(crate) fn draw_pending_banner(f: &mut TuiFrame, area: Rect, app: &App) {
    let selected = app.selected_id();
    let pending = &app.state.pending_permissions;
    let p = pending
        .values()
        .find(|p| Some(&p.session) == selected.as_ref())
        .or_else(|| pending.values().next());
    let Some(p) = p else { return };
    let label = app
        .state
        .sessions
        .get(&p.session)
        .map_or_else(|| short(&p.session), |s| s.label.clone());
    let mine = Some(&p.session) == selected.as_ref();
    let more = if pending.len() > 1 {
        format!(" (+{})", pending.len() - 1)
    } else {
        String::new()
    };
    let keys = if mine {
        "[a]llow [A]lways [d]eny"
    } else {
        "(j/k 選取該 session 後 a/d)"
    };
    let text = format!(
        "⏸ {label} 要求 {}：{}{more} {keys}",
        p.title,
        truncate(&p.detail, 40)
    );
    f.render_widget(
        Paragraph::new(Line::styled(
            text,
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        area,
    );
}

/// 上方 session 分頁列：寬度不夠時以選取項為中心水平捲動，兩端顯示 `‹` `›`。
pub(crate) fn draw_session_tabs(f: &mut TuiFrame, area: Rect, app: &mut App) {
    let ids = app.session_ids();
    if ids.is_empty() || area.width == 0 {
        f.render_widget(
            Paragraph::new(Span::styled(
                " （尚無 session，按 n 建立）",
                Style::default().fg(Color::DarkGray),
            )),
            area,
        );
        return;
    }
    let sel = app.selected.min(ids.len() - 1);
    let waiting: Vec<&SessionId> = app
        .state
        .pending_permissions
        .values()
        .map(|p| &p.session)
        .collect();
    // (文字, 樣式)
    let mut items: Vec<(String, Style)> = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let Some(s) = app.state.sessions.get(id) else {
            items.push((String::new(), Style::default()));
            continue;
        };
        let mut style = Style::default().fg(match s.status {
            SessionStatus::Failed => Color::Red,
            SessionStatus::Busy => Color::Green,
            SessionStatus::Sleeping => Color::Blue,
            SessionStatus::Idle => Color::Gray,
            _ => Color::DarkGray,
        });
        if s.status == SessionStatus::WaitingApproval || waiting.contains(&id) {
            style = Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD);
        }
        if i == sel {
            style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
        }
        items.push((format!(" {} {} ", s.status.glyph(), s.label), style));
    }
    let widths: Vec<usize> = items.iter().map(|(t, _)| t.width()).collect();
    let total = area.width as usize;
    let (lo, hi) = if widths.iter().sum::<usize>() <= total {
        (0, ids.len() - 1)
    } else {
        let avail = total.saturating_sub(2);
        let (mut lo, mut hi) = (sel, sel);
        let mut used = widths[sel];
        loop {
            if hi + 1 < widths.len() && used + widths[hi + 1] <= avail {
                hi += 1;
                used += widths[hi];
            } else if lo > 0 && used + widths[lo - 1] <= avail {
                lo -= 1;
                used += widths[lo];
            } else {
                break;
            }
        }
        (lo, hi)
    };
    let overflow = lo > 0 || hi + 1 < ids.len();
    let mut spans: Vec<Span> = Vec::new();
    let mut x = area.x;
    let end = area.x + area.width;
    if overflow {
        spans.push(Span::raw(if lo > 0 { "‹" } else { " " }));
        x += 1;
    }
    let limit = if overflow { end - 1 } else { end };
    for (i, item) in items.iter().enumerate().take(hi + 1).skip(lo) {
        if x >= limit {
            break;
        }
        let mut text = item.0.clone();
        let room = (limit - x) as usize;
        if text.width() > room {
            text = truncate(&text, room);
        }
        let tw = text.width() as u16;
        app.tab_areas.push((Rect::new(x, area.y, tw, 1), i));
        spans.push(Span::styled(text, item.1));
        x += tw;
    }
    if overflow {
        let pad = limit.saturating_sub(x) as usize;
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::raw(if hi + 1 < ids.len() { "›" } else { " " }));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(crate) fn tab(label: &str, active: bool) -> Span<'static> {
    if active {
        Span::styled(
            format!(" [{label}] "),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(format!("  {label}  "), Style::default().fg(Color::DarkGray))
    }
}

pub(crate) fn draw_header(f: &mut TuiFrame, area: Rect, app: &App) {
    let live = app.state.live_sessions().count();
    let cost = app.state.total_cost();

    let tabs = Line::from(vec![
        Span::styled(
            " ade ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        tab("Overview", app.view == CenterView::Transcript),
        tab("Diff (g)", app.view == CenterView::Diff),
        tab("Worktrees", app.view == CenterView::Worktrees),
        Span::raw("   "),
        Span::styled(
            format!("{live}/{} live", app.state.sessions.len()),
            Style::default().fg(Color::Green),
        ),
        Span::styled(
            format!("  · {} tasks", app.state.tasks.len()),
            Style::default().fg(Color::Gray),
        ),
    ]);

    let md_mode = if app.raw_md { "raw" } else { "md" };
    let follow = if app.view == CenterView::Transcript {
        match app.transcript_scroll {
            None => "▼ follow",
            Some(_) => "⏸ paused",
        }
    } else {
        ""
    };
    let stats = Line::from(vec![
        Span::styled("  cost ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("${:.4}", cost.usd),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            format!("  ({} in / {} out)", cost.input_tokens, cost.output_tokens),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("   layout:{}", app.layout.id()),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            if app.unread_bus() > 0 {
                format!("  bus:{} 未讀", app.unread_bus())
            } else {
                String::new()
            },
            Style::default().fg(Color::Magenta),
        ),
        Span::styled(
            format!("   md:{md_mode}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("   think:{}", if app.show_thoughts { "on" } else { "off" }),
            Style::default().fg(if app.show_thoughts {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
        ),
        Span::styled(
            format!("   mouse:{}", if app.mouse_capture { "on" } else { "off" }),
            Style::default().fg(if app.mouse_capture {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
        ),
        Span::styled(
            if follow.is_empty() {
                String::new()
            } else {
                format!("   {follow}")
            },
            Style::default().fg(if app.transcript_scroll.is_none() {
                Color::Green
            } else {
                Color::Yellow
            }),
        ),
        Span::styled(
            if app.state.log_ok { "" } else { "   log ✗" },
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    ]);

    f.render_widget(Paragraph::new(vec![tabs, stats]), area);
}

pub(crate) fn draw_sessions(f: &mut TuiFrame, area: Rect, app: &mut App) {
    app.sessions_area = area;
    let ids = app.session_ids();
    let items: Vec<ListItem> = ids
        .iter()
        .filter_map(|id| app.state.sessions.get(id))
        .map(|s| {
            let color = match s.status {
                SessionStatus::Failed => Color::Red,
                SessionStatus::WaitingApproval => Color::Yellow,
                SessionStatus::Busy => Color::Green,
                SessionStatus::Sleeping => Color::Blue,
                SessionStatus::Idle => Color::Gray,
                _ => Color::DarkGray,
            };
            let branch = s.branch.as_deref().unwrap_or("");
            let note = s.status_note.as_deref().unwrap_or("");
            let mut spans = vec![
                Span::styled(format!("{} ", s.status.glyph()), Style::default().fg(color)),
                Span::raw(s.label.clone()),
                Span::styled(
                    format!(
                        "  [{}{}]{}",
                        s.kind.id(),
                        s.model
                            .as_deref()
                            .map(|m| format!(":{m}"))
                            .unwrap_or_default()
                            + if s.permission == PermissionPolicy::Bypass {
                                " !bypass"
                            } else {
                                ""
                            },
                        if branch.is_empty() {
                            String::new()
                        } else {
                            format!(" @{branch}")
                        }
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            if !note.is_empty() {
                spans.push(Span::styled(
                    format!(" · {}", truncate(note, 16)),
                    Style::default().fg(Color::Cyan),
                ));
            }
            let line = Line::from(spans);
            ListItem::new(line)
        })
        .collect();

    let mut list_state = ListState::default();
    if !ids.is_empty() {
        list_state.select(Some(app.selected.min(ids.len() - 1)));
    }

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Sessions "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ");
    f.render_stateful_widget(list, area, &mut list_state);
    app.sessions_offset = list_state.offset();
}

/// 逐字稿指紋：串流增量會改變末行長度或行數，滿額截斷會改變首行。
pub(crate) fn transcript_stamp(app: &App, id: &SessionId) -> (usize, usize, usize) {
    app.state.transcripts.get(id).map_or((0, 0, 0), |b| {
        (
            b.len(),
            b.front().map_or(0, String::len),
            b.back().map_or(0, String::len),
        )
    })
}

/// 依目前模式（Markdown / 原始）與寬度，把逐字稿建成樣式化且已換行的行。
/// 工具呼叫（`▸ …`）與結果（`  ✓ …`／`  ✗ …`）行：原樣顯示並上色。
fn tool_line(l: &str) -> Option<Line<'static>> {
    let style = if l.starts_with("▸ ") {
        Style::default().fg(Color::Cyan)
    } else if l.starts_with("  ✓") {
        Style::default().fg(Color::DarkGray)
    } else if l.starts_with("  ✗") {
        Style::default().fg(Color::Red)
    } else {
        return None;
    };
    Some(Line::from(Span::styled(l.to_string(), style)))
}

pub(crate) fn build_transcript_lines(
    app: &App,
    id: &SessionId,
    width: usize,
) -> Vec<Line<'static>> {
    let buf = match app.state.transcripts.get(id) {
        Some(b) => b,
        None => return Vec::new(),
    };
    let src: Vec<String> = buf
        .iter()
        .filter(|l| app.show_thoughts || !l.starts_with("💭 "))
        .cloned()
        .collect();
    let lines: Vec<Line<'static>> = if app.raw_md {
        src.iter().map(|l| Line::raw(l.clone())).collect()
    } else {
        // 工具呼叫／結果行是程式產生的摘要，不能當 Markdown：
        // `_`、`*`、反斜線會被吃掉（例如 mcp__ade__ade_peers 變成 mcpadeadepeers）。
        let mut out: Vec<Line<'static>> = Vec::new();
        let mut batch: Vec<String> = Vec::new();
        for l in src {
            if let Some(line) = tool_line(&l) {
                if !batch.is_empty() {
                    out.extend(markdown::Renderer::new().render(&batch, width));
                    batch.clear();
                }
                out.push(line);
            } else {
                batch.push(l);
            }
        }
        if !batch.is_empty() {
            out.extend(markdown::Renderer::new().render(&batch, width));
        }
        out
    };
    markdown::wrap_lines(lines, width)
}

pub(crate) fn draw_transcript(f: &mut TuiFrame, area: Rect, app: &mut App) {
    let selected = app.selected_id();
    // 保留右側一行給捲軸。
    let inner_w = area.width.saturating_sub(3).max(8) as usize;

    let mut need_rebuild = match (&app.md_cache, &selected) {
        (Some(c), Some(id)) => {
            c.session != *id
                || c.raw != app.raw_md
                || c.width != area.width
                || (c.stamp != transcript_stamp(app, id) && {
                    // 只有內容增量：節流，避免串流時每個 delta 都重算整份。
                    let due = app.last_rebuild.elapsed() >= MD_REBUILD_INTERVAL;
                    app.rebuild_pending = !due;
                    due
                })
        }
        _ => true,
    };
    if selected.is_none() && app.md_cache.is_none() {
        need_rebuild = false;
    }
    if need_rebuild {
        match &selected {
            Some(id) => {
                if app.md_cache.as_ref().map(|c| &c.session) != Some(id) {
                    app.transcript_scroll = None; // 換 session 時回到跟隨
                    app.selection = None;
                }
                let lines = build_transcript_lines(app, id, inner_w);
                app.last_rebuild = std::time::Instant::now();
                app.rebuild_pending = false;
                app.md_cache = Some(MdCache {
                    session: id.clone(),
                    stamp: transcript_stamp(app, id),
                    raw: app.raw_md,
                    width: area.width,
                    lines,
                });
            }
            None => app.md_cache = None,
        }
    }

    let viewport = area.height.saturating_sub(2) as usize;
    let total = app.md_cache.as_ref().map_or(0, |c| c.lines.len());
    app.transcript_viewport = viewport;
    app.transcript_total = total;
    app.transcript_area = area;
    app.transcript_inner_w = inner_w;

    let max_off = total.saturating_sub(viewport);
    let off = match app.transcript_scroll {
        Some(o) => {
            let o = o.min(max_off);
            if o >= max_off {
                app.transcript_scroll = None;
            }
            o
        }
        None => max_off,
    };

    let pct = (off * 100)
        .checked_div(max_off)
        .map(|p| p.min(100))
        .unwrap_or(100);
    let title = match &selected {
        Some(id) => {
            let s = &app.state.sessions[id];
            format!(
                " Transcript: {} · {} · {} (${:.4}) · {}% · {} ",
                s.label,
                s.kind.id(),
                if s.transport.is_empty() {
                    "-"
                } else {
                    &s.transport
                },
                s.cost.usd,
                pct,
                if app.raw_md { "raw" } else { "md" },
            )
        }
        None => " (尚無 session：按 n 建立) ".into(),
    };

    let selection = app.selection;
    let mut visible: Vec<Line> = Vec::new();
    if let Some(c) = &app.md_cache {
        for (idx, line) in c.lines.iter().enumerate().skip(off).take(viewport) {
            let line = match selection.and_then(|sel| selection_columns(sel, idx, inner_w)) {
                Some((from, to)) => highlight_columns(line, from, to),
                None => line.clone(),
            };
            visible.push(line);
        }
    }
    if visible.is_empty() {
        visible.push(Line::styled(
            "這裡會顯示 agent 的輸出。按 i 輸入 prompt、n 建立 session、g 切換 Diff 視圖。",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let para = Paragraph::new(visible).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(para, area);
    app.transcript_offset = off;

    if total > viewport {
        let mut sb_state = ScrollbarState::new(max_off).position(off);
        let sb_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        };
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            sb_area,
            &mut sb_state,
        );
    }
}

pub(crate) fn diff_line_style(l: &str) -> Style {
    if l.starts_with("+++")
        || l.starts_with("---")
        || l.starts_with("diff ")
        || l.starts_with("index ")
    {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if l.starts_with("@@") {
        Style::default().fg(Color::Cyan)
    } else if l.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if l.starts_with('-') {
        Style::default().fg(Color::Red)
    } else {
        Style::default()
    }
}

pub(crate) fn draw_diff(f: &mut TuiFrame, area: Rect, app: &App) {
    let title = match app.selected_id() {
        Some(id) => {
            let s = &app.state.sessions[&id];
            format!(
                " Diff: {} ({})  +{} -{}  ·  j/k 選行 · {{/}} hunk · c 註解 · M 合併 · [ ] 換 session ",
                s.label,
                s.branch.as_deref().unwrap_or("main"),
                app.diff_added,
                app.diff_removed,
            )
        }
        None => " Diff ".into(),
    };

    let items: Vec<ListItem> = if app.diff_lines.is_empty() {
        vec![ListItem::new(Line::styled(
            "(尚無 diff 資料，按 r 重新載入)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.diff_lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let (old, new) = app.diff_gutter.get(i).copied().unwrap_or((None, None));
                let g_old = old
                    .map(|n| format!("{n:>4}"))
                    .unwrap_or_else(|| "    ".into());
                let g_new = new
                    .map(|n| format!("{n:>4}"))
                    .unwrap_or_else(|| "    ".into());
                let spans = vec![
                    Span::styled(
                        format!("{g_old} "),
                        Style::default().fg(Color::Rgb(150, 100, 100)),
                    ),
                    Span::styled(
                        format!("{g_new} "),
                        Style::default().fg(Color::Rgb(100, 150, 100)),
                    ),
                    Span::styled("│ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(l.clone(), diff_line_style(l)),
                ];
                ListItem::new(Line::from(spans))
            })
            .collect()
    };

    let mut list_state = ListState::default();
    if !app.diff_lines.is_empty() {
        list_state.select(Some(app.diff_cursor.min(app.diff_lines.len() - 1)));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ");
    f.render_stateful_widget(list, area, &mut list_state);

    let viewport = area.height.saturating_sub(2) as usize;
    let total = app.diff_lines.len();
    if total > viewport {
        let max_off = total - viewport;
        let pos = app.diff_cursor.saturating_sub(viewport / 2).min(max_off);
        let mut sb_state = ScrollbarState::new(max_off).position(pos);
        let sb_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        };
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            sb_area,
            &mut sb_state,
        );
    }
}

pub(crate) fn draw_worktrees(f: &mut TuiFrame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();
    if app.worktrees.is_empty() {
        lines.push(Line::styled(
            "(目前沒有獨立 worktree；建立 session 時開啟 --worktree 或按 Ctrl-W)",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        for wt in &app.worktrees {
            let state_tag = if wt.live { "● live" } else { "✓ exited" };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<20} ", wt.label),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("@{}  ", wt.branch),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(format!("{state_tag}  {}  ({})", wt.summary, wt.path)),
            ]));
        }
    }
    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Worktrees（M 合併選取 session · G 清理已結束 worktree） "),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

pub(crate) fn draw_bus(f: &mut TuiFrame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();

    for p in app.state.pending_permissions.values() {
        lines.push(Line::from(vec![
            Span::styled(
                "⏸ 審批 ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("{}：{}", p.title, truncate(&p.detail, 40))),
        ]));
    }
    if !app.state.pending_permissions.is_empty() {
        lines.push(Line::styled(
            "  [a] 允許  [A] 一律允許  [d] 拒絕",
            Style::default().fg(Color::DarkGray),
        ));
    }

    if !app.state.tasks.is_empty() {
        for t in app.state.tasks.values() {
            let color = match t.status {
                ade_core::TaskStatus::Done => Color::Green,
                ade_core::TaskStatus::Doing | ade_core::TaskStatus::Review => Color::Cyan,
                ade_core::TaskStatus::Blocked => Color::Red,
                ade_core::TaskStatus::Todo => Color::Gray,
            };
            lines.push(Line::from(vec![
                Span::styled(format!("[{}] ", t.status.id()), Style::default().fg(color)),
                Span::raw(truncate(&t.title, 26)),
            ]));
        }
        lines.push(Line::styled(
            "────────────────────────",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let h = area.height.saturating_sub(2 + lines.len() as u16) as usize;
    let msgs = &app.state.messages;
    let start = msgs.len().saturating_sub(h);
    for m in msgs.iter().skip(start) {
        let to = match &m.to {
            Target::Direct(s) => format!("@{s}"),
            Target::Channel(c) => format!("#{c}"),
            Target::Broadcast => "*".into(),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}→{} ", short(&m.from), short(&to)),
                Style::default().fg(Color::Magenta),
            ),
            Span::raw(truncate(&m.body, 60)),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "agent 之間的訊息與任務看板會出現在這裡。",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Bus / Tasks "),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

pub(crate) fn draw_input(f: &mut TuiFrame, area: Rect, app: &App) {
    let title = match app.mode {
        Mode::Normal => {
            " 輸入（Space=選單, :=命令, i=prompt/@peer/#ch, n=新增 agent, Ctrl-K=跳轉, g=Diff, t=原始/渲染, T=思考, PgUp/PgDn=捲動, F2=滑鼠） "
                .to_string()
        }
        Mode::Prompt => " prompt（@檔案/圖片 附件、Tab 補全、Ctrl-V 貼剪貼簿圖片；@peer／#channel 傳訊息；Enter 送出） ".to_string(),
        Mode::Spawn => match &app.spawn_target {
            Some((kind, model)) => format!(
                " spawn → {}{}{}：prompt（Ctrl-W worktree={}, Ctrl-E 權限={}, Ctrl-F effort, Enter 送出） ",
                kind.id(),
                model.as_deref().map(|m| format!(":{m}")).unwrap_or_default(),
                app.spawn_effort.as_deref().map(|e| format!(" effort={e}")).unwrap_or_default(),
                if app.spawn_worktree { "on" } else { "off" },
                app.spawn_permission.map_or("預設", |p| p.id()),
            ),
            None => " spawn：<agent[:model]> <prompt>（Ctrl-W worktree, Ctrl-E 權限, Enter 送出） "
                .to_string(),
        },
        Mode::Menu => " Space 選單（Esc 關閉） ".to_string(),
        Mode::Pick => " 選擇中（↑↓ 移動、輸入過濾、Enter 確認、Esc 取消） ".to_string(),
        Mode::Message => " message → 選取的 session（Enter 送出） ".to_string(),
        Mode::Review => " review 註解 → 送回選取的 session 修改（Enter 送出） ".to_string(),
        Mode::Command => " : 命令（Tab 補全；spawn / rm / clean / kill / merge / diff / gc / task …） ".to_string(),
        Mode::Jump => " Jump Anywhere：輸入關鍵字跳轉 session（Enter 確認） ".to_string(),
    };
    let style = if app.mode == Mode::Normal {
        Style::default()
    } else {
        Style::default().fg(Color::Cyan)
    };

    // 水平捲動：讓游標永遠留在可見範圍內（支援中文等寬字元）。
    let inner_w = area.width.saturating_sub(2) as usize;
    let chars: Vec<char> = app.input.chars().collect();
    let cursor = app.input_cursor.min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    let mut cursor_col = UnicodeWidthStr::width(before.as_str());
    let mut start = 0usize;
    while inner_w > 0 && cursor_col >= inner_w && start < cursor {
        let w = chars[start].width().unwrap_or(0).max(1);
        cursor_col = cursor_col.saturating_sub(w);
        start += 1;
    }
    let visible: String = chars[start..].iter().collect();

    let para = Paragraph::new(visible)
        .style(style)
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(para, area);

    // 把終端游標放到輸入框內，IME 的 preedit 就會錨定在這裡。
    if area.width > 2 && area.height > 2 {
        let x = area
            .x
            .saturating_add(1)
            .saturating_add(cursor_col as u16)
            .min(area.x + area.width.saturating_sub(2));
        f.set_cursor_position((x, area.y + 1));
    }
}

pub(crate) fn draw_status(f: &mut TuiFrame, area: Rect, app: &App) {
    let hints = match app.view {
        CenterView::Transcript => {
            "  q 離開(保留 daemon)  Q 關閉 daemon  Tab 切換  i prompt  n spawn  t 原始/渲染  T 思考  PgUp/PgDn 捲動  / 命令"
        }
        CenterView::Diff => {
            "  q 離開  Q 關閉 daemon  j/k 選行  {/} hunk  c 審查註解  M 合併  [ ] 換 session  r 重整"
        }
        CenterView::Worktrees => "  q 離開  Q 關閉 daemon  M 合併選取  G 清理已結束  r 重整",
    };
    let text = Line::from(vec![
        Span::styled(
            format!(" {} ", app.status),
            Style::default().fg(Color::Black).bg(Color::DarkGray),
        ),
        Span::raw(hints),
    ]);
    f.render_widget(Paragraph::new(text), area);
}
