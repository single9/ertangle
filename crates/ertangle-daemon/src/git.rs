//! Git／worktree 輔助函式：建立、狀態、diff、auto-commit、merge、PR、gc。

use super::*;
use ertangle_protocol::MergeStrategy;
use tr::tr;

/// 記錄自動掛載進 worktree 的 symlink 相對路徑清單 (存放於 `<wt_dir>/.ertangle/provisioned_links`)，
/// 讓 `git status`、`compute_worktree_diff`、`auto_commit_worktree` 與 `remove_worktree` 能忽略並安全清理它們。
const PROVISIONED_LINKS_FILE: &str = ".ertangle/provisioned_links";

/// 在 `repo` 建立一個 worktree，回傳 (路徑, 分支)。
pub(crate) fn make_worktree(repo: &Path, id: &str) -> Result<(PathBuf, String)> {
    let branch = format!("ertangle/{id}");
    let dir = repo.join(".ertangle").join("wt").join(id);
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "add", "-b", &branch])
        .arg(&dir)
        .output()
        .with_context(|| tr!("執行 git worktree 失敗"))?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    if crate::config::Config::load(&crate::paths::config_dir(), repo)
        .agent_spawn
        .shared_worktree_deps()
    {
        provision_worktree_deps(repo, &dir);
    }
    Ok((dir, branch))
}

/// Checks whether `rel` is tracked by git in `repo`.
fn is_git_tracked(repo: &Path, rel: &str) -> bool {
    git_output(repo, &["ls-files", "--error-unmatch", "--", rel]).is_ok_and(|o| o.status.success())
}

