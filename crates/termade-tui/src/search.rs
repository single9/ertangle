//! 在逐字稿與 diff 裡搜尋（`f` 或 `:find <文字>`）。
//!
//! 搜尋的對象是畫面上實際顯示的行：逐字稿是 `App.md_cache` 裡渲染、換行後的行，
//! diff 是 `App.diff_lines`。符合項的索引可以直接當捲動位置或游標行用。
//! 被摺疊的工具呼叫、隱藏的思考行、已被捨棄的舊內容都不在範圍內；
//! 跨越自動換行處的字串也找不到。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Style};
use ratatui::text::Line;

use crate::app::{App, CenterView, Mode};
use crate::input::{backspace_input, byte_index, delete_input, insert_input};

pub(crate) struct Search {
    pub(crate) query: String,
    /// 符合的顯示行索引（遞增）。
    pub(crate) matches: Vec<usize>,
    /// `matches` 裡目前所在的那一個。
    pub(crate) current: usize,
    /// 開始搜尋時的位置；邊打邊找時每次都從這裡往後找。
    anchor: usize,
}

impl Search {
    pub(crate) fn current_line(&self) -> Option<usize> {
        self.matches.get(self.current).copied()
    }
}

/// smartcase 子字串比對：查詢全小寫時不分大小寫，含大寫時區分。
pub(crate) fn find_matches<'a>(lines: impl Iterator<Item = &'a str>, query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let sensitive = query.chars().any(char::is_uppercase);
    lines
        .enumerate()
        .filter(|(_, l)| {
            if sensitive {
                l.contains(query)
            } else {
                l.to_lowercase().contains(query)
            }
        })
        .map(|(i, _)| i)
        .collect()
}

