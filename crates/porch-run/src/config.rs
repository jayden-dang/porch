//! Parse trusted `.porch.yaml` bytes into certify command strings.

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

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct PorchYaml {
    #[serde(default)]
    commands: Commands,
}

/// Parse `.porch.yaml` bytes. Empty input → empty commands. Unknown keys ignored.
///
/// # Errors
///
/// Returns a parse error string when YAML is present but unparseable.
pub fn parse_porch_yaml(bytes: &[u8]) -> Result<Commands, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!(".porch.yaml not utf-8: {e}"))?;
    if text.trim().is_empty() {
        return Ok(Commands::default());
    }
    let doc: PorchYaml =
        serde_yaml::from_str(text).map_err(|e| format!(".porch.yaml parse error: {e}"))?;
    Ok(doc.commands)
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
}
