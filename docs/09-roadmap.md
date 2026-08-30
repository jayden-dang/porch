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
- Transitional: OCR wrapper as default review engine. **Replaced by M10 agent default.**

## After M9 — borrow UX, finish the workflow, review quality last

Operator-approved sequencing (2026-08-29). Ideas only from other inner gates (D13, D15). Each milestone is a **vertical slice**: toy repo tests first, then klynt/mailgate dogfood. Do not start M16 until M10–M15 are usable on klynt without OCR.

**Do not borrow:** nine pipeline steps, nine agent adapters, six forges, TOON-only AXI, babysit every GitHub check, review auto-fix default on, wizard as a substitute for `git push porch`.

### Borrow list (into milestones)

| Idea | Why porch wants it | Where |
|---|---|---|
| One-shot installer + PATH | `cargo install` + missing `~/.cargo/bin` is the #1 operator fail | M11 |
| Skill installed at `init` | Agents already first-class (`porch agent` JSON); markdown in-tree is unused until copied | M11 |
| `porch agent run` (headless drive) | Skill needs a verb besides status/respond | M14 |
| Park TUI: finding body, hunk/diff, notes | Park is unusable if you only see `f0 warning path` | M12 |
| TUI `fix --yes` / yolo as **explicit** | Headless has `--yes`; TUI does not | M12 |
| `eject` | Leave a clone without leftover remote/hooks | M13 |
| Rebase-conflict **park** (not fail-only) | E15 was temporary until a park TUI existed; M8 shipped attach | M13 |
| `rerun` | Failed/cancelled runs today have no first-class retry | M13 |
| Sync / custody UX | Pipeline commits can leave the author’s branch behind | M13 |
| Setup also offers daemon install | Service files exist; first-run still detached-only unless the operator knows `daemon install` | M11 |
| Cold-worktree PATH | Certify dies on biome in the disposable tree; daemon PATH ≠ author PATH | M13 |
| `--intent` on push/notify | `PORCH_INTENT` is easy to miss | M14 |
| Richer PR body from intent + findings | Deliver body exists; still thin | M14 |
| Socket activation, APFS clone | Named in M8; still later | after M15 or never year-1 |
| Eval / gold findings | Research tool, not operator loop | with M16 or skip |

### M10 — Coding-agent review (replace OCR as default) (done)

**Goal:** `git push porch` reviews via a **session-free native/ACP agent**, not `ocr`. Workflow unblocks without the OCR product. Quality of that review is **known-limited**; do not pretend otherwise.

Still true: D6 auto-fix off; E9 reviewer ≠ fixer; findings JSON + park TUI/`porch agent` unchanged; `run_review` porch-side contract stays “produce `Finding`s + coverage list”; gate crate still must not depend on agent.

Work:

- [x] Engine registry: add `agent` (default after setup). Keep `generic` for PATH fakes. **OCR engine becomes optional/legacy** (`porch setup --engine ocr` still works; not the documented default).
- [x] Reviewer prompt file under `$PORCH_HOME` (like fixer). Missing prompt **refuses**. Include intent, `path_instructions` JSON, changed-file list, “emit JSON findings only”.
- [x] Spawn: same process-group kill as fixer; **session-free** (no resume id). Timeout fails the run (not park).
- [x] Schema: map agent JSON → existing `Finding`. Fail closed on unparseable output.
- [x] **Coverage-lite (not M16):** porch sends the changed-file list; agent must emit a pass or explicit skip per path; missing path → fail (same spirit as today’s `files[]` check). No grouping/relocation yet.
- [x] **Shell policy:** reviewer must not run full-suite tests or edit files. Prefer a reviewer invocation that cannot write (ACP readonly / prompt + refuse). If the native CLI cannot drop the shell, neutralize repo jailbreak docs (existing fixer concern) and treat file writes as a failed review.
- [x] `porch setup`: detect one native CLI (D8 — pick the same family as fixer or a dedicated `review.agent_bin`). Do not download. Fail closed if none found. Doctor: review ok via agent, not via `ocr` wrapper.
- [x] Tests: PATH fake agent that prints findings JSON + coverage; no real LLM. Existing M3–M9 tests keep working with `generic` fakes.

**Out of M10:** OCR-class grouping/rules/relocate; GitHub review comments; second native agent matrix.

### M11 — Install, PATH, skill, daemon in setup (done)

**Goal:** a new machine can reach `git push porch` without a Rust-path lecture.

Work:

1. [x] Install story: small `install.sh` puts the binary on PATH (macOS/Linux). Document `~/.cargo/bin`. Not crates.io until slices publish.
2. [x] `porch init` copies `docs/porch-agent.md` (embedded skill) into user skill dirs for the coding agents we already detect — JSON/`porch agent`, not TOON.
3. [x] Setup wizard (still one screen): optional “install daemon as login service” checkbox; default remains detached (`--install-daemon` headless).
4. [x] Doctor: if `~/.cargo/bin/porch` exists but is not on PATH, say so explicitly.

### M12 — Park TUI that a human can actually use

**Goal:** parked review is a decision surface, not a status LED.

Work:

1. [x] Finding panel: id, severity, **message**, path:line (already stored).
2. [x] On-demand diff/hunk for the selected finding (cap size; fetch via RPC, not the event stream).
3. [x] Optional per-finding note (persisted on the run, fed to fixer).
4. [x] TUI `y` = `respond fix --yes` (one round); still not the default.
5. [x] Footer always lists keys; success/error after respond (M8 `working` completion already).
6. [x] Tests: TestBackend, no TTY.

