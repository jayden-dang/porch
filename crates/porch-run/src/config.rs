//! Parse trusted `.porch.yaml` bytes into certify + deliver settings.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Shell commands from `.porch.yaml` (`commands.*`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Commands {
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub lint: String,
    /// Accepted but unused in M5.
    #[serde(default)]
    pub test: String,
}

/// Trusted GitHub deliver settings (`deliver.github.*`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DeliverGithub {
    /// Allowlisted PR check names to babysit. Empty → push+PR, no watch.
    #[serde(default)]
    pub watch_checks: Vec<String>,
    /// Provider transient rerun budget. Default **0** (never `gh run rerun`).
    #[serde(default = "default_rerun_transient")]
    pub rerun_transient: u32,
}

impl Default for DeliverGithub {
    fn default() -> Self {
        Self {
            watch_checks: Vec::new(),
            rerun_transient: default_rerun_transient(),
        }
    }
}

fn default_rerun_transient() -> u32 {
    0
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct DeliverSection {
    #[serde(default)]
    github: DeliverGithub,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct PrSection {
    #[serde(default)]
    base_branch: String,
}

/// One `review.path_instructions` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct PathInstruction {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub instructions: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct ReviewSection {
    #[serde(default)]
    path_instructions: Vec<PathInstruction>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct AutoFixSection {
    /// Parsed; D6 runtime still defaults off unless a consumer exists.
    #[serde(default)]
    review: u32,
}

/// Full trusted config parse result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PorchConfig {
    pub commands: Commands,
    pub deliver_github: DeliverGithub,
    /// Non-empty → rebase onto / `gh pr create --base` use this instead of
    /// `repos.default_branch`.
    pub pr_base_branch: String,
    pub path_instructions: Vec<PathInstruction>,
    pub auto_fix_review: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct PorchYaml {
    #[serde(default)]
    commands: Commands,
    #[serde(default)]
    deliver: DeliverSection,
    #[serde(default)]
    pr: PrSection,
    #[serde(default)]
    review: ReviewSection,
    #[serde(default)]
    auto_fix: AutoFixSection,
}

/// Parse `.porch.yaml` including deliver / pr / review fields.
///
/// # Errors
///
/// Returns a parse error string when YAML is present but unparseable.
pub fn parse_porch_config(bytes: &[u8]) -> Result<PorchConfig, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!(".porch.yaml not utf-8: {e}"))?;
    if text.trim().is_empty() {
        return Ok(PorchConfig::default());
    }
    let doc: PorchYaml =
        serde_yaml::from_str(text).map_err(|e| format!(".porch.yaml parse error: {e}"))?;
    Ok(PorchConfig {
        commands: doc.commands,
        deliver_github: doc.deliver.github,
        pr_base_branch: doc.pr.base_branch,
        path_instructions: doc.review.path_instructions,
        auto_fix_review: doc.auto_fix.review,
    })
}

/// Prefer non-empty `pr.base_branch`; otherwise `repos.default_branch`.
#[must_use]
pub fn effective_base_branch<'a>(pr_base_branch: &'a str, default_branch: &'a str) -> &'a str {
    let trimmed = pr_base_branch.trim();
    if trimmed.is_empty() {
        default_branch
    } else {
        trimmed
    }
}

const TRUSTED_CONFIG_PATH: &str = ".porch.yaml";

/// Resolve `origin/<default_branch>` tip after it has been fetched (E10 pin source).
///
/// # Errors
///
/// Fail closed when the tip cannot be resolved.
pub fn resolve_default_branch_tip(
    bare: &porch_git::GitDir,
    default_branch: &str,
) -> Result<String, String> {
    let origin_ref = format!("refs/remotes/origin/{default_branch}");
    porch_git::rev_parse(bare, &origin_ref)
        .map_err(|e| format!("resolve origin/{default_branch}: {e}"))
}