fn line_text(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// 目前視圖裡符合 `query` 的顯示行。
fn compute(app: &App, query: &str) -> Vec<usize> {
    match app.view {
        CenterView::Transcript => {
            let texts: Vec<String> = app
                .md_cache
                .as_ref()
                .map(|c| c.lines.iter().map(line_text).collect())
                .unwrap_or_default();
            find_matches(texts.iter().map(String::as_str), query)
        }
        CenterView::Diff => find_matches(app.diff_lines.iter().map(String::as_str), query),
        CenterView::Worktrees | CenterView::Tasks => Vec::new(),
    }
}

/// 進入搜尋模式。`prefill` 為 `None` 時預填上一次的查詢。
pub(crate) fn open(app: &mut App, prefill: Option<&str>) {
    let anchor = match app.view {
        CenterView::Transcript => app.transcript_offset,
        CenterView::Diff => app.diff_cursor,
        CenterView::Worktrees | CenterView::Tasks => {
            app.status = "這個視圖不支援搜尋（逐字稿與 Diff 才有）".into();
            return;
        }
    };
    app.mode = Mode::Search;
    app.input = prefill.map_or_else(|| app.last_search.clone(), str::to_string);
    app.input_cursor = app.input.chars().count();
    app.search = Some(Search {
        query: String::new(),
        matches: Vec::new(),
        current: 0,
        anchor,
    });
    query_changed(app);
}

/// 查詢文字變了：重算符合項，跳到開始位置之後的第一個（沒有就繞回第一個）。
/// 沒有符合項時不動捲動位置。
pub(crate) fn query_changed(app: &mut App) {
    let query = app.input.clone();
    let matches = compute(app, &query);
    let Some(s) = app.search.as_mut() else { return };
    s.current = matches.iter().position(|&l| l >= s.anchor).unwrap_or(0);
    s.matches = matches;
    s.query = query;
    jump(app);
}

/// 顯示內容變了（新輸出、寬度改變、換 session、diff 重新載入）：重算符合項，
/// 停在原本那一行或它之後最近的一個，不捲動畫面。
pub(crate) fn refresh(app: &mut App) {
    let Some(query) = app.search.as_ref().map(|s| s.query.clone()) else {
        return;
    };
    let matches = compute(app, &query);
    let Some(s) = app.search.as_mut() else { return };
    let old = s.current_line().unwrap_or(0);
    s.current = matches
        .iter()
        .position(|&l| l >= old)
        .unwrap_or(matches.len().saturating_sub(1));
    s.matches = matches;
}

/// 跳到下一個／上一個符合項，到頭後繞回另一端。
pub(crate) fn step(app: &mut App, forward: bool) {
    let Some(s) = app.search.as_mut() else { return };
    let n = s.matches.len();
    if n == 0 {
        return;
    }
    s.current = if forward {
        (s.current + 1) % n
    } else {
        (s.current + n - 1) % n
    };
    jump(app);
}

fn jump(app: &mut App) {
    let Some(line) = app.search.as_ref().and_then(Search::current_line) else {
        return;
    };
    match app.view {
        // 等同手動捲動，會暫停跟隨最新。
        CenterView::Transcript => app.transcript_scroll = Some(line),
        CenterView::Diff => app.diff_cursor = line,
        CenterView::Worktrees | CenterView::Tasks => {}
    }
}

/// 離開搜尋：停在目前位置，記住查詢供下次預填。
pub(crate) fn close(app: &mut App) {
    if let Some(s) = app.search.take() {
        app.status = if s.query.is_empty() {
            "已離開搜尋".into()
        } else if s.matches.is_empty() {
            format!("找不到「{}」", s.query)
        } else {
            format!(
                "搜尋「{}」停在 {}/{}（f 再搜尋）",
                s.query,
                s.current + 1,
                s.matches.len()
            )
        };
        if !s.query.is_empty() {
            app.last_search = s.query;
        }
    }
    app.mode = Mode::Normal;
    app.input.clear();
    app.input_cursor = 0;
}

pub(crate) fn handle_search_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let before = app.input.clone();
    match key.code {
        KeyCode::Esc => return close(app),
        KeyCode::Char('c') if ctrl => return close(app),
        KeyCode::Enter | KeyCode::Down => return step(app, true),
        KeyCode::Up => return step(app, false),
        KeyCode::Char('u') if ctrl => {
            let b = byte_index(&app.input, app.input_cursor);
            app.input.drain(..b);
            app.input_cursor = 0;
        }
        KeyCode::Backspace => backspace_input(app),
        KeyCode::Delete => delete_input(app),
        KeyCode::Left => app.input_cursor = app.input_cursor.saturating_sub(1),
        KeyCode::Right => {
            app.input_cursor = (app.input_cursor + 1).min(app.input.chars().count());
        }
        KeyCode::Home => app.input_cursor = 0,
        KeyCode::End => app.input_cursor = app.input.chars().count(),
        KeyCode::Char(c) if !ctrl => insert_input(app, c),
        _ => {}
    }
    if app.input != before {
        query_changed(app);
    }
}

/// 搜尋模式下狀態列要顯示的文字（`3/17`、「找不到」）。
pub(crate) fn status_text(app: &App) -> Option<String> {
    let s = app.search.as_ref()?;
    Some(if s.query.is_empty() {
        "搜尋：輸入文字（只找畫面上顯示的內容）".into()
    } else if s.matches.is_empty() {
        format!("搜尋：找不到「{}」", s.query)
    } else {
        format!("搜尋 {}/{}", s.current + 1, s.matches.len())
    })
}

/// 第 `idx` 行是否為符合項：`Some(true)` 是目前這一個，`Some(false)` 是其他符合項。
pub(crate) fn mark(app: &App, idx: usize) -> Option<bool> {
    let s = app.search.as_ref()?;
    let pos = s.matches.binary_search(&idx).ok()?;
    Some(pos == s.current)
}

/// 符合行的底色；目前這一個用更醒目的顏色。
pub(crate) fn mark_style(current: bool) -> Style {
    if current {
        Style::default().fg(Color::Black).bg(Color::Yellow)
    } else {
        Style::default().bg(Color::Rgb(70, 70, 120))
    }
}

/// 整行套上符合項的底色。
pub(crate) fn highlight(line: &Line<'static>, current: bool) -> Line<'static> {
    let style = mark_style(current);
    let mut out = line.clone();
    for span in &mut out.spans {
        span.style = span.style.patch(style);
    }
    out
}
