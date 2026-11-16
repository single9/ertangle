//! 極簡 Markdown 渲染器 (零外部相依性)。
//!
//! 把 agent 的逐字稿純文字轉成 Ratatui 的 [`Line`] / [`Span`]，支援：
//! 標題、粗體／斜體／刪除線、行內程式碼、連結、無序／有序／工作清單、
//! 引言、分隔線、圍籬程式碼區塊 (含語言標籤與底色)、GFM 表格。
//!
//! 也認得 termade 自己在逐字稿裡加的語意前綴：`❯ ` (使用者 prompt)、
//! `💭 ` (思考)、`▸ ` (工具呼叫)、`💬 ` (審查註解)，各自用不同顏色呈現。
//!
//! 設計上刻意保持「行導向」：一次 `render` 會把整段來源重新渲染，
//! 因此圍籬／表格狀態不需要跨次呼叫儲存。

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const CODE_FG: Color = Color::Rgb(214, 216, 228);
pub const CODE_BG: Color = Color::Rgb(30, 30, 40);
pub const CODE_BAR: Color = Color::Rgb(86, 122, 186);
pub const THOUGHT: Color = Color::Rgb(128, 132, 152);
pub const TOOL: Color = Color::Rgb(224, 184, 96);
pub const REVIEW: Color = Color::Rgb(198, 122, 206);
pub const PROMPT: Color = Color::Rgb(126, 222, 160);
pub const LINK: Color = Color::Rgb(112, 172, 255);
pub const HEADING: Color = Color::Rgb(122, 202, 255);
pub const BULLET: Color = Color::Rgb(122, 202, 255);
pub const QUOTE: Color = Color::Rgb(122, 182, 122);
pub const RULE: Color = Color::Rgb(80, 84, 100);

/// 逐字稿中的可點擊連結 (包含行索引、顯示欄位範圍與目標 URL)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkTarget {
    pub line: usize,
    pub col_start: usize,
    pub col_end: usize,
    pub url: String,
}

/// 逐行 Markdown 渲染器。
#[derive(Default)]
pub struct Renderer {
    in_fence: bool,
}

impl Renderer {
    pub fn new() -> Self {
        Self { in_fence: false }
    }

    /// 渲染並切換換行，同時回傳所有精準欄位對齊的連結目標。
    pub fn render_and_wrap(
        &mut self,
        src: &[String],
        width: usize,
        base_line_idx: usize,
    ) -> (Vec<Line<'static>>, Vec<LinkTarget>) {
        let lines_with_links = self.render_with_links(src, width);
        wrap_lines_and_collect_links(lines_with_links, width, base_line_idx)
    }

