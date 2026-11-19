//! `:quota` 額度浮層：列出本機可用的 agent 與各額度視窗的用量 progress bar。

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use termade_core::{format_remaining, AgentKind, Quota};
use tr::tr;

use crate::app::{App, Mode};
use crate::TuiFrame;

/// progress bar 的格數。
const BAR_CELLS: usize = 20;

/// 開啟浮層時偵測到的 agent (偵測要查 PATH，只在開啟時做一次)。
pub(crate) fn open(app: &mut App) {
    let mut agents: Vec<String> = AgentKind::builtins()
        .iter()
        .filter(|k| termade_adapter::supports_structured(k))
        .map(|k| k.id().to_string())
        .collect();
    // 有回報過額度但不在本機 (例如遠端 daemon) 的 agent 也列出來。
    for id in app.state.quotas.keys() {
        if !agents.contains(id) {
            agents.push(id.clone());
        }
    }
    app.quota_agents = agents;
    app.mode = Mode::Quota;
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    if matches!(
        key.code,
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char(' ')
    ) {
        app.mode = Mode::Normal;
    }
}

/// 用量對應的顏色：<70% 綠、<90% 黃、其餘紅。
fn level_color(pct: f32) -> Color {
    if pct >= 90.0 {
        Color::Red
    } else if pct >= 70.0 {
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

/// 一個 agent 的區塊 (標題列 + 每個視窗一列)。
pub(crate) fn agent_lines(agent: &str, quota: Option<&Quota>, now_ms: u64) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut head = vec![Span::styled(
        agent.to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    let Some(q) = quota else {
        head.push(Span::styled(
            format!("  {}", tr!("此 agent 尚未回報額度資料")),
            Style::default().fg(Color::DarkGray),
        ));
        out.push(Line::from(head));
        return out;
    };
    if q.reached {
        head.push(Span::styled(
            format!("  {}", tr!("✗額度已滿")),
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
            "  {}",
            tr!(
                "{ago} 前更新",
                ago = format_remaining(now_ms.saturating_sub(q.updated_ms))
            )
        ),
        Style::default().fg(Color::DarkGray),
    ));
    out.push(Line::from(head));
    for w in &q.windows {
        let reset = match w.resets_at_ms {
            Some(r) if r <= now_ms => Some(tr!("已重置")),
            Some(r) => Some(tr!("{t} 後重置", t = format_remaining(r - now_ms))),
            None => None,
        };
        // 已過重置時間的視窗用量已歸零，不再顯示舊的百分比。
        let pct = match w.resets_at_ms {
            Some(r) if r <= now_ms => 0.0,
            _ => w.used_percent,
        };
        let (fill, empty) = bar(pct, BAR_CELLS);
        let color = level_color(pct);
        let mut spans = vec![
            Span::raw(format!("  {:<4}", w.name)),
            Span::styled(fill, Style::default().fg(color)),
            Span::styled(empty, Style::default().fg(Color::DarkGray)),
            Span::styled(format!(" {pct:>3.0}%"), Style::default().fg(color)),
        ];
        if let Some(r) = reset {
            spans.push(Span::styled(
                format!("  {r}"),
                Style::default().fg(Color::Gray),
            ));
        }
        out.push(Line::from(spans));
    }
    out
}

pub(crate) fn draw(f: &mut TuiFrame, app: &App) {
    let now = termade_core::now_millis();
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.quota_agents.is_empty() {
        lines.push(Line::from(Span::styled(
            tr!("找不到已安裝的 agent"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    for (i, agent) in app.quota_agents.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.extend(agent_lines(agent, app.state.quotas.get(agent), now));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        tr!("按 Esc、Enter、Space 或 q 關閉"),
        Style::default().fg(Color::DarkGray),
    )));

    let full = f.area();
    let w = 72.min(full.width);
    let h = (lines.len() as u16 + 2).min(full.height);
    let area = Rect::new(
        full.x + full.width.saturating_sub(w) / 2,
        full.y + full.height.saturating_sub(h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            format!(" {} ", tr!("Agent 額度")),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    f.render_widget(Paragraph::new(lines).block(block), area);
}
