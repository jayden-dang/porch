//! Parse trusted `.porch.yaml` bytes into certify + deliver settings.

use serde::Deserialize;

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

/// Full trusted config parse result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PorchConfig {
    pub commands: Commands,
    pub deliver_github: DeliverGithub,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct PorchYaml {
    #[serde(default)]
    commands: Commands,
    #[serde(default)]
    deliver: DeliverSection,
}

/// Parse `.porch.yaml` bytes. Empty input → defaults. Unknown keys ignored.
///
/// # Errors
///
/// Returns a parse error string when YAML is present but unparseable.
pub fn parse_porch_yaml(bytes: &[u8]) -> Result<Commands, String> {
    Ok(parse_porch_config(bytes)?.commands)
}

/// Parse `.porch.yaml` including deliver GitHub fields.
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bytes_yield_empty_commands() {
        let cmds = parse_porch_yaml(b"").unwrap();
        assert_eq!(cmds, Commands::default());
        assert!(cmds.format.is_empty());
        assert!(cmds.lint.is_empty());
    }

    #[test]
    fn whitespace_only_yields_empty_commands() {
        let cmds = parse_porch_yaml(b"  \n\t\n").unwrap();
        assert_eq!(cmds, Commands::default());
    }

    #[test]
    fn format_only() {
        let cmds = parse_porch_yaml(
            br#"
commands:
  format: "biome check --write ."
"#,
        )
        .unwrap();
        assert_eq!(cmds.format, "biome check --write .");
        assert!(cmds.lint.is_empty());
        assert!(cmds.test.is_empty());
    }

    #[test]
    fn format_and_lint() {
        let cmds = parse_porch_yaml(
            br#"
commands:
  format: fmt-cmd
  lint: lint-cmd
  test: should-not-run
"#,
        )
        .unwrap();
        assert_eq!(cmds.format, "fmt-cmd");
        assert_eq!(cmds.lint, "lint-cmd");
        assert_eq!(cmds.test, "should-not-run");
    }

    #[test]
    fn unknown_keys_ignored() {
        let cmds = parse_porch_yaml(
            br#"
agent: other
commands:
  format: ok
ignore_patterns:
  - "*.lock"
"#,
        )
        .unwrap();
        assert_eq!(cmds.format, "ok");
    }

    #[test]
    fn unparseable_fails_closed() {
        let err = parse_porch_yaml(b"commands: [not, a, map").unwrap_err();
        assert!(err.contains("parse error"), "{err}");
    }

    #[test]
    fn non_utf8_fails_closed() {
        let err = parse_porch_yaml(b"\xff\xfe commands:\n  format: x\n").unwrap_err();
        assert!(err.contains("not utf-8"), "{err}");
    }

    #[test]
    fn deliver_defaults_empty_allowlist_rerun_zero() {
        let cfg = parse_porch_config(b"").unwrap();
        assert!(cfg.deliver_github.watch_checks.is_empty());
        assert_eq!(cfg.deliver_github.rerun_transient, 0);

        let cfg = parse_porch_config(
            br#"
commands:
  format: x
"#,
        )
        .unwrap();
        assert!(cfg.deliver_github.watch_checks.is_empty());
        assert_eq!(cfg.deliver_github.rerun_transient, 0);
    }

    #[test]
    fn deliver_github_watch_checks_and_rerun() {
        let cfg = parse_porch_config(
            br#"
deliver:
  github:
    watch_checks: [lint, types-check]
    rerun_transient: 0
"#,
        )
        .unwrap();
        assert_eq!(cfg.deliver_github.watch_checks, vec!["lint", "types-check"]);
        assert_eq!(cfg.deliver_github.rerun_transient, 0);
    }

    #[test]
    fn deliver_github_omitted_rerun_defaults_zero() {
        let cfg = parse_porch_config(
            br#"
deliver:
  github:
    watch_checks: [lint]
"#,
        )
        .unwrap();
        assert_eq!(cfg.deliver_github.watch_checks, vec!["lint"]);
        assert_eq!(cfg.deliver_github.rerun_transient, 0);
    }
}
