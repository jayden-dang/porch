//! M1: git CLI wrapper (`--git-dir` absolute).

use std::process::Command;

use porch_git::{
    GitDir, PushDecision, RemoteTip, fetch_git_args, force_fetch_refspec, init_bare, ls_remote_sha,
    push_exact_sha, remote_commits_incorporated, resolve_push_decision, run, run_c, show_path_at,
    stdout_trim,
};
use tempfile::TempDir;

fn write_commit(work: &std::path::Path) {
    Command::new("git")
        .current_dir(work)
        .args(["init"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(work)
        .args(["config", "user.email", "porch@example.com"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(work)
        .args(["config", "user.name", "Porch"])
        .status()
        .unwrap();
    std::fs::write(work.join("README"), "hi\n").unwrap();
    Command::new("git")
        .current_dir(work)
        .args(["add", "README"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(work)
        .args(["commit", "-m", "init"])
        .status()
        .unwrap();
}

#[test]
fn fetch_args_disable_prune_and_force_refspec() {
    assert_eq!(
        force_fetch_refspec("refs/heads/main:refs/remotes/origin/main"),
        "+refs/heads/main:refs/remotes/origin/main"
    );
    assert_eq!(
        force_fetch_refspec("+refs/heads/dev:refs/remotes/origin/dev"),
        "+refs/heads/dev:refs/remotes/origin/dev"
    );
    assert_eq!(
        fetch_git_args("origin", "refs/heads/main:refs/remotes/origin/main"),
        vec![
            "-c",
            "fetch.prune=false",
            "fetch",
            "origin",
            "+refs/heads/main:refs/remotes/origin/main",
        ]
    );
}

#[test]
fn git_dir_rejects_relative_path() {
    let err = GitDir::new("relative/path").unwrap_err();
    assert!(matches!(err, porch_git::Error::GitDirNotAbsolute(_)));
}

#[test]
fn rev_parse_head_uses_absolute_git_dir() {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().canonicalize().unwrap();
    write_commit(&work);
    let git_dir = GitDir::new(work.join(".git")).unwrap();
    let out = run(&git_dir, &["rev-parse", "HEAD"]).unwrap();
    let sha = stdout_trim(&out);
    assert_eq!(sha.len(), 40, "expected full SHA, got {sha:?}");
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn init_bare_creates_repository_at_absolute_path() {
    let tmp = TempDir::new().unwrap();
    let bare = tmp.path().canonicalize().unwrap().join("gate.git");
    let git_dir = init_bare(&bare).unwrap();
    let out = run(&git_dir, &["rev-parse", "--is-bare-repository"]).unwrap();
    assert_eq!(stdout_trim(&out), "true");
}

#[test]
fn run_c_rejects_relative_work_tree() {
    let err = run_c(std::path::Path::new("rel"), &["status"]).unwrap_err();
    assert!(matches!(err, porch_git::Error::GitDirNotAbsolute(_)));
}

#[test]
fn show_path_at_returns_none_when_path_missing() {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().canonicalize().unwrap();
    write_commit(&work);
    let git_dir = GitDir::new(work.join(".git")).unwrap();
    let sha = stdout_trim(&run(&git_dir, &["rev-parse", "HEAD"]).unwrap());
    let got = show_path_at(&git_dir, &sha, ".porch.yaml").unwrap();
    assert_eq!(got, None);
}

#[test]
fn show_path_at_returns_blob_when_present() {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().canonicalize().unwrap();
    write_commit(&work);
    let git_dir = GitDir::new(work.join(".git")).unwrap();
    let sha = stdout_trim(&run(&git_dir, &["rev-parse", "HEAD"]).unwrap());
    let got = show_path_at(&git_dir, &sha, "README").unwrap();
    assert_eq!(got.as_deref(), Some(b"hi\n".as_slice()));
}

#[test]
fn show_path_at_fails_closed_on_unreadable_commit() {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().canonicalize().unwrap();
    write_commit(&work);
    let git_dir = GitDir::new(work.join(".git")).unwrap();
    let err = show_path_at(
        &git_dir,
        "0000000000000000000000000000000000000001",
        ".porch.yaml",
    )
    .unwrap_err();
    assert!(
        matches!(err, porch_git::Error::Command { .. }),
        "expected command error, got {err:?}"
    );
}

#[test]
fn worktree_add_detach_creates_path() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    write_commit(&work);
    let bare = root.join("bare.git");
    let git_dir = init_bare(&bare).unwrap();
    let sha = stdout_trim(
        &run(
            &GitDir::new(work.join(".git")).unwrap(),
            &["rev-parse", "HEAD"],
        )
        .unwrap(),
    );
    Command::new("git")
        .current_dir(&work)
        .args(["remote", "add", "gate", bare.to_str().unwrap()])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&work)
        .args(["push", "gate", "HEAD:refs/heads/main"])
        .status()
        .unwrap();
    let wt = root.join("wt");
    porch_git::worktree_add_detach(&git_dir, &wt, &sha).unwrap();
    assert!(wt.join("README").is_file());
    porch_git::worktree_remove_force(&git_dir, &wt).unwrap();
}

fn seed_bare_with_main(root: &std::path::Path) -> (porch_git::GitDir, std::path::PathBuf, String) {
    let origin = root.join("origin.git");
    let origin_gd = init_bare(&origin).unwrap();
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    write_commit(&seed);
    Command::new("git")
        .current_dir(&seed)
        .args(["remote", "add", "origin", origin.to_str().unwrap()])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["push", "-u", "origin", "HEAD:refs/heads/main"])
        .status()
        .unwrap();
    let sha = stdout_trim(&run(&origin_gd, &["rev-parse", "refs/heads/main"]).unwrap());
    (origin_gd, seed, sha)
}

#[test]
fn ls_remote_absent_ref_is_none() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (origin_gd, _seed, _sha) = seed_bare_with_main(&root);

    let gate = root.join("gate.git");
    let gate_gd = init_bare(&gate).unwrap();
    run(
        &gate_gd,
        &[
            "remote",
            "add",
            "origin",
            origin_gd.as_path().to_str().unwrap(),
        ],
    )
    .unwrap();

    let tip = ls_remote_sha(&gate_gd, "origin", "refs/heads/feat").unwrap();
    assert_eq!(tip, RemoteTip::Absent);
}

#[test]
fn ls_remote_present_returns_sha() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (origin_gd, _seed, sha) = seed_bare_with_main(&root);

    let gate = root.join("gate.git");
    let gate_gd = init_bare(&gate).unwrap();
    run(
        &gate_gd,
        &[
            "remote",
            "add",
            "origin",
            origin_gd.as_path().to_str().unwrap(),
        ],
    )
    .unwrap();

    let tip = ls_remote_sha(&gate_gd, "origin", "refs/heads/main").unwrap();
    assert_eq!(tip, RemoteTip::Present(sha));
}

#[test]
fn push_exact_sha_new_branch_then_up_to_date() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (origin_gd, seed, main_sha) = seed_bare_with_main(&root);

    std::fs::write(seed.join("feat.txt"), "x\n").unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["add", "feat.txt"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["commit", "-m", "feat"])
        .status()
        .unwrap();
    let feat_sha = stdout_trim(
        &run(
            &GitDir::new(seed.join(".git")).unwrap(),
            &["rev-parse", "HEAD"],
        )
        .unwrap(),
    );

    let gate = root.join("gate.git");
    let gate_gd = init_bare(&gate).unwrap();
    // Need objects in gate: fetch from origin + push feat objects via receive from seed.
    run(
        &gate_gd,
        &[
            "remote",
            "add",
            "origin",
            origin_gd.as_path().to_str().unwrap(),
        ],
    )
    .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["remote", "add", "gate", gate.to_str().unwrap()])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["push", "gate", "HEAD:refs/heads/feat"])
        .status()
        .unwrap();

    // Push feat to origin via gate bare.
    push_exact_sha(
        &gate_gd,
        "origin",
        "refs/heads/feat",
        &feat_sha,
        PushDecision::NewBranch,
    )
    .unwrap();
    let tip = ls_remote_sha(&gate_gd, "origin", "refs/heads/feat").unwrap();
    assert_eq!(tip, RemoteTip::Present(feat_sha.clone()));

    // Second push is up-to-date no-op.
    push_exact_sha(
        &gate_gd,
        "origin",
        "refs/heads/feat",
        &feat_sha,
        PushDecision::UpToDate,
    )
    .unwrap();
    let tip = ls_remote_sha(&gate_gd, "origin", "refs/heads/feat").unwrap();
    assert_eq!(tip, RemoteTip::Present(feat_sha));

    let _ = main_sha;
}

