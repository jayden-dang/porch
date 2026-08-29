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
    pub engine: Option<String>,
    #[serde(default)]
    pub bin: Option<String>,
    #[serde(default)]
    pub wrapper: Option<String>,
}

impl ReviewConfig {
    /// Parsed engine kind when present and known.
    #[must_use]
    pub fn engine_kind(&self) -> Option<EngineKind> {
        self.engine.as_deref().and_then(EngineKind::parse)
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