    /// 渲染並包含每行內部的連結欄位區間。
    pub fn render_with_links(
        &mut self,
        src: &[String],
        width: usize,
    ) -> Vec<(Line<'static>, Vec<(usize, usize, String)>)> {
        self.in_fence = false;
        let mut out = Vec::with_capacity(src.len());
        let mut i = 0usize;
        while i < src.len() {
            let line = &src[i];

            if self.in_fence {
                if fence(line).is_some() {
                    self.in_fence = false;
                    out.push((Line::styled("  ▌ ```", Style::default().fg(CODE_BAR)), Vec::new()));
                } else {
                    for cl in code_line(line, width) {
                        out.push((cl, Vec::new()));
                    }
                }
                i += 1;
                continue;
            }

            if let Some(lang) = fence(line) {
                self.in_fence = true;
                let label = if lang.is_empty() {
                    "code"
                } else {
                    lang.as_str()
                };
                out.push((
                    Line::from(vec![
                        Span::styled("  ▌ ", Style::default().fg(CODE_BAR)),
                        Span::styled(
                            format!("``` {label}"),
                            Style::default().fg(CODE_BAR).add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Vec::new(),
                ));
                i += 1;
                continue;
            }

            if is_table_row(line) && i + 1 < src.len() && is_table_sep(&src[i + 1]) {
                let mut block = vec![line.clone()];
                let mut j = i + 2;
                while j < src.len() && is_table_row(&src[j]) {
                    block.push(src[j].clone());
                    j += 1;
                }
                for tl in render_table(&block, &src[i + 1], width) {
                    out.push((tl, Vec::new()));
                }
                i = j;
                continue;
            }

            out.push(render_line_with_links(line, width));
            i += 1;
        }
        out
    }

    /// 把來源行渲染成樣式化行。`width` 是可用顯示寬度 (不含邊框)。
    #[allow(dead_code)]
    pub fn render(&mut self, src: &[String], width: usize) -> Vec<Line<'static>> {
        self.render_with_links(src, width)
            .into_iter()
            .map(|(l, _)| l)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// 行渲染
// ---------------------------------------------------------------------------

fn render_line_with_links(
    line: &str,
    width: usize,
) -> (Line<'static>, Vec<(usize, usize, String)>) {
    if line.trim().is_empty() {
        return (Line::raw(String::new()), Vec::new());
    }
    if let Some(rest) = line.strip_prefix("❯ ") {
        return prefixed_with_links("❯ ", rest, PROMPT, false);
    }
    if let Some(rest) = line.strip_prefix("💭 ") {
        return prefixed_with_links("💭 ", rest, THOUGHT, true);
    }
    if let Some(rest) = line.strip_prefix("▸ ") {
        return (tool_line(rest), Vec::new());
    }
    if let Some(rest) = line.strip_prefix("💬 ") {
        return prefixed_with_links("💬 ", rest, REVIEW, false);
    }
    if let Some((level, rest)) = heading(line) {
        return heading_line_with_links(level, rest);
    }
    if is_hr(line) {
        return (
            Line::styled("─".repeat(width.max(1)), Style::default().fg(RULE)),
            Vec::new(),
        );
    }
    if let Some(rest) = line.strip_prefix("> ") {
        let prefix_span = Span::styled("▏ ", Style::default().fg(QUOTE));
        let prefix_w = prefix_span.content.width();
        let (inline_spans, inline_links) = inline_with_links(
            rest,
            Style::default().fg(QUOTE).add_modifier(Modifier::ITALIC),
        );
        let mut spans = vec![prefix_span];
        spans.extend(inline_spans);
        let shifted = inline_links
            .into_iter()
            .map(|(s, e, u)| (s + prefix_w, e + prefix_w, u))
            .collect();
        return (Line::from(spans), shifted);
    }
    if line.trim() == ">" {
        return (Line::styled("▏", Style::default().fg(QUOTE)), Vec::new());
    }
    if let Some((l, links)) = list_line_with_links(line) {
        return (l, links);
    }
    let (spans, links) = inline_with_links(line, Style::default());
    (Line::from(spans), links)
}

fn prefixed_with_links(
    prefix: &str,
    rest: &str,
    color: Color,
    italic: bool,
) -> (Line<'static>, Vec<(usize, usize, String)>) {
    let mut base = Style::default().fg(color);
    if italic {
        base = base.add_modifier(Modifier::ITALIC);
    }
    let p_span = Span::styled(
        prefix.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    );
    let p_w = p_span.content.width();
    let (inline_spans, inline_links) = inline_with_links(rest, base);
    let mut spans = vec![p_span];
    spans.extend(inline_spans);
    let shifted = inline_links
        .into_iter()
        .map(|(s, e, u)| (s + p_w, e + p_w, u))
        .collect();
    (Line::from(spans), shifted)
}

fn tool_line(rest: &str) -> Line<'static> {
    let bold = Style::default().fg(TOOL).add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled("▸ ", bold)];
    let mut it = rest.splitn(2, ' ');
    let name = it.next().unwrap_or("");
    let tail = it.next().unwrap_or("");
    spans.push(Span::styled(name.to_string(), bold));
    if !tail.is_empty() {
        spans.push(Span::raw(" "));
        spans.extend(inline(tail, Style::default().fg(TOOL)));
    }
    Line::from(spans)
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &t[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((hashes, rest.trim_start()))
}

fn heading_line_with_links(
    level: usize,
    rest: &str,
) -> (Line<'static>, Vec<(usize, usize, String)>) {
    let style = match level {
        1 => Style::default()
            .fg(HEADING)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        2 => Style::default().fg(HEADING).add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(HEADING)
            .add_modifier(Modifier::BOLD | Modifier::DIM),
    };
    let p_span = Span::styled("▎ ", Style::default().fg(HEADING));
    let p_w = p_span.content.width();
    let (inline_spans, inline_links) = inline_with_links(rest, style);
    let mut spans = vec![p_span];
    spans.extend(inline_spans);
    let shifted = inline_links
        .into_iter()
        .map(|(s, e, u)| (s + p_w, e + p_w, u))
        .collect();
    (Line::from(spans), shifted)
}

fn is_hr(line: &str) -> bool {
    let t = line.trim();
    if t.len() < 3 {
        return false;
    }
    let mut ch: Option<char> = None;
    for c in t.chars() {
        if c == ' ' {
            continue;
        }
        match ch {
            None if c == '-' || c == '*' || c == '_' => ch = Some(c),
            Some(x) if x == c => {}
            _ => return false,
        }
    }
    ch.is_some()
}

fn list_line_with_links(line: &str) -> Option<(Line<'static>, Vec<(usize, usize, String)>)> {
    let indent_len = line.len() - line.trim_start().len();
    let indent = " ".repeat(indent_len);
    let t = line.trim_start();

    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(m) {
            let (mark, body) = task_marker(rest);
            let s1 = Span::raw(indent);
            let s2 = Span::styled(format!("{mark} "), Style::default().fg(BULLET));
            let prefix_w = s1.content.width() + s2.content.width();
            let (inline_spans, inline_links) = inline_with_links(body, Style::default());
            let mut spans = vec![s1, s2];
            spans.extend(inline_spans);
            let shifted = inline_links
                .into_iter()
                .map(|(s, e, u)| (s + prefix_w, e + prefix_w, u))
                .collect();
            return Some((Line::from(spans), shifted));
        }
    }

    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let after = &t[digits.len()..];
        if let Some(body) = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
        {
            let s1 = Span::raw(indent);
            let s2 = Span::styled(format!("{digits}. "), Style::default().fg(BULLET));
            let prefix_w = s1.content.width() + s2.content.width();
            let (inline_spans, inline_links) = inline_with_links(body, Style::default());
            let mut spans = vec![s1, s2];
            spans.extend(inline_spans);
            let shifted = inline_links
                .into_iter()
                .map(|(s, e, u)| (s + prefix_w, e + prefix_w, u))
                .collect();
            return Some((Line::from(spans), shifted));
        }
    }
    None
}

fn task_marker(rest: &str) -> (&'static str, &str) {
    if let Some(r) = rest.strip_prefix("[ ] ") {
        return ("☐", r);
    }
    if let Some(r) = rest
        .strip_prefix("[x] ")
        .or_else(|| rest.strip_prefix("[X] "))
    {
        return ("☑", r);
    }
    ("•", rest)
}

// ---------------------------------------------------------------------------
// 圍籬程式碼
// ---------------------------------------------------------------------------

/// 若此行是圍籬 (``` 或 ~~~)則回傳語言標籤 (可為空)。
fn fence(line: &str) -> Option<String> {
    let t = line.trim_start();
    let rest = t.strip_prefix("```").or_else(|| t.strip_prefix("~~~"))?;
    Some(rest.trim().to_string())
}

fn code_line(line: &str, width: usize) -> Vec<Line<'static>> {
    let budget = width.saturating_sub(4).max(8);
    let chunks = split_display_width(if line.is_empty() { " " } else { line }, budget);
    chunks
        .into_iter()
        .map(|s| {
            let pad = budget.saturating_sub(UnicodeWidthStr::width(s.as_str()));
            Line::from(vec![
                Span::styled("  ▌ ", Style::default().fg(CODE_BAR)),
                Span::styled(
                    format!("{s}{}", " ".repeat(pad)),
                    Style::default().fg(CODE_FG).bg(CODE_BG),
                ),
            ])
        })
        .collect()
}

