# Roadmap

Dogfood target: mailgate. Each milestone should be runnable on a toy git repo first, then on mailgate.

## M0 — Repo and briefing (this milestone)

- [x] Research dump in `docs/`
- [x] Locked decisions
- [x] `LICENSE` Apache-2.0 when ready to publish
- [x] crates.io name check (`porch` free as of 2026-08-29; HTTP 404)
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
- [x] HEAD continuity before certify/deliver
- [x] Extra M1–M3 scenarios: coverage miss, parked-across-restart, status without `--run-id`, fetch fail closed, followTags

## M5 — Certify adapters (1 week)

- [x] `commands.format` / `commands.lint` from **trusted** config
- [x] Fail closed on unreadable/unparseable trusted yaml; missing file → empty commands
- [x] Non-zero format/lint fails certify; process-group kill on every end path
- [x] Correction commits for dirty format/lint (`--no-verify` + empty `core.hooksPath`)
- [ ] Mailgate sketch smoke: biome + types/api/docs drift (operator-gated; not `cargo test`)
- [x] No Postgres, no Playwright as certify defaults

## M6 — Deliver GitHub (push+PR+allowlist watch+repair)

- [x] Lease-push exact SHA (`--force-with-lease=<ref>:<observed>`; never bare `--force`)
- [x] `gh pr create/update`, body + `<!-- porch-attestation … -->` binding `head_sha`
- [x] Watch allowlisted checks only (`deliver.github.watch_checks`); empty allowlist → push+PR, no babysit
- [x] `rerun_transient` default **0**; no `gh run rerun` in this milestone
- [x] Fail closed on incorporate refuse / missing `gh` (before push) / non-repairable or budget-exhausted allowlisted red
- [x] `runs.pr_url`; daemon restart while watching → `ci_monitor_interrupted` (not failed)
- [x] Repair: mechanical allowlisted CI-fix / CONFLICTING rebase → restart at **review** → certify → lease-push (budget 3; cancelled/timed_out fail closed; no `gh run rerun`)

## M7 — Dogfood yaml + porch gaps (done)

- [x] Canonical yaml authored: `docs/examples/mailgate.porch.yaml`, `docs/examples/klynt.porch.yaml`
- [x] Allowlist **skip-as-Ready** (skip/skipped/skipping/neutral); missing name still Waiting
- [x] Trusted `pr.base_branch` → rebase fetch/onto + `gh pr create --base` (empty → `repos.default_branch`)
- [x] Parse `review.path_instructions`; persist matching-or-all JSON under `$PORCH_HOME/runs/<id>/`
- [x] Docs: `04-klynt.md`, mailgate sketch → canonical example; index/roadmap/references/AGENTS dogfood table
- [ ] Measurement of live PR-check rates / ask-user park rates is **observational** — not claimed as a shipped metric
- Worktree cold-compile pain: document sccache; still do not run full `just gate` / `moon ci`

## M8 — Operator UX

- [x] TUI attach (ratatui in `crates/porch`; no `porch-tui` crate) — park findings, a/f/s/x/q
- [x] Event mailbox + RPC (`list_runs` / `get_run` / `subscribe`); thread-per-connection
- [x] Managed service: `porch daemon install|uninstall|start|stop|status` (launchd/systemd render + write; Task Scheduler name render). KeepAlive / Restart=always + detached `ensure_daemon` fallback
- [x] Headless operator CLI: bare `porch`, `porch runs`, `porch status`, `porch attach` (non-TTY snapshot)
- [x] `porch agent` skill markdown (`docs/porch-agent.md`) — thin; TUI optional, agent JSON unchanged
- [x] `porch doctor` + init next-steps + publish metadata (0.1.0 operator UX)
- [ ] Socket activation (`LISTEN_FDS`) — **not this slice / later** (E6); KeepAlive managed service + detached fallback is the M8 story
- [ ] APFS clone / reflink worktrees — **not this slice / later** (would need unsafe or a new dep)
- [ ] Eval corpus of gold findings (mailgate diffs) — **not this slice / later**

## M9 — First-run setup (after M8)

- [x] **`porch setup`** headless JSON (`--yes` / `--verify` / `--engine` / `--apply`) **and** easy one-screen TUI (not a long wizard; `porch` with no args opens it when setup incomplete)
- [x] Write `$PORCH_HOME/config.yaml` (operator config). Env overrides config (`PORCH_REVIEW_BIN` > wrapper > `review`)
- [x] Engine registry: `ocr` + `generic` only; porch-owned `$PORCH_HOME/bin/review` wrapper (`exec <ocr> review "$@"`) so `run_review` argv stays `--from/--to/--format json --output`
- [x] Detect `gh`, optional fixer, repo tools; record in config
- [x] Fail-closed verify (backend, wrapper body under PORCH_HOME, `--help`, ocr `--preview` on tempfile repo); never leave config pointing at a broken wrapper
- [x] `porch init --yes` / `--skip-setup`; non-TTY prints hint (no hang)
- [x] Doctor config-aware; suggest `porch setup` when review missing
- [x] OCR fixture parse derives coverage when top-level `files` absent
- Do **not** embed the review engine (D9). Do **not** download binaries.

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
