//! `:quota` 額度浮層：列出本機可用的 agent 與各額度視窗的用量 progress bar。

use crossterm::event::{KeyCode, KeyEvent};
use ertangle_core::{format_remaining, AgentKind, Quota};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use tr::tr;

use crate::app::{App, Mode};
use crate::TuiFrame;

/// progress bar 的格數。
const BAR_CELLS: usize = 20;

/// 開啟浮層時偵測到的 agent (偵測要查 PATH，只在開啟時做一次)。
pub(crate) fn open(app: &mut App) {
    let mut agents: Vec<String> = AgentKind::builtins()
        .iter()
        .filter(|k| ertangle_adapter::is_installed(k))
        .map(|k| k.id().to_string())
        .collect();
    // 有回報過額度但不在本機 (例如遠端 daemon) 的 agent 也列出來。
    for id in app.state.quotas.keys() {
        if !agents.contains(id) {
            agents.push(id.clone());
        }
    }
    // 已回報額度的 agent 排在前面，尚未回報資料者 (如 opencode) 往後排。
    agents.sort_by_key(|id| !app.state.quotas.contains_key(id));
    app.quota_agents = agents;
    app.quota_tab = 0;
    app.mode = Mode::Quota;
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    let tabs = app.quota_agents.len().saturating_add(1);
    match key.code {
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
            app.quota_tab = (app.quota_tab + 1) % tabs;
        }
        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
            app.quota_tab = (app.quota_tab + tabs - 1) % tabs;
        }
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char(' ') => {
            app.mode = Mode::Normal;
        }
        _ => {}
    }
}

/// 剩餘量對應的顏色：≤10% 紅、≤30% 黃、其餘綠。
fn level_color_remaining(rem: f32) -> Color {
    if rem <= 10.0 {
        Color::Red
    } else if rem <= 30.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

/// `██████░░░░` 形式的 progress bar (四捨五入到格，超出範圍會夾住)。
pub(crate) fn bar(pct: f32, cells: usize) -> (String, String) {
    let filled = ((pct.clamp(0.0, 100.0) / 100.0) * cells as f32).round() as usize;
    ("█".repeat(filled), "░".repeat(cells - filled))
}

/// 將內部視窗代號轉成易讀的顯示名稱 (例如 Antigravity 的 `3p-*` 標示為 `Claude / GPT`)。
fn window_label(agent: &str, name: &str) -> String {
    if agent == "antigravity" {
        return match name {
            "5h" => "Gemini (5h)".into(),
            "7d" => "Gemini (7d)".into(),
            "argon-5h" => "Gemini 4 Argon (5h)".into(),
            "argon-7d" => "Gemini 4 Argon (7d)".into(),
            "3p-5h" => "Claude / GPT (5h)".into(),
            "3p-7d" => "Claude / GPT (7d)".into(),
            other => other.into(),
        };
    }
    name.into()
}

/// 一個 agent 的區塊 (標題列 + 每個視窗一列)。
pub(crate) fn agent_lines(agent: &str, quota: Option<&Quota>, now_ms: u64) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let Some(q) = quota else {
        out.push(Line::from(vec![
            Span::styled(
                agent.to_string(),
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", tr!("此 agent 尚未回報額度資料")),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        return out;
    };
    let mut head = vec![Span::styled(
        agent.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    if q.is_reached(now_ms) {
        head.push(Span::styled(
            format!("  {}", tr!("✗ 額度已滿")),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    } else if q.warning {
        head.push(Span::styled(
            format!("  {}", tr!("⚠ 接近上限")),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    head.push(Span::styled(
        format!(
            "  ·  {}",
            tr!(
                "{ago} 前更新",
                ago = format_remaining(now_ms.saturating_sub(q.updated_ms))
            )
        ),
        Style::default().fg(Color::DarkGray),
    ));
    out.push(Line::from(head));
    let labels: Vec<String> = q
        .windows
        .iter()
        .map(|w| window_label(agent, &w.name))
        .collect();
    let name_w = labels
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    for (w, label) in q.windows.iter().zip(labels.iter()) {
        let reset = match w.resets_at_ms {
            Some(r) if r <= now_ms => Some(tr!("已重置")),
            Some(r) => Some(tr!(
                "{t} 後重置",
                t = format_remaining(r.saturating_sub(now_ms))
            )),
            None => None,
        };
        // 已過重置時間的視窗視為已恢復 100% 剩餘額度。
        let rem = match w.resets_at_ms {
            Some(r) if r <= now_ms => 100.0,
            _ => (100.0 - w.used_percent).clamp(0.0, 100.0),
        };
        let (fill, empty) = bar(rem, BAR_CELLS);
        let color = level_color_remaining(rem);
        let pct_label = tr!("剩 {pct}%", pct = format!("{rem:>3.0}"));
        let mut spans = vec![
            Span::styled(
                format!("  {label:<width$}  ", width = name_w),
                Style::default().fg(Color::White),
            ),
            Span::styled(fill, Style::default().fg(color)),
            Span::styled(empty, Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("  {pct_label}"),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ];
        if let Some(r) = reset {
            spans.push(Span::styled(
                format!("  ·  {r}"),
                Style::default().fg(Color::Gray),
            ));
        }
        out.push(Line::from(spans));
    }
    out
}

pub(crate) fn draw(f: &mut TuiFrame, app: &App) {
    let now = ertangle_core::now_millis();
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.quota_agents.is_empty() {
        lines.push(Line::from(Span::styled(
            tr!("找不到已安裝的 agent"),
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let active = app.quota_tab.min(app.quota_agents.len());
        let mut tab_spans: Vec<Span<'static>> = Vec::new();
        for idx in 0..=app.quota_agents.len() {
            if idx > 0 {
                tab_spans.push(Span::raw(" "));
            }
            let title = if idx == 0 {
                tr!("全部")
            } else {
                app.quota_agents[idx - 1].clone()
            };
            let style = if idx == active {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            tab_spans.push(Span::styled(format!(" {title} "), style));
        }
        lines.push(Line::from(tab_spans));
        lines.push(Line::from(""));

        let visible: Vec<&String> = if active == 0 {
            app.quota_agents.iter().collect()
        } else {
            vec![&app.quota_agents[active - 1]]
        };
        for (i, agent) in visible.iter().enumerate() {
            if i > 0 {
                lines.push(Line::from(""));
            }
            lines.extend(agent_lines(agent, app.state.quotas.get(*agent), now));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        tr!("Tab / ← → 切換 agent · Esc / q 關閉"),
        Style::default().fg(Color::DarkGray),
    )));

    let full = f.area();
    let w = 78.min(full.width);
    let h = (lines.len() as u16 + 4).min(full.height);
    let area = Rect::new(
        full.x + full.width.saturating_sub(w) / 2,
        full.y + full.height.saturating_sub(h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::new(2, 2, 1, 1))
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            format!(" {} ", tr!("Agent 額度 (剩餘)")),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    f.render_widget(Paragraph::new(lines).block(block), area);
}