**Out:** multi-run TUI picker, CI panel fidelity, commit-from-TUI wizard.

### M13 — Finish the gate workflow (eject, rebase park, rerun, sync, cold PATH)

**Goal:** klynt dogfood does not die on the papercuts around the five phases.

Work:

1. [x] **`porch eject`:** remove `porch` remote; **neutralize** bare hooks (overwrite with `exit 0` stubs — do not delete the hook files); leave `$PORCH_HOME` unless `--purge`.
2. [x] **Rebase conflict → park** (supersede E15 for this milestone): abort the rebase, keep worktree, `status=parked`, phase=rebase; respond `fix` (fixer) / `abort`. Tests with a synthetic conflict. Fail-closed if abort itself fails.
3. [x] **`porch rerun [--run-id]`:** enqueue a new run from the same branch tip (or recorded SHA); do not silently reuse a half-applied worktree.
4. [x] **Sync / custody:** `porch agent sync` (JSON) + TUI hint when pipeline HEAD is ahead of the author’s branch; `git fetch porch` instructions; recover unpublished pipeline commits if already recorded. No rewrite of `origin`.
5. [x] **Cold worktree PATH:** daemon inherit / config `tools` from setup; document; test that certify sees `biome` when recorded in config even if the daemon was started with a thin PATH.
6. [x] Lefthook note in klynt docs: `git push --no-verify porch` is expected (porch is not lefthook).

### M14 — Agent-driven loop (skill + `agent run` + intent) (done)

**Goal:** a coding agent can drive the same gate as a human, in JSON.

Work:

1. [x] `porch agent run` — ensure daemon, optionally wait until parked or terminal; JSON snapshot stream or poll.
2. [x] Skill: `/porch` → `porch agent run|status|respond|sync`; stop at park or completed; **never** merge; **never** babysit deploy.
3. [x] `--intent` on notify/`agent run` written to `PORCH_INTENT` / run row; `init --intent` prints a tip only (E17: empty skips, does not fail).
4. [x] PR body: Intent, What Changed (short), Review/Certify summary already sketched — fill from run artifacts.
5. [x] Unattended: `porch agent respond fix --yes` remains one round; no default yolo on the whole gate.

### M15 — Workflow dogfood freeze (done)

**Goal:** klynt + a toy repo complete the loop **without OCR**: setup → push porch → agent review → park TUI or JSON → certify cheap → PR to `dev`. Mailgate second.

- [x] Operator checklist: [`10-operator-checklist.md`](10-operator-checklist.md)
- [x] Toy loop (temp `PORCH_HOME`, `engine=generic` coverage fake, not OCR) → completed
- [x] Klynt loop (`engine=agent` + coverage-aware PATH fake reviewer; `git push --no-verify porch`; PR base `dev`; empty `watch_checks`) → completed (do not merge dogfood PR)
- [x] Papercuts only (init tip agent-aware; certify default timeout 600s; deliver `--no-verify`; certify stderr head+tail truncate; biome pin note)
- [ ] Mailgate second — **skipped**: no `.porch.yaml` on mailgate default branch; land canonical example via a normal mailgate change first (see checklist)

Work: operator checklist, fix only papercuts found in dogfood, no new subsystems. Tagged **0.2.0** (git/tag install; see [install.md](install.md)).

**Then stop adding workflow features.**

### M16 — Review quality engine (done; last)

**Goal:** review that is actually good — coverage, grouping, line anchors, language rules, precision bias. **Porch-owned.** Ideas may come from constrained review CLIs (file selection, DiffMap, relocate, coverage denominator, “no shell / no edit”). **Do not** wrap, fork, or vendor that product (D13). Do not start this slice because M10 agent review “feels weak” mid-workflow — wait until M15.

- [x] Brief: [`docs/11-review-quality-brief.md`](11-review-quality-brief.md) + fixtures under `tests/fixtures/quality/`
- [x] Engine crate/binary `porch-quality` — M3 argv `--from/--to/--format json --output`; `EngineKind::Quality` setup/doctor
- [x] Coverage manifest required; skip + reason (lockfile/binary); fail closed if missing
- [x] Line relocate on drifted hunks (precision bias: drop ambiguous/unanchored)
- [x] Language/rule packs as YAML data (generic / rust / js); grouping by lang/dir
- [x] Session-free, no shell, no edits; fixer unchanged
- [x] Toy-repo binary smoke + setup wiring tests; klynt/mailgate live dogfood optional/operator-gated (mailgate still blocked on trusted yaml)

## Explicitly later / never

| Later | Never (as porch) |
|---|---|
| Socket activation, APFS clone | Replace mailgate/klynt CI/CD or E2E |
| GitLab/Gitea | libgit2 gate operations |
| More native agents (beyond D8) | Nine adapters day-1 |
| Evidence branch publish | Contributor `no_ci` |
| Document phase as a sixth pipeline step | Auto-fix review default on |
| Transcript intent inference | Merge bot |
| M16 review quality engine | Composing/vendoring a third-party review CLI as the engine |
| crates.io slice publish | Wizard replacing `git push porch` as consent |

## First coding session checklist

1. Read `docs/decisions.md` and this file.
2. Scaffold `Cargo.toml` + `src/main.rs` clap + `src/git.rs` stub.
3. Do not implement review in the first PR.
4. Keep the tree English, `rustfmt`, clippy `-D warnings`.
