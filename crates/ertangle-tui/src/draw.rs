//! 所有繪製函式 (版面、標題、sessions、transcript、diff、bus、輸入框)。

use super::*;
use tr::tr;

/// Braille spinner frames for live thinking indicators.
pub(crate) const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Current spinner frame, advancing every 120ms so busy redraws animate it.
pub(crate) fn spinner_frame() -> &'static str {
    let idx = (ertangle_core::now_millis() / 120) as usize % SPINNER_FRAMES.len();
    SPINNER_FRAMES[idx]
}

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
    let right_bus = layout == LayoutMode::Three && app.right_bus;
    app.bus_visible = right_bus || app.bus_overlay;
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
        (34, 46)
    } else if w >= 110 {
        (30, 40)
    } else if w >= 84 {
        (26, 32)
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

    // Pending approvals and questions must remain visible.
    let has_pending =
        !app.state.pending_permissions.is_empty() || !banner_questions(app).is_empty();
    let center = if has_pending {
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
        CenterView::Grid => crate::project::draw_grid(f, center, app),
        CenterView::Flow => {
            let model = crate::flow::build_flow_model(app, ertangle_core::now_millis());
            let cursor = app.flow_cursor.min(model.nodes.len().saturating_sub(1));
            if app.flow_list || !crate::flow::draw_flow_graph(f, center, &model, cursor) {
                crate::flow::draw_flow(f, center, &model, cursor);
            }
        }
    }
    if let Some(a) = bus_area {
        if right_bus {
            crate::flow::draw_flow_pane(f, a, app);
        } else {
            crate::project::draw_project(f, a, app);
        }
    } else if app.bus_overlay {
        let ow = (main.width * 2 / 5).max(30).min(main.width);
        let r = Rect::new(main.x + main.width - ow, main.y, ow, main.height);
        f.render_widget(Clear, r);
        crate::flow::draw_flow_pane(f, r, app);
    }
    draw_input(f, chunks[2], app);
    draw_status(f, chunks[3], app);
    match app.mode {
        Mode::Menu => draw_menu(f, chunks[2], app),
        Mode::Command => draw_command_hints(f, chunks[2], app),
        Mode::Prompt | Mode::Spawn | Mode::Message => draw_file_suggestions(f, chunks[2], app),
        Mode::Pick
            if matches!(
                app.picker.as_ref().map(|p| &p.stage),
                Some(crate::menu::PickStage::Question { .. })
            ) =>
        {
            draw_question_picker(f, chunks[2], app)
        }
        Mode::Pick => draw_picker(f, app),
        Mode::Help => draw_help(f, app),
        Mode::Settings => draw_settings(f, app),
        Mode::Qr => draw_qr_modal(f, app),
        Mode::Quota => crate::quota::draw(f, app),
        Mode::Ps => crate::ps::draw(f, app),
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
        .map(|i| ertangle_core::tr_dyn!(i.label))
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
                .title(tr!(" 命令 (Tab 自動完成) ")),
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
    let sections = crate::settings::ITEMS
        .iter()
        .filter(|i| i.section().is_some())
        .count() as u16;
    let h = (crate::settings::ITEMS.len() as u16 + sections + 5).min(full.height);
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
    let mut lines: Vec<Line> = Vec::new();
    for (i, item) in crate::settings::ITEMS.iter().enumerate() {
        if let Some(title) = item.section() {
            lines.push(Line::from(Span::styled(
                format!(" ── {title} ──"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        lines.push({
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
        });
    }
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
            Style::default().fg(Color::Black).bg(Color::White),
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
    let is_resume = matches!(p.stage, crate::menu::PickStage::Resume);
    let max_w = if is_resume { 96 } else { 72 };
    let w = max_w.min(f.area().width.saturating_sub(2));
    let extra_h: u16 = if is_resume { 5 } else { 4 };
    let h = (vis.len() as u16 + extra_h)
        .clamp(6, 18)
        .min(f.area().height);
    let area = Rect::new(
        f.area().x + (f.area().width.saturating_sub(w)) / 2,
        f.area().y + (f.area().height.saturating_sub(h)) / 3,
        w,
        h,
    );
    let mut lines = Vec::new();
    if is_resume {
        let here_active = p.resume_scope == crate::menu::ResumeScope::Here;
        let here_style = if here_active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let all_style = if !here_active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" [ {} ] ", tr!("本專案")), here_style),
            Span::raw(" "),
            Span::styled(format!(" [ {} ] ", tr!("全部專案")), all_style),
            Span::styled(tr!(" (Tab 切換)"), Style::default().fg(Color::DarkGray)),
        ]));
    }
    let cur = p.filter_cursor.min(p.filter.chars().count());
    let before: String = p.filter.chars().take(cur).collect();
    let after: String = p.filter.chars().skip(cur).collect();
    let mut input_line = vec![
        Span::styled("> ", Style::default().fg(Color::Cyan)),
        Span::raw(before),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
        Span::raw(after),
    ];
    if p.filter.is_empty() {
        input_line.push(Span::styled(
            p.placeholder(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.push(Line::from(input_line));
    let header_lines = if is_resume { 2 } else { 1 };
    let room = h.saturating_sub(header_lines + 2) as usize;
    let start = p.cursor.saturating_sub(room.saturating_sub(1));
    for (i, (label, value)) in vis.iter().enumerate().skip(start).take(room) {
        let sel = i == p.cursor;
        let prefix = match &p.stage {
            crate::menu::PickStage::Question {
                multi_select: true,
                selected,
                ..
            } => {
                let check = if selected.contains(value) {
                    "[x] "
                } else {
                    "[ ] "
                };
                format!("{} {check}", if sel { "▸" } else { " " })
            }
            _ => format!("{} ", if sel { "▸" } else { " " }),
        };
        lines.push(Line::from(Span::styled(
            format!("{prefix}{label}"),
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
        let empty_msg =
            if is_resume && p.resume_scope == crate::menu::ResumeScope::Here && p.filter.is_empty()
            {
                tr!(
                    "{project} 沒有已結束的 session；按 Tab 切換至全部專案",
                    project = crate::app::project_name(&app.home_project, &[])
                )
            } else if p.filter.is_empty() {
                tr!("(沒有選項)")
            } else {
                tr!("Enter 使用自訂值：{value}", value = p.filter)
            };
        lines.push(Line::from(Span::styled(
            empty_msg,
            Style::default().fg(Color::DarkGray),
        )));
    }
    f.render_widget(ratatui::widgets::Clear, area);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" {} ", p.title));
    if let Some(hint) = p.hint() {
        block = block.title_bottom(
            Line::styled(format!(" {hint} "), Style::default().fg(Color::DarkGray)).right_aligned(),
        );
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// 依顯示寬度折行 (CJK 佔兩欄)；保留原有換行。
fn wrap_display(text: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let width = width.max(2);
    let mut out = Vec::new();
    for raw in text.lines() {
        let mut cur = String::new();
        let mut w = 0;
        for ch in raw.chars() {
            let cw = ch.width().unwrap_or(0);
            if w + cw > width {
                out.push(std::mem::take(&mut cur));
                w = 0;
            }
            cur.push(ch);
            w += cw;
        }
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// 問題選擇器：多行標題 (header + 完整問題 + 提示)、選項下方灰字說明、自訂答案列。
fn draw_question_picker(f: &mut TuiFrame, input_area: Rect, app: &App) {
    use crate::menu::PickStage;
    let Some(p) = app.picker.as_ref() else { return };
    let PickStage::Question {
        multi_select,
        selected,
        questions,
        index,
        custom_mode,
        custom_text,
        custom_cursor,
        ..
    } = &p.stage
    else {
        return;
    };
    let Some(q) = questions.get(*index) else {
        return;
    };
    let w = input_area.width;
    let inner_w = w.saturating_sub(4) as usize;
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(h) = q.header.as_deref().filter(|h| !h.is_empty()) {
        for l in wrap_display(h, inner_w) {
            lines.push(Line::from(Span::styled(
                l,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
        }
    }
    for l in wrap_display(&q.question, inner_w) {
        lines.push(Line::from(l));
    }
    lines.push(Line::from(""));
    let mut cursor_line = lines.len();
    let mut custom_cursor_at: Option<(usize, (usize, usize))> = None;
    let n_opts = p.items.len();
    for (i, opt) in q.options.iter().enumerate() {
        let sel = i == p.cursor && !*custom_mode;
        if sel {
            cursor_line = lines.len();
        }
        let mark = if *multi_select {
            if selected.contains(&opt.label) {
                "[x] "
            } else {
                "[ ] "
            }
        } else {
            ""
        };
        let text = format!(
            "{} {}. {mark}{}",
            if sel { "▸" } else { " " },
            i + 1,
            opt.label
        );
        let style = if sel {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(text, style)));
        if let Some(d) = opt.description.as_deref().filter(|d| !d.is_empty()) {
            for l in wrap_display(d, inner_w.saturating_sub(5)) {
                lines.push(Line::from(Span::styled(format!("     {l}"), dim)));
            }
        }
    }
    if q.custom {
        let sel = p.cursor == n_opts || *custom_mode;
        if sel {
            cursor_line = lines.len();
        }
        let style = if sel && !*custom_mode {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default()
        };
        let num = if n_opts < 9 {
            format!("{}. ", n_opts + 1)
        } else {
            String::new()
        };
        lines.push(Line::from(Span::styled(
            format!(
                "{} {num}{}",
                if sel { "▸" } else { " " },
                tr!("✎ 自訂答案…")
            ),
            style,
        )));
        if *custom_mode {
            // Wrap the answer into rows so a long text keeps the cursor visible.
            let rows = crate::inputbox::rows(custom_text, inner_w.saturating_sub(7).max(2));
            custom_cursor_at = Some((
                lines.len(),
                crate::inputbox::cursor_pos(custom_text, &rows, *custom_cursor),
            ));
            for (i, row) in rows.iter().enumerate() {
                let prefix = if i == 0 { "     > " } else { "       " };
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::Cyan)),
                    Span::raw(crate::inputbox::row_text(custom_text, *row)),
                ]));
            }
        }
    }
    lines.push(Line::from(""));
    let hint = if *custom_mode {
        tr!("輸入自訂答案 · Enter 送出 · Esc 返回選項")
    } else if *multi_select {
        tr!("↑↓ 移動 · 1-9 或 Space 勾選 · Enter 送出 · Esc 稍後回答")
    } else {
        tr!("↑↓ 移動 · 1-9 直接選擇 · Enter 選擇 · Esc 稍後回答")
    };
    let hint = if questions.len() > 1 && !*custom_mode {
        format!("{hint} · {}", tr!("s 跳過此題"))
    } else if questions.len() > 1 && q.options.is_empty() {
        format!("{hint} · {}", tr!("留空 Enter 跳過此題"))
    } else {
        hint
    };
    for l in wrap_display(&hint, inner_w) {
        lines.push(Line::from(Span::styled(l, dim)));
    }
    // 貼在輸入框正上方，往上長高。
    let max_h = input_area.y.saturating_sub(f.area().y);
    let h = (lines.len() as u16 + 2).min(max_h);
    let area = Rect::new(input_area.x, input_area.y - h, w, h);
    let room = h.saturating_sub(2) as usize;
    if let Some((start, (r, _))) = custom_cursor_at {
        cursor_line = start + r;
    }
    let scroll = (cursor_line + 1).saturating_sub(room) as u16;
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(
        Paragraph::new(lines).scroll((scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(format!(" {} ", p.title)),
        ),
        area,
    );
    // Put the terminal cursor at the edit point so the IME preedit anchors there.
    if let Some((start, (r, col))) = custom_cursor_at {
        let row_y = (start + r).saturating_sub(scroll as usize);
        if row_y < room && area.width > 2 {
            let x = (area.x + 1 + 7 + col as u16).min(area.x + area.width.saturating_sub(2));
            f.set_cursor_position((x, area.y + 1 + row_y as u16));
        }
    }
}

/// One-line approval or question banner (used when the bus is hidden): prefer the selected session.
/// 頂端橫幅要顯示的問題：選取 session 的問題在選擇器開啟時由選擇器本身呈現，不重複顯示。
fn banner_questions(app: &App) -> Vec<&ertangle_core::UserQuestion> {
    let open_for = match app.picker.as_ref().map(|p| &p.stage) {
        Some(crate::menu::PickStage::Question { session, .. }) => Some(session),
        _ => None,
    };
    app.state
        .pending_questions
        .values()
        .filter(|q| Some(&q.session) != open_for)
        .collect()
}

pub(crate) fn draw_pending_banner(f: &mut TuiFrame, area: Rect, app: &App) {
    let selected = app.selected_id();
    let perm_mine = app
        .state
        .pending_permissions
        .values()
        .find(|p| Some(&p.session) == selected.as_ref());
    let questions = banner_questions(app);
    // 沒有選取的 session 在等時，橫幅顯示 w 會跳到的那一個，標題與按鍵才一致。
    let target = app.next_pending_session();
    let qst_mine = questions
        .iter()
        .copied()
        .find(|q| Some(&q.session) == selected.as_ref());

    if let Some(q) = qst_mine.or_else(|| {
        if perm_mine.is_none() {
            questions
                .iter()
                .copied()
                .find(|q| Some(&q.session) == target.as_ref())
                .or_else(|| questions.first().copied())
        } else {
            None
        }
    }) {
        let label = app
            .state
            .sessions
            .get(&q.session)
            .map_or_else(|| short(&q.session), |s| s.label.clone());
        let mine = Some(&q.session) == selected.as_ref();
        let total = questions.len() + app.state.pending_permissions.len();
        let qst_for_session = questions
            .iter()
            .filter(|item| Some(&item.session) == selected.as_ref())
            .count();
        let more = if qst_for_session > 1 {
            format!(" (+{} 問題)", qst_for_session - 1)
        } else if total > 1 {
            format!(" (+{})", total - 1)
        } else {
            String::new()
        };
        let keys = if mine {
            tr!("[Enter 回答]")
        } else {
            tr!("(按 w 跳到該 session 回答)")
        };
        let default_header = tr!("提問");
        let header_str = q.header.as_deref().unwrap_or(&default_header);
        let text = tr!(
            "⏸ {label} 提問 {header}：{question}{more} {keys}",
            label = label,
            header = header_str,
            question = truncate(&q.question, 40),
            more = more,
            keys = keys
        );
        f.render_widget(
            Paragraph::new(Line::styled(
                text,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            area,
        );
        return;
    }

    let p = perm_mine
        .or_else(|| {
            app.state
                .pending_permissions
                .values()
                .find(|p| Some(&p.session) == target.as_ref())
        })
        .or_else(|| app.state.pending_permissions.values().next());
    let Some(p) = p else { return };
    let label = app
        .state
        .sessions
        .get(&p.session)
        .map_or_else(|| short(&p.session), |s| s.label.clone());
    let mine = Some(&p.session) == selected.as_ref();
    let total = app.state.pending_permissions.len() + app.state.pending_questions.len();
    let more = if total > 1 {
        format!(" (+{})", total - 1)
    } else {
        String::new()
    };
    let keys = if mine {
        tr!("[a]llow [A]lways [d]eny")
    } else {
        tr!("(按 w 跳到該 session 後 a/d)")
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
    let mut waiting: Vec<&SessionId> = app
        .state
        .pending_permissions
        .values()
        .map(|p| &p.session)
        .collect();
    waiting.extend(app.state.pending_questions.values().map(|q| &q.session));
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

/// 標題列的額度摘要：每個 agent 各自依剩餘額度上色，剩餘 ≤30% 變黃、≤10% 或已滿變紅。
fn quota_spans(app: &App) -> Vec<Span<'static>> {
    let now = ertangle_core::now_millis();
    let mut spans = Vec::new();
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
        let Some(w) = live
            .iter()
            .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
        else {
            continue;
        };
        let rem = (100.0 - w.used_percent).clamp(0.0, 100.0);
        let color = if q.reached || rem <= 10.0 {
            Color::Red
        } else if rem <= 30.0 || q.warning {
            Color::Yellow
        } else {
            Color::Gray
        };
        let short = match q.agent.as_str() {
            "antigravity" => "agy",
            other => other,
        };
        let pct_label = tr!("剩{pct}%", pct = format!("{rem:.0}"));
        spans.push(Span::styled(
            format!("  {short}:{} {pct_label}", w.name),
            Style::default().fg(color),
        ));
    }
    spans
}

pub(crate) fn draw_header(f: &mut TuiFrame, area: Rect, app: &mut App) {
    let live = app.state.live_sessions().count();
    let cost = app.state.total_cost();

    let mut tab_spans = vec![
        Span::styled(
            " ertangle ",
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

    if app.view == CenterView::Grid {
        tab_spans.insert(5, tab("Grid", true));
    }

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

    let mut stat_spans = vec![
        Span::styled("  cost ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("${:.4}", cost.usd),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            format!("  ({} in / {} out)", cost.input_tokens, cost.output_tokens),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    let q_start = stat_spans.len();
    stat_spans.extend(quota_spans(app));
    let q_end = stat_spans.len();
    stat_spans.extend([
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
    let stats = Line::from(stat_spans);

    // 額度摘要的位置，給滑鼠點擊開啟 `:quota` 浮層。
    let before: usize = stats.spans[..q_start].iter().map(|s| s.width()).sum();
    let qw: usize = stats.spans[q_start..q_end].iter().map(|s| s.width()).sum();
    app.quota_area = if qw > 0 && area.height > 1 {
        Rect::new(
            area.x + before as u16,
            area.y + 1,
            (qw as u16).min(area.width.saturating_sub(before as u16)),
            1,
        )
    } else {
        Rect::default()
    };
    f.render_widget(Paragraph::new(vec![tabs, stats]), area);
}

/// Sessions 清單每一筆固定佔用的列數 (單行：狀態 + 標籤)。
/// 滑鼠點擊換算索引時必須用這個值，否則會把列算成下一筆。
pub(crate) const SESSION_ITEM_ROWS: u16 = 1;

/// 把一段文字推入 spans 並累計顯示寬度。
fn push_span(
    spans: &mut Vec<Span<'static>>,
    used: &mut usize,
    text: impl Into<String>,
    style: Style,
) {
    let text = text.into();
    if text.is_empty() {
        return;
    }
    *used += UnicodeWidthStr::width(text.as_str());
    spans.push(Span::styled(text, style));
}

pub(crate) fn draw_sessions(f: &mut TuiFrame, area: Rect, app: &mut App) {
    app.sessions_area = area;
    let vis = app.visible_sessions();
    let multi_host = app.has_multiple_hosts();
    let sessions: Vec<(String, &ertangle_core::Session)> = vis
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
    // 邊框佔 2 欄，`▸ ` 選取符號另外佔 2 欄。
    let inner_w = area.width.saturating_sub(4) as usize;

    // 每筆 session 只用一行：狀態 + 標籤 + 少數警示標記。
    // 模型、branch、context、成本等細節都在逐字稿下方 (footer) 顯示，側欄不再重複。
    let mut items: Vec<ListItem> = sessions
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
            let dim = Style::default().fg(Color::DarkGray);
            let depth = app.session_tree_depth(host_name, &s.id);
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut used = 0usize;

            if depth > 0 {
                push_span(&mut spans, &mut used, " ".repeat(depth.min(3) * 2), dim);
                push_span(&mut spans, &mut used, "↳", dim);
            }
            if show_project && depth == 0 {
                push_span(
                    &mut spans,
                    &mut used,
                    format!(
                        "{} ",
                        truncate_width(&project_name(s.project_root(), &roots), 8)
                    ),
                    Style::default()
                        .fg(Color::Black)
                        .bg(project_color(s.project_root())),
                );
            }
            if multi_host {
                push_span(
                    &mut spans,
                    &mut used,
                    format!("[{host_name}]"),
                    Style::default().fg(Color::Cyan),
                );
            }
            let glyph = if s.status == SessionStatus::Busy {
                spinner_frame()
            } else {
                s.status.glyph()
            };
            push_span(&mut spans, &mut used, glyph, Style::default().fg(color));

            // 右側只留真正需要注意的短標記：context 快滿、有背景處理程序。
            let mut right: Vec<(String, Color)> = Vec::new();
            if let Some(p) = s.context.and_then(|c| c.percent()).filter(|p| *p >= 80.0) {
                right.push((
                    format!("{p:.0}%"),
                    if p >= 92.0 { Color::Red } else { Color::Yellow },
                ));
            }
            if s.tool_calls > 0 {
                right.push((format!("{} calls", s.tool_calls), Color::DarkGray));
            }
            if !s.bg_procs.is_empty() {
                right.push((
                    format!("bg{}", s.bg_procs.len()),
                    if s.status == SessionStatus::Idle {
                        Color::Yellow
                    } else {
                        Color::DarkGray
                    },
                ));
            }
            let right_w: usize = right
                .iter()
                .map(|(t, _)| UnicodeWidthStr::width(t.as_str()) + 1)
                .sum();

            // 標籤吃掉剩下的寬度 (至少留 1 欄，太窄時用 `…`)。
            // 角色、模型、branch、context、成本等細節都在逐字稿 footer 顯示。
            let budget = inner_w.saturating_sub(used + 1 + right_w).max(1);
            push_span(
                &mut spans,
                &mut used,
                format!(" {}", truncate_width(&s.label, budget)),
                Style::default(),
            );
            if !right.is_empty() {
                let pad = inner_w.saturating_sub(used + right_w);
                push_span(&mut spans, &mut used, " ".repeat(pad), dim);
                for (text, c) in right {
                    push_span(
                        &mut spans,
                        &mut used,
                        format!(" {text}"),
                        Style::default().fg(c),
                    );
                }
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    // Fold header for ended sessions: never selectable (index past the last session).
    let ended = app.ended_count();
    if ended > 0 {
        let arrow = if app.show_ended { "▾" } else { "▸" };
        items.push(ListItem::new(Line::from(Span::styled(
            format!("{arrow} {}", tr!("已結束 ({n})", n = ended)),
            Style::default().fg(Color::DarkGray),
        ))));
    }

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
                        0 => tr!(" Sessions "),
                        n => tr!(" Sessions · +{n} ", n = n),
                    }
                } else if show_project {
                    tr!(" Sessions · 全部 ")
                } else {
                    " Sessions ".to_string()
                }),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ");
    f.render_stateful_widget(list, area, &mut list_state);
    app.sessions_offset = list_state.offset();
}

pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// 逐字稿指紋：串流增量會改變末行或行數，滿額截斷會改變首行。
pub(crate) fn transcript_stamp(app: &App, id: &SessionId) -> TranscriptStamp {
    app.state
        .transcripts
        .get(id)
        .map_or(TranscriptStamp::default(), |b| TranscriptStamp {
            len: b.len(),
            front_len: b.front().map_or(0, String::len),
            front_hash: b.front().map_or(0, |s| fnv1a(s.as_bytes())),
            back_len: b.back().map_or(0, String::len),
            back_hash: b.back().map_or(0, |s| fnv1a(s.as_bytes())),
        })
}

/// 尋找原始逐字稿的安全前綴分界點 (0..S 為安全前綴，S..len 為尾端)。
/// 保守選擇分界點：不可在未閉合的 Markdown 圍籬、表格、或進行中的工具呼叫中。
pub(crate) fn find_safe_boundary(buf: &std::collections::VecDeque<String>) -> usize {
    let len = buf.len();
    if len <= 2 {
        return 0;
    }

    let mut in_fence = false;
    let mut fence_after = Vec::with_capacity(len);
    for l in buf.iter() {
        if crate::markdown::fence(l).is_some() {
            in_fence = !in_fence;
        }
        fence_after.push(in_fence);
    }

    let search_end = len.saturating_sub(2);

    // 優先找 code fence 之外的空白行：空白行必非工具行、非表格、非段落中斷，且會 flush 工具摺疊
    for i in (1..=search_end).rev() {
        if !fence_after[i - 1] && buf[i - 1].trim().is_empty() && !buf[i].starts_with("  ✓") {
            return i;
        }
    }

    // 若無空白行，找 code fence 之外的獨立普通行
    for i in (1..=search_end).rev() {
        if fence_after[i - 1] {
            continue;
        }
        let prev = &buf[i - 1];
        let next = &buf[i];

        if prev.starts_with("▸ ") || prev.starts_with("  ✓") || prev.starts_with("  ✗") {
            continue;
        }
        // A boundary between two thought lines would split one folded run into two summaries.
        if crate::app::strip_thought_prefix(prev).is_some()
            && crate::app::strip_thought_prefix(next).is_some()
        {
            continue;
        }
        if next.starts_with("  ✓") {
            continue;
        }
        if crate::markdown::is_table_row(prev) || crate::markdown::is_table_sep(prev) {
            continue;
        }
        if crate::markdown::is_table_row(next) || crate::markdown::is_table_sep(next) {
            continue;
        }

        return i;
    }

    0
}

/// 依目前模式 (Markdown / 原始)與寬度，把逐字稿建成樣式化且已換行的行。
/// 工具呼叫 (`▸ …`)與結果 (`  ✓ …`／`  ✗ …`)行：原樣顯示並上色。
fn tool_line(l: &str) -> Option<Line<'static>> {
    let style = if l.starts_with("▸ [subagent]") {
        Style::default().fg(Color::Magenta)
    } else if l.starts_with("▸ ") {
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

pub(crate) fn render_transcript_slice<'a>(
    lines_iter: impl Iterator<Item = &'a String>,
    raw_md: bool,
    show_thoughts: bool,
    show_tools: bool,
    active_tools: &[ertangle_core::ActiveTool],
    width: usize,
    base_line_idx: usize,
) -> (Vec<Line<'static>>, Vec<markdown::LinkTarget>) {
    let src = crate::app::visible_transcript(lines_iter, show_thoughts, show_tools, active_tools);
    if raw_md {
        let lines: Vec<Line<'static>> = src
            .iter()
            .map(|l| Line::raw(l.as_ref().to_string()))
            .collect();
        (markdown::wrap_lines(lines, width), Vec::new())
    } else {
        let mut out: Vec<Line<'static>> = Vec::with_capacity(src.len());
        let mut links: Vec<markdown::LinkTarget> = Vec::new();
        let mut batch: Vec<&str> = Vec::new();
        for l in &src {
            let s: &str = l.as_ref();
            if let Some(line) = tool_line(s) {
                if !batch.is_empty() {
                    let (b_lines, b_links) = markdown::Renderer::new().render_and_wrap(
                        &batch,
                        width,
                        base_line_idx + out.len(),
                    );
                    out.extend(b_lines);
                    links.extend(b_links);
                    batch.clear();
                }
                out.push(line);
            } else {
                batch.push(s);
            }
        }
        if !batch.is_empty() {
            let (b_lines, b_links) =
                markdown::Renderer::new().render_and_wrap(&batch, width, base_line_idx + out.len());
            out.extend(b_lines);
            links.extend(b_links);
        }
        (out, links)
    }
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
    let active = app
        .state
        .sessions
        .get(id)
        .map(|s| s.active_tools.as_slice())
        .unwrap_or(&[]);
    render_transcript_slice(
        buf.iter(),
        app.raw_md,
        app.show_thoughts,
        app.show_tools,
        active,
        width,
        0,
    )
}

pub(crate) fn update_transcript_cache(
    app: &mut App,
    id: &SessionId,
    width: usize,
    area_width: u16,
) {
    let buf = match app.state.transcripts.get(id) {
        Some(b) => b,
        None => {
            app.md_cache = None;
            return;
        }
    };
    let active = app
        .state
        .sessions
        .get(id)
        .map(|s| s.active_tools.as_slice())
        .unwrap_or(&[]);
    let curr_stamp = transcript_stamp(app, id);

    let can_incremental = if let Some(c) = &app.md_cache {
        c.session == *id
            && c.raw == app.raw_md
            && c.width == area_width
            && c.show_tools == app.show_tools
            && c.show_thoughts == app.show_thoughts
            && buf.len() >= c.prefix_raw_len
            && (c.prefix_raw_len == 0 || {
                let front_ok = buf.front().map_or(0, String::len) == c.prefix_front_len
                    && buf.front().map_or(0, |s| fnv1a(s.as_bytes())) == c.prefix_front_hash;
                let last_prefix_line = &buf[c.prefix_raw_len - 1];
                let last_prefix_ok = last_prefix_line.len() == c.prefix_last_len
                    && fnv1a(last_prefix_line.as_bytes()) == c.prefix_last_hash;
                front_ok && last_prefix_ok
            })
    } else {
        false
    };

    let mut incremental_done = false;
    if can_incremental {
        if let Some(c) = app.md_cache.as_mut() {
            c.lines.truncate(c.prefix_lines_count);
            c.links.truncate(c.prefix_links_count);

            let tail_start = c.prefix_raw_len;
            let tail_end = buf.len();

            let (tail_lines, tail_links) = render_transcript_slice(
                buf.range(tail_start..tail_end),
                app.raw_md,
                app.show_thoughts,
                app.show_tools,
                active,
                width,
                c.prefix_lines_count,
            );

            c.lines.extend(tail_lines);
            c.links.extend(tail_links);
            c.stamp = curr_stamp;

            // 若 tail 累積超過 32 行，嘗試將新的安全邊界納入前綴
            if buf.len() > c.prefix_raw_len + 32 {
                let new_safe = find_safe_boundary(buf);
                if new_safe > c.prefix_raw_len {
                    let (mid_lines, mid_links) = render_transcript_slice(
                        buf.range(c.prefix_raw_len..new_safe),
                        app.raw_md,
                        app.show_thoughts,
                        app.show_tools,
                        &[],
                        width,
                        c.prefix_lines_count,
                    );
                    c.lines.truncate(c.prefix_lines_count);
                    c.links.truncate(c.prefix_links_count);
                    c.prefix_lines_count += mid_lines.len();
                    c.prefix_links_count += mid_links.len();
                    c.lines.extend(mid_lines);
                    c.links.extend(mid_links);

                    c.prefix_raw_len = new_safe;
                    c.prefix_last_len = buf[new_safe - 1].len();
                    c.prefix_last_hash = fnv1a(buf[new_safe - 1].as_bytes());

                    let (new_tail_lines, new_tail_links) = render_transcript_slice(
                        buf.range(new_safe..buf.len()),
                        app.raw_md,
                        app.show_thoughts,
                        app.show_tools,
                        active,
                        width,
                        c.prefix_lines_count,
                    );
                    c.lines.extend(new_tail_lines);
                    c.links.extend(new_tail_links);
                }
            }
            incremental_done = true;
        }
    }
    if !incremental_done {
        let safe_boundary = find_safe_boundary(buf);
        let (lines, links, prefix_lines_count, prefix_links_count) = if safe_boundary > 0 {
            let (mut p_lines, mut p_links) = render_transcript_slice(
                buf.range(0..safe_boundary),
                app.raw_md,
                app.show_thoughts,
                app.show_tools,
                &[],
                width,
                0,
            );
            let p_len = p_lines.len();
            let p_links_len = p_links.len();

            let (t_lines, t_links) = render_transcript_slice(
                buf.range(safe_boundary..buf.len()),
                app.raw_md,
                app.show_thoughts,
                app.show_tools,
                active,
                width,
                p_len,
            );

            p_lines.extend(t_lines);
            p_links.extend(t_links);
            (p_lines, p_links, p_len, p_links_len)
        } else {
            let (all_lines, all_links) = render_transcript_slice(
                buf.iter(),
                app.raw_md,
                app.show_thoughts,
                app.show_tools,
                active,
                width,
                0,
            );
            (all_lines, all_links, 0, 0)
        };

        let prefix_front_len = buf.front().map_or(0, String::len);
        let prefix_front_hash = buf.front().map_or(0, |s| fnv1a(s.as_bytes()));
        let prefix_last_len = if safe_boundary > 0 {
            buf[safe_boundary - 1].len()
        } else {
            0
        };
        let prefix_last_hash = if safe_boundary > 0 {
            fnv1a(buf[safe_boundary - 1].as_bytes())
        } else {
            0
        };

        app.md_cache = Some(MdCache {
            session: id.clone(),
            stamp: curr_stamp,
            raw: app.raw_md,
            width: area_width,
            show_tools: app.show_tools,
            show_thoughts: app.show_thoughts,
            lines,
            links,
            prefix_raw_len: safe_boundary,
            prefix_lines_count,
            prefix_links_count,
            prefix_front_hash,
            prefix_front_len,
            prefix_last_hash,
            prefix_last_len,
        });
    }

    app.last_rebuild = std::time::Instant::now();
    app.rebuild_pending = false;
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
                || c.show_tools != app.show_tools
                || c.show_thoughts != app.show_thoughts
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
                update_transcript_cache(app, id, inner_w, area.width);
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

    // Always show an animated indicator while the selected session is busy with no tool
    // in flight, even before the first token arrives. Rendering here keeps the markdown
    // cache deterministic for unit tests.
    let live_text = selected
        .as_ref()
        .and_then(|id| {
            app.state
                .sessions
                .get(id)
                .filter(|s| s.status == SessionStatus::Busy && s.active_tools.is_empty())
                .map(|_| id)
        })
        .filter(|_| app.transcript_scroll.is_none())
        .map(|id| {
            let preview = app
                .state
                .transcripts
                .get(id)
                .and_then(|buf| buf.back())
                .and_then(|line| crate::app::strip_thought_prefix(line))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(60).collect::<String>());
            match preview {
                Some(p) => format!("{} {} · {}", spinner_frame(), tr!("思考中"), p),
                None => format!("{} {}", spinner_frame(), tr!("思考中...")),
            }
        });

    let content_viewport = if live_text.is_some() {
        viewport.saturating_sub(1)
    } else {
        viewport
    };
    let max_off = total.saturating_sub(content_viewport);
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
            if s.role != ertangle_core::Role::Worker || s.parent.is_some() {
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
            let mut spans = vec![Span::styled(
                truncate(&text, inner_w.max(8)),
                Style::default().fg(Color::DarkGray),
            )];
            if !s.bg_procs.is_empty() {
                let count = s.bg_procs.len();
                let name = &s.bg_procs[0].name;
                let bg_label = if count > 1 {
                    format!(" · bg:{count} ({name}…)")
                } else {
                    format!(" · bg:{count} ({name})")
                };
                let bg_color = if s.status == SessionStatus::Idle {
                    Color::Yellow
                } else {
                    Color::DarkGray
                };
                spans.push(Span::styled(bg_label, Style::default().fg(bg_color)));
            }
            Line::from(spans)
        }
        None => Line::default(),
    };

    let selection = app.selection;
    let mut visible: Vec<Line> = Vec::new();
    if let Some(c) = &app.md_cache {
        for (idx, line) in c.lines.iter().enumerate().skip(off).take(content_viewport) {
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

    if let Some(text) = live_text {
        if visible.len() < viewport {
            let live = Line::styled(
                text,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::ITALIC),
            );
            visible.push(live);
        }
    }

    let para = Paragraph::new(visible).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(footer),
    );
    f.render_widget(para, area);
    app.transcript_offset = off;

    if total > content_viewport {
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
    if l.starts_with("▸ ")
        || l.starts_with("+++")
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
    let file_part = crate::diff::current_file_progress(app)
        .map(|(idx, total, path)| format!("· [{idx}/{total}] {path}  "))
        .unwrap_or_default();
    let title = match app.selected_id() {
        Some(id) => {
            let s = &app.state.sessions[&id];
            tr!(
                " Diff: {label} ({branch})  {file_part}+{added} -{removed}  ·  j/k 選行 · {{/}} hunk · (/) 檔案 · Enter 摺疊 · X 退回 · c 註解 · M 合併 ",
                label = s.label,
                branch = s.branch.as_deref().unwrap_or("main"),
                file_part = file_part,
                added = app.diff_added,
                removed = app.diff_removed,
            )
        }
        None => {
            if file_part.is_empty() {
                " Diff ".into()
            } else {
                format!(" Diff {file_part}")
            }
        }
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

pub(crate) fn draw_input(f: &mut TuiFrame, area: Rect, app: &mut App) {
    app.input_area = area;
    let nudging = app.mode == Mode::Normal && app.input_nudging();
    let keys = app.input_key_hint();
    let title = match app.mode {
        Mode::Normal if nudging => tr!(" 請先按 {keys} 才能輸入 ", keys = keys),
        Mode::Normal => tr!(" 輸入 (i 輸入 · ? 說明) "),
        Mode::Help => tr!(" 說明 "),
        Mode::Settings => tr!(" 設定 "),
        Mode::Qr => tr!(" QR Code "),
        Mode::Quota => tr!(" 額度 "),
        Mode::Ps => tr!(" 處理程序 "),
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
                " spawn → {target} · role={role} · worktree={worktree} · 權限={permission} · Ctrl-R role · Ctrl-F effort ",
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
                role = app.spawn_role.id(),
                worktree = if app.spawn_worktree { "on" } else { "off" },
                permission = app
                    .spawn_permission
                    .map_or_else(|| tr!("預設"), |p| p.id().to_string()),
            ),
            None => tr!(" spawn：<agent[:model]> <prompt> · Ctrl-R role · Ctrl-F effort "),
        },
        Mode::Menu => tr!(" 選單 "),
        Mode::Pick => tr!(" 選擇 (Enter 確認 · Esc 取消) "),
        Mode::Message => tr!(" message → 選取的 session "),
        Mode::Review => tr!(" review 註解 → 選取的 session "),
        Mode::Command => tr!(" 命令 (Tab 自動完成) "),
        Mode::Jump => tr!(" 跳轉 session "),
        Mode::Search => tr!(" 搜尋 (Enter／↓ 下一個 · ↑ 上一個 · Esc 離開) "),
    };
    let style = if nudging {
        Style::default().fg(Color::Yellow)
    } else if app.mode == Mode::Normal {
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
    let sel = crate::inputbox::selection_range(app.input_sel);
    let mut visible: Vec<Line> = rows
        .iter()
        .skip(first)
        .take(visible_rows)
        .map(|r| crate::inputbox::row_line(&app.input, *r, sel))
        .collect();

    // `Normal` 模式沒有游標可打字時，留一行提示，避免使用者以為程式沒反應。
    if app.input.is_empty() && app.mode == Mode::Normal {
        let (hint, color) = if nudging {
            (
                tr!("請先按 {keys} 才能輸入文字", keys = keys),
                Color::Yellow,
            )
        } else {
            (
                tr!("按 {keys} 開始輸入，? 看說明", keys = keys),
                Color::DarkGray,
            )
        };
        visible = vec![Line::styled(hint, Style::default().fg(color))];
    }

    let border = if nudging {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let para = Paragraph::new(visible).style(style).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(title),
    );
    f.render_widget(para, area);

    // 把終端機游標放到輸入框內，IME 的 preedit 就會錨定在這裡。
    // `Normal` 模式沒有游標，配合提示文字讓使用者知道要先按 i 或 Enter。
    if app.mode != Mode::Normal && area.width > 2 && area.height > 2 {
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
        CenterView::Transcript => tr!("  i 輸入  ? 說明  Space 選單  : 命令  q 離開"),
        CenterView::Diff => tr!("  ? 說明  c 註解  M 合併  q 離開"),
        CenterView::Worktrees => tr!("  ? 說明  M 合併  G 清理  q 離開"),
        CenterView::Tasks => tr!("  j/k 選任務  Enter 跳到負責人  :task-status <狀態>  ? 說明"),
        CenterView::Flow => {
            tr!("  j/k 選 session  Enter 開逐字稿  l 圖／清單  Esc/F 返回  E 顯示已結束")
        }
        CenterView::Grid => tr!("  選 session 切換窗格 (每頁 4 個)  Enter/V 回單一逐字稿  ? 說明"),
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
