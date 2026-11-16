//! 所有繪製函式 (版面、標題、sessions、transcript、diff、bus、輸入框)。

use super::*;
use tr::tr;

pub(crate) fn draw(f: &mut TuiFrame, app: &mut App) {
    app.input_inner_w = f.area().width.saturating_sub(2) as usize;
    let input_h = crate::inputbox::height_for(&app.input, f.area().width)
        .min(f.area().height.saturating_sub(10).max(3));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),       // 標題、頁籤與成本
            Constraint::Min(6),          // 主區
            Constraint::Length(input_h), // 輸入
            Constraint::Length(1),       // 狀態
        ])
        .split(f.area());

    // 依終端機寬度調整三欄比例：寬螢幕給側欄多一點，窄螢幕留給中央。
    let w = f.area().width;
    let layout = effective_layout(app.layout, w);
    app.bus_visible = layout == LayoutMode::Three || app.bus_overlay;
    if app.bus_visible {
        app.bus_seen = app.state.messages.len();
    }
    // 隱藏的欄位要清掉區域，避免滑鼠命中上一次的位置。
    app.sessions_area = Rect::default();
    app.tab_areas.clear();
    app.tasks_area = Rect::default();
    app.task_rows.clear();

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

    // Bus 被隱藏時，待核准請求必須在別處看得到。
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
        CenterView::Tasks => crate::tasks::draw_tasks(f, center, app),
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
        Mode::Prompt | Mode::Spawn | Mode::Message => draw_file_suggestions(f, chunks[2], app),
        Mode::Pick => draw_picker(f, app),
        Mode::Help => draw_help(f, app),
        Mode::Settings => draw_settings(f, app),
        Mode::Qr => draw_qr_modal(f, app),
        _ => {}
    }
}

/// Helix 風格 which-key 浮層：貼在輸入框上方右側，列出下一個可按的鍵。
pub(crate) fn draw_menu(f: &mut TuiFrame, input_area: Rect, app: &App) {
    let Some((title, items)) = crate::menu::menu_items(&app.menu_path) else {
        return;
    };
    let labels: Vec<String> = items
        .iter()
        .map(|i| termade_core::tr_dyn!(i.label))
        .collect();
    let label_w = labels
        .iter()
        .map(|l| UnicodeWidthStr::width(l.as_str()))
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
        .zip(labels)
        .map(|(i, label)| {
            Line::from(vec![
                Span::styled(
                    format!(" {} ", i.key),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(label),
            ])
        })
        .collect();
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(format!(" {title} ")),
        ),
        area,
    );
}

/// `:` 命令列的提示浮層：貼在輸入框上方左側，列出符合的命令與用法。
pub(crate) fn draw_command_hints(f: &mut TuiFrame, input_area: Rect, app: &App) {
    let hints = crate::commands::hints_with_native(app);
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
        .map(|(_, d)| name_w + 2 + UnicodeWidthStr::width(d.as_str()))
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
                Span::styled(d.clone(), Style::default().fg(Color::Gray)),
            ])
        })
        .collect();
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(tr!(" 命令 (Tab 補全) ")),
        ),
        area,
    );
}

/// `@` 檔案建議浮層：貼在輸入框上方左側，列出專案內模糊搜尋到的檔案。
pub(crate) fn draw_file_suggestions(f: &mut TuiFrame, input_area: Rect, app: &App) {
    let Some((_, cands)) = crate::input::at_candidates(app) else {
        return;
    };
    let sel = if app.at_sel.0
        == crate::attach::current_at_token(&app.input, app.input_cursor)
            .map(|(_, t)| t)
            .unwrap_or_default()
    {
        app.at_sel.1.min(cands.len() - 1)
    } else {
        0
    };
    let text_w = cands
        .iter()
        .map(|c| UnicodeWidthStr::width(c.as_str()))
        .max()
        .unwrap_or(10);
    let w = (text_w + 6).min(f.area().width as usize).max(38) as u16;
    let h = (cands.len() as u16 + 2).min(input_area.y);
    let area = Rect::new(f.area().x, input_area.y.saturating_sub(h), w, h);
    let lines: Vec<Line> = cands
        .iter()
        .enumerate()
        .map(|(i, c)| {
            Line::from(Span::styled(
                format!("{} {c}", if i == sel { "▸" } else { " " }),
                if i == sel {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default()
                },
            ))
        })
        .collect();
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(tr!(" 專案檔案 (Tab 採用) ")),
        ),
        area,
    );
}