/// 依顯示寬度把字串切成多段 (用於程式碼區塊)。
fn split_display_width(s: &str, budget: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w > 0 && w + cw > budget {
            out.push(std::mem::take(&mut cur));
            w = 0;
        }
        cur.push(ch);
        w += cw;
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

// ---------------------------------------------------------------------------
// 表格
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Right,
    Center,
}

fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 2 && t.contains('|')
}

fn is_table_sep(line: &str) -> bool {
    let cells = split_row(line);
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim();
            let c = c.trim_start_matches(':').trim_end_matches(':');
            !c.is_empty() && c.chars().all(|ch| ch == '-')
        })
}

fn split_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = if t.starts_with("\\|") {
        t
    } else {
        t.strip_prefix('|').unwrap_or(t)
    };
    let t = if t.ends_with("\\|") {
        t
    } else {
        t.strip_suffix('|').unwrap_or(t)
    };
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut in_code = false;
    let mut escaped = false;
    for ch in t.chars() {
        if escaped {
            cur.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => {
                cur.push(ch);
                escaped = true;
            }
            '`' => {
                in_code = !in_code;
                cur.push(ch);
            }
            '|' if !in_code => {
                cells.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

fn parse_align(sep: &str, n: usize) -> Vec<Align> {
    let mut v: Vec<Align> = split_row(sep)
        .iter()
        .map(|c| {
            let l = c.starts_with(':');
            let r = c.ends_with(':');
            match (l, r) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            }
        })
        .collect();
    v.resize(n, Align::Left);
    v
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}

/// 依顯示寬度裁切 spans (保留樣式)，超出時以 `…` 收尾。
fn truncate_spans(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    if spans_width(&spans) <= max {
        return spans;
    }
    if max == 0 {
        return Vec::new();
    }
    let budget = max - 1;
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let mut buf = String::new();
        let mut full = false;
        for ch in span.content.chars() {
            let cw = ch.width().unwrap_or(0);
            if used + cw > budget {
                full = true;
                break;
            }
            buf.push(ch);
            used += cw;
        }
        if !buf.is_empty() {
            out.push(Span::styled(buf, span.style));
        }
        if full || used >= budget {
            break;
        }
    }
    out.push(Span::styled("…".to_string(), Style::default().fg(RULE)));
    out
}

fn render_table(rows: &[String], sep: &str, width: usize) -> Vec<Line<'static>> {
    let header = split_row(&rows[0]);
    let n = header.len().max(1);
    let aligns = parse_align(sep, n);
    let body: Vec<Vec<String>> = rows[1..].iter().map(|r| split_row(r)).collect();
    let header_style = Style::default().fg(HEADING).add_modifier(Modifier::BOLD);

    let mut widths = vec![1usize; n];
    for (c, cell) in header.iter().enumerate().take(n) {
        widths[c] = widths[c].max(spans_width(&inline(cell, header_style)));
    }
    for row in &body {
        for (c, cell) in row.iter().enumerate().take(n) {
            widths[c] = widths[c].max(spans_width(&inline(cell, Style::default())));
        }
    }

    // 收斂到可用寬度：每欄至少 1，超出的從最寬的欄扣。
    let overhead = 3 * n + 1;
    let avail = width.saturating_sub(overhead).max(n);
    let mut total: usize = widths.iter().sum();
    while total > avail {
        let widest = widths
            .iter()
            .enumerate()
            .max_by_key(|(_, w)| **w)
            .map(|(i, _)| i);
        match widest {
            Some(idx) if widths[idx] > 1 => {
                widths[idx] -= 1;
                total -= 1;
            }
            _ => break,
        }
    }

    let mut out = vec![
        clip(table_row(&header, &widths, &aligns, true), width),
        clip(table_sep(&widths), width),
    ];
    for row in &body {
        out.push(clip(table_row(row, &widths, &aligns, false), width));
    }
    out
}

