//! Git／worktree 輔助函式：建立、狀態、diff、auto-commit、merge、PR、gc。

use super::*;

/// 在 `repo` 建立一個 worktree，回傳 (路徑, 分支)。
pub(crate) fn make_worktree(repo: &Path, id: &str) -> Result<(PathBuf, String)> {
    let branch = format!("ade/{id}");
    let dir = repo.join(".ade").join("wt").join(id);
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "add", "-b", &branch])
        .arg(&dir)
        .output()
        .context("執行 git worktree 失敗")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok((dir, branch))
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
    let lines: Vec<&str> = text.lines().filter(|l| !is_ade_status_line(l)).collect();
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

/// 取得工作目錄的統一格式 diff（含已追蹤變更與新增未追蹤檔案的內容預覽）。
pub fn compute_worktree_diff(cwd: &Path) -> Result<String> {
    if !cwd.is_dir() {
        bail!("工作目錄不存在：{cwd:?}");
    }
    let mut out = String::new();

    // 1) 已追蹤檔案相對於 HEAD 的 diff。
    let diff = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["diff", "HEAD"])
        .output()
        .context("執行 git diff 失敗")?;
    let diff_str = String::from_utf8_lossy(&diff.stdout);
    if !diff_str.trim().is_empty() {
        out.push_str(&diff_str);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }

    // 2) 未追蹤的新檔案（排除 `.ade/`）：也產生 unified diff 樣式，方便在 TUI Diff 視圖審查與留註解。
    let untracked = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["ls-files", "--others", "--exclude-standard"])
        .output()
        .context("執行 git ls-files 失敗")?;
    for rel in String::from_utf8_lossy(&untracked.stdout).lines() {
        let rel = rel.trim();
        if rel.is_empty() || rel.starts_with(".ade/") {
            continue;
        }
        let full = cwd.join(rel);
        if let Ok(content) = fs::read_to_string(&full) {
            out.push_str(&format!("diff --git a/{rel} b/{rel}\n"));
            out.push_str("new file mode 100644\n");
            out.push_str("--- /dev/null\n");
            out.push_str(&format!("+++ b/{rel}\n"));
            let lines: Vec<&str> = content.lines().collect();
            out.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
            for (idx, l) in lines.iter().take(200).enumerate() {
                out.push_str(&format!("+{l}\n"));
                if idx == 199 && lines.len() > 200 {
                    out.push_str("+... (truncated)\n");
                }
            }
        }
    }

    if out.trim().is_empty() {
        Ok("(目前工作區沒有尚未合併的變更)\n".to_string())
    } else {
        Ok(out)
    }
}

/// `git status --porcelain` 的一行是否屬於 `.ade/` 目錄下的路徑。
pub(crate) fn is_ade_status_line(line: &str) -> bool {
    let path = line.get(3..).unwrap_or("").trim();
    path == ".ade" || path.starts_with(".ade/")
}

pub(crate) fn git_output(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("執行 git {} 失敗", args.join(" ")))
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

/// 目錄內是否有未 commit 的變更（忽略 `.ade/`）。
pub(crate) fn has_uncommitted(dir: &Path) -> Result<bool> {
    let out = git_output(dir, &["status", "--porcelain"])?;
    if !out.status.success() {
        bail!(
            "git status 失敗：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| !l.trim().is_empty() && !is_ade_status_line(l)))
}

/// 若 worktree 內有尚未 commit 的變更，先自動 commit 到該 worktree 分支上。
pub(crate) fn auto_commit_worktree(wt_dir: &Path, label: &str) -> Result<()> {
    if !has_uncommitted(wt_dir)? {
        return Ok(());
    }
    let add = git_output(wt_dir, &["add", "-A", "--", ".", ":(exclude).ade"])?;
    if !add.status.success() {
        bail!(
            "git add 失敗：{}",
            String::from_utf8_lossy(&add.stderr).trim()
        );
    }
    let commit = git_output(
        wt_dir,
        &["commit", "-m", &format!("ade: changes from {label}")],
    )?;
    if !commit.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&commit.stdout),
            String::from_utf8_lossy(&commit.stderr)
        );
        if !text.contains("nothing to commit") && !text.contains("nothing added to commit") {
            bail!("commit worktree 失敗：{}", text.trim());
        }
    }
    Ok(())
}

/// 把 session 的 worktree 分支合併回主工作區。
pub(crate) fn merge_worktree(main_repo: &Path, session: &Session) -> Result<String> {
    let branch = session
        .branch
        .as_deref()
        .context("此 session 沒有獨立的 worktree 分支")?;
    // 前置檢查：任一不成立就不動 repo。
    let git_dir_out = git_output(main_repo, &["rev-parse", "--git-dir"])?;
    if !git_dir_out.status.success() {
        bail!("{main_repo:?} 不是 git repo");
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
            bail!("主工作區正在合併／rebase 中（{marker}），請先處理完畢");
        }
    }
    if has_uncommitted(main_repo)? {
        bail!("主工作區有未 commit 的變更，請先 commit 或 stash 後再合併");
    }
    let target = git_output(main_repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let target = String::from_utf8_lossy(&target.stdout).trim().to_string();

    let wt_dir = Path::new(&session.cwd);
    if wt_dir.is_dir() {
        auto_commit_worktree(wt_dir, &session.label)?;
    }
    let out = git_output(main_repo, &["merge", "--no-edit", branch])?;
    if !out.status.success() {
        let conflicts = git_output(main_repo, &["diff", "--name-only", "--diff-filter=U"])
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let _ = git_output(main_repo, &["merge", "--abort"]);
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if conflicts.is_empty() {
            bail!("合併 {branch} 到 {target} 失敗，已還原：{detail}");
        }
        bail!(
            "合併 {branch} 到 {target} 發生衝突，已還原。衝突檔案：{}",
            conflicts.join(", ")
        );
    }
    Ok(format!("已將 {branch}（{}）合併進 {target}", session.label))
}

/// 透過 `gh pr create` 建立 Pull Request。
pub(crate) fn create_worktree_pr(session: &Session, title: &str, body: &str) -> Result<String> {
    let branch = session
        .branch
        .as_deref()
        .context("此 session 沒有獨立分支，無法建立 PR")?;
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
        .context("找不到或無法執行 `gh` CLI")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// gc 預設保護：未合併或有未 commit 變更時回傳原因。
pub(crate) fn gc_block_reason(main_repo: &Path, session: &Session) -> Option<String> {
    let wt_dir = Path::new(&session.cwd);
    if wt_dir.is_dir() {
        match has_uncommitted(wt_dir) {
            Ok(true) => return Some("有未 commit 變更".into()),
            Ok(false) => {}
            Err(e) => return Some(format!("無法檢查狀態：{e}")),
        }
    }
    if let Some(branch) = &session.branch {
        match git_output(main_repo, &["merge-base", "--is-ancestor", branch, "HEAD"]) {
            Ok(o) if o.status.success() => {}
            Ok(_) => return Some("尚未合併".into()),
            Err(e) => return Some(format!("無法檢查合併狀態：{e}")),
        }
    }
    None
}

/// 移除某個 session 的 worktree 與對應分支；`force` 時連未合併／未 commit 的內容一併丟棄。
pub(crate) fn remove_worktree(main_repo: &Path, session: &Session, force: bool) -> Result<()> {
    let wt_dir = Path::new(&session.cwd);
    if wt_dir.exists() {
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
    if let Some(branch) = &session.branch {
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
