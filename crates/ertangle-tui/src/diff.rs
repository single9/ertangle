//! Diff 視圖輔助：檔案與 hunk 解析、單檔摺疊、行號邊欄、統計、導覽與局部退回。

use crate::app::{App, CenterView};
use tr::tr;

/// 單一檔案在原始 unified diff 中的範圍與統計。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffFile {
    pub(crate) path: String,
    /// 在 `diff_raw_lines` 中的起始行索引 (含，`diff --git` 行)。
    pub(crate) start_line: usize,
    /// 在 `diff_raw_lines` 中的結束行索引 (不含)。
    pub(crate) end_line: usize,
    pub(crate) added: usize,
    pub(crate) removed: usize,
    /// 是否為未追蹤的新檔案 (`new file mode` + `--- /dev/null`)。
    pub(crate) is_untracked: bool,
}

/// 掃描原始 diff 行，解析各檔案邊界與統計資訊。
pub(crate) fn parse_files(raw_lines: &[String]) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut current_start: Option<(usize, String)> = None;

    for (idx, line) in raw_lines.iter().enumerate() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some((start, initial_path)) = current_start.take() {
                files.push(build_diff_file(raw_lines, start, idx, initial_path));
            }
            let path = rest
                .rsplit_once(" b/")
                .map(|(_, b)| b.trim().to_string())
                .or_else(|| {
                    rest.strip_prefix("a/")
                        .and_then(|s| s.split_whitespace().next())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| rest.trim().to_string());
            current_start = Some((idx, path));
        }
    }

    if let Some((start, initial_path)) = current_start.take() {
        files.push(build_diff_file(
            raw_lines,
            start,
            raw_lines.len(),
            initial_path,
        ));
    }

    files
}

fn build_diff_file(
    raw_lines: &[String],
    start_line: usize,
    end_line: usize,
    mut path: String,
) -> DiffFile {
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut has_new_file_mode = false;
    let mut has_dev_null_old = false;
    let mut in_header = true;

    for l in raw_lines.get(start_line..end_line).unwrap_or(&[]) {
        if l.starts_with("@@") {
            in_header = false;
            continue;
        }
        if in_header {
            if l.starts_with("new file mode") {
                has_new_file_mode = true;
            } else if l.starts_with("--- /dev/null") {
                has_dev_null_old = true;
            } else if let Some(p) = l.strip_prefix("+++ b/") {
                path = p.trim().to_string();
            } else if let Some(p) = l.strip_prefix("--- a/") {
                if path.is_empty() {
                    path = p.trim().to_string();
                }
            }
            continue;
        }
        if l.starts_with("+++") || l.starts_with("---") {
            continue;
        }
        if l.starts_with('+') {
            added += 1;
        } else if l.starts_with('-') {
            removed += 1;
        }
    }

    DiffFile {
        path,
        start_line,
        end_line,
        added,
        removed,
        is_untracked: has_new_file_mode && has_dev_null_old,
    }
}

fn ensure_diff_parsed(app: &mut App) {
    if app.diff_raw_lines.is_empty() && !app.diff_lines.is_empty() {
        app.diff_raw_lines = app.diff_lines.clone();
    }
    if app.diff_files.is_empty() && !app.diff_raw_lines.is_empty() {
        app.diff_files = parse_files(&app.diff_raw_lines);
    }
}

/// 計算每個 `DiffFile` 在目前可見 `diff_lines` 中的 `(vis_start, vis_end)` 範圍 (不含 `vis_end`)。
pub(crate) fn visible_file_ranges(
    files: &[DiffFile],
    collapsed: &std::collections::BTreeSet<String>,
) -> Vec<(usize, usize)> {
    let mut ranges = Vec::with_capacity(files.len());
    let mut pos = files.first().map_or(0, |f| f.start_line);
    for f in files {
        let len = if collapsed.contains(&f.path) {
            1
        } else {
            f.end_line.saturating_sub(f.start_line)
        };
        ranges.push((pos, pos + len));
        pos += len;
    }
    ranges
}

