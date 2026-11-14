//! Diff 視圖輔助：hunk 解析、行號邊欄、統計與 hunk 跳轉。

use crate::app::{App, CenterView};
use tr::tr;

/// 在 Diff 視圖跳到上一個／下一個 hunk（`@@` 行）。
pub(crate) fn jump_hunk(app: &mut App, forward: bool) {
    if app.view != CenterView::Diff || app.diff_lines.is_empty() {
        return;
    }
    let n = app.diff_lines.len();
    let cur = app.diff_cursor.min(n - 1);
    let is_hunk = |i: usize| app.diff_lines[i].starts_with("@@");
    if forward {
        if let Some(i) = ((cur + 1)..n).find(|&i| is_hunk(i)) {
            app.diff_cursor = i;
            return;
        }
    } else if let Some(i) = (0..cur).rev().find(|&i| is_hunk(i)) {
        app.diff_cursor = i;
        return;
    }
    app.status = tr!("沒有更多 hunk");
}

/// 從 `@@ -a,b +c,d @@` 解析新舊檔起始行號。
pub(crate) fn parse_hunk(line: &str) -> Option<(u64, u64)> {
    let rest = line.strip_prefix("@@")?;
    let mut old = None;
    let mut new = None;
    for tok in rest.split_whitespace() {
        if let Some(v) = tok.strip_prefix('-') {
            old = v.split(',').next().and_then(|s| s.parse::<u64>().ok());
        } else if let Some(v) = tok.strip_prefix('+') {
            new = v.split(',').next().and_then(|s| s.parse::<u64>().ok());
        }
        if old.is_some() && new.is_some() {
            break;
        }
    }
    Some((old?, new?))
}

/// 為每行 diff 標上（舊檔行號, 新檔行號）；非內容行則為 `None`。
pub(crate) fn build_gutter(lines: &[String]) -> Vec<(Option<u64>, Option<u64>)> {
    let mut out = Vec::with_capacity(lines.len());
    let (mut old, mut new) = (0u64, 0u64);
    for l in lines {
        if l.starts_with("@@") {
            if let Some((o, n)) = parse_hunk(l) {
                old = o;
                new = n;
            }
            out.push((None, None));
        } else if l.starts_with("+++")
            || l.starts_with("---")
            || l.starts_with("diff ")
            || l.starts_with("index ")
            || l.starts_with('\\')
        {
            out.push((None, None));
        } else if l.starts_with('+') {
            out.push((None, Some(new)));
            new += 1;
        } else if l.starts_with('-') {
            out.push((Some(old), None));
            old += 1;
        } else {
            out.push((Some(old), Some(new)));
            old += 1;
            new += 1;
        }
    }
    out
}

/// 統計 diff 的新增／刪除行數。
pub(crate) fn diff_stat(lines: &[String]) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for l in lines {
        if l.starts_with("+++") || l.starts_with("---") {
            continue;
        }
        if l.starts_with('+') {
            added += 1;
        } else if l.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}