/// 說明面板：置中的大浮層，可捲動。
pub(crate) fn draw_help(f: &mut TuiFrame, app: &App) {
    let full = f.area();
    let w = full.width.saturating_sub(6).clamp(40, 110);
    let h = full.height.saturating_sub(4).max(8);
    let area = Rect::new(
        full.x + full.width.saturating_sub(w) / 2,
        full.y + full.height.saturating_sub(h) / 2,
        w.min(full.width),
        h.min(full.height),
    );
    let key_w = crate::help::help_lines()
        .iter()
        .filter(|(title, _, _)| !title)
        .map(|(_, k, _)| UnicodeWidthStr::width(k.as_str()))
        .max()
        .unwrap_or(10)
        .min(18);
    let lines: Vec<Line> = crate::help::help_lines()
        .into_iter()
        .skip(app.help_scroll)
        .map(|(is_title, k, d)| {
            if is_title {
                Line::from(Span::styled(
                    format!(" ▎{k}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            } else if k.is_empty() {
                Line::raw("")
            } else {
                let pad = key_w.saturating_sub(UnicodeWidthStr::width(k.as_str()));
                Line::from(vec![
                    Span::styled(
                        format!("   {k}{}  ", " ".repeat(pad)),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw(d),
                ])
            }
        })
        .collect();
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(tr!(" 說明 (Esc 關閉) ")),
        ),
        area,
    );
}

/// 設定面板：每列一個設定，目前值用 ◀ ▶ 包起來；選取列反相顯示。
pub(crate) fn draw_settings(f: &mut TuiFrame, app: &App) {
    let Some(s) = app.settings.as_ref() else {
        return;
    };
    let full = f.area();
    let w = full.width.saturating_sub(6).clamp(40, 100);
    let h = (crate::settings::ITEMS.len() as u16 + 5).min(full.height);
    let area = Rect::new(
        full.x + full.width.saturating_sub(w) / 2,
        full.y + full.height.saturating_sub(h) / 2,
        w.min(full.width),
        h,
    );
    let label_w = crate::settings::ITEMS
        .iter()
        .map(|i| UnicodeWidthStr::width(i.label(s).as_str()))
        .max()
        .unwrap_or(20);
    let mut lines: Vec<Line> = crate::settings::ITEMS
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let label = item.label(s);
            let pad = label_w.saturating_sub(UnicodeWidthStr::width(label.as_str()));
            let value = item.display(s);
            let value = if item.is_toggle() {
                format!("[{value}]")
            } else {
                format!("{value} ▾")
            };
            let sel = i == s.cursor;
            let base = if sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(format!(" {label}{}  ", " ".repeat(pad)), base),
                Span::styled(value, base.fg(Color::Yellow)),
            ])
        })
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        tr!(" ↑↓ 選項目 · Enter 開清單選擇 (開關直接切換)· ←→ 輪替 · Esc 關閉　變更立即儲存"),
        Style::default().fg(Color::Gray),
    )));
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(tr!(" 設定 (寫入全域 config.toml) ")),
        ),
        area,
    );
    if let Some(sub) = &s.sub {
        draw_settings_sub(f, s, sub, area);
    }
}

/// 設定值的選擇清單：疊在設定面板上，第一列是過濾輸入。
fn draw_settings_sub(
    f: &mut TuiFrame,
    s: &crate::settings::Settings,
    sub: &crate::settings::Sub,
    panel: Rect,
) {
    let item = crate::settings::ITEMS[s.cursor];
    let all = s.sub_visible();
    let w = panel.width.saturating_sub(8).clamp(30, 80);
    let h = (all.len() as u16 + 3)
        .clamp(5, panel.height.max(5))
        .min(f.area().height);
    let area = Rect::new(
        panel.x + panel.width.saturating_sub(w) / 2,
        panel.y + 1,
        w.min(f.area().width),
        h,
    );
    let rows = h.saturating_sub(3) as usize;
    let top = sub.cursor.saturating_sub(rows.saturating_sub(1));
    let cur = item.display(s);
    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::styled(tr!(" 過濾："), Style::default().fg(Color::Gray)),
        Span::raw(sub.filter.clone()),
        Span::styled("▏", Style::default().fg(Color::Yellow)),
    ])];
    if all.is_empty() {
        lines.push(Line::from(Span::styled(
            tr!(" (沒有符合的項目)"),
            Style::default().fg(Color::Gray),
        )));
    }
    for (i, (_, label)) in all.iter().enumerate().skip(top).take(rows) {
        let mark = if *label == cur { "●" } else { " " };
        let style = if i == sub.cursor {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(format!(" {mark} {label}"), style)));
    }
    let loading = item == crate::settings::Item::Model && s.loading_models_pub();
    let title = tr!(
        " {item}{loading} · Enter 選定 · Esc 返回 ",
        item = item.label(s),
        loading = if loading {
            tr!("(載入中…)")
        } else {
            String::new()
        }
    );
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(title),
        ),
        area,
    );
}

