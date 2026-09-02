//! Dedicated resolver for the mandatory deterministic floor.
//!
//! Derives `porch-quality` as a canonical sibling of the running executable.
//! Never consults operator config, review-bin env, the wrapper, or PATH.

use std::path::{Path, PathBuf};

use crate::Error;
use crate::pathutil::is_executable;
use crate::plan::{
    AdapterKind, DeclaredEngineKind, InvocationPlan, InvocationRecord, PreparedInvocation,
    ProducerDescriptor, ReportedVersion, SelectionSource, observe_opaque_entrypoint,
};

const FLOOR_EXECUTABLE_STEM: &str = "porch-quality";

#[cfg(test)]
thread_local! {
    static LAUNCH_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
struct LaunchOverride;

#[cfg(test)]
impl LaunchOverride {
    fn set(path: PathBuf) -> Self {
        LAUNCH_OVERRIDE.with(|slot| *slot.borrow_mut() = Some(path));
        Self
    }
}

#[cfg(test)]
impl Drop for LaunchOverride {
    fn drop(&mut self) {
        LAUNCH_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Resolve the floor producer from the running installation.
///
/// # Errors
///
/// Returns [`Error::FloorUnresolved`] when an executable canonical sibling cannot
/// be established. Never searches `PATH`.
pub fn resolve() -> Result<PreparedInvocation, Error> {
    let launch = crate::plan::canonicalize_best_effort(&launch_path()?);
    let Some(parent) = launch.parent() else {
        return Err(unresolved(format!(
            "running executable {} has no parent directory",
            launch.display()
        )));
    };
    let sibling = parent.join(floor_executable_name());
    if !is_executable(&sibling) {
        return Err(unresolved(format!(
            "canonical sibling {} is missing or not executable",
            sibling.display()
        )));
    }
    let sibling = crate::plan::canonicalize_best_effort(&sibling);
    prepared_from_sibling(&sibling)
}

fn unresolved(reason: String) -> Error {
    Error::FloorUnresolved { reason }
}

fn floor_executable_name() -> String {
    format!("{FLOOR_EXECUTABLE_STEM}{}", std::env::consts::EXE_SUFFIX)
}

fn launch_path() -> Result<PathBuf, Error> {
    #[cfg(test)]
    {
        if let Some(path) = LAUNCH_OVERRIDE.with(|slot| slot.borrow().clone()) {
            return Ok(path);
        }
    }
    std::env::current_exe()
        .map_err(|e| unresolved(format!("cannot resolve the running executable: {e}")))
}

fn prepared_from_sibling(sibling: &Path) -> Result<PreparedInvocation, Error> {
    let abs_s = crate::plan::path_to_utf8(sibling).map_err(|e| unresolved(e.to_string()))?;
    let argv_prefix = Vec::new();
    let (wrapper, backend, stamps, observed) =
        observe_opaque_entrypoint(AdapterKind::PorchJsonCli, sibling, &argv_prefix);
    let descriptor = ProducerDescriptor {
        adapter_kind: AdapterKind::PorchJsonCli,
        declared_engine_kind: DeclaredEngineKind::Quality,
        selection_source: SelectionSource::CanonicalSibling,
        invocation: InvocationRecord {
            requested_target: abs_s.clone(),
            spawned_target_absolute: abs_s,
            argv_prefix: argv_prefix.clone(),
        },
        wrapper,
        backend,
        observed_version_identity: observed,
        reported_version: ReportedVersion::Unavailable("not_reported".into()),
        consumed_context: Vec::new(),
    };
    Ok(PreparedInvocation {
        plan: InvocationPlan {
            spawned_target_absolute: sibling.to_path_buf(),
            argv_prefix,
            descriptor,
            artifact_stamps: stamps,
        },
        context_elements: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::resolve;
    use crate::REVIEW_AGENT_BIN_ENV;
    use crate::REVIEW_BIN_ENV;
    use crate::home_config::{HomeConfig, ReviewConfig, write_home_config};
    use crate::pathutil::chmod_755;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn quality_name() -> String {
        format!("porch-quality{}", std::env::consts::EXE_SUFFIX)
    }

    fn install_exe(dir: &Path, name: &str, marker: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\necho {marker}\nexit 0\n")).unwrap();
        chmod_755(&path).unwrap();
        path
    }

    struct HostileFixture {
        launch: PathBuf,
        sibling: PathBuf,
        review_bin: PathBuf,
        agent_bin: PathBuf,
        wrapper: PathBuf,
        config_bin: PathBuf,
        path_quality: PathBuf,
        path_dir: PathBuf,
        home: PathBuf,
    }

    fn install_hostile_fixture(root: &Path) -> HostileFixture {
        let install = root.join("install");
        let launch = install_exe(&install, "porch", "porch-launch");
        let sibling = install_exe(&install, &quality_name(), "canonical-floor");

        let substitute = root.join("substitute");
        let review_bin = install_exe(&substitute, "review-cli", "env-review-bin");
        let agent_bin = install_exe(&substitute, "agent-cli", "env-agent-bin");
        let config_bin = install_exe(&substitute, "config-bin", "review-dot-bin");

        let home = root.join("home");
        let wrapper = install_exe(&home.join("bin"), "review", "home-wrapper");
        write_home_config(
            &home,
            &HomeConfig {
                review: ReviewConfig {
                    engine: Some("agent".into()),
                    bin: Some(config_bin.display().to_string()),
                    wrapper: Some(wrapper.display().to_string()),
                    agent_bin: Some(agent_bin.display().to_string()),
                },
                ..HomeConfig::default()
            },
        )
        .unwrap();
        fs::write(
            root.join(".porch.yaml"),
            "review:\n  engine: agent\n  bin: /tmp/hostile-porch-yaml\n",
        )
        .unwrap();

        let path_dir = root.join("on-path");
        let path_quality = install_exe(&path_dir, &quality_name(), "path-fallback");

        HostileFixture {
            launch,
            sibling,
            review_bin,
            agent_bin,
            wrapper,
            config_bin,
            path_quality,
            path_dir,
            home,
        }
    }

    fn assert_child_ok(output: &std::process::Output) {
        assert!(
            output.status.success(),
            "child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn hostile_configuration_does_not_redirect_the_floor() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fixture = install_hostile_fixture(tmp.path());

        if std::env::var_os("PORCH_FLOOR_CHILD").is_none() {
            let thread = std::thread::current();
            let test_name = thread.name().expect("test thread name");
            let mut path = fixture.path_dir.display().to_string();
            if let Ok(orig) = std::env::var("PATH") {
                path = format!("{path}:{orig}");
            }
            let output = Command::new(std::env::current_exe().unwrap())
                .current_dir(tmp.path())
                .env("PORCH_FLOOR_CHILD", "1")
                .env("PORCH_HOME", &fixture.home)
                .env(REVIEW_BIN_ENV, &fixture.review_bin)
                .env(REVIEW_AGENT_BIN_ENV, &fixture.agent_bin)
                .env("PATH", path)
                .args(["--exact", test_name])
                .output()
                .unwrap();
            assert_child_ok(&output);
            return;
        }

        let _launch = super::LaunchOverride::set(fixture.launch);
        let prepared = resolve().expect("floor should resolve to the canonical sibling");
        let got = &prepared.plan.spawned_target_absolute;
        let sibling = fixture.sibling.canonicalize().unwrap();
        assert_eq!(got, &sibling, "resolved target must be the sibling");
        assert_ne!(got, &fixture.review_bin.canonicalize().unwrap());
        assert_ne!(got, &fixture.agent_bin.canonicalize().unwrap());
        assert_ne!(got, &fixture.wrapper.canonicalize().unwrap());
        assert_ne!(got, &fixture.config_bin.canonicalize().unwrap());
        assert_ne!(got, &fixture.path_quality.canonicalize().unwrap());
    }

    #[test]
    fn missing_sibling_is_unresolved_and_skips_path_lookup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let install = tmp.path().join("install");
        let launch = install_exe(&install, "porch", "porch-launch");
        let path_dir = tmp.path().join("on-path");
        let path_quality = install_exe(&path_dir, &quality_name(), "path-fallback");
        let expected_sibling = launch
            .canonicalize()
            .unwrap()
            .parent()
            .unwrap()
            .join(quality_name());

        if std::env::var_os("PORCH_FLOOR_CHILD").is_none() {
            let thread = std::thread::current();
            let test_name = thread.name().expect("test thread name");
            let mut path = path_dir.display().to_string();
            if let Ok(orig) = std::env::var("PATH") {
                path = format!("{path}:{orig}");
            }
            let output = Command::new(std::env::current_exe().unwrap())
                .current_dir(tmp.path())
                .env("PORCH_FLOOR_CHILD", "1")
                .env("PORCH_HOME", tmp.path().join("home"))
                .env(REVIEW_BIN_ENV, &path_quality)
                .env(REVIEW_AGENT_BIN_ENV, &path_quality)
                .env("PATH", path)
                .args(["--exact", test_name])
                .output()
                .unwrap();
            assert_child_ok(&output);
            return;
        }

        let _launch = super::LaunchOverride::set(launch);
        let err = resolve().expect_err("missing sibling must not resolve");
        match err {
            crate::Error::FloorUnresolved { reason } => {
                assert!(!reason.trim().is_empty(), "unresolved reason required");
                assert!(
                    reason.contains(&expected_sibling.display().to_string()),
                    "reason should name the sibling, got {reason}"
                );
                assert!(
                    !reason.contains("not found on PATH"),
                    "must not fall back to PATH, got {reason}"
                );
                assert!(
                    !reason.contains(REVIEW_BIN_ENV),
                    "must not mention review-bin env, got {reason}"
                );
            }
            other => panic!("expected unresolved floor, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_launch_path_resolves_to_the_same_canonical_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real_dir = tmp.path().join("real");
        let launch_real = install_exe(&real_dir, "porch", "porch-launch");
        let sibling = install_exe(&real_dir, &quality_name(), "canonical-floor");
        let link_dir = tmp.path().join("link");
        fs::create_dir_all(&link_dir).unwrap();
        let launch_link = link_dir.join("porch");
        std::os::unix::fs::symlink(&launch_real, &launch_link).unwrap();

        let _launch = super::LaunchOverride::set(launch_link);
        let first = resolve().expect("symlink launch should resolve");
        let second = resolve().expect("second invocation should resolve");
        let canonical = sibling.canonicalize().unwrap();
        assert_eq!(first.plan.spawned_target_absolute, canonical);
        assert_eq!(
            second.plan.spawned_target_absolute, canonical,
            "both invocations must agree on the canonical target"
        );
        assert_eq!(
            first.plan.descriptor.invocation.spawned_target_absolute,
            canonical.to_str().unwrap()
        );
    }

    #[test]
    fn recorded_path_matches_content_identity_and_replacement_fails_stability() {
        let tmp = tempfile::TempDir::new().unwrap();
        let install = tmp.path().join("install");
        let launch = install_exe(&install, "porch", "porch-launch");
        let sibling = install_exe(&install, &quality_name(), "canonical-floor");
        let _launch = super::LaunchOverride::set(launch);

        let prepared = resolve().expect("floor should resolve");
        let canonical = sibling.canonicalize().unwrap();
        assert_eq!(prepared.plan.spawned_target_absolute, canonical);
        assert_eq!(
            prepared.plan.descriptor.invocation.spawned_target_absolute,
            canonical.to_str().unwrap()
        );

        let bytes = fs::read(&canonical).unwrap();
        let entry_sha = {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(&bytes))
        };
        let expected = crate::plan::composite_artifact_identity(
            crate::plan::AdapterKind::PorchJsonCli,
            None,
            "opaque_entrypoint",
            Some(&entry_sha),
            &[],
        );
        match &prepared.plan.descriptor.observed_version_identity {
            crate::plan::ObservedVersionIdentity::ArtifactSha256(got) => {
                assert_eq!(got, &expected, "identity must come from observed content");
            }
            crate::plan::ObservedVersionIdentity::Unavailable(reason) => {
                panic!("identity must be content-derived, got unavailable: {reason}");
            }
        }

        crate::plan::check_artifacts_stable(&prepared.plan).expect("stamps should match");
        fs::write(&canonical, "#!/bin/sh\n# replaced artifact\nexit 0\n").unwrap();
        chmod_755(&canonical).unwrap();
        let err = crate::plan::check_artifacts_stable(&prepared.plan).unwrap_err();
        assert!(
            matches!(err, crate::Error::ProducerArtifactChanged),
            "replaced sibling must fail stability, got {err:?}"
        );
    }
}