/// Symlinks polyglot untracked dependencies (`node_modules`, `.venv`) and untracked local config
/// files (`.env*`, `appsettings.Development.json`) from `repo` into `wt_dir` (T100).
pub(crate) fn provision_worktree_deps(repo: &Path, wt_dir: &Path) {
    let mut provisioned: Vec<String> = Vec::new();

    // 1) Shared dependency directories (`node_modules`, `.venv`).
    for dir_name in ["node_modules", ".venv"] {
        let src = repo.join(dir_name);
        let dst = wt_dir.join(dir_name);
        if src.is_dir()
            && fs::symlink_metadata(&dst).is_err()
            && !is_git_tracked(repo, dir_name)
            && std::os::unix::fs::symlink(&src, &dst).is_ok()
        {
            provisioned.push(dir_name.to_string());
        }
    }

    // 2) Top-level untracked `.env*` and `appsettings.Development.json`.
    if let Ok(entries) = fs::read_dir(repo) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            let is_env_file = (name_str == ".env" || name_str.starts_with(".env."))
                && !name_str.ends_with(".example")
                && !name_str.ends_with(".sample")
                && !name_str.ends_with(".template");
            let is_appsettings =
                name_str == "appsettings.Development.json" || name_str == "appsettings.Local.json";
            if !is_env_file && !is_appsettings {
                continue;
            }
            let src = entry.path();
            let dst = wt_dir.join(name_str);
            if src.is_file()
                && fs::symlink_metadata(&dst).is_err()
                && !is_git_tracked(repo, name_str)
                && std::os::unix::fs::symlink(&src, &dst).is_ok()
            {
                provisioned.push(name_str.to_string());
            }
        }
    }

    // 3) Immediate child or `src/<Project>/appsettings.Development.json` for .NET Core solutions.
    let mut sub_dirs: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(repo) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let n = e.file_name();
                let ns = n.to_string_lossy();
                if !ns.starts_with('.') && ns != "node_modules" && ns != "target" {
                    sub_dirs.push(p.clone());
                    if ns == "src" {
                        if let Ok(src_entries) = fs::read_dir(&p) {
                            for se in src_entries.flatten() {
                                if se.path().is_dir() {
                                    sub_dirs.push(se.path());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for sub in sub_dirs {
        for cfg_name in ["appsettings.Development.json", ".env.local", ".env"] {
            let src = sub.join(cfg_name);
            if !src.is_file() {
                continue;
            }
            let Ok(rel) = src.strip_prefix(repo) else {
                continue;
            };
            let rel_str = rel.to_string_lossy().to_string();
            let dst = wt_dir.join(rel);
            if dst.parent().is_some_and(|p| p.is_dir())
                && fs::symlink_metadata(&dst).is_err()
                && !is_git_tracked(repo, &rel_str)
                && std::os::unix::fs::symlink(&src, &dst).is_ok()
            {
                provisioned.push(rel_str);
            }
        }
    }

    if !provisioned.is_empty() {
        let meta_path = wt_dir.join(PROVISIONED_LINKS_FILE);
        if let Some(parent) = meta_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(meta_path, provisioned.join("\n"));
    }
}

/// Reads the list of symlinked paths provisioned by `provision_worktree_deps`.
pub(crate) fn read_provisioned_links(wt_dir: &Path) -> Vec<String> {
    let meta_path = wt_dir.join(PROVISIONED_LINKS_FILE);
    fs::read_to_string(meta_path)
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Removes symlinked dependencies/configs provisioned into `wt_dir` without touching their targets in `repo`.
pub(crate) fn cleanup_provisioned_links(wt_dir: &Path) {
    for rel in read_provisioned_links(wt_dir) {
        let p = wt_dir.join(&rel);
        if let Ok(meta) = fs::symlink_metadata(&p) {
            if meta.file_type().is_symlink() {
                let _ = fs::remove_file(&p);
            }
        }
    }
    let _ = fs::remove_file(wt_dir.join(PROVISIONED_LINKS_FILE));
}

fn is_provisioned_path(wt_dir: &Path, rel_path: &str) -> bool {
    let trimmed = rel_path.trim().trim_end_matches('/');
    read_provisioned_links(wt_dir)
        .iter()
        .any(|p| p == trimmed || trimmed.starts_with(&format!("{p}/")))
}

/// 查詢某個工作目錄的變更檔案數與簡短摘要。
pub(crate) fn git_status_summary(cwd: &Path) -> (usize, String) {
    let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain"])
        .output()
    else {
        return (0, String::new());
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| {
            !is_ertangle_status_line(l) && !is_provisioned_path(cwd, l.get(3..).unwrap_or(l).trim())
        })
        .collect();
    let count = lines.len();
    let files: Vec<&str> = lines
        .iter()
        .take(3)
        .map(|l| l.get(3..).unwrap_or(l).trim())
        .collect();
    let summary = if count == 0 {
        "clean".to_string()
    } else {
        format!("{count} changed ({})", files.join(", "))
    };
    (count, summary)
}

/// Options for filtering, folding, and truncating worktree diffs (T98).
#[derive(Debug, Clone, Default)]
pub struct DiffOptions {
    pub stat_only: bool,
    pub paths: Vec<String>,
    pub max_lines: Option<usize>,
    pub fold_generated: bool,
}

/// Returns true if `path` is a known giant lockfile, snapshot, or machine-generated designer file (T98).
pub fn is_generated_or_lock_file(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        file_name,
        "Cargo.lock"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "bun.lock"
            | "bun.lockb"
            | "packages.lock.json"
            | "project.assets.json"
            | "uv.lock"
            | "poetry.lock"
            | "Pipfile.lock"
            | "go.sum"
            | "composer.lock"
            | "Gemfile.lock"
    ) || file_name.ends_with(".Designer.cs")
        || file_name.ends_with(".designer.cs")
        || file_name.ends_with("ModelSnapshot.cs")
        || file_name.ends_with(".g.cs")
        || file_name.ends_with(".generated.cs")
        || file_name.ends_with(".snap")
        || file_name.ends_with(".min.js")
        || file_name.ends_with(".min.css")
        || file_name.ends_with(".bundle.js")
}

fn path_matches_filter(rel: &str, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }
    filters.iter().any(|f| {
        let f = f.trim().trim_start_matches("./");
        if f.is_empty() {
            return true;
        }
        let f_dir = f.trim_end_matches('/');
        rel == f || rel == f_dir || rel.starts_with(&format!("{f_dir}/"))
    })
}

/// Parses a complete unified diff into per-file blocks `(file_path, raw_block, added_lines, removed_lines)`.
fn split_diff_blocks(raw: &str) -> Vec<(String, String, usize, usize)> {
    let mut blocks: Vec<(String, String, usize, usize)> = Vec::new();
    let mut cur_path = String::new();
    let mut cur_buf = String::new();
    let mut added = 0usize;
    let mut removed = 0usize;

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if !cur_buf.is_empty() {
                blocks.push((cur_path.clone(), cur_buf.clone(), added, removed));
                cur_buf.clear();
                added = 0;
                removed = 0;
            }
            cur_path = rest
                .split_whitespace()
                .last()
                .and_then(|b| b.strip_prefix("b/").or(Some(b)))
                .unwrap_or(rest)
                .to_string();
        } else if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            removed += 1;
        }
        cur_buf.push_str(line);
        cur_buf.push('\n');
    }
    if !cur_buf.is_empty() {
        blocks.push((cur_path, cur_buf, added, removed));
    }
    blocks
}