#[test]
fn push_exact_sha_skips_client_pre_push_hooks() {
    // Deliver must pass --no-verify so a rejecting client pre-push (e.g. lefthook
    // planted into the bare gate hooks dir) cannot block origin forward.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (origin_gd, seed, _main_sha) = seed_bare_with_main(&root);

    std::fs::write(seed.join("feat.txt"), "x\n").unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["add", "feat.txt"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["commit", "-m", "feat"])
        .status()
        .unwrap();
    let feat_sha = stdout_trim(
        &run(
            &GitDir::new(seed.join(".git")).unwrap(),
            &["rev-parse", "HEAD"],
        )
        .unwrap(),
    );

    let gate = root.join("gate.git");
    let gate_gd = init_bare(&gate).unwrap();
    run(
        &gate_gd,
        &[
            "remote",
            "add",
            "origin",
            origin_gd.as_path().to_str().unwrap(),
        ],
    )
    .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["remote", "add", "gate", gate.to_str().unwrap()])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["push", "gate", "HEAD:refs/heads/feat"])
        .status()
        .unwrap();

    let hook = gate.join("hooks/pre-push");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho 'pre-push should be skipped' >&2\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook, perms).unwrap();
    }

    push_exact_sha(
        &gate_gd,
        "origin",
        "refs/heads/feat",
        &feat_sha,
        PushDecision::NewBranch,
    )
    .unwrap();
    let tip = ls_remote_sha(&gate_gd, "origin", "refs/heads/feat").unwrap();
    assert_eq!(tip, RemoteTip::Present(feat_sha));
}

