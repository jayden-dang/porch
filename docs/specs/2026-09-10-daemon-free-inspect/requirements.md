# Requirements: Daemon-free inspect

Feature code: LOOK
Status: Implemented
Date: 2026-09-10

Roadmap item: ROAD-10 (MILE-4 — Escape without the daemon), second wave. Completes
GOAL-4's inspect clause: when automatic reconciliation cannot complete, the operator
can inspect state without a healthy daemon and without hand-editing the database.

Respects: ARCH-1, ARCH-2, ARCH-10, ARCH-11, ARCH-13.

ESCAPE shipped custody and detach. DFAULT shipped bounded RPC and named **Daemon
condition**s. This wave owns the remaining inspect surface. It does not implement
ROAD-24 purge, does not decide `--force` SIGKILL, and does not make the daemon not
wedge.

Framing and an independent advisor agreed that `porch status` is the inspect
command, that inspect must not spawn a daemon, and that `Db::open` cannot be an
inspect primitive. They disagreed on the forward source: framing would project only
stored `forward_reconciliations` rows; the advisor required a read-only reuse of
`classify` over the **Forward record** when no verdict has been written yet, because
that is the SIGKILL window GOAL-4 names. Design.md records the choice: stored row
wins; otherwise derive and never write.

Vocabulary is locked in `CONTEXT.md`. These criteria use **Forward verdict**,
**Forward record**, **Forward reconciliation**, **Daemon condition**, **Custody**,
**Eject**, **Run**, **Phase attempt**. They do not redefine them.

## 1. Inspect does not start the gate it is asked to report on

**Story:** As an operator whose daemon is dead, wedged, or refusing, I want
`porch status` to tell me that without starting a new daemon, so that asking does
not change the answer.

- **LOOK-1.1** `porch status` and `porch runs` SHALL NOT call `ensure_daemon` or
  otherwise spawn a daemon.
- **LOOK-1.2** WHEN those commands run against **Daemon condition** `unreachable`,
  `refusing`, or `not-answering` THE SYSTEM SHALL terminate within the existing RPC
  bound, leave the daemon pid unchanged, and SHALL NOT create a `state.sqlite` that
  was absent.
- **LOOK-1.3** `porch status` SHALL report the **Daemon condition** with the same
  labels `porch daemon status` and `porch doctor` already use. `--json` SHALL add
  `condition` additively. `daemon_healthy` SHALL remain as `condition == ready` so
  existing parsers keep working.
- **LOOK-1.4** Work commands (`porch rerun`, `porch agent run`, TTY `porch attach`
  / TTY bare `porch`) MAY still spawn. This wave does not close DFAULT-7.3 for those.
- **LOOK-1.5** Non-TTY bare `porch` that only prints a run summary SHALL NOT spawn.
  TTY attach behaviour is unchanged.

## 2. A look at the state root cannot mutate it

**Story:** As an operator inspecting a wedged gate, I want opening the database
to be incapable of creating a file, migrating schema, or terminalizing active
**Run**s, so that a look is not a write.

- **LOOK-2.1** THE SYSTEM SHALL provide `Db::open_read` that opens an existing
  `state.sqlite` with `SQLITE_OPEN_READ_ONLY` (no `CREATE`), does not
  `create_dir_all`, does not set `journal_mode`, does not run DDL,
  `rounds::migrate`, `install_writer_fence`, or `fail_forward_active_runs_for_protocol_upgrade`,
  and does not `reject_stale_writer` as a hard fail.
- **LOOK-2.2** `PRAGMA query_only=ON` after `Db::open` SHALL NOT count as
  `open_read`. The writer fence would already have run.
- **LOOK-2.3** `open_read` SHALL use a short `busy_timeout`. `SQLITE_BUSY` is an
  unreadable database arm, not a hang. Inspect SHALL still report **Daemon
  condition** and git **Custody** tips when the DB arm fails.
- **LOOK-2.4** `porch agent status` and `porch agent sync` (the inspect half) SHALL
  use `open_read`. `porch agent respond` SHALL keep `Db::open` because it writes.
- **LOOK-2.5** ROAD-24's purge manifest SHALL be able to call `open_read`. This
  wave SHALL NOT implement purge, `--abandon`, or the refusal predicate.

## 3. One join of three sources, each legal with the others down

**Story:** As an operator whose reconciliation did not finish, I want one
command to show the daemon's condition, every reachable porch-authored tip, and
what the **Forward record** proves about `origin`, without needing a restart to
manufacture those facts.

- **LOOK-3.1** `porch status` SHALL join, fail-soft:
  1. **Daemon condition** (no daemon, no DB);
  2. **Custody** tips via `git for-each-ref` of `refs/porch/recover` on the
     layout-derived bare, plus leftover worktree HEADs under
     `$PORCH_HOME/worktrees/<repo_id>/` (ADR-0005 tips a declined pin left behind);
  3. **Forward record** ± stored **Forward verdict**.