/// 繪製 Web 版 QR Code 彈窗。
pub(crate) fn draw_qr_modal(f: &mut TuiFrame, app: &App) {
    let Some(modal) = app.qr_modal.as_ref() else {
        return;
    };
    let full = f.area();
    let qr_w = modal
        .qr_lines
        .first()
        .map_or(30, |l| UnicodeWidthStr::width(l.as_str())) as u16;
    let url_w = UnicodeWidthStr::width(modal.url.as_str()) as u16;
    let w = qr_w
        .max(url_w)
        .saturating_add(6)
        .clamp(36, 90)
        .min(full.width);
    let h = (modal.qr_lines.len() as u16 + 6).min(full.height);
    let area = Rect::new(
        full.x + full.width.saturating_sub(w) / 2,
        full.y + full.height.saturating_sub(h) / 2,
        w,
        h,
    );

    f.render_widget(ratatui::widgets::Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            format!(" {} ", modal.title),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(tr!("連線網址："), Style::default().fg(Color::Gray)),
        Span::styled(
            modal.url.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::UNDERLINED),
        ),
    ]));
    lines.push(Line::from(""));
    for line in &modal.qr_lines {
        lines.push(Line::from(Span::styled(
            line.clone(),
            Style::default().fg(Color::White),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        tr!("按 Esc、Enter、Space 或 q 關閉"),
        Style::default().fg(Color::DarkGray),
    )));

    let para = Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center);
    f.render_widget(para, inner);
}