/// 硬裁切一行到 `width` 個顯示欄位 (超出時以 `…` 收尾)，保留各 span 樣式。
fn clip(line: Line<'static>, width: usize) -> Line<'static> {
    if spans_width(&line.spans) <= width {
        line
    } else {
        Line::from(truncate_spans(line.spans, width))
    }
}

fn table_row(cells: &[String], widths: &[usize], aligns: &[Align], header: bool) -> Line<'static> {
    let style = if header {
        Style::default().fg(HEADING).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let rule = Style::default().fg(RULE);
    let mut spans = Vec::new();
    for (c, w) in widths.iter().enumerate() {
        let cell = cells.get(c).map(String::as_str).unwrap_or("");
        let cell_spans = truncate_spans(inline(cell, style), *w);
        let cw = spans_width(&cell_spans);
        let pad = w.saturating_sub(cw);
        let (left, right) = match aligns[c] {
            Align::Left => (0, pad),
            Align::Right => (pad, 0),
            Align::Center => (pad / 2, pad - pad / 2),
        };
        spans.push(Span::styled("│ ", rule));
        if left > 0 {
            spans.push(Span::raw(" ".repeat(left)));
        }
        spans.extend(cell_spans);
        if right > 0 {
            spans.push(Span::raw(" ".repeat(right)));
        }
        spans.push(Span::styled(" ", rule));
    }
    spans.push(Span::styled("│", rule));
    Line::from(spans)
}

fn table_sep(widths: &[usize]) -> Line<'static> {
    let mut s = String::from("├");
    for (i, w) in widths.iter().enumerate() {
        s.push_str(&"─".repeat(w + 2));
        s.push(if i + 1 < widths.len() { '┼' } else { '┤' });
    }
    Line::styled(s, Style::default().fg(RULE))
}

