//! Best-effort install of the `/porch` agent skill into user skill dirs.
//!
//! Copies an embedded skill (`porch-agent.md` in this crate) for coding agents already
//! on PATH (`claude`, `codex`). Fail soft when agent home dirs are missing.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Skill directory name under each agent's `skills/` root.
pub const SKILL_NAME: &str = "porch";

const AGENT_BODY: &str = include_str!("../porch-agent.md");

/// One coding-agent binary → relative skills root under `$HOME`.
const AGENT_TARGETS: &[(&str, &str)] = &[("claude", ".claude/skills"), ("codex", ".codex/skills")];

/// Result of a best-effort skill install.
#[derive(Debug, Clone, Default)]
pub struct SkillInstallReport {
    pub written: Vec<PathBuf>,
    pub skipped_identical: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

/// Render SKILL.md (YAML frontmatter + in-tree body). JSON/`porch agent`, not TOON.
#[must_use]
pub fn skill_markdown() -> String {
    let mut out = String::from(
        "---\n\
name: porch\n\
description: >-\n\
  Drive the porch local git gate headlessly via `porch agent` JSON\n\
  (run / status / respond / sync). Use to push-and-wait, handle parked\n\
  review or rebase, or check gate status / custody without a TUI.\n\
  Never merge; never babysit deploy.\n\
---\n\n",
    );
    out.push_str(AGENT_BODY.trim_start());
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Install skill into dirs for agents currently on `PATH`.
///
/// Only agents found on `PATH` are considered. If the agent config root
/// (e.g. `~/.claude`) does not exist, skip with a warning — do not create it.
/// When the root exists, create `skills/porch/` as needed and overwrite
/// `SKILL.md` when content differs.
#[must_use]
pub fn install_agent_skills(user_home: &Path) -> SkillInstallReport {
    let detected: Vec<&str> = AGENT_TARGETS
        .iter()
        .filter(|(bin, _)| bin_on_path(bin))
        .map(|(bin, _)| *bin)
        .collect();
    install_agent_skills_for(user_home, &detected)
}

/// Install skill for the given agent binary names (testable without mutating PATH).
#[must_use]
pub fn install_agent_skills_for(user_home: &Path, detected_bins: &[&str]) -> SkillInstallReport {
    let mut report = SkillInstallReport::default();
    let content = skill_markdown();
    let content_bytes = content.as_bytes();

    for &(bin, skills_rel) in AGENT_TARGETS {
        if !detected_bins.contains(&bin) {
            continue;
        }
        let agent_root = user_home.join(agent_home_dir(skills_rel));
        if !agent_root.is_dir() {
            report.warnings.push(format!(
                "skill: `{bin}` on PATH but {} missing — skip (create it or re-run init later)",
                agent_root.display()
            ));
            continue;
        }
        let dest_dir = user_home.join(skills_rel).join(SKILL_NAME);
        let dest = dest_dir.join("SKILL.md");
        match write_skill_file(&dest_dir, &dest, content_bytes) {
            Ok(SkillWrite::Written) => report.written.push(dest),
            Ok(SkillWrite::Overwritten) => {
                report.warnings.push(format!(
                    "skill: replaced local edits at {} with bundled skill",
                    dest.display()
                ));
                report.written.push(dest);
            }
            Ok(SkillWrite::Identical) => report.skipped_identical.push(dest),
            Err(e) => report
                .warnings
                .push(format!("skill: could not write {}: {e}", dest.display())),
        }
    }

    report
}

/// `$HOME` from the environment, if set.
#[must_use]
pub fn user_home_from_env() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn agent_home_dir(skills_rel: &str) -> &str {
    // ".claude/skills" → ".claude"
    skills_rel
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(skills_rel)
}

enum SkillWrite {
    Written,
    /// Existing file differed; bundled content replaced local edits.
    Overwritten,
    Identical,
}

fn write_skill_file(dest_dir: &Path, dest: &Path, content: &[u8]) -> std::io::Result<SkillWrite> {
    let overwrite = if dest.is_file() {
        let existing = fs::read(dest)?;
        if existing == content {
            return Ok(SkillWrite::Identical);
        }
        true
    } else {
        false
    };
    fs::create_dir_all(dest_dir)?;
    fs::write(dest, content)?;
    Ok(if overwrite {
        SkillWrite::Overwritten
    } else {
        SkillWrite::Written
    })
}

fn bin_on_path(name: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return true;
        }
    }
    false
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn skill_markdown_has_frontmatter_and_agent_json() {
        let md = skill_markdown();
        assert!(md.starts_with("---\n"));
        assert!(md.contains("name: porch"));
        assert!(md.contains("porch agent"));
        assert!(md.contains("porch agent run"));
        assert!(md.contains("never merge") || md.contains("Never merge"));
        assert!(md.contains("babysit deploy"));
        assert!(md.contains("fix --yes"));
        assert!(!md.to_lowercase().contains("toon"));
    }

    #[test]
    fn install_writes_for_detected_agents_only() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();

        let report = install_agent_skills_for(&home, &["claude"]);

        assert_eq!(report.written.len(), 1);
        assert!(report.written[0].ends_with(".claude/skills/porch/SKILL.md"));
        assert!(!home.join(".codex/skills/porch/SKILL.md").exists());
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn install_skips_when_agent_home_missing() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();

        let report = install_agent_skills_for(&home, &["claude"]);

        assert!(report.written.is_empty());
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains(".claude") && w.contains("missing"))
        );
    }

    #[test]
    fn install_idempotent_when_identical() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".claude")).unwrap();

        let first = install_agent_skills_for(&home, &["claude"]);
        assert_eq!(first.written.len(), 1);
        let second = install_agent_skills_for(&home, &["claude"]);
        assert!(second.written.is_empty());
        assert_eq!(second.skipped_identical.len(), 1);
    }

    #[test]
    fn install_warns_when_replacing_diverged_local_edits() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let dest_dir = home.join(".claude/skills/porch");
        fs::create_dir_all(&dest_dir).unwrap();
        fs::write(dest_dir.join("SKILL.md"), b"local edits\n").unwrap();

        let report = install_agent_skills_for(&home, &["claude"]);

        assert_eq!(report.written.len(), 1);
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("replaced local edits")),
            "warnings={:?}",
            report.warnings
        );
        let body = fs::read_to_string(dest_dir.join("SKILL.md")).unwrap();
        assert!(body.starts_with("---\n"));
    }
}
