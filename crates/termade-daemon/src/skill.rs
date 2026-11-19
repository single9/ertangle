//! `termade-ade` skill：內嵌 SKILL.md，供 `termade skill install|uninstall|show` 與 `[collab] inject_skill` 使用。

use std::path::{Path, PathBuf};

/// 內嵌的 SKILL.md 全文。
pub const SKILL_MD: &str = include_str!("skills/termade-ade/SKILL.md");
pub const SKILL_NAME: &str = "termade-ade";

/// 去掉 frontmatter 後的內文 (給 `inject_skill` 用)。
pub fn skill_body() -> &'static str {
    let rest = SKILL_MD.strip_prefix("---\n").unwrap_or(SKILL_MD);
    match rest.split_once("\n---\n") {
        Some((_, body)) => body.trim(),
        None => SKILL_MD.trim(),
    }
}

/// 安裝目標：`claude` → `.claude/skills`、`agents` → `.agents/skills`、`codex` → `.codex/skills`。
pub fn target_dir(target: &str) -> Option<&'static str> {
    match target {
        "claude" => Some(".claude/skills"),
        "agents" => Some(".agents/skills"),
        "codex" => Some(".codex/skills"),
        _ => None,
    }
}

/// SKILL.md 的完整路徑；`base` 為家目錄 (global) 或專案根目錄 (project)。
pub fn skill_path(base: &Path, target: &str) -> Option<PathBuf> {
    Some(
        base.join(target_dir(target)?)
            .join(SKILL_NAME)
            .join("SKILL.md"),
    )
}

pub fn install(base: &Path, target: &str) -> std::io::Result<PathBuf> {
    let path = skill_path(base, target).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "unknown skill target")
    })?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, SKILL_MD)?;
    Ok(path)
}

/// 移除已安裝的 skill 目錄；原本就不存在回 `Ok(None)`。
pub fn uninstall(base: &Path, target: &str) -> std::io::Result<Option<PathBuf>> {
    let path = skill_path(base, target).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "unknown skill target")
    })?;
    let Some(dir) = path.parent() else {
        return Ok(None);
    };
    if !dir.exists() {
        return Ok(None);
    }
    std::fs::remove_dir_all(dir)?;
    Ok(Some(dir.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_is_short_and_has_frontmatter() {
        assert!(SKILL_MD.starts_with("---\nname: termade-ade\n"));
        assert!(SKILL_MD.lines().count() < 120);
        assert!(skill_body().starts_with("# termade ADE"));
    }

    #[test]
    fn install_and_uninstall_paths() {
        let t = std::env::temp_dir().join(termade_core::new_id("skill"));
        std::fs::create_dir_all(&t).unwrap();
        for (target, rel) in [
            ("claude", ".claude/skills/termade-ade/SKILL.md"),
            ("agents", ".agents/skills/termade-ade/SKILL.md"),
            ("codex", ".codex/skills/termade-ade/SKILL.md"),
        ] {
            let p = install(&t, target).unwrap();
            assert_eq!(p, t.join(rel));
            assert_eq!(std::fs::read_to_string(&p).unwrap(), SKILL_MD);
            assert!(uninstall(&t, target).unwrap().is_some());
            assert!(!p.exists());
            assert!(uninstall(&t, target).unwrap().is_none());
        }
        assert!(install(&t, "bogus").is_err());
        assert!(uninstall(&t, "bogus").is_err());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn skill_path_handling() {
        let base = Path::new("/tmp/test_dir");
        assert_eq!(
            skill_path(base, "claude"),
            Some(base.join(".claude/skills/termade-ade/SKILL.md"))
        );
        assert_eq!(
            skill_path(base, "agents"),
            Some(base.join(".agents/skills/termade-ade/SKILL.md"))
        );
        assert_eq!(
            skill_path(base, "codex"),
            Some(base.join(".codex/skills/termade-ade/SKILL.md"))
        );
        assert_eq!(skill_path(base, "unknown"), None);
        assert_eq!(skill_path(base, ""), None);
        assert!(uninstall(base, "unknown").is_err());
    }
}