/// 依 `diff_raw_lines`、`diff_files` 與 `diff_collapsed` 重建可見的 `diff_lines` 與 `diff_gutter`。
pub(crate) fn rebuild_visible_diff(app: &mut App) {
    ensure_diff_parsed(app);
    let visible = if app.diff_files.is_empty() {
        app.diff_raw_lines.clone()
    } else {
        let mut out = Vec::new();
        let first_start = app.diff_files.first().map_or(0, |f| f.start_line);
        if first_start > 0 && first_start <= app.diff_raw_lines.len() {
            if let Some(slice) = app.diff_raw_lines.get(..first_start) {
                out.extend_from_slice(slice);
            }
        }
        for f in &app.diff_files {
            if app.diff_collapsed.contains(&f.path) {
                out.push(tr!(
                    "▸ [已摺疊] {path} (+{added} -{removed}) — 按 Enter 展開",
                    path = f.path,
                    added = f.added,
                    removed = f.removed
                ));
            } else {
                let end = f.end_line.min(app.diff_raw_lines.len());
                if f.start_line < end {
                    if let Some(slice) = app.diff_raw_lines.get(f.start_line..end) {
                        out.extend_from_slice(slice);
                    }
                }
            }
        }
        out
    };

    app.diff_gutter = build_gutter(&visible);
    app.diff_lines = visible;
    let (added, removed) = diff_stat(&app.diff_raw_lines);
    app.diff_added = added;
    app.diff_removed = removed;
    if app.diff_lines.is_empty() {
        app.diff_cursor = 0;
    } else if app.diff_cursor >= app.diff_lines.len() {
        app.diff_cursor = app.diff_lines.len() - 1;
    }

    if let Some(entry) = app.hosts.get_mut(&app.active_host) {
        entry.diff_raw_lines = app.diff_raw_lines.clone();
        entry.diff_files = app.diff_files.clone();
        entry.diff_collapsed = app.diff_collapsed.clone();
        entry.diff_lines = app.diff_lines.clone();
        entry.diff_gutter = app.diff_gutter.clone();
        entry.diff_added = app.diff_added;
        entry.diff_removed = app.diff_removed;
        entry.diff_cursor = app.diff_cursor;
    }
}

/// 回傳目前游標所在的檔案進度 `(1-based 檔案序號, 總檔案數, 檔案路徑)`。
pub(crate) fn current_file_progress(app: &App) -> Option<(usize, usize, String)> {
    if app.diff_lines.is_empty() {
        return None;
    }
    let cow_files: std::borrow::Cow<'_, [DiffFile]> = if !app.diff_files.is_empty() {
        std::borrow::Cow::Borrowed(&app.diff_files)
    } else {
        std::borrow::Cow::Owned(parse_files(&app.diff_lines))
    };
    if cow_files.is_empty() {
        return None;
    }
    let ranges = visible_file_ranges(&cow_files, &app.diff_collapsed);
    let cur = app.diff_cursor.min(app.diff_lines.len().saturating_sub(1));
    let idx = ranges
        .iter()
        .position(|&(s, e)| cur >= s && cur < e)
        .unwrap_or_else(|| {
            if ranges.first().is_some_and(|r| cur < r.0) {
                0
            } else {
                ranges.len().saturating_sub(1)
            }
        });
    let path = cow_files
        .get(idx)
        .map_or_else(String::new, |f| f.path.clone());
    Some((idx + 1, cow_files.len(), path))
}

/// Parses the new-file start line from a unified diff hunk header (`@@ -old +new[,count] @@`).
fn parse_hunk_new_start(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("@@ ")?;
    let plus_pos = rest.find('+')?;
    let after_plus = rest.get(plus_pos + 1..)?;
    let num_str: String = after_plus
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    num_str.parse().ok()
}

