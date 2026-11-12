//! 極簡 Markdown 渲染器（零外部依賴）。
//!
//! 把 agent 的逐字稿純文字轉成 Ratatui 的 [`Line`] / [`Span`]，支援：
//! 標題、粗體／斜體／刪除線、行內程式碼、連結、無序／有序／工作清單、
//! 引言、分隔線、圍籬程式碼區塊（含語言標籤與底色）、GFM 表格。
//!
//! 也認得 ade 自己在逐字稿裡加的語意前綴：`💭 `（思考）、`▸ `（工具呼叫）、
//! `💬 `（審查註解），各自用不同顏色呈現。
//!
//! 設計上刻意保持「行導向」：一次 `render` 會把整段來源重新渲染，
//! 因此圍籬／表格狀態不需要跨次呼叫保存。

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub const CODE_FG: Color = Color::Rgb(214, 216, 228);
pub const CODE_BG: Color = Color::Rgb(30, 30, 40);
pub const CODE_BAR: Color = Color::Rgb(86, 122, 186);
pub const THOUGHT: Color = Color::Rgb(128, 132, 152);
pub const TOOL: Color = Color::Rgb(224, 184, 96);
pub const REVIEW: Color = Color::Rgb(198, 122, 206);
pub const LINK: Color = Color::Rgb(112, 172, 255);
pub const HEADING: Color = Color::Rgb(122, 202, 255);
pub const BULLET: Color = Color::Rgb(122, 202, 255);
pub const QUOTE: Color = Color::Rgb(122, 182, 122);
pub const RULE: Color = Color::Rgb(80, 84, 100);

/// 逐行 Markdown 渲染器。
#[derive(Default)]
pub struct Renderer {
    in_fence: bool,
}

impl Renderer {
    pub fn new() -> Self {
        Self { in_fence: false }
    }

    /// 把來源行渲染成樣式化行。`width` 是可用顯示寬度（不含邊框）。
    pub fn render(&mut self, src: &[String], width: usize) -> Vec<Line<'static>> {
        self.in_fence = false;
        let mut out: Vec<Line<'static>> = Vec::with_capacity(src.len());
        let mut i = 0usize;
        while i < src.len() {
            let line = &src[i];

            if self.in_fence {
                if fence(line).is_some() {
                    self.in_fence = false;
                    out.push(Line::styled("  ▌ ```", Style::default().fg(CODE_BAR)));
                } else {
                    out.extend(code_line(line, width));
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
                out.push(Line::from(vec![
                    Span::styled("  ▌ ", Style::default().fg(CODE_BAR)),
                    Span::styled(
                        format!("``` {label}"),
                        Style::default().fg(CODE_BAR).add_modifier(Modifier::BOLD),
                    ),
                ]));
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
                out.extend(render_table(&block, &src[i + 1], width));
                i = j;
                continue;
            }

            out.push(render_line(line, width));
            i += 1;
        }
        out
    }
}

// ---------------------------------------------------------------------------
// 行渲染
// ---------------------------------------------------------------------------

fn render_line(line: &str, width: usize) -> Line<'static> {
    if line.trim().is_empty() {
        return Line::raw(String::new());
    }
    if let Some(rest) = line.strip_prefix("💭 ") {
        return prefixed("💭 ", rest, THOUGHT, true);
    }
    if let Some(rest) = line.strip_prefix("▸ ") {
        return tool_line(rest);
    }
    if let Some(rest) = line.strip_prefix("💬 ") {
        return prefixed("💬 ", rest, REVIEW, false);
    }
    if let Some((level, rest)) = heading(line) {
        return heading_line(level, rest);
    }
    if is_hr(line) {
        return Line::styled("─".repeat(width.max(1)), Style::default().fg(RULE));
    }
    if let Some(rest) = line.strip_prefix("> ") {
        let mut spans = vec![Span::styled("▏ ", Style::default().fg(QUOTE))];
        spans.extend(inline(
            rest,
            Style::default().fg(QUOTE).add_modifier(Modifier::ITALIC),
        ));
        return Line::from(spans);
    }
    if line.trim() == ">" {
        return Line::styled("▏", Style::default().fg(QUOTE));
    }
    if let Some(l) = list_line(line) {
        return l;
    }
    Line::from(inline(line, Style::default()))
}

