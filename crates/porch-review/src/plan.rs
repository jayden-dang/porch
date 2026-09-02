//! One-shot producer resolution: immutable invocation plan and descriptor.
//!
//! Stateless — mints nothing durable. Callers open the round from the plan, then
//! spawn exactly the recorded absolute target.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::engine::{EngineKind, wrapper_body_matches};
use crate::home_config::load_home_config;
use crate::pathutil::resolve_bin;
use crate::setup::wrapper_path;
use crate::{Error, REVIEW_BIN_ENV, agent_review_bin, review_uses_agent};

/// Domain tag for composite producer artifact identity.
const IDENTITY_DOMAIN: &[u8] = b"porch-producer-identity/v1";

/// Options for resolving one producer invocation plan.
#[derive(Debug, Clone)]
pub struct PrepareOpts<'a> {
    pub porch_home: Option<&'a Path>,
    /// Explicit review CLI target (stands in for `PORCH_REVIEW_BIN` without mutating env).
    pub review_bin: Option<&'a str>,
    /// Explicit agent target (stands in for `PORCH_REVIEW_AGENT_BIN`).
    pub agent_bin: Option<&'a str>,
    /// `Some(true)` forces native agent; `Some(false)` forces CLI; `None` detects.
    pub prefer_agent: Option<bool>,
    /// Effective intent bytes the producer will receive, if any.
    pub intent: Option<&'a [u8]>,
    /// Effective path-instructions bytes the producer will receive, if any.
    pub path_instructions: Option<&'a [u8]>,
}

/// Resolved plan plus per-element context declarations for this producer.
#[derive(Debug, Clone)]
pub struct PreparedInvocation {
    pub plan: InvocationPlan,
    pub context_elements: Vec<PreparedContextElement>,
}

/// One immutable spawn plan for a producer.
#[derive(Debug, Clone)]
pub struct InvocationPlan {
    pub spawned_target_absolute: PathBuf,
    pub argv_prefix: Vec<String>,
    pub descriptor: ProducerDescriptor,
    /// Stamps captured at resolve time for post-spawn stability checks.
    pub artifact_stamps: Vec<ArtifactStamp>,
}

/// Producer descriptor recorded for a round invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducerDescriptor {
    pub adapter_kind: AdapterKind,
    pub declared_engine_kind: DeclaredEngineKind,
    pub selection_source: SelectionSource,
    pub invocation: InvocationRecord,
    pub wrapper: WrapperObservation,
    pub backend: BackendObservation,
    pub observed_version_identity: ObservedVersionIdentity,
    pub reported_version: ReportedVersion,
    pub consumed_context: Vec<String>,
}

/// How the producer is adapted into porch's review contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    NativeAgent,
    PorchJsonCli,
}

/// Declared engine kind, or why it could not be declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclaredEngineKind {
    Agent,
    Quality,
    Generic,
    Ocr,
    #[serde(untagged)]
    Unavailable {
        unavailable: String,
    },
}

/// Where the spawned target was selected from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionSource {
    EnvReviewBin,
    EnvAgentBin,
    HomeConfigWrapper,
    HomeConfigAgent,
    PathDetection,
    DefaultPathName,
}

/// Requested vs absolute spawn target and wrapper argv prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationRecord {
    pub requested_target: String,
    pub spawned_target_absolute: String,
    pub argv_prefix: Vec<String>,
}

/// Observation of a porch-owned wrapper artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WrapperObservation {
    None,
    #[serde(untagged)]
    Artifact {
        absolute_path: String,
        sha256: String,
    },
}

/// Observation of the backend behind a wrapper, or an opaque entrypoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendObservation {
    OpaqueEntrypoint,
    #[serde(untagged)]
    Known {
        absolute_path: String,
        sha256: String,
    },
    #[serde(untagged)]
    Unavailable {
        unavailable: String,
    },
}

/// Composite artifact identity, or why it could not be observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObservedVersionIdentity {
    #[serde(rename = "artifact_sha256")]
    ArtifactSha256(String),
    #[serde(rename = "unavailable")]
    Unavailable(String),
}

/// Operator-reported version string (audit-only; never probed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReportedVersion {
    #[serde(rename = "unavailable")]
    Unavailable(String),
}