/// 取得工作目錄的統一格式 diff (含已追蹤變更與新增未追蹤檔案的內容預覽)。
pub fn compute_worktree_diff(cwd: &Path) -> Result<String> {
    compute_worktree_diff_with_options(cwd, &DiffOptions::default())
}

/// Computes the worktree diff with optional `stat_only`, `paths` filtering, generated-file folding, and `max_lines` truncation (T98).
pub fn compute_worktree_diff_with_options(cwd: &Path, opts: &DiffOptions) -> Result<String> {
    if !cwd.is_dir() {
        bail!("{}", tr!("工作目錄不存在：{cwd}", cwd = format!("{cwd:?}")));
    }
    let mut raw = String::new();

    // 1) 已追蹤檔案相對於 HEAD 的 diff。
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(cwd).args(["diff", "HEAD"]);
    let clean_paths: Vec<&str> = opts
        .paths
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if !clean_paths.is_empty() {
        cmd.arg("--");
        for p in &clean_paths {
            cmd.arg(p);
        }
    }
    let diff = cmd.output().with_context(|| tr!("執行 git diff 失敗"))?;
    let diff_str = String::from_utf8_lossy(&diff.stdout);
    if !diff_str.trim().is_empty() {
        raw.push_str(&diff_str);
        if !raw.ends_with('\n') {
            raw.push('\n');
        }
    }

    // 2) 未追蹤的新檔案 (排除 `.ertangle/` 與 provisioned links)：也產生 unified diff 樣式。
    let untracked = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["ls-files", "--others", "--exclude-standard"])
        .output()
        .with_context(|| tr!("執行 git ls-files 失敗"))?;
    for rel in String::from_utf8_lossy(&untracked.stdout).lines() {
        let rel = rel.trim();
        if rel.is_empty()
            || rel == ".ertangle"
            || rel.starts_with(".ertangle/")
            || is_provisioned_path(cwd, rel)
            || !path_matches_filter(rel, &opts.paths)
        {
            continue;
        }
        let full = cwd.join(rel);
        if fs::symlink_metadata(&full)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&full) {
            raw.push_str(&format!("diff --git a/{rel} b/{rel}\n"));
            raw.push_str("new file mode 100644\n");
            raw.push_str("--- /dev/null\n");
            raw.push_str(&format!("+++ b/{rel}\n"));
            let lines: Vec<&str> = content.lines().collect();
            raw.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
            for (idx, l) in lines.iter().take(200).enumerate() {
                raw.push_str(&format!("+{l}\n"));
                if idx == 199 && lines.len() > 200 {
                    raw.push_str("+... (truncated)\n");
                }
            }
        }
    }

    if raw.trim().is_empty() {
        return Ok(format!("{}\n", tr!("(目前工作區沒有尚未合併的變更)")));
    }

    let blocks = split_diff_blocks(&raw);
    let mut out = String::new();

    if opts.stat_only {
        let mut total_add = 0usize;
        let mut total_rem = 0usize;
        for (path, _, add, rem) in &blocks {
            total_add += *add;
            total_rem += *rem;
            let tag = if is_generated_or_lock_file(path) {
                " (generated/lock)"
            } else {
                ""
            };
            out.push_str(&format!("{path} | +{add} -{rem}{tag}\n"));
        }
        out.push_str(&format!(
            "{} file(s) changed, +{} -{}\n",
            blocks.len(),
            total_add,
            total_rem
        ));
    } else {
        for (path, block, add, rem) in &blocks {
            if opts.fold_generated && is_generated_or_lock_file(path) {
                out.push_str(&format!("diff --git a/{path} b/{path}\n"));
                out.push_str(&format!(
                    "--- {path} | +{add} -{rem} (generated/lock file folded; pass paths=[\"{path}\"] to expand)\n"
                ));
            } else {
                out.push_str(block);
            }
        }
    }

    if let Some(max) = opts.max_lines.filter(|m| *m > 0) {
        let all_lines: Vec<&str> = out.lines().collect();
        if all_lines.len() > max {
            let omitted = all_lines.len() - max;
            let mut truncated = all_lines[..max].join("\n");
            truncated.push_str(&format!(
                "\n... ({omitted} more lines truncated; use stat_only=true or paths=[...] to narrow scope)\n"
            ));
            return Ok(truncated);
        }
    }

    Ok(out)
}