fn prefixed(prefix: &str, rest: &str, color: Color, italic: bool) -> Line<'static> {
    let mut base = Style::default().fg(color);
    if italic {
        base = base.add_modifier(Modifier::ITALIC);
    }
    let mut spans = vec![Span::styled(
        prefix.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )];
    spans.extend(inline(rest, base));
    Line::from(spans)
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

fn heading_line(level: usize, rest: &str) -> Line<'static> {
    let style = match level {
        1 => Style::default()
            .fg(HEADING)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        2 => Style::default().fg(HEADING).add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(HEADING)
            .add_modifier(Modifier::BOLD | Modifier::DIM),
    };
    let mut spans = vec![Span::styled("▎ ", Style::default().fg(HEADING))];
    spans.extend(inline(rest, style));
    Line::from(spans)
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

fn list_line(line: &str) -> Option<Line<'static>> {
    let indent_len = line.len() - line.trim_start().len();
    let indent = " ".repeat(indent_len);
    let t = line.trim_start();

    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(m) {
            let (mark, body) = task_marker(rest);
            let mut spans = vec![
                Span::raw(indent),
                Span::styled(format!("{mark} "), Style::default().fg(BULLET)),
            ];
            spans.extend(inline(body, Style::default()));
            return Some(Line::from(spans));
        }
    }

    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let after = &t[digits.len()..];
        if let Some(body) = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
        {
            let mut spans = vec![
                Span::raw(indent),
                Span::styled(format!("{digits}. "), Style::default().fg(BULLET)),
            ];
            spans.extend(inline(body, Style::default()));
            return Some(Line::from(spans));
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

/// 若此行是圍籬（``` 或 ~~~）則回傳語言標籤（可為空）。
fn fence(line: &str) -> Option<String> {
    let t = line.trim_start();
    let rest = t.strip_prefix("```").or_else(|| t.strip_prefix("~~~"))?;
    Some(rest.trim().to_string())
}

fn code_line(line: &str, width: usize) -> Vec<Line<'static>> {
    let budget = width.saturating_sub(4).max(8);
    let chars: Vec<char> = if line.is_empty() {
        vec![' ']
    } else {
        line.chars().collect()
    };
    let mut out = Vec::new();
    for chunk in chars.chunks(budget) {
        let s: String = chunk.iter().collect();
        out.push(Line::from(vec![
            Span::styled("  ▌ ", Style::default().fg(CODE_BAR)),
            Span::styled(
                format!("{s:<budget$}"),
                Style::default().fg(CODE_FG).bg(CODE_BG),
            ),
        ]));
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
            c.len() >= 2 && c.chars().all(|ch| ch == '-')
        })
}

fn split_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
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