/// Context element applicability declared for this producer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedContextElement {
    pub element_name: String,
    pub applied: bool,
    pub effective_digest: Option<String>,
}

/// Filesystem identity captured at resolve time for TOCTOU narrowing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactStamp {
    pub path: PathBuf,
    pub sha256: String,
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: u64,
}

/// Resolve one immutable invocation plan (absolute target, argv, descriptor).
///
/// # Errors
///
/// Returns when the selected target cannot be resolved to an executable path.
pub fn prepare(opts: &PrepareOpts<'_>) -> Result<PreparedInvocation, Error> {
    let use_agent = match opts.prefer_agent {
        Some(v) => v,
        None => {
            if opts.review_bin.is_some() {
                false
            } else if opts.agent_bin.is_some() {
                true
            } else {
                review_uses_agent(opts.porch_home)
            }
        }
    };

    if use_agent {
        prepare_agent(opts)
    } else {
        prepare_cli(opts)
    }
}

/// Re-stat planned artifacts; fail when any stamp no longer matches.
///
/// # Errors
///
/// Returns [`Error::ProducerArtifactChanged`] when a stamped path differs.
pub fn check_artifacts_stable(plan: &InvocationPlan) -> Result<(), Error> {
    for stamp in &plan.artifact_stamps {
        let Ok(current) = stamp_path(&stamp.path) else {
            return Err(Error::ProducerArtifactChanged);
        };
        if current.dev != stamp.dev
            || current.ino != stamp.ino
            || current.size != stamp.size
            || current.mtime_ns != stamp.mtime_ns
            || current.sha256 != stamp.sha256
        {
            return Err(Error::ProducerArtifactChanged);
        }
    }
    Ok(())
}

/// Composite artifact identity preimage digest (hex SHA-256).
#[must_use]
pub fn composite_artifact_identity(
    adapter_kind: AdapterKind,
    wrapper_sha256: Option<&str>,
    backend_tag: &str,
    backend_sha256: Option<&str>,
    argv_prefix: &[String],
) -> String {
    let adapter = adapter_kind.as_str();
    let wrap = wrapper_sha256.unwrap_or("-");
    let back = backend_sha256.unwrap_or("-");
    let argv = argv_prefix.join("\u{1f}");
    let preimage = length_delimited_join(&[
        IDENTITY_DOMAIN,
        adapter.as_bytes(),
        wrap.as_bytes(),
        backend_tag.as_bytes(),
        back.as_bytes(),
        argv.as_bytes(),
    ]);
    sha256_hex(&preimage)
}

impl AdapterKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::NativeAgent => "native_agent",
            Self::PorchJsonCli => "porch_json_cli",
        }
    }
}

impl DeclaredEngineKind {
    fn from_engine(kind: EngineKind) -> Self {
        match kind {
            EngineKind::Agent => Self::Agent,
            EngineKind::Quality => Self::Quality,
            EngineKind::Generic => Self::Generic,
            EngineKind::Ocr => Self::Ocr,
        }
    }
}

fn prepare_cli(opts: &PrepareOpts<'_>) -> Result<PreparedInvocation, Error> {
    let (requested, selection) = resolve_cli_target(opts);
    let absolute = resolve_bin(&requested).ok_or_else(|| Error::BinNotFound {
        bin: requested.clone(),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found on PATH"),
    })?;
    let absolute = canonicalize_best_effort(&absolute);
    let abs_s = absolute
        .to_str()
        .ok_or_else(|| Error::Msg(format!("non-utf8 review bin {}", absolute.display())))?
        .to_string();

    let engine = declared_engine_from_home(opts.porch_home);
    let argv_prefix: Vec<String> = engine
        .map(|e| {
            e.wrapper_prefix()
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        })
        .unwrap_or_default();

    let is_porch_wrapper = opts.porch_home.is_some_and(|home| {
        let wrap = wrapper_path(home);
        paths_equal(&absolute, &wrap)
            || absolute
                .canonicalize()
                .ok()
                .zip(wrap.canonicalize().ok())
                .is_some_and(|(a, b)| a == b)
    });

    let (wrapper, backend, stamps, observed) = if is_porch_wrapper {
        observe_wrapper_invocation(opts.porch_home, &absolute, &argv_prefix, engine)?
    } else {
        observe_opaque_entrypoint(AdapterKind::PorchJsonCli, &absolute, &argv_prefix)
    };

    let context_elements = consumed_context_cli(opts);
    let descriptor = ProducerDescriptor {
        adapter_kind: AdapterKind::PorchJsonCli,
        declared_engine_kind: match engine {
            Some(e) => DeclaredEngineKind::from_engine(e),
            None => DeclaredEngineKind::Unavailable {
                unavailable: "engine_not_configured".into(),
            },
        },
        selection_source: selection,
        invocation: InvocationRecord {
            requested_target: requested,
            spawned_target_absolute: abs_s,
            argv_prefix: argv_prefix.clone(),
        },
        wrapper,
        backend,
        observed_version_identity: observed,
        reported_version: ReportedVersion::Unavailable("not_reported".into()),
        consumed_context: applied_names(&context_elements),
    };

    Ok(PreparedInvocation {
        plan: InvocationPlan {
            spawned_target_absolute: absolute,
            argv_prefix,
            descriptor,
            artifact_stamps: stamps,
        },
        context_elements,
    })
}