/// `git status --porcelain` 的一行是否屬於 `.ertangle/` 目錄下的路徑。
pub(crate) fn is_ertangle_status_line(line: &str) -> bool {
    let path = line.get(3..).unwrap_or("").trim();
    path == ".ertangle" || path.starts_with(".ertangle/")
}

pub(crate) fn git_output(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| tr!("執行 git {args} 失敗", args = args.join(" ")))
}

/// 取得 `dir` 所在 repo 的根目錄。
pub(crate) fn git_toplevel(dir: &Path) -> Option<PathBuf> {
    let out = git_output(dir, &["rev-parse", "--show-toplevel"]).ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// session 所屬的主 repo；舊事件沒有記錄時退回預設目錄。
pub(crate) fn session_repo(default_cwd: &Path, s: &Session) -> PathBuf {
    s.repo_root
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_cwd.to_path_buf())
}

/// 目錄內是否有未 commit 的變更 (忽略 `.ertangle/`)。
pub(crate) fn has_uncommitted(dir: &Path) -> Result<bool> {
    let out = git_output(dir, &["status", "--porcelain"])?;
    if !out.status.success() {
        bail!(
            "{}",
            tr!(
                "git status 失敗：{detail}",
                detail = String::from_utf8_lossy(&out.stderr).trim()
            )
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().any(|l| {
        !l.trim().is_empty()
            && !is_ertangle_status_line(l)
            && !is_provisioned_path(dir, l.get(3..).unwrap_or(l).trim())
    }))
}

/// 合併衝突資訊。
#[derive(Debug, Clone)]
pub(crate) struct MergeConflict {
    pub branch: String,
    pub target: String,
    pub files: Vec<String>,
}

impl std::fmt::Display for MergeConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            tr!(
                "合併 {branch} 到 {target} 發生衝突，已還原。衝突檔案：{files}",
                branch = self.branch,
                target = self.target,
                files = self.files.join(", ")
            )
        )
    }
}

impl std::error::Error for MergeConflict {}

/// 從指定工作目錄收集衝突檔案清單。
fn collect_conflicts(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    if let Ok(out) = git_output(dir, &["diff", "--name-only", "--diff-filter=U"]) {
        files.extend(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
    }
    if files.is_empty() {
        if let Ok(st) = git_output(dir, &["status", "--porcelain"]) {
            for line in String::from_utf8_lossy(&st.stdout).lines() {
                let bytes = line.as_bytes();
                if bytes.len() >= 4 {
                    let code = &bytes[0..2];
                    if matches!(code, b"UU" | b"AA" | b"UD" | b"DU" | b"DD" | b"AU" | b"UA") {
                        files.push(line[3..].trim().to_string());
                    }
                }
            }
        }
    }
    files
}

/// 在 session 的 worktree 執行 `git merge <target_branch> --no-commit` 產生衝突標記，並回傳衝突檔案清單。
pub(crate) fn prepare_conflict_in_worktree(
    wt_dir: &Path,
    target_branch: &str,
) -> Result<Vec<String>> {
    let _ = git_output(wt_dir, &["merge", "--no-commit", target_branch])?;
    Ok(collect_conflicts(wt_dir))
}

fn run_pre_merge_command(wt_dir: &Path, cmd: &str) -> Result<()> {
    let child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(wt_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| tr!("啟動合併前驗證指令失敗：{cmd}", cmd = cmd))?;

    let child_id = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = child.wait_with_output();
        let _ = tx.send(out);
    });

    match rx.recv_timeout(std::time::Duration::from_secs(300)) {
        Ok(Ok(output)) => {
            if !output.status.success() {
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                let lines: Vec<&str> = combined.lines().collect();
                let start_idx = lines.len().saturating_sub(20);
                let tail = lines[start_idx..].join("\n");
                bail!(
                    "{}",
                    tr!(
                        "合併前驗證指令失敗 ({cmd})：\n{tail}",
                        cmd = cmd,
                        tail = tail
                    )
                );
            }
            Ok(())
        }
        Ok(Err(e)) => bail!("{}", tr!("執行合併前驗證指令失敗：{e}", e = e)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let _ = std::process::Command::new("kill")
                .args(["-9", &child_id.to_string()])
                .output();
            bail!("{}", tr!("合併前驗證指令逾時 (300 秒)"));
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            bail!("{}", tr!("執行合併前驗證指令異常結束"));
        }
    }
}