/// Returns the file path and 1-based new-file line number under `diff_cursor` (T106).
pub(crate) fn current_file_and_line(app: &App) -> Option<(String, Option<usize>)> {
    let (_, _, path) = current_file_progress(app)?;
    if path.is_empty() {
        return None;
    }
    let cur = app.diff_cursor.min(app.diff_lines.len().saturating_sub(1));
    let mut hunk_idx = None;
    let mut new_start = None;
    let mut idx = cur;
    while let Some(l) = app.diff_lines.get(idx) {
        if l.starts_with("@@ ") {
            new_start = parse_hunk_new_start(l);
            hunk_idx = Some(idx);
            break;
        }
        if l.starts_with("diff --git ") || idx == 0 {
            break;
        }
        idx -= 1;
    }
    let line_num = match (hunk_idx, new_start) {
        (Some(hi), Some(start)) if cur > hi => {
            let mut n = start;
            for l in app.diff_lines.get(hi + 1..cur).unwrap_or(&[]) {
                if !l.starts_with('-') && !l.starts_with('\\') {
                    n += 1;
                }
            }
            Some(n)
        }
        (_, Some(start)) => Some(start),
        _ => None,
    };
    Some((path, line_num))
}

/// 在 Diff 視圖跳到上一個／下一個檔案 (`diff --git` 或已摺疊摘要行)。
pub(crate) fn jump_file(app: &mut App, forward: bool) {
    if app.view != CenterView::Diff || app.diff_lines.is_empty() {
        return;
    }
    ensure_diff_parsed(app);
    app.diff_discard_armed = None;
    if app.diff_files.is_empty() {
        app.status = tr!("沒有更多檔案");
        return;
    }
    let ranges = visible_file_ranges(&app.diff_files, &app.diff_collapsed);
    let n = app.diff_lines.len();
    let cur = app.diff_cursor.min(n.saturating_sub(1));
    let target_idx = if forward {
        (0..ranges.len()).find(|&idx| ranges.get(idx).is_some_and(|r| r.0 > cur))
    } else {
        (0..ranges.len())
            .rev()
            .find(|&idx| ranges.get(idx).is_some_and(|r| r.0 < cur))
    };
    if let Some(idx) = target_idx {
        if let Some(r) = ranges.get(idx) {
            app.diff_cursor = r.0;
        }
        if let Some(f) = app.diff_files.get(idx) {
            app.status = tr!(
                "[{idx}/{total}] {path} (+{added} -{removed})",
                idx = idx + 1,
                total = ranges.len(),
                path = f.path,
                added = f.added,
                removed = f.removed
            );
        }
    } else {
        app.status = tr!("沒有更多檔案");
    }
}

/// 摺疊或展開目前游標所在的檔案。
pub(crate) fn toggle_collapse_current_file(app: &mut App) {
    if app.view != CenterView::Diff || app.diff_lines.is_empty() {
        return;
    }
    ensure_diff_parsed(app);
    if app.diff_files.is_empty() {
        return;
    }
    let ranges = visible_file_ranges(&app.diff_files, &app.diff_collapsed);
    let cur = app.diff_cursor.min(app.diff_lines.len().saturating_sub(1));
    let Some(file_idx) = ranges.iter().position(|&(s, e)| cur >= s && cur < e) else {
        return;
    };
    let Some(f) = app.diff_files.get(file_idx) else {
        return;
    };
    let path = f.path.clone();
    let collapsed = if app.diff_collapsed.contains(&path) {
        app.diff_collapsed.remove(&path);
        false
    } else {
        app.diff_collapsed.insert(path.clone());
        true
    };
    app.diff_discard_armed = None;
    rebuild_visible_diff(app);
    let new_ranges = visible_file_ranges(&app.diff_files, &app.diff_collapsed);
    if let Some(&(new_start, _)) = new_ranges.get(file_idx) {
        app.diff_cursor = new_start;
        if let Some(entry) = app.hosts.get_mut(&app.active_host) {
            entry.diff_cursor = new_start;
        }
    }
    crate::search::refresh(app);
    app.status = if collapsed {
        tr!("已摺疊 {path}", path = path)
    } else {
        tr!("已展開 {path}", path = path)
    };
}