fn prepare_agent(opts: &PrepareOpts<'_>) -> Result<PreparedInvocation, Error> {
    let (requested, selection) = resolve_agent_target(opts)?;
    let absolute = resolve_bin(&requested).ok_or_else(|| Error::BinNotFound {
        bin: requested.clone(),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found on PATH"),
    })?;
    let absolute = canonicalize_best_effort(&absolute);
    let abs_s = absolute
        .to_str()
        .ok_or_else(|| Error::Msg(format!("non-utf8 agent bin {}", absolute.display())))?
        .to_string();

    let argv_prefix: Vec<String> = Vec::new();
    let (wrapper, backend, stamps, observed) =
        observe_opaque_entrypoint(AdapterKind::NativeAgent, &absolute, &argv_prefix);

    let context_elements = consumed_context_agent(opts);
    let descriptor = ProducerDescriptor {
        adapter_kind: AdapterKind::NativeAgent,
        declared_engine_kind: DeclaredEngineKind::Agent,
        selection_source: selection,
        invocation: InvocationRecord {
            requested_target: requested,
            spawned_target_absolute: abs_s,
            argv_prefix: argv_prefix.clone(),
        },
        wrapper,
        backend,
        observed_version_identity: observed,
        reported_version: ReportedVersion::Unavailable("not_reported".into()),
        consumed_context: applied_names(&context_elements),
    };

    Ok(PreparedInvocation {
        plan: InvocationPlan {
            spawned_target_absolute: absolute,
            argv_prefix,
            descriptor,
            artifact_stamps: stamps,
        },
        context_elements,
    })
}

fn resolve_cli_target(opts: &PrepareOpts<'_>) -> (String, SelectionSource) {
    if let Some(v) = opts.review_bin.map(str::trim).filter(|s| !s.is_empty()) {
        return (v.to_string(), SelectionSource::EnvReviewBin);
    }
    if let Ok(v) = std::env::var(REVIEW_BIN_ENV) {
        if !v.trim().is_empty() {
            return (v, SelectionSource::EnvReviewBin);
        }
    }
    if let Some(home) = opts.porch_home {
        if let Ok(Some(cfg)) = load_home_config(home) {
            if let Some(w) = cfg.review.wrapper.as_deref() {
                if !w.trim().is_empty() {
                    return (w.to_string(), SelectionSource::HomeConfigWrapper);
                }
            }
        }
    }
    ("review".to_string(), SelectionSource::DefaultPathName)
}

fn resolve_agent_target(opts: &PrepareOpts<'_>) -> Result<(String, SelectionSource), Error> {
    if let Some(v) = opts.agent_bin.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok((v.to_string(), SelectionSource::EnvAgentBin));
    }
    if let Ok(v) = std::env::var(crate::REVIEW_AGENT_BIN_ENV) {
        if !v.trim().is_empty() {
            return Ok((v, SelectionSource::EnvAgentBin));
        }
    }
    let home = opts.porch_home.ok_or_else(|| {
        Error::Msg("agent plan requires porch_home when agent_bin is unset".into())
    })?;
    if let Ok(Some(cfg)) = load_home_config(home) {
        if let Some(b) = cfg
            .review
            .agent_bin
            .as_deref()
            .or(cfg.review.bin.as_deref())
            .filter(|s| !s.trim().is_empty())
        {
            return Ok((b.to_string(), SelectionSource::HomeConfigAgent));
        }
    }
    // Fall back to the shared resolver (PATH detection).
    let bin = agent_review_bin(home)?;
    Ok((bin, SelectionSource::PathDetection))
}