/// 若 worktree 內有尚未 commit 的變更，先自動 commit 到該 worktree 分支上。
pub(crate) fn auto_commit_worktree(wt_dir: &Path, label: &str) -> Result<()> {
    let unmerged_out = git_output(wt_dir, &["diff", "--name-only", "--diff-filter=U"])?;
    let has_unmerged_diff = String::from_utf8_lossy(&unmerged_out.stdout)
        .lines()
        .any(|l| !l.trim().is_empty());
    let mut has_unmerged_status = false;
    if let Ok(st) = git_output(wt_dir, &["status", "--porcelain"]) {
        for line in String::from_utf8_lossy(&st.stdout).lines() {
            let bytes = line.as_bytes();
            if bytes.len() >= 4 {
                let code = &bytes[0..2];
                if matches!(code, b"UU" | b"AA" | b"UD" | b"DU" | b"DD" | b"AU" | b"UA") {
                    has_unmerged_status = true;
                    break;
                }
            }
        }
    }
    if has_unmerged_diff || has_unmerged_status {
        bail!("{}", tr!("工作區仍有未解決的合併衝突標記"));
    }

    if !has_uncommitted(wt_dir)? {
        return Ok(());
    }
    let provisioned = read_provisioned_links(wt_dir);
    let mut add_args: Vec<String> = vec![
        "add".to_string(),
        "-A".to_string(),
        "--".to_string(),
        ".".to_string(),
        ":(exclude).ertangle".to_string(),
    ];
    for p in provisioned {
        add_args.push(format!(":(exclude){p}"));
    }
    let add_refs: Vec<&str> = add_args.iter().map(|s| s.as_str()).collect();
    let add = git_output(wt_dir, &add_refs)?;
    if !add.status.success() {
        bail!(
            "{}",
            tr!(
                "git add 失敗：{detail}",
                detail = String::from_utf8_lossy(&add.stderr).trim()
            )
        );
    }
    let commit = git_output(
        wt_dir,
        &["commit", "-m", &format!("ertangle: changes from {label}")],
    )?;
    if !commit.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&commit.stdout),
            String::from_utf8_lossy(&commit.stderr)
        );
        if !text.contains("nothing to commit") && !text.contains("nothing added to commit") {
            bail!(
                "{}",
                tr!("commit worktree 失敗：{detail}", detail = text.trim())
            );
        }
    }
    Ok(())
}

/// 把 session 的 worktree 分支合併回主工作區 (預設一般合併)。
#[allow(dead_code)]
pub(crate) fn merge_worktree(main_repo: &Path, session: &Session, dry_run: bool) -> Result<String> {
    merge_worktree_with(main_repo, session, dry_run, None)
}

/// 帶自訂前置驗證指令 (如測試時覆寫，避免多執行緒共用設定檔競爭) 的合併實作 (預設一般合併)。
#[allow(dead_code)]
pub(crate) fn merge_worktree_with(
    main_repo: &Path,
    session: &Session,
    dry_run: bool,
    pre_merge_cmd_override: Option<&str>,
) -> Result<String> {
    merge_worktree_strategy_with(
        main_repo,
        session,
        MergeStrategy::Merge,
        dry_run,
        pre_merge_cmd_override,
    )
}

/// 帶指定合併策略的 entry point。
pub(crate) fn merge_worktree_strategy(
    main_repo: &Path,
    session: &Session,
    strategy: MergeStrategy,
    dry_run: bool,
) -> Result<String> {
    merge_worktree_strategy_with(main_repo, session, strategy, dry_run, None)
}