- **LOOK-3.2** Repo identity for that join SHALL be eject's resolver
  (`porch.repo-id`, then the `porch` remote path), not `repo_id_for`. The bare
  path SHALL be `$PORCH_HOME/repos/<repo_id>.git` (`ESCAPE-2.2`). Respects: ARCH-2.
- **LOOK-3.3** WHEN a stored **Forward verdict** exists for a `deliver` **Phase
  attempt** THE SYSTEM SHALL project that row (latest `seq`) and SHALL NOT
  re-classify in a way that can disagree with it.
- **LOOK-3.4** WHEN no verdict row exists, the attempt has no `terminal` phase
  event, and a **Forward record** is present THE SYSTEM SHALL call `classify` as a
  **read** over those records plus the optional local tracking SHA
  (`observed_tracking_sha` — no network). THE SYSTEM SHALL NOT call
  `append_verdict_tx`. The projection SHALL mark the result `source: derived`
  (human: awaiting reconciliation). Respects: ARCH-13.
- **LOOK-3.5** Inspect SHALL NOT assert that a pull request is absent. A
  `reached_origin` projection MAY say pull-request state is `unrecorded`. Human
  copy SHALL reuse `operator_note`. `not_attempted` SHALL still appear as a typed
  verdict when `operator_note` is `None`.
- **LOOK-3.6** An unknown `verdict` string SHALL be reported as unknown, not
  skipped (`Verdict::parse` already fail-closed).
- **LOOK-3.7** WHEN repo identity cannot be resolved (post-**Eject** checkout)
  THE SYSTEM SHALL still report **Daemon condition** and SHALL say why custody
  and forward arms are absent, without scanning every `repos/*.git`.

## 4. Surfaces

**Story:** As an operator who already runs `porch status`, I do not want a
second inspect verb, and I do not want `doctor` or `agent sync` to become catalogs.

- **LOOK-4.1** THE SYSTEM SHALL NOT add `porch inspect`.
- **LOOK-4.2** `porch doctor` SHALL add a read-only DB-readable check and, when
  cwd is a porch clone, a **count** of recover refs. It SHALL NOT dump verdicts or
  become the catalog.
- **LOOK-4.3** `porch runs` SHALL use the same `open_read` join as `porch status`,
  keep JSON, and add a structured `forward` object additively. It SHALL NOT spawn.
- **LOOK-4.4** `porch agent sync` SHALL keep its one-run JSON contract
  (`state`, `relation`, `fetch_hint`, `recoverable`). `--recover` remains a
  mutation of the operator's local branch and SHALL NOT rewrite `origin`
  (ARCH-1). Sync SHALL switch its DB open to `open_read` and resolve the repo
  through eject's resolver. It SHALL NOT become the recover-ref catalog.
- **LOOK-4.5** `porch audit` / `get_audit` remain daemon RPC. This wave does not
  build a daemon-free **Audit document**.

## 5. Verification

- **LOOK-5.1** Store test: `open_read` on a missing file fails and does not create
  the file; a writing `open` in the same process is a different constructor.
- **LOOK-5.2** Store test: `open_read` against a protocol-skewed fixture leaves
  `min_writer_protocol` and `runs.status` untouched.
- **LOOK-5.3** `porch status` / `porch runs` against `unreachable`, `refusing`,
  and `not-answering`: terminate within the RPC bound, pid unchanged, no new
  daemon, no new `state.sqlite` if absent.
- **LOOK-5.4** Intent-only **Forward record**, daemon dead, no verdict row: human
  and JSON say undetermined / awaiting reconciliation, reuse `operator_note`
  wording, never “no pull request”.
- **LOOK-5.5** After a real restart, the same command shows the **stored** verdict.
- **LOOK-5.6** Two recover refs on one branch, latest run is not the pinned one:
  `porch status` lists both; `porch agent sync` without `--run-id` still shows
  one.
- **LOOK-5.7** `porch.repo-id` ≠ path hash: inspect hits the configured bare.
- **LOOK-5.8** `open_read` cannot `INSERT`.

## Out of scope

- ROAD-24 purge, `--abandon`, active-run interlock, `Purged`-on-failure repair.
- `porch daemon stop --force` escalating to SIGKILL. LOOK unblocks that decision
  by showing an intent-only forward *before* the next start writes a verdict; it
  does not take the decision.
- Making the daemon not wedge (`DFAULT-7.1`).
- Recover-ref sweep / custody lifetime (ESCAPE F.1).
- `repo_id_for` vs `porch.repo-id` at every remaining call site (ADR-0010 / MILE-8).
- A fifth **Daemon condition**.
- Re-running classification as a writer, probing `origin`, or treating a missing
  tracking ref as proof a push failed.