// ---------------------------------------------------------------------------
// 行內樣式
// ---------------------------------------------------------------------------

/// 解析行內 Markdown (粗體、斜體、刪除線、程式碼、連結)，並回傳 (樣式化 Spans, 連結欄位區間)。
pub fn inline_with_links(
    text: &str,
    base: Style,
) -> (Vec<Span<'static>>, Vec<(usize, usize, String)>) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut links: Vec<(usize, usize, String)> = Vec::new();
    let mut cur_col = 0usize;
    let mut buf = String::new();
    let len = text.len();
    let mut i = 0usize;

    macro_rules! flush_buf {
        () => {
            if !buf.is_empty() {
                cur_col += buf.width();
                spans.push(Span::styled(std::mem::take(&mut buf), base));
            }
        };
    }

    while i < len {
        let rest = &text[i..];

        if rest.starts_with('\\') && rest.len() > 1 {
            let ch = rest[1..].chars().next().unwrap();
            buf.push(ch);
            i += 1 + ch.len_utf8();
            continue;
        }

        if rest.starts_with("**") || rest.starts_with("__") {
            let m = &rest[..2];
            if let Some(end) = rest[2..].find(m) {
                flush_buf!();
                let content = &rest[2..2 + end];
                cur_col += content.width();
                spans.push(Span::styled(
                    content.to_string(),
                    base.add_modifier(Modifier::BOLD),
                ));
                i += 2 + end + 2;
                continue;
            }
        }

        if let Some(after) = rest.strip_prefix("~~") {
            if let Some(end) = after.find("~~") {
                flush_buf!();
                let content = &after[..end];
                cur_col += content.width();
                spans.push(Span::styled(
                    content.to_string(),
                    base.add_modifier(Modifier::CROSSED_OUT),
                ));
                i += 2 + end + 2;
                continue;
            }
        }

        if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                flush_buf!();
                let content = &after[..end];
                cur_col += content.width();
                spans.push(Span::styled(
                    content.to_string(),
                    Style::default().fg(CODE_FG).bg(CODE_BG),
                ));
                i += 1 + end + 1;
                continue;
            }
        }

        if rest.starts_with('[') {
            if let Some(close) = rest.find("](") {
                if let Some(paren) = rest[close + 2..].find(')') {
                    flush_buf!();
                    let label = &rest[1..close];
                    let url = &rest[close + 2..close + 2 + paren];
                    let link_style = Style::default().fg(LINK).add_modifier(Modifier::UNDERLINED);
                    let display_text = if label.is_empty() { url } else { label };
                    let w = display_text.width();
                    let col_start = cur_col;
                    let col_end = cur_col + w;
                    cur_col += w;
                    spans.push(Span::styled(display_text.to_string(), link_style));
                    links.push((col_start, col_end, url.to_string()));
                    i += close + 2 + paren + 1;
                    continue;
                }
            }
        }

        if rest.starts_with('*') || rest.starts_with('_') {
            let m = &rest[..1];
            if let Some(end) = rest[1..].find(m) {
                if end > 0 {
                    flush_buf!();
                    let content = &rest[1..1 + end];
                    cur_col += content.width();
                    spans.push(Span::styled(
                        content.to_string(),
                        base.add_modifier(Modifier::ITALIC),
                    ));
                    i += 1 + end + 1;
                    continue;
                }
            }
        }

        let ch = rest.chars().next().unwrap();
        buf.push(ch);
        i += ch.len_utf8();
    }

    if !buf.is_empty() {
        spans.push(Span::styled(buf, base));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    (spans, links)
}

