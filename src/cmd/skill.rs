//! `ertangle skill install|uninstall|show`：安裝內嵌的 `ertangle-ade` skill。

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tr::tr;

use crate::SkillCmd;

/// 解析安裝根目錄：`--global` 用家目錄，否則用目前專案 (git 根目錄，沒有就是目前目錄)。
fn base_dir(global: bool) -> Result<PathBuf> {
    if global {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .with_context(|| tr!("找不到家目錄 (HOME)"))?;
        return Ok(home);
    }
    let cwd = std::env::current_dir()?;
    Ok(cwd
        .ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(&cwd)
        .to_path_buf())
}

pub(crate) fn run_in(base: &Path, cmd: &SkillCmd) -> Result<String> {
    match cmd {
        SkillCmd::Show => Ok(ertangle_daemon::skill::SKILL_MD.to_string()),
        SkillCmd::Install { target, .. } => {
            let p = ertangle_daemon::skill::install(base, target)?;
            Ok(tr!("已安裝 skill：{p}", p = p.display()))
        }
        SkillCmd::Uninstall { target, .. } => {
            match ertangle_daemon::skill::uninstall(base, target)? {
                Some(d) => Ok(tr!("已移除 skill：{p}", p = d.display())),
                None => bail!(tr!("沒有安裝 skill (target {t})", t = target)),
            }
        }
    }
}

pub(crate) fn cmd_skill(cmd: SkillCmd) -> Result<()> {
    let global = match &cmd {
        SkillCmd::Install { global, .. } | SkillCmd::Uninstall { global, .. } => *global,
        SkillCmd::Show => false,
    };
    let base = if matches!(cmd, SkillCmd::Show) {
        PathBuf::new()
    } else {
        base_dir(global)?
    };
    let out = run_in(&base, &cmd)?;
    if matches!(cmd, SkillCmd::Show) {
        print!("{out}");
    } else {
        println!("{out}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_show_uninstall_in_temp_dir() {
        let t = std::env::temp_dir().join(ertangle_core::new_id("skillcli"));
        std::fs::create_dir_all(&t).unwrap();
        let inst = SkillCmd::Install {
            target: "claude".into(),
            global: false,
            project: true,
        };
        run_in(&t, &inst).unwrap();
        assert!(t.join(".claude/skills/ertangle-ade/SKILL.md").is_file());
        let un = SkillCmd::Uninstall {
            target: "claude".into(),
            global: false,
            project: true,
        };
        run_in(&t, &un).unwrap();
        assert!(run_in(&t, &un).is_err());
        assert!(run_in(&t, &SkillCmd::Show)
            .unwrap()
            .contains("ertangle-ade"));
        let _ = std::fs::remove_dir_all(&t);
    }
}