/// 擷取目前游標所在檔案或 Hunk 的退回資訊：`Some((file_path, optional_hunk_patch))`。
/// - 若游標位於檔案標頭行 (`diff --git`、`---`、`+++`)、已摺疊摘要行，或該檔案為未追蹤新檔，回傳 `(path, None)` 代表整檔退回。
/// - 若游標位於已追蹤檔案的某個 Hunk (`@@ ... @@`) 內，回傳 `(path, Some(patch))` 代表局部 Hunk 退回。
pub(crate) fn extract_current_hunk_patch(app: &App) -> Option<(String, Option<String>)> {
    if app.diff_lines.is_empty() {
        return None;
    }
    let raw_lines = if app.diff_raw_lines.is_empty() {
        &app.diff_lines
    } else {
        &app.diff_raw_lines
    };
    let cow_files: std::borrow::Cow<'_, [DiffFile]> = if !app.diff_files.is_empty() {
        std::borrow::Cow::Borrowed(&app.diff_files)
    } else {
        std::borrow::Cow::Owned(parse_files(raw_lines))
    };
    if cow_files.is_empty() {
        return None;
    }
    let ranges = visible_file_ranges(&cow_files, &app.diff_collapsed);
    let cur = app.diff_cursor.min(app.diff_lines.len().saturating_sub(1));
    let file_idx = ranges.iter().position(|&(s, e)| cur >= s && cur < e)?;
    let file = cow_files.get(file_idx)?;

    if app.diff_collapsed.contains(&file.path) || file.is_untracked {
        return Some((file.path.clone(), None));
    }

    let (vis_start, _) = *ranges.get(file_idx)?;
    let end = file.end_line.min(raw_lines.len());
    if file.start_line >= end {
        return Some((file.path.clone(), None));
    }
    let file_lines = raw_lines.get(file.start_line..end).unwrap_or(&[]);
    let rel_cur = (cur - vis_start).min(file_lines.len().saturating_sub(1));

    let Some(first_hunk) = file_lines.iter().position(|l| l.starts_with("@@")) else {
        return Some((file.path.clone(), None));
    };
    // 游標在 `diff --git` / `index` / `---` / `+++` 等檔案標頭行時，對整個檔案退回。
    if rel_cur < first_hunk {
        return Some((file.path.clone(), None));
    }

    let hunk_start = (first_hunk..=rel_cur)
        .rev()
        .find(|&i| file_lines.get(i).is_some_and(|l| l.starts_with("@@")))?;
    let hunk_end = ((hunk_start + 1)..file_lines.len())
        .find(|&i| file_lines.get(i).is_some_and(|l| l.starts_with("@@")))
        .unwrap_or(file_lines.len());

    let mut patch_lines: Vec<&str> = Vec::new();
    if let Some(head_slice) = file_lines.get(..first_hunk) {
        for l in head_slice {
            if l.starts_with("diff --git ") || l.starts_with("--- ") || l.starts_with("+++ ") {
                patch_lines.push(l.as_str());
            }
        }
    }
    if let Some(hunk_slice) = file_lines.get(hunk_start..hunk_end) {
        for l in hunk_slice {
            patch_lines.push(l.as_str());
        }
    }
    let mut patch = patch_lines.join("\n");
    patch.push('\n');
    Some((file.path.clone(), Some(patch)))
}

/// 在 Diff 視圖跳到上一個／下一個 hunk (`@@` 行)。
pub(crate) fn jump_hunk(app: &mut App, forward: bool) {
    if app.view != CenterView::Diff || app.diff_lines.is_empty() {
        return;
    }
    app.diff_discard_armed = None;
    let n = app.diff_lines.len();
    let cur = app.diff_cursor.min(n.saturating_sub(1));
    let is_hunk = |i: usize| app.diff_lines.get(i).is_some_and(|l| l.starts_with("@@"));
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

/// 為每行 diff 標上 (舊檔行號, 新檔行號)；非內容行則為 `None`。
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
            || l.starts_with("new file mode ")
            || l.starts_with("deleted file mode ")
            || l.starts_with("old mode ")
            || l.starts_with("new mode ")
            || l.starts_with("similarity index ")
            || l.starts_with("rename from ")
            || l.starts_with("rename to ")
            || l.starts_with("Binary files ")
            || l.starts_with("▸ ")
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