/// 解析行內 Markdown (粗體、斜體、刪除線、程式碼、連結)。
pub fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    inline_with_links(text, base).0
}

// ---------------------------------------------------------------------------
// 換行 (把已渲染的行依顯示寬度硬切)
// ---------------------------------------------------------------------------

/// 把單一樣式化行依顯示寬度切成多行，保留樣式，並映射連結欄位位置。
pub fn wrap_line_with_links(
    line: Line<'static>,
    line_links: &[(usize, usize, String)],
    start_line_idx: usize,
    width: usize,
) -> (Vec<Line<'static>>, Vec<LinkTarget>) {
    if width == 0 {
        let targets = line_links
            .iter()
            .map(|(s, e, u)| LinkTarget {
                line: start_line_idx,
                col_start: *s,
                col_end: *e,
                url: u.clone(),
            })
            .collect();
        return (vec![line], targets);
    }
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut targets: Vec<LinkTarget> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    let mut orig_col = 0usize;

    let mut active_link: Option<(usize, usize)> = None;

    for span in line.spans {
        let style = span.style;
        let mut buf = String::new();
        for ch in span.content.chars() {
            let cw = ch.width().unwrap_or(0);

            let matching_link_idx = line_links
                .iter()
                .position(|(s, e, _)| orig_col >= *s && orig_col < *e);

            if let Some(l_idx) = matching_link_idx {
                if active_link.is_none() {
                    active_link = Some((l_idx, cur_w));
                }
            } else if let Some((l_idx, w_start)) = active_link.take() {
                targets.push(LinkTarget {
                    line: start_line_idx + out.len(),
                    col_start: w_start,
                    col_end: cur_w,
                    url: line_links[l_idx].2.clone(),
                });
            }

            if cur_w > 0 && cur_w + cw > width {
                if let Some((l_idx, w_start)) = active_link.take() {
                    targets.push(LinkTarget {
                        line: start_line_idx + out.len(),
                        col_start: w_start,
                        col_end: cur_w,
                        url: line_links[l_idx].2.clone(),
                    });
                    active_link = Some((l_idx, 0));
                }
                if !buf.is_empty() {
                    cur.push(Span::styled(std::mem::take(&mut buf), style));
                }
                out.push(Line::from(std::mem::take(&mut cur)));
                cur_w = 0;
            }
            buf.push(ch);
            cur_w += cw;
            orig_col += cw;
        }
        if !buf.is_empty() {
            cur.push(Span::styled(buf, style));
        }
    }
    if let Some((l_idx, w_start)) = active_link.take() {
        targets.push(LinkTarget {
            line: start_line_idx + out.len(),
            col_start: w_start,
            col_end: cur_w,
            url: line_links[l_idx].2.clone(),
        });
    }
    out.push(Line::from(cur));
    (out, targets)
}

/// 把單一樣式化行依顯示寬度切成多行，保留樣式。
pub fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    wrap_line_with_links(line, &[], 0, width).0
}