fn observe_wrapper_invocation(
    porch_home: Option<&Path>,
    wrapper: &Path,
    argv_prefix: &[String],
    engine: Option<EngineKind>,
) -> Result<
    (
        WrapperObservation,
        BackendObservation,
        Vec<ArtifactStamp>,
        ObservedVersionIdentity,
    ),
    Error,
> {
    let wrap_stamp = stamp_path(wrapper)
        .map_err(|e| Error::Msg(format!("cannot observe wrapper {}: {e}", wrapper.display())))?;
    let wrap_obs = WrapperObservation::Artifact {
        absolute_path: path_to_utf8(wrapper)?,
        sha256: wrap_stamp.sha256.clone(),
    };

    let backend_path = resolve_wrapper_backend(porch_home, wrapper, engine);
    match backend_path {
        Some(backend) => match stamp_path(&backend) {
            Ok(back_stamp) => {
                let back_obs = BackendObservation::Known {
                    absolute_path: path_to_utf8(&backend)?,
                    sha256: back_stamp.sha256.clone(),
                };
                let identity =
                    ObservedVersionIdentity::ArtifactSha256(composite_artifact_identity(
                        AdapterKind::PorchJsonCli,
                        Some(&wrap_stamp.sha256),
                        "known",
                        Some(&back_stamp.sha256),
                        argv_prefix,
                    ));
                Ok((wrap_obs, back_obs, vec![wrap_stamp, back_stamp], identity))
            }
            Err(e) => Ok((
                wrap_obs,
                BackendObservation::Unavailable {
                    unavailable: format!("backend_unreadable: {e}"),
                },
                vec![wrap_stamp],
                ObservedVersionIdentity::Unavailable(format!("backend_unreadable: {e}")),
            )),
        },
        None => Ok((
            wrap_obs,
            BackendObservation::Unavailable {
                unavailable: "backend_not_resolved".into(),
            },
            vec![wrap_stamp],
            ObservedVersionIdentity::Unavailable("backend_not_resolved".into()),
        )),
    }
}

fn observe_opaque_entrypoint(
    adapter: AdapterKind,
    entrypoint: &Path,
    argv_prefix: &[String],
) -> (
    WrapperObservation,
    BackendObservation,
    Vec<ArtifactStamp>,
    ObservedVersionIdentity,
) {
    match stamp_path(entrypoint) {
        Ok(stamp) => {
            let identity = ObservedVersionIdentity::ArtifactSha256(composite_artifact_identity(
                adapter,
                None,
                "opaque_entrypoint",
                Some(&stamp.sha256),
                argv_prefix,
            ));
            (
                WrapperObservation::None,
                BackendObservation::OpaqueEntrypoint,
                vec![stamp],
                identity,
            )
        }
        Err(e) => (
            WrapperObservation::None,
            BackendObservation::OpaqueEntrypoint,
            Vec::new(),
            ObservedVersionIdentity::Unavailable(format!("entrypoint_unreadable: {e}")),
        ),
    }
}

fn resolve_wrapper_backend(
    porch_home: Option<&Path>,
    wrapper: &Path,
    engine: Option<EngineKind>,
) -> Option<PathBuf> {
    if let Some(home) = porch_home {
        if let Ok(Some(cfg)) = load_home_config(home) {
            if let Some(bin) = cfg.review.bin.as_deref().filter(|s| !s.trim().is_empty()) {
                if let Some(p) = resolve_bin(bin) {
                    return Some(canonicalize_best_effort(&p));
                }
                // Recorded backend path may be absolute but currently missing.
                let p = PathBuf::from(bin);
                if p.is_absolute() {
                    return Some(p);
                }
            }
        }
    }
    // Parse `exec <backend> …` from a porch-owned wrapper body.
    let body = fs::read_to_string(wrapper).ok()?;
    let backend = parse_exec_backend(&body)?;
    if let Some(kind) = engine {
        if !wrapper_body_matches(kind, Path::new(&backend), &body) {
            // Still use the parsed path; body mismatch is an observation concern.
        }
    }
    let p = PathBuf::from(&backend);
    Some(if p.is_absolute() {
        p
    } else {
        resolve_bin(&backend).unwrap_or(p)
    })
}