/// Load trusted `.porch.yaml` from a pinned commit SHA (E10).
///
/// # Errors
///
/// Fail closed when the commit is unreadable or YAML is unparseable.
pub fn load_trusted_at_sha(
    bare: &porch_git::GitDir,
    trusted_sha: &str,
) -> Result<PorchConfig, String> {
    match porch_git::show_path_at(bare, trusted_sha, TRUSTED_CONFIG_PATH)
        .map_err(|e| format!("read trusted {TRUSTED_CONFIG_PATH} at {trusted_sha}: {e}"))?
    {
        None => Ok(PorchConfig::default()),
        Some(bytes) => parse_porch_config(&bytes),
    }
}

/// Persist matching (or all, if none match) path instructions under
/// `$PORCH_HOME/runs/<run_id>/path_instructions.json`.
///
/// # Errors
///
/// Returns I/O or JSON errors.
pub fn persist_path_instructions(
    home: &Path,
    run_id: &str,
    instructions: &[PathInstruction],
    changed_files: &[String],
) -> Result<PathBuf, String> {
    let selected = select_path_instructions(instructions, changed_files);
    let dir = porch_gate::run_artifact_dir(home, run_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create run artifact dir: {e}"))?;
    let path = dir.join("path_instructions.json");
    let json = serde_json::to_vec_pretty(&selected)
        .map_err(|e| format!("serialize path_instructions: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write path_instructions: {e}"))?;
    Ok(path)
}

fn select_path_instructions(
    instructions: &[PathInstruction],
    changed_files: &[String],
) -> Vec<PathInstruction> {
    if instructions.is_empty() {
        return Vec::new();
    }
    let matched: Vec<PathInstruction> = instructions
        .iter()
        .filter(|ins| {
            changed_files
                .iter()
                .any(|f| path_glob_matches(&ins.path, f))
        })
        .cloned()
        .collect();
    if matched.is_empty() {
        instructions.to_vec()
    } else {
        matched
    }
}

/// Minimal glob: `/**`, `/*`, one `*` per path segment (infix or trailing), exact.
/// Basename-only patterns (no `/`, e.g. `.env*`) also match any path's final component.
fn path_glob_matches(pattern: &str, file: &str) -> bool {
    let pat = pattern.trim();
    if pat.is_empty() {
        return false;
    }
    if let Some(prefix) = pat.strip_suffix("/**") {
        return file == prefix || file.starts_with(&format!("{prefix}/"));
    }
    if let Some(prefix) = pat.strip_suffix("/*") {
        if let Some(rest) = file.strip_prefix(&format!("{prefix}/")) {
            return !rest.is_empty() && !rest.contains('/');
        }
        return false;
    }
    if match_path_segments(pat, file) {
        return true;
    }
    // `.env*` → `frontend/.env`, `.env.local`, etc.
    if !pat.contains('/') {
        if let Some(base) = file.rsplit('/').next() {
            return match_one_segment(pat, base);
        }
    }
    false
}

fn match_path_segments(pat: &str, file: &str) -> bool {
    let p: Vec<&str> = pat.split('/').collect();
    let f: Vec<&str> = file.split('/').collect();
    if p.len() != f.len() {
        return false;
    }
    p.iter()
        .zip(f.iter())
        .all(|(ps, fs)| match_one_segment(ps, fs))
}

/// One path segment; at most one `*` (prefix + suffix).
fn match_one_segment(pat: &str, seg: &str) -> bool {
    if pat == "*" {
        return true;
    }
    if let Some(star) = pat.find('*') {
        if pat[star + 1..].contains('*') {
            return false;
        }
        let prefix = &pat[..star];
        let suffix = &pat[star + 1..];
        return seg.starts_with(prefix)
            && seg.ends_with(suffix)
            && seg.len() >= prefix.len() + suffix.len();
    }
    pat == seg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bytes_yield_empty_commands() {
        let cmds = parse_porch_config(b"").unwrap().commands;
        assert_eq!(cmds, Commands::default());
        assert!(cmds.format.is_empty());
        assert!(cmds.lint.is_empty());
    }

    #[test]
    fn whitespace_only_yields_empty_commands() {
        let cmds = parse_porch_config(b"  \n\t\n").unwrap().commands;
        assert_eq!(cmds, Commands::default());
    }

    #[test]
    fn format_only() {
        let cmds = parse_porch_config(
            br#"
commands:
  format: "biome check --write ."
"#,
        )
        .unwrap()
        .commands;
        assert_eq!(cmds.format, "biome check --write .");
        assert!(cmds.lint.is_empty());
        assert!(cmds.test.is_empty());
    }

    #[test]
    fn format_and_lint() {
        let cmds = parse_porch_config(
            br"
commands:
  format: fmt-cmd
  lint: lint-cmd
  test: should-not-run
",
        )
        .unwrap()
        .commands;
        assert_eq!(cmds.format, "fmt-cmd");
        assert_eq!(cmds.lint, "lint-cmd");
        assert_eq!(cmds.test, "should-not-run");
    }

    #[test]
    fn unknown_keys_ignored() {
        let cmds = parse_porch_config(
            br#"
agent: other
commands:
  format: ok
ignore_patterns:
  - "*.lock"
"#,
        )
        .unwrap()
        .commands;
        assert_eq!(cmds.format, "ok");
    }

    #[test]
    fn unparseable_fails_closed() {
        let err = parse_porch_config(b"commands: [not, a, map").unwrap_err();
        assert!(err.contains("parse error"), "{err}");
    }

    #[test]
    fn non_utf8_fails_closed() {
        let err = parse_porch_config(b"\xff\xfe commands:\n  format: x\n").unwrap_err();
        assert!(err.contains("not utf-8"), "{err}");
    }

    #[test]
    fn deliver_defaults_empty_allowlist_rerun_zero() {
        let cfg = parse_porch_config(b"").unwrap();
        assert!(cfg.deliver_github.watch_checks.is_empty());
        assert_eq!(cfg.deliver_github.rerun_transient, 0);

        let cfg = parse_porch_config(
            br"
commands:
  format: x
",
        )
        .unwrap();
        assert!(cfg.deliver_github.watch_checks.is_empty());
        assert_eq!(cfg.deliver_github.rerun_transient, 0);
    }

    #[test]
    fn deliver_github_watch_checks_and_rerun() {
        let cfg = parse_porch_config(
            br"
deliver:
  github:
    watch_checks: [lint, types-check]
    rerun_transient: 0
",
        )
        .unwrap();
        assert_eq!(cfg.deliver_github.watch_checks, vec!["lint", "types-check"]);
        assert_eq!(cfg.deliver_github.rerun_transient, 0);
    }

    #[test]
    fn deliver_github_omitted_rerun_defaults_zero() {
        let cfg = parse_porch_config(
            br"
deliver:
  github:
    watch_checks: [lint]
",
        )
        .unwrap();
        assert_eq!(cfg.deliver_github.watch_checks, vec!["lint"]);
        assert_eq!(cfg.deliver_github.rerun_transient, 0);
    }

    #[test]
    fn pr_base_branch_defaults_empty() {
        let cfg = parse_porch_config(b"").unwrap();
        assert!(cfg.pr_base_branch.is_empty());
        let cfg = parse_porch_config(
            br"
commands:
  format: x
",
        )
        .unwrap();
        assert!(cfg.pr_base_branch.is_empty());
    }

    #[test]
    fn pr_base_branch_parsed() {
        let cfg = parse_porch_config(
            br"
pr:
  base_branch: dev
",
        )
        .unwrap();
        assert_eq!(cfg.pr_base_branch, "dev");
    }

    #[test]
    fn path_instructions_parsed_unknown_keys_ignored() {
        let cfg = parse_porch_config(
            br"
review:
  path_instructions:
    - path: crates/enclave/**
      instructions: Treat TEE as ask-user.
      extra_ignored: true
    - path: infra/**
      instructions: Do not mutate secrets.
  unknown_review_key: ignored
auto_fix:
  review: 0
  other: ignored
",
        )
        .unwrap();
        assert_eq!(cfg.path_instructions.len(), 2);
        assert_eq!(cfg.path_instructions[0].path, "crates/enclave/**");
        assert_eq!(
            cfg.path_instructions[0].instructions,
            "Treat TEE as ask-user."
        );
        assert_eq!(cfg.path_instructions[1].path, "infra/**");
        assert_eq!(cfg.auto_fix_review, 0);
    }

    #[test]
    fn auto_fix_review_parses_u32_default_zero() {
        let cfg = parse_porch_config(b"").unwrap();
        assert_eq!(cfg.auto_fix_review, 0);
        let cfg = parse_porch_config(
            br"
auto_fix:
  review: 2
",
        )
        .unwrap();
        assert_eq!(cfg.auto_fix_review, 2);
    }

    #[test]
    fn effective_base_branch_prefers_pr_when_set() {
        assert_eq!(effective_base_branch("dev", "main"), "dev");
        assert_eq!(effective_base_branch("", "main"), "main");
        assert_eq!(effective_base_branch("  ", "main"), "main");
    }

    #[test]
    fn persist_path_instructions_writes_json_outside_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let instructions = vec![
            PathInstruction {
                path: "a/**".into(),
                instructions: "ask-user".into(),
            },
            PathInstruction {
                path: "b/**".into(),
                instructions: "careful".into(),
            },
        ];
        // Only "a/x.rs" changed → matching-or-all keeps the a/** entry.
        let path = persist_path_instructions(
            home,
            "run-1",
            &instructions,
            &["a/x.rs".into(), "README.md".into()],
        )
        .unwrap();
        assert!(path.starts_with(home.join("runs").join("run-1")));
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: Vec<PathInstruction> = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].path, "a/**");
    }

    #[test]
    fn persist_path_instructions_all_when_none_match() {
        let tmp = tempfile::TempDir::new().unwrap();
        let instructions = vec![PathInstruction {
            path: "secret/**".into(),
            instructions: "ask-user".into(),
        }];
        let path =
            persist_path_instructions(tmp.path(), "run-2", &instructions, &["README.md".into()])
                .unwrap();
        let parsed: Vec<PathInstruction> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].path, "secret/**");
    }

    #[test]
    fn path_glob_infix_star_in_segment() {
        assert!(path_glob_matches(
            ".github/workflows/deploy*.yml",
            ".github/workflows/deploy-dev.yml"
        ));
        assert!(path_glob_matches(
            ".github/workflows/deploy*.yml",
            ".github/workflows/deploy-production.yml"
        ));
        assert!(!path_glob_matches(
            ".github/workflows/deploy*.yml",
            ".github/workflows/ci.yml"
        ));
        assert!(path_glob_matches(
            "backend/crates/gateways/src/routes/auth*",
            "backend/crates/gateways/src/routes/auth_github.rs"
        ));
    }

    #[test]
    fn path_glob_env_star_matches_subdir_basename() {
        assert!(path_glob_matches(".env*", ".env"));
        assert!(path_glob_matches(".env*", ".env.local"));
        assert!(path_glob_matches(".env*", "frontend/.env"));
        assert!(path_glob_matches(".env*", "frontend/.env.local"));
        assert!(path_glob_matches("frontend/.env*", "frontend/.env"));
        assert!(!path_glob_matches(".env*", "frontend/src/app.ts"));
    }

    #[test]
    fn path_glob_double_star_and_exact_still_work() {
        assert!(path_glob_matches(
            "crates/enclave/**",
            "crates/enclave/src/lib.rs"
        ));
        assert!(path_glob_matches(
            "docker-compose.yml",
            "docker-compose.yml"
        ));
        assert!(!path_glob_matches(
            "docker-compose.yml",
            "docker/compose.yml"
        ));
    }

    #[test]
    fn select_keeps_deploy_rule_when_deploy_file_changed() {
        let instructions = vec![
            PathInstruction {
                path: "README.md".into(),
                instructions: "docs".into(),
            },
            PathInstruction {
                path: ".github/workflows/deploy*.yml".into(),
                instructions: "deploy ask-user".into(),
            },
        ];
        let selected =
            select_path_instructions(&instructions, &[".github/workflows/deploy-dev.yml".into()]);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].path, ".github/workflows/deploy*.yml");
    }
}