/// 帶自訂前置驗證指令與指定合併策略的合併實作。
pub(crate) fn merge_worktree_strategy_with(
    main_repo: &Path,
    session: &Session,
    strategy: MergeStrategy,
    dry_run: bool,
    pre_merge_cmd_override: Option<&str>,
) -> Result<String> {
    let branch = session
        .branch
        .as_deref()
        .with_context(|| tr!("此 session 沒有獨立的 worktree 分支"))?;
    // 前置檢查：任一不成立就不動 repo。
    let git_dir_out = git_output(main_repo, &["rev-parse", "--git-dir"])?;
    if !git_dir_out.status.success() {
        bail!(
            "{}",
            tr!("{repo} 不是 git repo", repo = format!("{main_repo:?}"))
        );
    }
    let git_dir = {
        let p = String::from_utf8_lossy(&git_dir_out.stdout)
            .trim()
            .to_string();
        let p = PathBuf::from(p);
        if p.is_absolute() {
            p
        } else {
            main_repo.join(p)
        }
    };
    for marker in ["MERGE_HEAD", "rebase-merge", "rebase-apply"] {
        if git_dir.join(marker).exists() {
            bail!(
                "{}",
                tr!(
                    "主工作區正在合併、rebase 中 ({marker})，請先處理完畢",
                    marker = marker
                )
            );
        }
    }
    if has_uncommitted(main_repo)? {
        bail!(
            "{}",
            tr!("主工作區有未 commit 的變更，請先 commit 或 stash 後再合併")
        );
    }
    let target = git_output(main_repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let target = String::from_utf8_lossy(&target.stdout).trim().to_string();

    let wt_dir = Path::new(&session.cwd);
    if dry_run {
        // 乾跑：不改動 repo，回報會 commit 的檔案與是否可快轉。
        let mut pending_files = Vec::new();
        if wt_dir.is_dir() {
            if let Ok(st) = git_output(wt_dir, &["status", "--porcelain"]) {
                for line in String::from_utf8_lossy(&st.stdout).lines() {
                    if !line.trim().is_empty() && !is_ertangle_status_line(line) {
                        pending_files.push(line.get(3..).unwrap_or(line).trim().to_string());
                    }
                }
            }
        }
        let can_ff = git_output(main_repo, &["merge-base", "--is-ancestor", "HEAD", branch])
            .map(|o| o.status.success())
            .unwrap_or(false);
        let ff_desc = if can_ff {
            tr!("可快轉 (fast-forward)")
        } else {
            tr!("需三方合併 (three-way merge)")
        };
        let pending_desc = if pending_files.is_empty() {
            tr!("無未提交變更")
        } else {
            tr!(
                "待提交檔案 ({count})：{files}",
                count = pending_files.len(),
                files = pending_files.join(", ")
            )
        };
        return Ok(tr!(
            "[dry-run] [{strategy}] 合併 {branch} ({label})至 {target}：{ff_desc}，{pending_desc}",
            strategy = strategy,
            branch = branch,
            label = session.label,
            target = target,
            ff_desc = ff_desc,
            pending_desc = pending_desc
        ));
    }

    if wt_dir.is_dir() {
        auto_commit_worktree(wt_dir, &session.label)?;
        let pre_merge_cmd = pre_merge_cmd_override.map(str::to_string).or_else(|| {
            crate::config::Config::load_file(&crate::paths::config_dir().join("config.toml"))
                .git
                .pre_merge_command
        });
        if let Some(cmd) = pre_merge_cmd
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            run_pre_merge_command(wt_dir, cmd)?;
        }
    }

    match strategy {
        MergeStrategy::Merge => {
            let out = git_output(main_repo, &["merge", "--no-edit", branch])?;
            if !out.status.success() {
                let conflicts = collect_conflicts(main_repo);
                let _ = git_output(main_repo, &["merge", "--abort"]);
                let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if conflicts.is_empty() {
                    bail!(
                        "{}",
                        tr!(
                            "合併 {branch} 到 {target} 失敗，已還原：{detail}",
                            branch = branch,
                            target = target,
                            detail = detail
                        )
                    );
                }
                return Err(anyhow::Error::new(MergeConflict {
                    branch: branch.to_string(),
                    target,
                    files: conflicts,
                }));
            }
            Ok(tr!(
                "已將 {branch} ({label})合併進 {target}",
                branch = branch,
                label = session.label,
                target = target
            ))
        }
        MergeStrategy::Squash => {
            let out = git_output(main_repo, &["merge", "--squash", "--no-edit", branch])?;
            if !out.status.success() {
                let conflicts = collect_conflicts(main_repo);
                let _ = git_output(main_repo, &["reset", "--hard", "HEAD"]);
                let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if conflicts.is_empty() {
                    bail!(
                        "{}",
                        tr!(
                            "合併 {branch} 到 {target} 失敗，已還原：{detail}",
                            branch = branch,
                            target = target,
                            detail = detail
                        )
                    );
                }
                return Err(anyhow::Error::new(MergeConflict {
                    branch: branch.to_string(),
                    target,
                    files: conflicts,
                }));
            }
            let diff_cached = git_output(main_repo, &["diff", "--cached", "--quiet"])?;
            if diff_cached.status.success() {
                return Ok(tr!(
                    "{branch} ({label}) 已是最新狀態，無需合併",
                    branch = branch,
                    label = session.label
                ));
            }
            let commit_msg = format!("chore(worktree): squash merge {branch}");
            let commit_out = git_output(main_repo, &["commit", "-m", &commit_msg])?;
            if !commit_out.status.success() {
                let detail = String::from_utf8_lossy(&commit_out.stderr)
                    .trim()
                    .to_string();
                bail!("{}", tr!("squash commit 失敗：{detail}", detail = detail));
            }
            Ok(tr!(
                "已將 {branch} ({label})合併進 {target}",
                branch = branch,
                label = session.label,
                target = target
            ))
        }
        MergeStrategy::Rebase => {
            if !wt_dir.is_dir() {
                bail!("{}", tr!("worktree 目錄不存在：{cwd}", cwd = session.cwd));
            }
            let rebase_out = git_output(wt_dir, &["rebase", &target])?;
            if !rebase_out.status.success() {
                let conflicts = collect_conflicts(wt_dir);
                let _ = git_output(wt_dir, &["rebase", "--abort"]);
                let detail = String::from_utf8_lossy(&rebase_out.stderr)
                    .trim()
                    .to_string();
                if conflicts.is_empty() {
                    bail!(
                        "{}",
                        tr!(
                            "rebase {branch} 至 {target} 失敗，已還原：{detail}",
                            branch = branch,
                            target = target,
                            detail = detail
                        )
                    );
                }
                return Err(anyhow::Error::new(MergeConflict {
                    branch: branch.to_string(),
                    target,
                    files: conflicts,
                }));
            }
            let ff_out = git_output(main_repo, &["merge", "--ff-only", branch])?;
            if !ff_out.status.success() {
                let detail = String::from_utf8_lossy(&ff_out.stderr).trim().to_string();
                bail!(
                    "{}",
                    tr!(
                        "快轉合併 {branch} 到 {target} 失敗：{detail}",
                        branch = branch,
                        target = target,
                        detail = detail
                    )
                );
            }
            Ok(tr!(
                "已將 {branch} ({label})合併進 {target}",
                branch = branch,
                label = session.label,
                target = target
            ))
        }
    }
}