fn parse_exec_backend(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("exec ")?;
        rest.split_whitespace().next().map(str::to_string)
    })
}

fn declared_engine_from_home(porch_home: Option<&Path>) -> Option<EngineKind> {
    let home = porch_home?;
    load_home_config(home)
        .ok()
        .flatten()
        .and_then(|c| c.review.engine_kind())
}

fn consumed_context_cli(opts: &PrepareOpts<'_>) -> Vec<PreparedContextElement> {
    // CLI producers do not receive intent / path-instruction prompt bytes.
    vec![
        not_applied("intent", opts.intent),
        not_applied("path_instructions", opts.path_instructions),
    ]
}

fn consumed_context_agent(opts: &PrepareOpts<'_>) -> Vec<PreparedContextElement> {
    vec![
        applied_or_absent("intent", opts.intent),
        applied_or_absent("path_instructions", opts.path_instructions),
    ]
}

fn applied_or_absent(name: &str, bytes: Option<&[u8]>) -> PreparedContextElement {
    match bytes {
        Some(b) => PreparedContextElement {
            element_name: name.into(),
            applied: true,
            effective_digest: Some(context_digest(name, b)),
        },
        None => PreparedContextElement {
            element_name: name.into(),
            applied: false,
            effective_digest: None,
        },
    }
}

fn not_applied(name: &str, _bytes: Option<&[u8]>) -> PreparedContextElement {
    PreparedContextElement {
        element_name: name.into(),
        applied: false,
        effective_digest: None,
    }
}

fn applied_names(elements: &[PreparedContextElement]) -> Vec<String> {
    elements
        .iter()
        .filter(|e| e.applied)
        .map(|e| e.element_name.clone())
        .collect()
}

fn context_digest(element_name: &str, effective_bytes: &[u8]) -> String {
    let len = effective_bytes.len().to_string();
    let preimage = length_delimited_join(&[
        b"porch-review-context/v1",
        element_name.as_bytes(),
        b"present",
        len.as_bytes(),
        effective_bytes,
    ]);
    sha256_hex(&preimage)
}

fn stamp_path(path: &Path) -> Result<ArtifactStamp, std::io::Error> {
    let meta = fs::metadata(path)?;
    let bytes = fs::read(path)?;
    let (dev, ino, mtime_ns) = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mtime_ns = u64::try_from(meta.mtime())
                .unwrap_or(0)
                .saturating_mul(1_000_000_000)
                .saturating_add(
                    u64::try_from(meta.mtime_nsec())
                        .unwrap_or(0)
                        .min(999_999_999),
                );
            (meta.dev(), meta.ino(), mtime_ns)
        }
        #[cfg(not(unix))]
        {
            let _ = &meta;
            (0, 0, 0)
        }
    };
    Ok(ArtifactStamp {
        path: path.to_path_buf(),
        sha256: sha256_hex(&bytes),
        dev,
        ino,
        size: meta.len(),
        mtime_ns,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn length_delimited_join(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push(0x1F);
        }
        out.extend_from_slice(part.len().to_string().as_bytes());
        out.extend_from_slice(part);
    }
    out
}

fn canonicalize_best_effort(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    a == b
}

fn path_to_utf8(path: &Path) -> Result<String, Error> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Msg(format!("non-utf8 path {}", path.display())))
}

#[allow(dead_code)] // used by stability helpers / future callers
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::wrapper_script;
    use crate::home_config::{HomeConfig, ReviewConfig, write_home_config};
    use crate::pathutil::chmod_755;
    use crate::setup::write_wrapper;
    use std::time::Duration;

    fn install_fake(dir: &Path, name: &str, body: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        chmod_755(&path).unwrap();
        path
    }

    #[test]
    fn prepare_records_absolute_target_used_for_spawn() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        let fake = install_fake(
            &bin_dir,
            "review-cli",
            r#"#!/bin/sh
OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output) OUT="$2"; shift 2 ;;
    *) shift ;;
  esac
