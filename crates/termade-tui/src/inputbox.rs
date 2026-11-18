//! 輸入框的多行排版：軟換行、游標位置與上下移動。

use unicode_width::UnicodeWidthChar;

/// 輸入框最多顯示幾行內容 (超過就捲動)。
pub(crate) const MAX_ROWS: usize = 8;

/// 一個顯示列：`[start, end)` 是字元索引範圍；`hard` 表示這列結尾是真正的換行字元
/// (換行字元本身在索引 `end`，不屬於任何列的內容)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) hard: bool,
}

fn char_width(c: char) -> usize {
    c.width().unwrap_or(0).max(1)
}

/// 把輸入依寬度排成顯示列。永遠至少有一列。
pub(crate) fn rows(text: &str, width: usize) -> Vec<Row> {
    let width = width.max(2);
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    let mut w = 0;
    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            out.push(Row {
                start,
                end: i,
                hard: true,
            });
            start = i + 1;
            w = 0;
            continue;
        }
        let cw = char_width(c);
        if w + cw > width && i > start {
            out.push(Row {
                start,
                end: i,
                hard: false,
            });
            start = i;
            w = 0;
        }
        w += cw;
    }
    out.push(Row {
        start,
        end: chars.len(),
        hard: false,
    });
    out
}

/// 游標所在的 (列索引, 欄位寬度)。
pub(crate) fn cursor_pos(text: &str, rows: &[Row], cursor: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let last = rows.len() - 1;
    let r = rows
        .iter()
        .enumerate()
        .find(|(i, row)| cursor < row.end || (cursor == row.end && (row.hard || *i == last)))
        .map_or(last, |(i, _)| i);
    let row = rows[r];
    let col = chars[row.start..cursor.max(row.start)]
        .iter()
        .map(|&c| char_width(c))
        .sum();
    (r, col)
}

/// 在第 `r` 列找最接近且不超過 `col` 欄的字元索引。
pub(crate) fn index_at(text: &str, rows: &[Row], r: usize, col: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let row = rows[r.min(rows.len() - 1)];
    let mut w = 0;
    let mut idx = row.start;
    while idx < row.end {
        let cw = char_width(chars[idx]);
        if w + cw > col {
            break;
        }
        w += cw;
        idx += 1;
    }
    idx
}

/// 目前所在邏輯行 (以換行字元分隔)的起點與終點字元索引。
pub(crate) fn line_bounds(text: &str, cursor: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let start = chars[..cursor]
        .iter()
        .rposition(|&c| c == '\n')
        .map_or(0, |p| p + 1);
    let end = chars[cursor..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(chars.len(), |p| cursor + p);
    (start, end)
}

/// 取出第 `r` 列的文字。
pub(crate) fn row_text(text: &str, row: Row) -> String {
    text.chars()
        .skip(row.start)
        .take(row.end - row.start)
        .collect()
}

/// 把一列畫成 `Line`；`sel` 是選取的字元索引範圍 `[lo, hi)`，範圍內以反白標示。
pub(crate) fn row_line(
    text: &str,
    row: Row,
    sel: Option<(usize, usize)>,
) -> ratatui::text::Line<'static> {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};

    let plain = row_text(text, row);
    let Some((lo, hi)) = sel else {
        return Line::raw(plain);
    };
    let from = lo.max(row.start).min(row.end);
    let to = hi.min(row.end).max(from);
    if from >= to {
        return Line::raw(plain);
    }
    let chars: Vec<char> = text.chars().collect();
    let prefix: String = chars[row.start..from].iter().collect();
    let mid: String = chars[from..to].iter().collect();
    let suffix: String = chars[to..row.end].iter().collect();
    Line::from(vec![
        Span::raw(prefix),
        Span::styled(mid, Style::default().add_modifier(Modifier::REVERSED)),
        Span::raw(suffix),
    ])
}

/// 選取範圍換成不含頭的字元索引 `[lo, hi)`；錨點與目前位置相同代表沒有選取。
pub(crate) fn selection_range(sel: Option<(usize, usize)>) -> Option<(usize, usize)> {
    let (a, h) = sel?;
    if a == h {
        None
    } else if a < h {
        Some((a, h + 1))
    } else {
        Some((h, a + 1))
    }
}

/// 選取範圍內的純文字。
pub(crate) fn selected_text(text: &str, sel: Option<(usize, usize)>) -> String {
    let Some((lo, hi)) = selection_range(sel) else {
        return String::new();
    };
    text.chars().skip(lo).take(hi.saturating_sub(lo)).collect()
}

/// 輸入框需要的高度 (含邊框)。
pub(crate) fn height_for(text: &str, outer_width: u16) -> u16 {
    let inner = (outer_width as usize).saturating_sub(2);
    let n = rows(text, inner).len().clamp(1, MAX_ROWS);
    n as u16 + 2
}