/// 對整段已渲染行做換行並收集連結。
pub fn wrap_lines_and_collect_links(
    lines_with_links: Vec<(Line<'static>, Vec<(usize, usize, String)>)>,
    width: usize,
    base_line_idx: usize,
) -> (Vec<Line<'static>>, Vec<LinkTarget>) {
    let mut out_lines = Vec::new();
    let mut out_targets = Vec::new();
    for (line, links) in lines_with_links {
        let (wl, wt) = wrap_line_with_links(line, &links, base_line_idx + out_lines.len(), width);
        out_lines.extend(wl);
        out_targets.extend(wt);
    }
    (out_lines, out_targets)
}

/// 對整段已渲染行做換行。
pub fn wrap_lines(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(lines.len());
    for l in lines {
        out.extend(wrap_line(l, width));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Vec<String> {
        text.lines().map(|l| l.to_string()).collect()
    }

    fn contents(line: &Line<'_>) -> Vec<String> {
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn inline_bold_italic_code_link() {
        let (spans, links) = inline_with_links("a **b** *c* `d` [e](http://x)", Style::default());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "a b c d e");

        let bold = spans.iter().find(|s| s.content == "b").expect("bold span");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let italic = spans
            .iter()
            .find(|s| s.content == "c")
            .expect("italic span");
        assert!(italic.style.add_modifier.contains(Modifier::ITALIC));
        let code = spans.iter().find(|s| s.content == "d").expect("code span");
        assert_eq!(code.style.bg, Some(CODE_BG));
        let link = spans.iter().find(|s| s.content == "e").expect("link span");
        assert_eq!(link.style.fg, Some(LINK));
        assert!(link.style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].2, "http://x");
    }

    #[test]
    fn fence_toggles_and_colors_code() {
        let mut r = Renderer::new();
        let src = plain("before\n```rust\nlet x = 1;\n```\nafter");
        let out = r.render(&src, 80);
        // before / open / code / close / after
        assert_eq!(out.len(), 5);
        assert!(contents(&out[0]).join("").contains("before"));
        assert!(contents(&out[1]).join("").contains("rust"));
        assert!(contents(&out[2]).join("").contains("let x = 1;"));
        assert_eq!(out[2].spans.last().unwrap().style.bg, Some(CODE_BG));
        assert!(contents(&out[4]).join("").contains("after"));
    }

    #[test]
    fn heading_and_list_and_quote() {
        let mut r = Renderer::new();
        let src = plain("# Title\n- one\n1. two\n> quote");
        let out = r.render(&src, 80);
        assert_eq!(contents(&out[0]).join(""), "▎ Title");
        assert!(out[0]
            .spans
            .last()
            .unwrap()
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        assert_eq!(contents(&out[1]).join(""), "• one");
        assert_eq!(contents(&out[2]).join(""), "1. two");
        assert_eq!(contents(&out[3]).join(""), "▏ quote");
    }

    #[test]
    fn table_aligns_columns() {
        let mut r = Renderer::new();
        let src = plain("| a | b |\n|---|--:|\n| 1 | 2 |");
        let out = r.render(&src, 80);
        assert_eq!(out.len(), 3);
        // header + separator + 1 row, each starting with the frame
        assert!(contents(&out[0]).join("").starts_with("│ "));
        assert!(contents(&out[1]).join("").starts_with("├"));
        // right-aligned column pads on the left
        let row = contents(&out[2]).join("");
        assert!(row.ends_with("│"));
    }

    #[test]
    fn table_alignment_left_center_right() {
        let mut r = Renderer::new();
        let src = plain("| left | mid | right |\n|:-----|:---:|------:|\n| x | y | z |");
        let out = r.render(&src, 80);
        assert_eq!(out.len(), 3);

        // 欄寬由最寬的儲存格決定：left=4, mid=3, right=5。
        assert_eq!(contents(&out[1]).join(""), "├──────┼─────┼───────┤");
        // 左靠、置中、右靠的補白各不相同。
        assert_eq!(contents(&out[2]).join(""), "│ x    │  y  │     z │");
    }

    #[test]
    fn table_header_is_bold() {
        let mut r = Renderer::new();
        let src = plain("| h |\n|---|\n| b |");
        let out = r.render(&src, 80);
        let header = out[0]
            .spans
            .iter()
            .find(|s| s.content == "h")
            .expect("header cell");
        assert!(header.style.add_modifier.contains(Modifier::BOLD));
        let body = out[2]
            .spans
            .iter()
            .find(|s| s.content == "b")
            .expect("body cell");
        assert!(!body.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn table_shrinks_to_width() {
        let mut r = Renderer::new();
        let src = plain("| aaaaaaaaaa | bbbbbbbbbb |\n|---|---|\n| 1 | 2 |");
        // 極窄寬度時仍應每行都不超過可用寬度。
        let out = r.render(&src, 12);
        for line in &out {
            let w = UnicodeWidthStr::width(contents(line).join("").as_str());
            assert!(
                w <= 12,
                "line too wide ({w}): {:?}",
                contents(line).join("")
            );
        }
    }

    fn line_width(line: &Line<'_>) -> usize {
        UnicodeWidthStr::width(contents(line).join("").as_str())
    }

    #[test]
    fn table_cjk_uses_display_width() {
        let mut r = Renderer::new();
        let src = plain("| 名稱 | 值 |\n|---|---|\n| 測試 | 1 |");
        let out = r.render(&src, 80);
        assert_eq!(out.len(), 3);
        assert_eq!(contents(&out[1]).join(""), "├──────┼────┤");
        assert_eq!(contents(&out[0]).join(""), "│ 名稱 │ 值 │");
        assert_eq!(contents(&out[2]).join(""), "│ 測試 │ 1  │");
        for line in &out {
            assert!(line_width(line) <= 80);
        }
    }

    #[test]
    fn table_cell_inline_markup_is_rendered() {
        let mut r = Renderer::new();
        let src = plain("| **b** | `c` |\n|---|---|\n| [x](http://e) | y |");
        let out = r.render(&src, 80);
        let all: String = out.iter().map(|l| contents(l).join("")).collect();
        assert!(!all.contains("**"), "bold markup leaked: {all}");
        assert!(!all.contains('`'), "code markup leaked: {all}");
        assert!(all.contains('b'));
        assert!(all.contains('c'));
        assert!(all.contains('x'));
    }

    #[test]
    fn table_pipe_inside_code_cell() {
        let mut r = Renderer::new();
        let src = plain("| a | b |\n|---|---|\n| `x|y` | z |");
        let out = r.render(&src, 80);
        let row = contents(&out[2]).join("");
        assert!(row.contains("x|y"), "code cell split wrongly: {row}");
        assert!(row.contains('z'));
    }

    #[test]
    fn wrap_uses_display_width_for_cjk() {
        let out = wrap_line(Line::raw("你好世界"), 4);
        assert_eq!(out.len(), 2);
        assert_eq!(contents(&out[0]).join(""), "你好");
        assert_eq!(contents(&out[1]).join(""), "世界");
        for line in &out {
            assert_eq!(line_width(line), 4);
        }
    }

    #[test]
    fn table_ignored_inside_code_fence() {
        let mut r = Renderer::new();
        let src = plain("```\n| a | b |\n|---|---|\n```");
        let out = r.render(&src, 80);
        assert_eq!(out.len(), 4, "fence open + 2 code lines + close");
        let all: String = out.iter().map(|l| contents(l).join("")).collect();
        assert!(!all.contains('┼'), "table should not be parsed in a fence");
    }

    #[test]
    fn link_shows_clean_label_and_tracks_target() {
        let (spans, links) = inline_with_links("see [docs](https://example.com/x) now", Style::default());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "see docs now");
        let link = spans
            .iter()
            .find(|s| s.content == "docs")
            .expect("link span");
        assert_eq!(link.style.fg, Some(LINK));
        assert!(link.style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].2, "https://example.com/x");
    }

    #[test]
    fn strike_and_escapes() {
        let spans = inline("~~gone~~ and \\*literal\\*", Style::default());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "gone and *literal*");
        let gone = spans
            .iter()
            .find(|s| s.content == "gone")
            .expect("strike span");
        assert!(gone.style.add_modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn semantic_prefixes() {
        let mut r = Renderer::new();
        let src = plain("💭 thinking\n▸ Bash ls\n💬 review x");
        let out = r.render(&src, 80);
        assert_eq!(out[0].spans[0].style.fg, Some(THOUGHT));
        assert_eq!(out[1].spans[1].style.fg, Some(TOOL));
        assert_eq!(out[2].spans[0].style.fg, Some(REVIEW));
    }

    #[test]
    fn wrap_preserves_style_and_width() {
        let spans = vec![Span::styled("abcdef", Style::default().fg(LINK))];
        let out = wrap_line(Line::from(spans), 4);
        assert_eq!(out.len(), 2);
        assert_eq!(contents(&out[0]).join(""), "abcd");
        assert_eq!(contents(&out[1]).join(""), "ef");
        assert_eq!(out[1].spans[0].style.fg, Some(LINK));
    }
}