done
: "${OUT:?}"
printf '%s\n' '{"comments":[],"files":["a.rs"]}' > "$OUT"
"#,
        );

        let prepared = prepare(&PrepareOpts {
            porch_home: None,
            review_bin: Some(fake.to_str().unwrap()),
            agent_bin: None,
            prefer_agent: Some(false),
            intent: None,
            path_instructions: None,
        })
        .unwrap();

        assert_eq!(
            prepared.plan.spawned_target_absolute,
            canonicalize_best_effort(&fake)
        );

        // A different binary would be chosen if resolution ran again.
        let other = install_fake(&bin_dir, "other-review", "#!/bin/sh\nexit 1\n");
        let wt = tmp.path().join("wt");
        fs::create_dir_all(wt.join(".porch-review")).unwrap();

        let outcome = crate::run_review(&crate::RunReviewOpts {
            work_tree: &wt,
            from_sha: "aaa",
            to_sha: "bbb",
            changed_files: &["a.rs".into()],
            bin: other.to_str().unwrap(), // would fail if used
            timeout: Duration::from_secs(5),
            porch_home: None,
            run_id: None,
            intent: None,
            plan: Some(&prepared.plan),
            artifact_dir: None,
        })
        .unwrap();
        assert_eq!(outcome.covered_files, vec!["a.rs"]);

        check_artifacts_stable(&prepared.plan).unwrap();
    }

    #[test]
    fn wrapper_identity_spans_wrapper_backend_and_argv() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let backend = install_fake(
            &tmp.path().join("backend"),
            "porch-quality",
            "#!/bin/sh\nexit 0\n",
        );
        write_wrapper(&home, EngineKind::Quality, &backend).unwrap();
        write_home_config(
            &home,
            &HomeConfig {
                review: ReviewConfig {
                    engine: Some("quality".into()),
                    bin: Some(backend.display().to_string()),
                    wrapper: Some(wrapper_path(&home).display().to_string()),
                    agent_bin: None,
                },
                ..HomeConfig::default()
            },
        )
        .unwrap();

        let prepared = prepare(&PrepareOpts {
            porch_home: Some(&home),
            review_bin: None,
            agent_bin: None,
            prefer_agent: Some(false),
            intent: None,
            path_instructions: None,
        })
        .unwrap();

        let wrap_sha = match &prepared.plan.descriptor.wrapper {
            WrapperObservation::Artifact { sha256, .. } => sha256.clone(),
            WrapperObservation::None => panic!("expected wrapper artifact"),
        };
        let ObservedVersionIdentity::ArtifactSha256(composite) =
            &prepared.plan.descriptor.observed_version_identity
        else {
            panic!("expected composite artifact identity");
        };
        assert_ne!(
            composite, &wrap_sha,
            "identity must not be the wrapper digest alone"
        );

        let backend2 = install_fake(
            &tmp.path().join("backend2"),
            "porch-quality",
            "#!/bin/sh\n# different backend\nexit 0\n",
        );
        write_wrapper(&home, EngineKind::Quality, &backend2).unwrap();
        write_home_config(
            &home,
            &HomeConfig {
                review: ReviewConfig {
                    engine: Some("quality".into()),
                    bin: Some(backend2.display().to_string()),
                    wrapper: Some(wrapper_path(&home).display().to_string()),
                    agent_bin: None,
                },
                ..HomeConfig::default()
            },
        )
        .unwrap();
        // Keep wrapper script bytes identical in form but backend path changes → body changes.
        // Also compare argv: OCR prefix must change identity even with same digests.
        let ocr_id = composite_artifact_identity(
            AdapterKind::PorchJsonCli,
            Some(&wrap_sha),
            "known",
            Some("backend-digest"),
            &["review".into()],
        );
        let quality_id = composite_artifact_identity(
            AdapterKind::PorchJsonCli,
            Some(&wrap_sha),
            "known",
            Some("backend-digest"),
            &[],
        );
        assert_ne!(ocr_id, quality_id);

        let prepared2 = prepare(&PrepareOpts {
            porch_home: Some(&home),
            review_bin: None,
            agent_bin: None,
            prefer_agent: Some(false),
            intent: None,
            path_instructions: None,
        })
        .unwrap();
        let ObservedVersionIdentity::ArtifactSha256(composite2) =
            &prepared2.plan.descriptor.observed_version_identity
        else {
            panic!("expected composite");
        };
        assert_ne!(composite, composite2, "backend change must change identity");
    }

    #[test]
    fn unobservable_version_records_unavailable_with_reason() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join("bin")).unwrap();
        let missing_backend = tmp.path().join("missing-backend-bin");
        let wrap = wrapper_path(&home);
        let body = wrapper_script(EngineKind::Generic, &missing_backend);
        fs::write(&wrap, &body).unwrap();
        chmod_755(&wrap).unwrap();
        write_home_config(
            &home,
            &HomeConfig {
                review: ReviewConfig {
                    engine: Some("generic".into()),
                    bin: Some(missing_backend.display().to_string()),
                    wrapper: Some(wrap.display().to_string()),
                    agent_bin: None,
                },
                ..HomeConfig::default()
            },
        )
        .unwrap();

        let prepared = prepare(&PrepareOpts {
            porch_home: Some(&home),
            review_bin: None,
            agent_bin: None,
            prefer_agent: Some(false),
            intent: None,
            path_instructions: None,
        })
        .unwrap();

        match &prepared.plan.descriptor.observed_version_identity {
            ObservedVersionIdentity::Unavailable(reason) => {
                assert!(!reason.is_empty(), "reason required");
            }
            ObservedVersionIdentity::ArtifactSha256(s) => {
                panic!("must not substitute a digest, got {s}");
            }
        }
        match &prepared.plan.descriptor.backend {
            BackendObservation::Unavailable { unavailable } => {
                assert!(!unavailable.is_empty());
            }
            other => panic!("expected unavailable backend, got {other:?}"),
        }
        assert_eq!(
            prepared.plan.descriptor.reported_version,
            ReportedVersion::Unavailable("not_reported".into())
        );
    }

    #[test]
    fn opaque_entrypoint_records_entrypoint_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = install_fake(
            &tmp.path().join("bin"),
            "standalone-review",
            "#!/bin/sh\nexit 0\n",
        );

        let prepared = prepare(&PrepareOpts {
            porch_home: None,
            review_bin: Some(fake.to_str().unwrap()),
            agent_bin: None,
            prefer_agent: Some(false),
            intent: None,
            path_instructions: None,
        })
        .unwrap();

        assert_eq!(
            prepared.plan.descriptor.backend,
            BackendObservation::OpaqueEntrypoint
        );
        assert_eq!(prepared.plan.descriptor.wrapper, WrapperObservation::None);
        match &prepared.plan.descriptor.observed_version_identity {
            ObservedVersionIdentity::ArtifactSha256(hex) => {
                let entry_sha = sha256_hex(&fs::read(&fake).unwrap());
                let expected = composite_artifact_identity(
                    AdapterKind::PorchJsonCli,
                    None,
                    "opaque_entrypoint",
                    Some(&entry_sha),
                    &[],
                );
                assert_eq!(hex, &expected);
            }
            ObservedVersionIdentity::Unavailable(r) => panic!("unexpected unavailable: {r}"),
        }
    }

    #[test]
    fn post_spawn_stability_detects_backend_swap() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let backend = install_fake(&tmp.path().join("backend"), "review", "#!/bin/sh\nexit 0\n");
        write_wrapper(&home, EngineKind::Generic, &backend).unwrap();
        write_home_config(
            &home,
            &HomeConfig {
                review: ReviewConfig {
                    engine: Some("generic".into()),
                    bin: Some(backend.display().to_string()),
                    wrapper: Some(wrapper_path(&home).display().to_string()),
                    agent_bin: None,
                },
                ..HomeConfig::default()
            },
        )
        .unwrap();

        let prepared = prepare(&PrepareOpts {
            porch_home: Some(&home),
            review_bin: None,
            agent_bin: None,
            prefer_agent: Some(false),
            intent: None,
            path_instructions: None,
        })
        .unwrap();
        check_artifacts_stable(&prepared.plan).unwrap();

        fs::write(&backend, "#!/bin/sh\n# swapped\nexit 0\n").unwrap();
        chmod_755(&backend).unwrap();
        let err = check_artifacts_stable(&prepared.plan).unwrap_err();
        assert!(matches!(err, Error::ProducerArtifactChanged));
    }
}