fn render_table(rows: &[String], sep: &str, width: usize) -> Vec<Line<'static>> {
    let header = split_row(&rows[0]);
    let n = header.len().max(1);
    let aligns = parse_align(sep, n);
    let body: Vec<Vec<String>> = rows[1..].iter().map(|r| split_row(r)).collect();

    let mut widths = vec![1usize; n];
    for (c, cell) in header.iter().enumerate().take(n) {
        widths[c] = widths[c].max(cell.chars().count());
    }
    for row in &body {
        for (c, cell) in row.iter().enumerate().take(n) {
            widths[c] = widths[c].max(cell.chars().count());
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

/// 硬裁切一行到 `width` 個字元（超出時以 `…` 收尾），保留各 span 樣式。
fn clip(line: Line<'static>, width: usize) -> Line<'static> {
    let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= width {
        return line;
    }
    let budget = width.saturating_sub(1);
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in line.spans {
        if used >= budget {
            break;
        }
        let mut buf = String::new();
        for ch in span.content.chars() {
            if used >= budget {
                break;
            }
            buf.push(ch);
            used += 1;
        }
        if !buf.is_empty() {
            out.push(Span::styled(buf, span.style));
        }
    }
    out.push(Span::styled("…".to_string(), Style::default().fg(RULE)));
    Line::from(out)
}

fn fit(s: &str, w: usize) -> String {
    let count = s.chars().count();
    if count == w {
        return s.to_string();
    }
    if count > w {
        if w == 0 {
            return String::new();
        }
        let mut out: String = s.chars().take(w.saturating_sub(1)).collect();
        out.push('…');
        return out;
    }
    s.to_string()
}

fn pad(s: &str, w: usize, align: Align) -> String {
    let s = fit(s, w);
    let len = s.chars().count();
    let pad = w.saturating_sub(len);
    match align {
        Align::Left => format!("{s}{}", " ".repeat(pad)),
        Align::Right => format!("{}{s}", " ".repeat(pad)),
        Align::Center => {
            let l = pad / 2;
            let r = pad - l;
            format!("{}{s}{}", " ".repeat(l), " ".repeat(r))
        }
    }
}

fn table_row(cells: &[String], widths: &[usize], aligns: &[Align], header: bool) -> Line<'static> {
    let style = if header {
        Style::default().fg(HEADING).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let mut spans = Vec::new();
    for (c, w) in widths.iter().enumerate() {
        let cell = cells.get(c).map(String::as_str).unwrap_or("");
        spans.push(Span::styled("│ ", Style::default().fg(RULE)));
        spans.push(Span::styled(pad(cell, *w, aligns[c]), style));
        spans.push(Span::styled(" ", Style::default().fg(RULE)));
    }
    spans.push(Span::styled("│", Style::default().fg(RULE)));
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

/// 解析行內 Markdown（粗體、斜體、刪除線、程式碼、連結）。
pub fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let len = text.len();
    let mut i = 0usize;

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
                flush(&mut spans, &mut buf, base);
                spans.push(Span::styled(
                    rest[2..2 + end].to_string(),
                    base.add_modifier(Modifier::BOLD),
                ));
                i += 2 + end + 2;
                continue;
            }
        }

        if let Some(after) = rest.strip_prefix("~~") {
            if let Some(end) = after.find("~~") {
                flush(&mut spans, &mut buf, base);
                spans.push(Span::styled(
                    after[..end].to_string(),
                    base.add_modifier(Modifier::CROSSED_OUT),
                ));
                i += 2 + end + 2;
                continue;
            }
        }

        if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                flush(&mut spans, &mut buf, base);
                spans.push(Span::styled(
                    after[..end].to_string(),
                    Style::default().fg(CODE_FG).bg(CODE_BG),
                ));
                i += 1 + end + 1;
                continue;
            }
        }

        if rest.starts_with('[') {
            if let Some(close) = rest.find("](") {
                if let Some(paren) = rest[close + 2..].find(')') {
                    flush(&mut spans, &mut buf, base);
                    let label = &rest[1..close];
                    spans.push(Span::styled(
                        label.to_string(),
                        Style::default().fg(LINK).add_modifier(Modifier::UNDERLINED),
                    ));
                    i += close + 2 + paren + 1;
                    continue;
                }
            }
        }

        if rest.starts_with('*') || rest.starts_with('_') {
            let m = &rest[..1];
            if let Some(end) = rest[1..].find(m) {
                if end > 0 {
                    flush(&mut spans, &mut buf, base);
                    spans.push(Span::styled(
                        rest[1..1 + end].to_string(),
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

    flush(&mut spans, &mut buf, base);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

fn flush(spans: &mut Vec<Span<'static>>, buf: &mut String, base: Style) {
    if !buf.is_empty() {
        spans.push(Span::styled(std::mem::take(buf), base));
    }
}

// ---------------------------------------------------------------------------
// 換行（把已渲染的行依顯示寬度硬切）
// ---------------------------------------------------------------------------

/// 把單一樣式化行依寬度切成多行，保留樣式。
pub fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![line];
    }
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    for span in line.spans {
        let style = span.style;
        let mut buf = String::new();
        for ch in span.content.chars() {
            if cur_w >= width {
                if !buf.is_empty() {
                    cur.push(Span::styled(std::mem::take(&mut buf), style));
                }
                out.push(Line::from(std::mem::take(&mut cur)));
                cur_w = 0;
            }
            buf.push(ch);
            cur_w += 1;
        }
        if !buf.is_empty() {
            cur.push(Span::styled(buf, style));
        }
    }
    out.push(Line::from(cur));
    out
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
        let spans = inline("a **b** *c* `d` [e](http://x)", Style::default());
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
            let w: usize = contents(line).join("").chars().count();
            assert!(
                w <= 12,
                "line too wide ({w}): {:?}",
                contents(line).join("")
            );
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
    fn link_shows_label_only() {
        let spans = inline("see [docs](https://example.com/x) now", Style::default());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "see docs now");
        assert!(!text.contains("http"), "url should be hidden");
        let link = spans
            .iter()
            .find(|s| s.content == "docs")
            .expect("link span");
        assert_eq!(link.style.fg, Some(LINK));
        assert!(link.style.add_modifier.contains(Modifier::UNDERLINED));
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
