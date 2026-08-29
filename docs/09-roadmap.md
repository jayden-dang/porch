# Roadmap

Dogfood target: mailgate. Each milestone should be runnable on a toy git repo first, then on mailgate.

## M0 — Repo and briefing (this milestone)

- [x] Research dump in `docs/`
- [x] Locked decisions
- [ ] `LICENSE` Apache-2.0 when ready to publish
- [ ] crates.io name check
- [ ] Confirm review-CLI flags for range review + JSON output against current `--help` at implementation time (flags drift). Details in `.research/`.

## M1 — Push into a dead gate (in progress)

Goal: `porch init` + `git push porch` updates a local bare repo and returns. No pipeline.

- Cargo binary, clap: `init`, `daemon run`, `daemon notify-push`, `daemon admit-push`
- Bare repo, hooks, remote `porch`
- Flock + socket + health RPC
- SQLite `repos` + `runs` insert on notify
- Git wrapper
- Tests: tempfile repo, init, push, run row

**Out:** TUI, review CLI, PR, Windows polish can be stubbed but process-group spawn wrapper should exist empty-tested.

## M2 — Worktree + intent + rebase (done)

- [x] Worktree add at recorded path
- [x] Phase runner skeleton (skip flags)
- [x] Intent: store `PORCH_INTENT` on the run
- [x] Rebase onto `origin/<default>` (`repos.default_branch`, default `main`)
- [x] Empty diff → complete run skipped remaining
- [x] Crash: fail stale running runs

## M3 — Review + park (done)

- [x] Require the review CLI on PATH (`PORCH_REVIEW_BIN`)
- [x] Run range review in the worktree
- [x] Parse JSON → findings; park on blocking
- [x] `porch agent status` / `respond` (JSON stdout; approve|skip|abort)
- [x] `review_approved_head_sha` on success (not on skip)
- [x] Fixtures: fake review binary

**Out:** fixer.

## M4 — Fixer + rereview + HEAD continuity (done)

- [x] Native fixer CLI (`PORCH_FIXER_BIN`); ACP later
- [x] `porch agent respond fix [--findings] [--yes]`
- [x] Session-free rereview; fixer may resume
- [x] Uncertified range on incomplete rereview
- [x] Process-group kill on fixer step end
- [x] Prompt files under `$PORCH_HOME` (refuse if missing)
- [x] HEAD continuity before certify/deliver stubs
- [x] Extra M1–M3 scenarios: coverage miss, parked-across-restart, status without `--run-id`, fetch fail closed, followTags

## M5 — Certify adapters (1 week)

- [x] `commands.format` / `commands.lint` from **trusted** config
- [x] Fail closed on unreadable/unparseable trusted yaml; missing file → empty commands
- [x] Non-zero format/lint fails certify; process-group kill on every end path
- [x] Correction commits for dirty format/lint (`--no-verify` + empty `core.hooksPath`)
- [ ] Mailgate sketch smoke: biome + types/api/docs drift (operator-gated; not `cargo test`)
- [x] No Postgres, no Playwright as certify defaults

## M6 — Deliver GitHub (2 weeks)

- Lease-push exact SHA
- `gh pr create/update`, body + attestation comment
- Watch allowlisted checks
- Repair: restart at **review**
- `rerun_transient = 0`

## M7 — Dogfood on mailgate

- `.porch.yaml` on default branch (trusted)
- Path instructions for enclave/auth/contract/infra
- Measure: did PR Checks fail less often for mechanical drift? Did review park real `ask-user` issues?
- Worktree cold-compile pain: document sccache; still do not run full `just gate`

## M8 — Operator UX

- TUI (ratatui)
- launchd/systemd/schtasks
- `porch agent` skill markdown
- Socket activation
- APFS clone worktrees
- Eval corpus of gold findings (mailgate diffs)

## Explicitly later / never

| Later | Never (as porch) |
|---|---|
| Embed the review engine as a library | Replace mailgate CI/CD or E2E |
| GitLab/Gitea | libgit2 gate operations |
| More native agents | Nine adapters day-1 |
| Evidence branch publish | Contributor `no_ci` |
| Document phase | Auto-fix review default on |
| Transcript intent inference | Merge bot |

## First coding session checklist

1. Read `docs/decisions.md` and this file.
2. Scaffold `Cargo.toml` + `src/main.rs` clap + `src/git.rs` stub.
3. Do not implement review in the first PR.
4. Keep the tree English, `rustfmt`, clippy `-D warnings`.
