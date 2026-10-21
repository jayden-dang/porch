//! Operator `$PORCH_HOME/config.yaml` (not trusted executing `.porch.yaml`).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::engine::EngineKind;

/// File name under `$PORCH_HOME`.
pub const CONFIG_FILE: &str = "config.yaml";

/// Operator home config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeConfig {
    #[serde(default)]
    pub review: ReviewConfig,
    #[serde(default)]
    pub fixer: FixerConfig,
    #[serde(default)]
    pub github: GithubConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
}

/// Review engine wiring written by `porch setup`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewConfig {
    #[serde(default)]
    pub assurance: Option<String>,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub bin: Option<String>,
    #[serde(default)]
    pub wrapper: Option<String>,
    /// Native coding-agent CLI for `engine: agent` (`claude` / `codex`).
    #[serde(default)]
    pub agent_bin: Option<String>,
}

/// Which config key decided the assurance shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceKey {
    Assurance,
    Engine,
}

/// Selected assurance shape and optional judgment reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceSelection {
    pub floor_only: bool,
    pub source: VoiceKey,
    pub reader: Option<EngineKind>,
}

/// Human phrase for a floor-only or floor-plus-judgment shape.
#[must_use]
pub fn voice_phrase(floor_only: bool) -> &'static str {
    if floor_only {
        "rules only (floor-only)"
    } else {
        "rules + reader (floor+judgment)"
    }
}

impl ReviewConfig {
    /// Parsed engine kind when present and known.
    #[must_use]
    pub fn engine_kind(&self) -> Option<EngineKind> {
        self.engine.as_deref().and_then(EngineKind::parse)
    }

    /// Resolve assurance shape; `review.assurance` wins when present.
    ///
    /// # Errors
    ///
    /// Returns when assurance is unknown, or `rules+reader` has no judgment reader.
    pub fn selected(&self) -> Result<VoiceSelection, Error> {
        match self.assurance.as_deref().map(str::trim) {
            Some("rules-only") => Ok(VoiceSelection {
                floor_only: true,
                source: VoiceKey::Assurance,
                reader: None,
            }),
            Some("rules+reader") => {
                let reader = self.engine_kind().filter(|k| *k != EngineKind::Quality);
                let Some(reader) = reader else {
                    return Err(Error::Msg(
                        "review.assurance is rules+reader but no judgment reader is configured \
                         (set review.engine to agent, generic, or ocr)"
                            .into(),
                    ));
                };
                Ok(VoiceSelection {
                    floor_only: false,
                    source: VoiceKey::Assurance,
                    reader: Some(reader),
                })
            }
            Some(other) if !other.is_empty() => Err(Error::Msg(format!(
                "review.assurance must be rules-only or rules+reader, got `{other}`"
            ))),
            Some(_) | None => match self.engine_kind() {
                Some(EngineKind::Quality) => Ok(VoiceSelection {
                    floor_only: true,
                    source: VoiceKey::Engine,
                    reader: None,
                }),
                Some(kind) => Ok(VoiceSelection {
                    floor_only: false,
                    source: VoiceKey::Engine,
                    reader: Some(kind),
                }),
                None => Err(Error::Msg(
                    "review config needs review.assurance or a known review.engine".into(),
                )),
            },
        }
    }
}

/// Optional native fixer CLI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixerConfig {
    #[serde(default)]
    pub bin: Option<String>,
}

/// Optional `gh` path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubConfig {
    #[serde(default)]
    pub bin: Option<String>,
}

/// Detected repo-tool paths (informational).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsConfig {
    #[serde(default)]
    pub biome: Option<String>,
    #[serde(default)]
    pub bun: Option<String>,
    #[serde(default)]
    pub cargo: Option<String>,
    #[serde(default)]
    pub just: Option<String>,
    #[serde(default)]
    pub moon: Option<String>,
}

/// Absolute path to `$PORCH_HOME/config.yaml`.
#[must_use]
pub fn config_path(porch_home: &Path) -> PathBuf {
    porch_home.join(CONFIG_FILE)
}

/// Load operator config if the file exists.
///
/// # Errors
///
/// Returns I/O or YAML errors. Missing file → `Ok(None)`.
pub fn load_home_config(porch_home: &Path) -> Result<Option<HomeConfig>, Error> {
    let path = config_path(porch_home);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    let cfg: HomeConfig = serde_yaml::from_str(&text)
        .map_err(|e| Error::Msg(format!("config.yaml parse error: {e}")))?;
    Ok(Some(cfg))
}

/// Write operator config (creates `$PORCH_HOME` as needed).
///
/// # Errors
///
/// Returns I/O or YAML errors.
pub fn write_home_config(porch_home: &Path, cfg: &HomeConfig) -> Result<PathBuf, Error> {
    fs::create_dir_all(porch_home)?;
    let path = config_path(porch_home);
    let text = serde_yaml::to_string(cfg)
        .map_err(|e| Error::Msg(format!("config.yaml serialize error: {e}")))?;
    let tmp = path.with_extension("yaml.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Remove operator config if present (best-effort).
pub fn remove_home_config(porch_home: &Path) {
    let path = config_path(porch_home);
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(assurance: Option<&str>, engine: Option<&str>) -> ReviewConfig {
        ReviewConfig {
            assurance: assurance.map(str::to_string),
            engine: engine.map(str::to_string),
            ..ReviewConfig::default()
        }
    }

    #[test]
    fn quality_engine_without_assurance_is_floor_only() {
        let sel = cfg(None, Some("quality")).selected().unwrap();
        assert!(sel.floor_only);
        assert_eq!(sel.source, VoiceKey::Engine);
        assert_eq!(sel.reader, None);
    }

    #[test]
    fn judgment_engines_without_assurance_keep_their_reader() {
        for (name, kind) in [
            ("agent", EngineKind::Agent),
            ("generic", EngineKind::Generic),
            ("ocr", EngineKind::Ocr),
        ] {
            let sel = cfg(None, Some(name)).selected().unwrap();
            assert!(!sel.floor_only, "{name}");
            assert_eq!(sel.source, VoiceKey::Engine, "{name}");
            assert_eq!(sel.reader, Some(kind), "{name}");
        }
    }

    #[test]
    fn rules_only_assurance_wins_over_agent_engine() {
        let sel = cfg(Some("rules-only"), Some("agent")).selected().unwrap();
        assert!(sel.floor_only);
        assert_eq!(sel.source, VoiceKey::Assurance);
        assert_eq!(sel.reader, None);
    }

    #[test]
    fn rules_plus_reader_with_agent_keeps_agent_reader() {
        let sel = cfg(Some("rules+reader"), Some("agent")).selected().unwrap();
        assert!(!sel.floor_only);
        assert_eq!(sel.source, VoiceKey::Assurance);
        assert_eq!(sel.reader, Some(EngineKind::Agent));
    }

    #[test]
    fn rules_plus_reader_needs_a_reader_engine() {
        for engine in [Some("quality"), None] {
            let err = cfg(Some("rules+reader"), engine)
                .selected()
                .expect_err("rules+reader without a reader");
            assert!(
                err.to_string().contains("reader"),
                "engine={engine:?} err={err}"
            );
        }
    }

    #[test]
    fn voice_phrase_names_both_shapes() {
        assert_eq!(voice_phrase(true), "rules only (floor-only)");
        assert_eq!(voice_phrase(false), "rules + reader (floor+judgment)");
    }

    #[test]
    fn missing_config_file_is_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(load_home_config(tmp.path()).unwrap(), None);
    }
}