/// 透過 `gh pr create` 建立 Pull Request。
pub(crate) fn create_worktree_pr(session: &Session, title: &str, body: &str) -> Result<String> {
    let branch = session
        .branch
        .as_deref()
        .with_context(|| tr!("此 session 沒有獨立分支，無法建立 PR"))?;
    let wt_dir = Path::new(&session.cwd);
    if wt_dir.is_dir() {
        auto_commit_worktree(wt_dir, &session.label)?;
    }
    let out = Command::new("gh")
        .current_dir(wt_dir)
        .args([
            "pr", "create", "--title", title, "--body", body, "--head", branch,
        ])
        .output()
        .with_context(|| tr!("找不到或無法執行 `gh` CLI"))?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 本地分支 `branch` 是否存在。
pub(crate) fn branch_exists(main_repo: &Path, branch: &str) -> bool {
    git_output(
        main_repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok_and(|o| o.status.success())
}

/// session 的 worktree 目錄與分支都已不存在 (沒有東西可清)。
pub(crate) fn worktree_gone(main_repo: &Path, session: &Session) -> bool {
    !Path::new(&session.cwd).exists()
        && session
            .branch
            .as_ref()
            .is_none_or(|b| !branch_exists(main_repo, b))
}

/// gc 預設保護：未合併或有未 commit 變更時回傳原因。
pub(crate) fn gc_block_reason(main_repo: &Path, session: &Session) -> Option<String> {
    let wt_dir = Path::new(&session.cwd);
    if wt_dir.is_dir() {
        match has_uncommitted(wt_dir) {
            Ok(true) => return Some(tr!("有未 commit 變更")),
            Ok(false) => {}
            Err(e) => return Some(tr!("無法檢查狀態：{e}", e = e)),
        }
    }
    if let Some(branch) = &session.branch {
        // 分支已不存在 (例如共用 worktree 的另一個 session 已把它刪掉)：
        // 目錄也不在就沒有東西要保護；目錄還在則保守地擋下。
        if !branch_exists(main_repo, branch) {
            return wt_dir.exists().then(|| tr!("分支不存在，worktree 仍在"));
        }
        match git_output(main_repo, &["merge-base", "--is-ancestor", branch, "HEAD"]) {
            Ok(o) if o.status.success() => {}
            Ok(_) => return Some(tr!("尚未合併")),
            Err(e) => return Some(tr!("無法檢查合併狀態：{e}", e = e)),
        }
    }
    None
}

/// 移除某個 session 的 worktree 與對應分支；`force` 時連未合併／未 commit 的內容一併丟棄。
pub(crate) fn remove_worktree(main_repo: &Path, session: &Session, force: bool) -> Result<()> {
    let wt_dir = Path::new(&session.cwd);
    if wt_dir.exists() {
        cleanup_provisioned_links(wt_dir);
        let wt = wt_dir.to_string_lossy();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&wt);
        let out = git_output(main_repo, &args)?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
    }
    if let Some(branch) = session
        .branch
        .as_ref()
        .filter(|b| branch_exists(main_repo, b))
    {
        let out = git_output(
            main_repo,
            &["branch", if force { "-D" } else { "-d" }, branch],
        )?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
    }
    Ok(())
}

/// 退回 (捨棄) `wt_dir` 中某個檔案的單一 Hunk (`patch` 為 `Some`) 或整個檔案變更 (`patch` 為 `None`)。
pub(crate) fn discard_worktree_change(
    wt_dir: &Path,
    file: &str,
    patch: Option<&str>,
) -> Result<String> {
    if !wt_dir.is_dir() {
        bail!(
            "{}",
            tr!("工作目錄不存在：{cwd}", cwd = format!("{wt_dir:?}"))
        );
    }
    let file = file.trim();
    let rel_path = Path::new(file);
    let is_safe = !file.is_empty()
        && !rel_path.is_absolute()
        && file != ".ertangle"
        && !file.starts_with(".ertangle/")
        && rel_path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
    if !is_safe {
        bail!("{}", tr!("不安全的檔案路徑：{file}", file = file));
    }

    if let Some(patch_str) = patch {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(wt_dir)
            .args(["apply", "-R", "--unidiff-zero", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| tr!("執行 git apply 失敗"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(patch_str.as_bytes())?;
            if !patch_str.ends_with('\n') {
                stdin.write_all(b"\n")?;
            }
        }
        let out = child
            .wait_with_output()
            .with_context(|| tr!("等待 git apply 失敗"))?;
        if !out.status.success() {
            bail!(
                "{}",
                tr!(
                    "退回 hunk 失敗：{detail}",
                    detail = String::from_utf8_lossy(&out.stderr).trim()
                )
            );
        }
        return Ok(tr!("已退回 {file} 的選取區塊", file = file));
    }

    let untracked = git_output(
        wt_dir,
        &["ls-files", "--others", "--exclude-standard", "--", file],
    )?;
    let is_untracked = String::from_utf8_lossy(&untracked.stdout)
        .lines()
        .any(|l| l.trim() == file);
    if is_untracked {
        let full = wt_dir.join(rel_path);
        if full.exists() {
            fs::remove_file(&full)
                .with_context(|| tr!("刪除未追蹤檔案失敗：{file}", file = file))?;
        }
    } else {
        let out = git_output(wt_dir, &["checkout", "HEAD", "--", file])?;
        if !out.status.success() {
            bail!(
                "{}",
                tr!(
                    "還原檔案失敗：{detail}",
                    detail = String::from_utf8_lossy(&out.stderr).trim()
                )
            );
        }
    }
    Ok(tr!("已還原檔案 {file}", file = file))
}