/// 置中的清單選擇器 (agent／model)，第一列是過濾輸入。
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
            tr!("  載入可用 model 中…"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    if vis.is_empty() && !loading {
        lines.push(Line::from(Span::styled(
            if p.filter.is_empty() {
                tr!("(沒有選項)")
            } else {
                tr!("Enter 使用自訂值：{value}", value = p.filter)
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

/// 一行待核准提示 (Bus 隱藏時使用)：優先顯示目前選取 session 的請求。
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
        tr!("[a]llow [A]lways [d]eny")
    } else {
        tr!("(j/k 選取該 session 後 a/d)")
    };
    let text = tr!(
        "⏸ {label} 要求 {title}：{detail}{more} {keys}",
        label = label,
        title = p.title,
        detail = truncate(&p.detail, 40),
        more = more,
        keys = keys
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
                tr!(" (尚無 session，按 n 建立)"),
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
    let vis = app.visible_sessions();
    let multi_host = app.has_multiple_hosts();
    // (文字, 樣式)
    let mut items: Vec<(String, Style)> = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let s_opt = vis
            .get(i)
            .and_then(|(h, _)| app.hosts.get(h))
            .and_then(|h| h.state.sessions.get(id))
            .or_else(|| app.state.sessions.get(id));
        let Some(s) = s_opt else {
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
        let label = if multi_host {
            let hname = vis.get(i).map(|(h, _)| h.as_str()).unwrap_or("");
            format!(" {} [{hname}] {} ", s.status.glyph(), s.label)
        } else {
            format!(" {} {} ", s.status.glyph(), s.label)
        };
        items.push((label, style));
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

/// 標題列的額度摘要：每個 agent 一段，用量 ≥70% 變黃、≥90% 或已滿變紅。
fn quota_span(app: &App) -> Span<'static> {
    let now = termade_core::now_millis();
    let mut text = String::new();
    let mut worst = 0.0f32;
    let mut reached = false;
    for q in app.state.quotas.values() {
        // 已過重置時間的視窗不再算數。
        let live: Vec<_> = q
            .windows
            .iter()
            .filter(|w| w.resets_at_ms.is_none_or(|r| r > now))
            .collect();
        if live.is_empty() {
            continue;
        }
        let used = live.iter().map(|w| w.used_percent).fold(0.0, f32::max);
        worst = worst.max(used);
        reached |= q.reached;
        let w = live
            .iter()
            .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
            .unwrap();
        text.push_str(&format!("  {}:{} {:.0}%", q.agent, w.name, w.used_percent));
    }
    let color = if reached || worst >= 90.0 {
        Color::Red
    } else if worst >= 70.0 {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    Span::styled(text, Style::default().fg(color))
}

pub(crate) fn draw_header(f: &mut TuiFrame, area: Rect, app: &App) {
    let live = app.state.live_sessions().count();
    let cost = app.state.total_cost();

    let mut tab_spans = vec![
        Span::styled(
            " termade ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", project_name(&app.home_project, &[])),
            Style::default()
                .fg(Color::Black)
                .bg(project_color(&app.home_project)),
        ),
        tab("Overview", app.view == CenterView::Transcript),
        tab("Diff (g)", app.view == CenterView::Diff),
        tab("Worktrees", app.view == CenterView::Worktrees),
        tab("Tasks", app.view == CenterView::Tasks),
        Span::raw("   "),
        Span::styled(
            format!("{live}/{} live", app.state.sessions.len()),
            Style::default().fg(Color::Green),
        ),
        Span::styled(
            format!("  · {} tasks", crate::tasks::open_task_count(app)),
            Style::default().fg(Color::Gray),
        ),
    ];

    if app.has_multiple_hosts() {
        tab_spans.push(Span::raw("  "));
        for (hname, hdata) in &app.hosts {
            let (sym, color) = match hdata.status {
                crate::hosts::HostStatus::Connected => ("●", Color::Green),
                crate::hosts::HostStatus::Reconnecting(_) => ("◌", Color::Yellow),
                crate::hosts::HostStatus::Disconnected => ("◌", Color::DarkGray),
            };
            let is_filtered = app.host_filter.as_deref() == Some(hname.as_str());
            let name_str = if is_filtered {
                format!(" [{hname}]")
            } else {
                format!(" {hname}")
            };
            tab_spans.push(Span::styled(format!(" {sym}"), Style::default().fg(color)));
            tab_spans.push(Span::styled(
                name_str,
                Style::default().fg(if is_filtered {
                    Color::Cyan
                } else {
                    Color::White
                }),
            ));
        }
    }

    let tabs = Line::from(tab_spans);

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
        quota_span(app),
        Span::styled(
            if app.unread_bus() > 0 {
                tr!("  bus:{n} 未讀", n = app.unread_bus())
            } else {
                String::new()
            },
            Style::default().fg(Color::Magenta),
        ),
        // 只顯示偏離預設的狀態，預設值不佔版面。
        Span::styled(
            [
                app.raw_md.then(|| "raw".to_string()),
                app.show_thoughts.then(|| tr!("思考")),
                app.show_tools.then(|| tr!("工具展開")),
                (!app.mouse_capture).then(|| tr!("滑鼠關")),
                (app.view == CenterView::Transcript && app.transcript_scroll.is_some())
                    .then(|| tr!("⏸暫停跟隨")),
            ]
            .into_iter()
            .flatten()
            .map(|t| format!("  [{t}]"))
            .collect::<String>(),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            if app.state.log_ok { "" } else { "   log ✗" },
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    ]);

    f.render_widget(Paragraph::new(vec![tabs, stats]), area);
}

/// Sessions 清單每一筆固定佔用的列數 (第一行標籤 + 第二行次要資訊)。
/// 滑鼠點擊換算索引時必須用這個值，否則會把第二行算成下一筆。
pub(crate) const SESSION_ITEM_ROWS: u16 = 2;

pub(crate) fn draw_sessions(f: &mut TuiFrame, area: Rect, app: &mut App) {
    app.sessions_area = area;
    let vis = app.visible_sessions();
    let multi_host = app.has_multiple_hosts();
    let sessions: Vec<(String, &termade_core::Session)> = vis
        .iter()
        .filter_map(|(host, id)| {
            app.hosts
                .get(host)
                .and_then(|h| h.state.sessions.get(id))
                .or_else(|| app.state.sessions.get(id))
                .map(|s| (host.clone(), s))
        })
        .collect();

    let roots: Vec<&str> = {
        let mut r: Vec<&str> = sessions.iter().map(|(_, s)| s.project_root()).collect();
        r.sort_unstable();
        r.dedup();
        r
    };
    let multi_project = roots.len() > 1;
    let show_project = multi_project || !app.scope_here;
    let total_sessions = if multi_host {
        app.hosts.values().map(|h| h.state.sessions.len()).sum()
    } else {
        app.state.sessions.len()
    };
    let hidden = total_sessions.saturating_sub(sessions.len());
    let items: Vec<ListItem> = sessions
        .iter()
        .map(|(host_name, s)| {
            let color = match s.status {
                SessionStatus::Failed => Color::Red,
                SessionStatus::WaitingApproval => Color::Yellow,
                SessionStatus::Busy => Color::Green,
                SessionStatus::Sleeping => Color::Blue,
                SessionStatus::Idle => Color::Gray,
                _ => Color::DarkGray,
            };
            let depth = app.session_tree_depth(host_name, &s.id);
            let indent = "  ".repeat(depth.min(6));
            let child_prefix = if depth > 0 { "↳ " } else { "" };
            let branch = s.branch.as_deref().unwrap_or("");
            let note = s.status_note.as_deref().unwrap_or("");

            // 第一行：階層 + 狀態 + 標籤 (精簡)，子 session/agent 可一眼看出。
            let mut line1: Vec<Span> = Vec::new();
            if !indent.is_empty() {
                line1.push(Span::styled(
                    indent.clone(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            if show_project && depth == 0 {
                line1.push(Span::styled(
                    format!(
                        " {} ",
                        truncate(&project_name(s.project_root(), &roots), 10)
                    ),
                    Style::default()
                        .fg(Color::Black)
                        .bg(project_color(s.project_root())),
                ));
                line1.push(Span::raw(" "));
            }
            if depth > 0 {
                line1.push(Span::styled(
                    child_prefix,
                    Style::default().fg(Color::DarkGray),
                ));
            }
            line1.push(Span::styled(
                format!("{} ", s.status.glyph()),
                Style::default().fg(color),
            ));
            if multi_host {
                line1.push(Span::styled(
                    format!("[{host_name}] "),
                    Style::default().fg(Color::Cyan),
                ));
            }
            line1.push(Span::raw(truncate(&s.label, 24)));
            if s.role != termade_core::Role::Worker || s.parent.is_some() {
                line1.push(Span::styled(
                    format!(" [{}]", s.role.id()),
                    Style::default().fg(Color::Magenta),
                ));
            }
            if !note.is_empty() {
                line1.push(Span::styled(
                    format!(" · {}", truncate(note, 18)),
                    Style::default().fg(Color::Cyan),
                ));
            }

            // 第二行：次要資訊 (kind/branch/context/budget)，避免主列過擠。
            // 模型與 effort 改顯示在 transcript 下方，這裡不再重複。
            let mut meta: Vec<String> = Vec::new();
            let mut km = s.kind.id().to_string();
            if s.permission == PermissionPolicy::Bypass {
                km.push_str("!bypass");
            }
            meta.push(km);
            if !branch.is_empty() {
                meta.push(format!("@{}", truncate(branch, 18)));
            }
            if let Some(c) = s.context {
                let pct = c.percent();
                meta.push(format!(
                    "ctx {}",
                    pct.map_or_else(
                        || termade_core::human_tokens(c.used),
                        |p| format!("{p:.0}%")
                    )
                ));
            }
            if let Some(bstr) = s.format_budget() {
                meta.push(bstr);
            }

            let mut line2: Vec<Span> = Vec::new();
            let detail_indent = if depth > 0 {
                format!("{indent}  ")
            } else {
                "  ".to_string()
            };
            line2.push(Span::raw(detail_indent));
            let meta_text = meta.join(" · ");
            // context/budget 警示色以整段標示簡化，仍保留可讀性。
            let meta_color = if s.over_budget
                || s.budget_usd
                    .zip(Some(s.cost.usd))
                    .is_some_and(|(l, u)| u >= l)
            {
                Color::Red
            } else if s
                .context
                .and_then(|c| c.percent())
                .is_some_and(|p| p >= 92.0)
            {
                Color::Red
            } else if s
                .context
                .and_then(|c| c.percent())
                .is_some_and(|p| p >= 80.0)
            {
                Color::Yellow
            } else {
                Color::DarkGray
            };
            line2.push(Span::styled(meta_text, Style::default().fg(meta_color)));

            ListItem::new(vec![Line::from(line1), Line::from(line2)])
        })
        .collect();

    let mut list_state = ListState::default();
    if !sessions.is_empty() {
        list_state.select(Some(app.selected.min(sessions.len() - 1)));
    }

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(if app.scope_here {
                    match hidden {
                        0 => tr!(" Sessions · 本專案 "),
                        n => tr!(" Sessions · 本專案 (另 {n} 個在別處，P 顯示) ", n = n),
                    }
                } else if show_project {
                    tr!(" Sessions · 全部專案 ")
                } else {
                    " Sessions ".to_string()
                }),
        )
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

/// 依目前模式 (Markdown / 原始)與寬度，把逐字稿建成樣式化且已換行的行。
/// 工具呼叫 (`▸ …`)與結果 (`  ✓ …`／`  ✗ …`)行：原樣顯示並上色。
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
) -> (Vec<Line<'static>>, Vec<markdown::LinkTarget>) {
    let buf = match app.state.transcripts.get(id) {
        Some(b) => b,
        None => return (Vec::new(), Vec::new()),
    };
    let src = crate::app::visible_transcript(buf.iter(), app.show_thoughts, app.show_tools);
    if app.raw_md {
        let lines: Vec<Line<'static>> = src.iter().map(|l| Line::raw(l.clone())).collect();
        (markdown::wrap_lines(lines, width), Vec::new())
    } else {
        // 工具呼叫／結果行是程式產生的摘要，不能當 Markdown：
        // `_`、`*`、反斜線會被吃掉 (例如 mcp__termade__termade_peers 變成 mcpadeadepeers)。
        let mut out: Vec<Line<'static>> = Vec::new();
        let mut links: Vec<markdown::LinkTarget> = Vec::new();
        let mut batch: Vec<String> = Vec::new();
        for l in src {
            if let Some(line) = tool_line(&l) {
                if !batch.is_empty() {
                    let (b_lines, b_links) =
                        markdown::Renderer::new().render_and_wrap(&batch, width, out.len());
                    out.extend(b_lines);
                    links.extend(b_links);
                    batch.clear();
                }
                out.push(line);
            } else {
                batch.push(l);
            }
        }
        if !batch.is_empty() {
            let (b_lines, b_links) =
                markdown::Renderer::new().render_and_wrap(&batch, width, out.len());
            out.extend(b_lines);
            links.extend(b_links);
        }
        (out, links)
    }
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
                let (lines, links) = build_transcript_lines(app, id, inner_w);
                app.last_rebuild = std::time::Instant::now();
                app.rebuild_pending = false;
                app.md_cache = Some(MdCache {
                    session: id.clone(),
                    stamp: transcript_stamp(app, id),
                    raw: app.raw_md,
                    width: area.width,
                    lines,
                    links,
                });
            }
            None => app.md_cache = None,
        }
        // 顯示行變了，搜尋的符合項要跟著重算。
        crate::search::refresh(app);
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
            // 搜尋中不恢復跟隨，新輸出才不會把目前的符合項捲走。
            if o >= max_off && app.search.is_none() {
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
                " Transcript: {}{} · {} · {}% · {} ",
                if multi_project_state(app) || !app.scope_here {
                    format!("[{}] ", project_name(s.project_root(), &all_roots(app)))
                } else {
                    String::new()
                },
                s.label,
                if s.transport.is_empty() {
                    "-"
                } else {
                    &s.transport
                },
                pct,
                if app.raw_md { "raw" } else { "md" },
            )
        }
        None => tr!(" (尚無 session：按 n 建立) "),
    };

    // 畫面下方顯示這個 session 的執行參數 (模型、effort、權限、context、成本)，
    // 左側清單只留辨識用的資訊，避免過擠。
    let footer = match &selected {
        Some(id) => {
            let s = &app.state.sessions[id];
            let mut parts: Vec<String> = Vec::new();
            let mut model = s.kind.id().to_string();
            if let Some(m) = s.model.as_deref().filter(|m| !m.is_empty()) {
                model.push(':');
                model.push_str(m);
            }
            parts.push(model);
            if let Some(e) = s.effort.as_deref().filter(|e| !e.is_empty()) {
                parts.push(format!("effort {e}"));
            }
            if s.role != termade_core::Role::Worker || s.parent.is_some() {
                parts.push(format!("role {}", s.role.id()));
            }
            if s.permission != PermissionPolicy::Auto {
                parts.push(format!("perm {}", s.permission.id()));
            }
            if let Some(c) = s.context {
                parts.push(format!("ctx {}", c.short()));
            }
            parts.push(
                s.format_budget()
                    .unwrap_or_else(|| format!("${:.4}", s.cost.usd)),
            );
            if let Some(branch) = s.branch.as_deref() {
                parts.push(format!("@{branch}"));
            }
            let text = format!(" {} ", parts.join(" · "));
            Line::from(Span::styled(
                truncate(&text, inner_w.max(8)),
                Style::default().fg(Color::DarkGray),
            ))
        }
        None => Line::default(),
    };

    let selection = app.selection;
    let mut visible: Vec<Line> = Vec::new();
    if let Some(c) = &app.md_cache {
        for (idx, line) in c.lines.iter().enumerate().skip(off).take(viewport) {
            let line = match selection.and_then(|sel| selection_columns(sel, idx, inner_w)) {
                Some((from, to)) => highlight_columns(line, from, to),
                None => line.clone(),
            };
            let line = match crate::search::mark(app, idx) {
                Some(current) => crate::search::highlight(&line, current),
                None => line,
            };
            visible.push(line);
        }
    }
    if visible.is_empty() {
        visible.push(Line::styled(
            tr!("這裡會顯示 agent 的輸出。按 i 輸入 prompt、n 建立 session、g 切換 Diff 視圖。"),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let para = Paragraph::new(visible).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(footer),
    );
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
            tr!(
                " Diff: {label} ({branch})  +{added} -{removed}  ·  j/k 選行 · {{/}} hunk · c 註解 · M 合併 · [ ] 換 session ",
                label = s.label,
                branch = s.branch.as_deref().unwrap_or("main"),
                added = app.diff_added,
                removed = app.diff_removed,
            )
        }
        None => " Diff ".into(),
    };

    let items: Vec<ListItem> = if app.diff_lines.is_empty() {
        vec![ListItem::new(Line::styled(
            tr!("(尚無 diff 資料，按 r 重新載入)"),
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
                    Span::styled(
                        l.clone(),
                        match crate::search::mark(app, i) {
                            Some(current) => {
                                diff_line_style(l).patch(crate::search::mark_style(current))
                            }
                            None => diff_line_style(l),
                        },
                    ),
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
            tr!("(目前沒有獨立 worktree；建立 session 時開啟 --worktree 或按 Ctrl-W)"),
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
        .block(Block::default().borders(Borders::ALL).title(" Worktrees "))
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

pub(crate) fn draw_bus(f: &mut TuiFrame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();

    for p in app.state.pending_permissions.values() {
        lines.push(Line::from(vec![
            Span::styled(
                tr!("⏸ 核准 "),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(tr!(
                "{title}：{detail}",
                title = p.title,
                detail = truncate(&p.detail, 40)
            )),
        ]));
    }
    if !app.state.pending_permissions.is_empty() {
        lines.push(Line::styled(
            tr!("  [a] 允許  [A] 一律允許  [d] 拒絕"),
            Style::default().fg(Color::DarkGray),
        ));
    }

    if !app.state.tasks.is_empty() {
        for t in app.state.tasks.values() {
            let color = match t.status {
                termade_core::TaskStatus::Done => Color::Green,
                termade_core::TaskStatus::Doing | termade_core::TaskStatus::Review => Color::Cyan,
                termade_core::TaskStatus::Blocked => Color::Red,
                termade_core::TaskStatus::Todo => Color::Gray,
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
            tr!("agent 之間的訊息與任務看板會出現在這裡。"),
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
        Mode::Normal => tr!(" 輸入 (i 輸入 · ? 說明) "),
        Mode::Help => tr!(" 說明 "),
        Mode::Settings => tr!(" 設定 "),
        Mode::Qr => tr!(" QR Code "),
        Mode::Prompt => match app
            .selected_id()
            .and_then(|id| app.state.queues.get(&id).map(Vec::len))
            .unwrap_or(0)
        {
            0 => tr!(" prompt (@ 附檔 · Enter 送出 · Alt-Enter 排隊) "),
            n => tr!(" prompt (排隊 {n} 則 · Alt-Enter 再排) ", n = n),
        },
        Mode::Spawn => match &app.spawn_target {
            Some((kind, model)) => tr!(
                " spawn → {target} · worktree={worktree} · 權限={permission} · Ctrl-F effort ",
                target = format!(
                    "{}{}{}",
                    kind.id(),
                    model
                        .as_deref()
                        .map(|m| format!(":{m}"))
                        .unwrap_or_default(),
                    app.spawn_effort
                        .as_deref()
                        .map(|e| format!(" effort={e}"))
                        .unwrap_or_default(),
                ),
                worktree = if app.spawn_worktree { "on" } else { "off" },
                permission = app
                    .spawn_permission
                    .map_or_else(|| tr!("預設"), |p| p.id().to_string()),
            ),
            None => tr!(" spawn：<agent[:model]> <prompt> · Ctrl-F effort "),
        },
        Mode::Menu => tr!(" 選單 "),
        Mode::Pick => tr!(" 選擇 (Enter 確認 · Esc 取消) "),
        Mode::Message => tr!(" message → 選取的 session "),
        Mode::Review => tr!(" review 註解 → 選取的 session "),
        Mode::Command => tr!(" 命令 (Tab 補全) "),
        Mode::Jump => tr!(" 跳轉 session "),
        Mode::Search => tr!(" 搜尋 (Enter／↓ 下一個 · ↑ 上一個 · Esc 離開) "),
    };
    let style = if app.mode == Mode::Normal {
        Style::default()
    } else {
        Style::default().fg(Color::Cyan)
    };

    // 軟換行排版；超過 MAX_ROWS 時只顯示游標附近的視窗。
    let inner_w = area.width.saturating_sub(2) as usize;
    let rows = crate::inputbox::rows(&app.input, inner_w);
    let (cur_row, cursor_col) = crate::inputbox::cursor_pos(&app.input, &rows, app.input_cursor);
    let visible_rows = (area.height.saturating_sub(2) as usize).max(1);
    let first = (cur_row + 1)
        .saturating_sub(visible_rows)
        .min(rows.len().saturating_sub(visible_rows));
    let visible: Vec<Line> = rows
        .iter()
        .skip(first)
        .take(visible_rows)
        .map(|r| Line::raw(crate::inputbox::row_text(&app.input, *r)))
        .collect();

    let para = Paragraph::new(visible)
        .style(style)
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(para, area);

    // 把終端機游標放到輸入框內，IME 的 preedit 就會錨定在這裡。
    if area.width > 2 && area.height > 2 {
        let x = area
            .x
            .saturating_add(1)
            .saturating_add(cursor_col as u16)
            .min(area.x + area.width.saturating_sub(2));
        let y = area.y + 1 + (cur_row - first) as u16;
        f.set_cursor_position((x, y));
    }
}

pub(crate) fn draw_status(f: &mut TuiFrame, area: Rect, app: &App) {
    let hints = match app.view {
        CenterView::Transcript => tr!("  ? 說明  Space 選單  : 命令  q 離開"),
        CenterView::Diff => tr!("  ? 說明  c 註解  M 合併  q 離開"),
        CenterView::Worktrees => tr!("  ? 說明  M 合併  G 清理  q 離開"),
        CenterView::Tasks => tr!("  j/k 選任務  Enter 跳到負責人  :task-status <狀態>  ? 說明"),
    };
    let text = Line::from(vec![
        Span::styled(
            format!(
                " {} ",
                crate::search::status_text(app)
                    .as_deref()
                    .unwrap_or(&app.status)
            ),
            Style::default().fg(Color::Black).bg(Color::DarkGray),
        ),
        Span::raw(hints),
    ]);
    f.render_widget(Paragraph::new(text), area);
}

fn all_roots(app: &App) -> Vec<&str> {
    let mut r: Vec<&str> = app
        .state
        .sessions
        .values()
        .map(|s| s.project_root())
        .collect();
    r.sort_unstable();
    r.dedup();
    r
}

fn multi_project_state(app: &App) -> bool {
    app.project_count() > 1
}
