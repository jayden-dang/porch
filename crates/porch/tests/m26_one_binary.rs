//! M26 (ONEBIN): the installed pair, and what porch says about it.
//!
//! Every test here installs `porch` and its floor sibling into a directory the test
//! owns, so that "the sibling next to the running executable" means the fixture's
//! pair and not the workspace's `target/` build. That matters more than usual in
//! this file: two packages in this workspace emit a binary named `porch-quality`
//! into one path (`ONEBIN-8.1`), so a test that resolves the artifact by name cannot
//! say which package it got.
//!
//! `ONEBIN-2` -- the refusal when porch is replaced while running -- is proven in
//! `porch-review`'s `floor` unit tests instead of here. Reaching that state through
//! a real binary means unlinking an executable after its process has started and
//! before it resolves the floor, which is a timer race; the unit tests set the end
//! state directly.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use tempfile::TempDir;

fn chmod_755(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

struct Install {
    /// Kept alive so the install directory outlives the commands run against it.
    tmp: TempDir,
    /// Holds `porch` and its floor sibling; deliberately kept off `PATH`.
    bin: PathBuf,
    /// Holds `git` and whatever else a test wants found by lookup.
    path_dir: PathBuf,
    home: PathBuf,
    work: PathBuf,
}

impl Install {
    fn porch(&self) -> StdCommand {
        let mut cmd = StdCommand::new(self.bin.join("porch"));
        cmd.current_dir(&self.work)
            .env("PATH", &self.path_dir)
            .env("PORCH_HOME", &self.home)
            .env("HOME", self.tmp.path())
            .env_remove("PORCH_REVIEW_BIN")
            .env_remove("PORCH_REVIEW_AGENT_BIN")
            .env_remove("PORCH_GH_BIN")
            .env_remove("PORCH_FIXER_BIN");
        cmd
    }

    fn floor(&self) -> PathBuf {
        self.bin.join("porch-quality")
    }

    /// Put the install directory on `PATH`, as `cargo install` into `~/.cargo/bin` does.
    fn with_bin_on_path(&self, cmd: &mut StdCommand) {
        cmd.env(
            "PATH",
            format!("{}:{}", self.bin.display(), self.path_dir.display()),
        );
    }
}

/// Install the pair the way `cargo install porch` does: two files, one directory.
fn install() -> Install {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let bin = root.join("bin");
    let path_dir = root.join("path");
    let home = root.join("home");
    let work = root.join("work");
    for d in [&bin, &path_dir, &home, &work] {
        fs::create_dir_all(d).unwrap();
    }

    for name in ["porch", "porch-quality"] {
        let src = assert_cmd::cargo::cargo_bin(name);
        let dst = bin.join(name);
        fs::copy(&src, &dst).unwrap_or_else(|e| panic!("copy {}: {e}", src.display()));
        chmod_755(&dst);
    }

    // A real git: setup and doctor both look for one, and the fixture's own repo needs it.
    let real_git = which_git();
    std::os::unix::fs::symlink(&real_git, path_dir.join("git")).unwrap();

    git(&work, &["init", "-q", "-b", "main"]);

    Install {
        tmp,
        bin,
        path_dir,
        home,
        work,
    }
}

fn which_git() -> PathBuf {
    let out = StdCommand::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
}

fn git(work: &Path, args: &[&str]) {
    let ok = StdCommand::new("git")
        .current_dir(work)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed in {}", work.display());
}

fn stdout_of(cmd: &mut StdCommand) -> String {
    let out = cmd.output().unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Install a fake coding agent so engine detection has something to prefer.
fn install_fake_agent(dir: &Path) {
    let path = dir.join("claude");
    fs::write(&path, "#!/bin/sh\necho fake-claude\nexit 0\n").unwrap();
    chmod_755(&path);
}

// --- ONEBIN-3: the release smoke gate asserts something true ---------------------

#[test]
fn the_release_smoke_commands_succeed_against_a_correct_installation() {
    // docs/agents/project.md release step 7. It asserted `porch-quality --version`,
    // which has never existed; three tags shipped past it because nothing ran it.
    let inst = install();

    let porch = StdCommand::new(inst.bin.join("porch"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(porch.status.success(), "porch --version must exit 0");
    assert!(
        String::from_utf8_lossy(&porch.stdout).contains("porch"),
        "porch --version should name itself"
    );

    let floor = StdCommand::new(inst.floor())
        .arg("--help")
        .output()
        .unwrap();
    assert!(
        floor.status.success(),
        "the floor's smoke command must exit 0, got {:?}\n{}",
        floor.status.code(),
        String::from_utf8_lossy(&floor.stderr)
    );
}

#[test]
fn the_floor_still_has_no_version_flag_so_the_old_smoke_step_would_fail() {
    // Pins why step 7 was corrected rather than the binary taught a flag: adding one
    // is a compatibility promise on an executable MILE-5's first blocker may delete,
    // and plumbing a real version into the descriptor breaks the audit read path
    // (ONEBIN-7.2). If a `--version` is ever added, this test should be deleted along
    // with the reasoning in the spec.
    let inst = install();
    let out = StdCommand::new(inst.floor())
        .arg("--version")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "unexpected --version support; revisit ONEBIN-3.3 and ONEBIN-7.2"
    );
}

// --- ONEBIN-4: setup does not require a lookup the floor refuses to perform ------

#[test]
fn setup_engine_quality_succeeds_with_the_sibling_present_and_off_path() {
    let inst = install();
    let out = stdout_of(inst.porch().args(["setup", "--yes", "--engine", "quality"]));

    assert!(
        out.contains("\"ok\": true"),
        "setup must accept a correct installation; PATH held no porch-quality\n{out}"
    );
    assert!(
        out.contains("\"engine\": \"quality\""),
        "engine should be quality\n{out}"
    );
}

#[test]
fn setup_engine_quality_still_fails_when_no_floor_can_be_resolved_and_says_so() {
    let inst = install();
    fs::remove_file(inst.floor()).unwrap();

    let out = stdout_of(inst.porch().args(["setup", "--yes", "--engine", "quality"]));
    assert!(
        out.contains("\"ok\": false"),
        "must still fail closed\n{out}"
    );
    assert!(
        out.contains("porch-quality"),
        "must name what it looked for\n{out}"
    );
}

// --- ONEBIN-5: doctor reports what it observed ------------------------------------

#[test]
fn doctor_reports_the_floors_observed_identity() {
    let inst = install();
    let out = stdout_of(inst.porch().arg("doctor"));

    assert!(out.contains("[ok  ] floor:"), "floor should resolve\n{out}");
    let identity = floor_identity(&out).expect("doctor must report an identity");
    assert_eq!(identity.len(), 64, "expected a sha256, got {identity}");
    assert!(
        identity.chars().all(|c| c.is_ascii_hexdigit()),
        "expected hex, got {identity}"
    );
}

#[test]
fn a_different_floor_reports_a_different_identity() {
    // The observable an operator needs to compare two installations, without porch
    // deciding which is correct -- see the tripwire below.
    let inst = install();
    let before = floor_identity(&stdout_of(inst.porch().arg("doctor"))).unwrap();

    fs::write(inst.floor(), "#!/bin/sh\necho not-the-floor\nexit 0\n").unwrap();
    chmod_755(&inst.floor());
    let after = floor_identity(&stdout_of(inst.porch().arg("doctor"))).unwrap();

    assert_ne!(
        before, after,
        "a replaced floor must be observably different"
    );
}

#[test]
fn doctor_warns_and_names_a_remedy_when_the_floor_is_gone() {
    let inst = install();
    fs::remove_file(inst.floor()).unwrap();

    let out = stdout_of(inst.porch().arg("doctor"));
    assert!(out.contains("[warn] floor:"), "must warn\n{out}");
    assert!(
        out.contains("cargo install porch"),
        "must name a remedy command\n{out}"
    );
}

fn floor_identity(doctor_stdout: &str) -> Option<String> {
    doctor_stdout
        .lines()
        .find(|l| l.contains("] floor:"))
        .and_then(|l| l.split("identity=").nth(1))
        .map(|s| s.trim().to_string())
}

// --- ONEBIN-8: tripwires. Today's behaviour pinned, not endorsed ------------------

#[test]
fn tripwire_two_packages_declare_a_bin_named_porch_quality() {
    // ONEBIN-8.1. Two bin targets cannot produce one file name, so cargo warns that
    // this "may become a hard error". Removing it decides MILE-5's first blocker --
    // the compatibility period -- so no coding session may do it. This pin fails if
    // the collision is either resolved or worsened, which is when the roadmap and
    // the spec need revisiting.
    let root = workspace_root();
    // porch-git and porch-gate are in the scan so the filter has to discriminate.
    // Without them a predicate that always answered "yes" would pass this test.
    let scanned = ["porch", "porch-quality", "porch-git", "porch-gate"];
    let owners: Vec<&str> = scanned
        .iter()
        .filter(|name| declares_quality_bin(&root.join("crates").join(name).join("Cargo.toml")))
        .copied()
        .collect();

    assert_eq!(
        owners,
        vec!["porch", "porch-quality"],
        "expected exactly these two packages to declare a `porch-quality` bin; \
         if this changed, MILE-5's compatibility blocker was resolved and \
         docs/specs/2026-09-09-one-binary-install-coherence/requirements.md \
         section 7.3 is stale"
    );
}

#[test]
fn tripwire_engine_selection_depends_on_whether_porchs_bindir_is_on_path() {
    // ONEBIN-8.2, MILE-5's second blocker made executable. Same machine, same coding
    // agent, one PATH entry different. Installing porch puts porch-quality on PATH,
    // so porch's own artifact is read as evidence that the operator chose floor-only.
    // This asserts what porch does today. It does NOT assert that it is right.
    let inst = install();
    install_fake_agent(&inst.path_dir);

    let off_path = stdout_of(inst.porch().args(["setup", "--yes"]));
    assert!(
        off_path.contains("\"engine\": \"agent\""),
        "without porch's bindir on PATH, the judgment engine wins\n{off_path}"
    );

    fs::remove_dir_all(&inst.home).unwrap();
    fs::create_dir_all(&inst.home).unwrap();

    let mut cmd = inst.porch();
    cmd.args(["setup", "--yes"]);
    inst.with_bin_on_path(&mut cmd);
    let on_path = stdout_of(&mut cmd);
    assert!(
        on_path.contains("\"engine\": \"quality\""),
        "with porch's bindir on PATH, porch's own sibling wins and the agent is \
         dropped with no warning\n{on_path}"
    );
}

#[test]
fn tripwire_a_foreign_executable_of_the_right_name_is_accepted_as_the_floor() {
    // ONEBIN-8.3. FLOOR-9.2 states its no-redirect guarantee "covers configuration
    // and environment only; it does not extend to an owner who replaces the installed
    // porch or porch-quality binaries", so this is declared behaviour, not a bug --
    // and neither ARCH-9 nor ARCH-12 says otherwise. Binding it needs an ADR
    // (ONEBIN-7.1). Pinned so that question has a record that moves if the answer does.
    let inst = install();
    fs::write(inst.floor(), "#!/bin/sh\necho not-the-floor\nexit 0\n").unwrap();
    chmod_755(&inst.floor());

    let out = stdout_of(inst.porch().arg("doctor"));
    assert!(
        out.contains("[ok  ] floor:"),
        "today a three-line shell script passes as the floor\n{out}"
    );
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

fn declares_quality_bin(manifest: &Path) -> bool {
    let text =
        fs::read_to_string(manifest).unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));
    text.split("[[bin]]").skip(1).any(|section| {
        section
            .lines()
            .take_while(|l| !l.trim_start().starts_with('['))
            .any(|l| l.trim() == r#"name = "porch-quality""#)
    })
}