#[test]
fn lease_push_updates_when_incorporated() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (origin_gd, seed, _main_sha) = seed_bare_with_main(&root);

    // First tip on origin/feat.
    std::fs::write(seed.join("a.txt"), "a\n").unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["add", "a.txt"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["commit", "-m", "a"])
        .status()
        .unwrap();
    let old_sha = stdout_trim(
        &run(
            &GitDir::new(seed.join(".git")).unwrap(),
            &["rev-parse", "HEAD"],
        )
        .unwrap(),
    );
    Command::new("git")
        .current_dir(&seed)
        .args(["push", "origin", "HEAD:refs/heads/feat"])
        .status()
        .unwrap();

    // Advance locally.
    std::fs::write(seed.join("b.txt"), "b\n").unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["add", "b.txt"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["commit", "-m", "b"])
        .status()
        .unwrap();
    let new_sha = stdout_trim(
        &run(
            &GitDir::new(seed.join(".git")).unwrap(),
            &["rev-parse", "HEAD"],
        )
        .unwrap(),
    );

    let gate = root.join("gate.git");
    let gate_gd = init_bare(&gate).unwrap();
    run(
        &gate_gd,
        &[
            "remote",
            "add",
            "origin",
            origin_gd.as_path().to_str().unwrap(),
        ],
    )
    .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["remote", "add", "gate", gate.to_str().unwrap()])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["push", "gate", "HEAD:refs/heads/feat"])
        .status()
        .unwrap();

    assert!(remote_commits_incorporated(&gate_gd, &new_sha, &old_sha, None).unwrap());
    let decision = resolve_push_decision(&RemoteTip::Present(old_sha.clone()), &new_sha, true);
    push_exact_sha(&gate_gd, "origin", "refs/heads/feat", &new_sha, decision).unwrap();
    let tip = ls_remote_sha(&gate_gd, "origin", "refs/heads/feat").unwrap();
    assert_eq!(tip, RemoteTip::Present(new_sha));
}

#[test]
#[allow(clippy::too_many_lines)]
fn incorporate_refuses_divergent_remote() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (origin_gd, seed, main_sha) = seed_bare_with_main(&root);

    // Local feat from main.
    Command::new("git")
        .current_dir(&seed)
        .args(["checkout", "-b", "feat"])
        .status()
        .unwrap();
    std::fs::write(seed.join("local.txt"), "local\n").unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["add", "local.txt"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["commit", "-m", "local"])
        .status()
        .unwrap();
    let local_sha = stdout_trim(
        &run(
            &GitDir::new(seed.join(".git")).unwrap(),
            &["rev-parse", "HEAD"],
        )
        .unwrap(),
    );

    // Divergent tip on origin: commit from a different clone of main.
    let other = root.join("other");
    Command::new("git")
        .args([
            "clone",
            origin_gd.as_path().to_str().unwrap(),
            other.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&other)
        .args(["config", "user.email", "porch@example.com"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&other)
        .args(["config", "user.name", "Porch"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&other)
        .args(["checkout", "-b", "feat"])
        .status()
        .unwrap();
    std::fs::write(other.join("remote.txt"), "remote\n").unwrap();
    Command::new("git")
        .current_dir(&other)
        .args(["add", "remote.txt"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&other)
        .args(["commit", "-m", "remote-only"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&other)
        .args(["push", "origin", "HEAD:refs/heads/feat"])
        .status()
        .unwrap();
    let remote_sha = stdout_trim(
        &run(
            &GitDir::new(other.join(".git")).unwrap(),
            &["rev-parse", "HEAD"],
        )
        .unwrap(),
    );

    let gate = root.join("gate.git");
    let gate_gd = init_bare(&gate).unwrap();
    run(
        &gate_gd,
        &[
            "remote",
            "add",
            "origin",
            origin_gd.as_path().to_str().unwrap(),
        ],
    )
    .unwrap();
    // Import both tips into gate so rev-list can see them.
    Command::new("git")
        .current_dir(&seed)
        .args(["remote", "add", "gate", gate.to_str().unwrap()])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&seed)
        .args(["push", "gate", "HEAD:refs/heads/feat"])
        .status()
        .unwrap();
    porch_git::fetch(
        &gate_gd,
        "origin",
        "refs/heads/feat:refs/remotes/origin/feat",
    )
    .unwrap();

    assert!(
        !remote_commits_incorporated(&gate_gd, &local_sha, &remote_sha, Some(&main_sha)).unwrap()
    );
}
