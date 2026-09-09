# Requirements — custody preservation and daemon-free detach

**Feature code:** ESCAPE
**Roadmap item:** ROAD-10 (MILE-4), first wave
**Goals:** GOAL-4
**Status:** Implemented

## Context

**GOAL-4** promises that when automatic reconciliation cannot complete, the operator can
inspect state, recover every reachable porch-authored commit, and detach porch from the
checkout — without a healthy daemon, and without hand-editing hooks, git config, refs,
or the database.

Framing ROAD-10 found that the recover-and-detach half of that promise is not merely
unimplemented: there are shipped paths that violate its literal wording.

- Porch **destroys porch-authored commits on two ordinary paths**, with no `eject` and
  no operator intent to discard. The recovery pin declines silently after any rebase
  that moved commits, and a superseding push force-removes a parked run's worktree with
  no pin attempted at all.
- `porch eject --purge` **cannot complete** on any repo that has run a gate pass, and it
  removes the remote and neutralizes hooks *before* it reaches the step that fails —
  leaving a checkout that is neither attached nor detached and from which the documented
  command cannot be retried.

This feature owns those two defects and the ordering that makes detach survivable. It is
the first wave of ROAD-10, scoped to what needs no policy decision.

It does **not** own the refusal and explicit-abandon policy for `eject --purge`. That is
MILE-4's named blocker, owned by Jayden, due before the milestone is approved for
implementation (`docs/roadmap/INDEX.md`). Section 5 records the recommendation framing
produced for ratification and states what this wave deliberately did not decide. MILE-4
stays `Planned`.

## 1. Porch keeps custody of the commits it authored

**Story:** As an operator whose run failed, I want the commits porch wrote on my behalf
to still exist afterwards, so that a failed run costs me a retry rather than my work.

Porch authors commits the operator does not have: certify's correction commits
(`porch: apply format`, `porch: apply lint`) and fixer commits. They are created in a
disposable worktree under `$PORCH_HOME/worktrees/`, and the only durable name porch ever
gives them is `refs/porch/recover/<run_id>` on the bare.

- **ESCAPE-1.1** WHEN porch removes a disposable run worktree THE SYSTEM SHALL first pin
  the worktree's HEAD under `refs/porch/recover/<run_id>` on the bare, unless that HEAD
  is the SHA the operator pushed.
- **ESCAPE-1.2** THE SYSTEM SHALL NOT condition the pin on the pushed SHA being an
  ancestor of the worktree HEAD. The pushed SHA is recorded once at admission and is
  never rewritten, so a rebase that moved commits makes it a non-ancestor of every
  commit porch subsequently authored — which is exactly when the pin is load-bearing.
- **ESCAPE-1.3** WHEN a pin fails THE SYSTEM SHALL keep the worktree and SHALL NOT remove
  it, so that the commits remain reachable from the worktree's own HEAD.
- **ESCAPE-1.4** WHEN a new push supersedes a parked run THE SYSTEM SHALL pin that run's
  worktree HEAD before removing the worktree, on the same terms as `ESCAPE-1.1`.
- **ESCAPE-1.5** THE SYSTEM SHALL have exactly one implementation of the pin-then-remove
  sequence, reachable from every path that removes a run worktree.

## 2. Detach does not depend on the state it is escaping

**Story:** As an operator whose gate is wedged or whose state root a newer binary
touched, I want `porch eject` to detach my checkout anyway, so that the escape hatch is
not locked by the failure I am escaping.

`porch eject` opens the database as its second act and refuses outright when the repo row
is missing or the open fails. `Db::open` is a writing open: it migrates, raises
`min_writer_protocol`, and can reject an older binary. So the detach path inherits every
failure mode of the state root.

- **ESCAPE-2.1** THE SYSTEM SHALL remove the `porch` remote, unset `porch.repo-id`, and
  neutralize the bare's `pre-receive` and `post-receive` hooks without opening the
  database.
- **ESCAPE-2.2** THE SYSTEM SHALL derive this repo's bare path from `$PORCH_HOME` and the
  repo id rather than from the database row, because the layout is deterministic
  (`$PORCH_HOME/repos/<repo_id>.git`) and the row may be unreadable.
- **ESCAPE-2.3** WHEN the database cannot be opened, or holds no row for this repo,
  THE SYSTEM SHALL still detach, SHALL report that it detached, and SHALL report that
  gate state was left behind and why.
- **ESCAPE-2.4** THE SYSTEM SHALL detach when `porch.repo-id` is unset but the checkout
  still has a `porch` remote, deriving the repo id from the remote's path. An operator
  whose previous eject was interrupted after the unset must not be told the checkout was
  never initialized.
- **ESCAPE-2.5** Detaching SHALL be idempotent: running it again from any intermediate
  state SHALL succeed and SHALL NOT report a failure that requires hand-editing git
  config.
- **ESCAPE-2.6** Detach without `--purge` SHALL continue to preserve the database, the
  bare repository, recovery refs, and custody evidence, per `CONTEXT.md`.

## 3. Purge completes or changes nothing

**Story:** As an operator detaching for good, I want `--purge` to either finish or leave
my checkout exactly as it was, so that I am never left half-attached.

`Db::delete_repo` deletes `runs` rows while five tables added in M18, M21, ROAD-7, and
ROAD-8 declare `REFERENCES runs(id)` with no `ON DELETE CASCADE`, and `foreign_keys` is
`ON`. Every run since M21 writes `phase_attempts` and `phase_events` rows, so the delete
violates an immediate foreign key on any repo that has run a gate pass.

- **ESCAPE-3.1** WHEN deleting a repo's rows THE SYSTEM SHALL delete every row that
  references that repo's runs, in an order that satisfies the declared foreign keys, in
  one transaction.
- **ESCAPE-3.2** `porch eject --purge` on a repo with at least one completed run SHALL
  succeed and SHALL leave no rows for that repo in `runs`, `repos`, `step_results`,
  `uncertified_pipeline_ranges`, `review_rounds`, `authority_events`, `phase_attempts`,
  `phase_events`, `forward_records`, or `forward_reconciliations`.
- **ESCAPE-3.3** WHEN row deletion fails THE SYSTEM SHALL NOT have destroyed the bare
  repository, and SHALL report which state was left behind.
- **ESCAPE-3.4** THE SYSTEM SHALL order purge so that the destructive filesystem steps
  run after the recoverable ones, and SHALL report the outcome of each.

## 4. The operator is told which of these states they are in

**Story:** As an operator, I want the words porch prints to distinguish "detached and
gate state removed" from "detached, gate state left behind", so that I know whether
anything remains to clean up.

- **ESCAPE-4.1** THE SYSTEM SHALL report a distinct, named outcome for each of:
  detached with gate state preserved; detached with this repo's gate state purged;
  detached with gate state left behind because it was unreadable.
- **ESCAPE-4.2** WHEN gate state is left behind THE SYSTEM SHALL name the bare path and
  the reason, so that the operator can act without inspecting `$PORCH_HOME` themselves.
- **ESCAPE-4.3** THE SYSTEM SHALL NOT claim to have purged state it did not purge.

## 5. What this wave did not decide

- **ESCAPE-5.1** THE SYSTEM SHALL NOT add a refusal to `eject --purge`. Purge remains
  the lossy door that `CONTEXT.md` already declares outside GOAL-4's no-loss guarantee.
  A refusal predicate, an `--abandon` override, and a dry-run manifest are all held for
  the blocker's ratification.
- **ESCAPE-5.2** This wave SHALL NOT move MILE-4 from `Planned` to `Committed`.

The framing recommendation carried forward for ratification, recorded here so the
decision is made once and in the open:

- **Predicate:** refuse when a porch-authored commit is neither reachable from the
  operator's checkout nor recorded as having reached `origin`. Discriminated against
  "refuse on any unreachable commit", which fires on the benign post-success case — a
  correction commit that is on `origin` and absent from the checkout — and would
  therefore refuse nearly always, training the override and protecting nothing.
- **Shape:** a dry-run manifest by default, `--abandon` as the single explicit override,
  and the abandoned SHAs and run ids written outside the deleted tree before deletion.
- **Reading of `CONTEXT.md`:** purge stays outside GOAL-4's no-loss guarantee; a refusal
  exists to make the loss *chosen*, not to extend the guarantee over purge.

This wave is a prerequisite for that policy either way: a refusal layered on a path that
mutates before it can fail, and then cannot complete, is worse than no refusal.

## Out of scope

- The daemon-free **inspect** surface, and projecting ROAD-8's `indeterminate` verdict
  into an operator-facing command. Framing found that verdict reaches an operator only
  as prose appended to `runs.error`, and that no surface ROAD-10 names knows forward
  verdicts exist. That is the MILE-3 → MILE-4 handoff and it is the next wave.
- The `rpc_call` read timeout that would make a wedged daemon a reported state rather
  than a hang. Needed by the inspect wave; not needed to detach, which never opens a
  socket.
- `porch status` spawning a daemon it was asked to report on. A real defect against
  GOAL-4, but on the inspect surface, not this one.
- Making the daemon not wedge, and any change to `reconcile_stale` / `recover_stale`
  semantics.
- Multi-repo or global purge. `--purge` stays scoped to this repo.
- The MILE-3 blocker on whether an approval may outlive the reviewed SHA.

## Verification

- Store test: a repo with a completed run, a phase attempt, phase events, a forward
  record, and a reconciliation row purges cleanly (`ESCAPE-3.1`, `ESCAPE-3.2`).
- Store test: the pin fires when the worktree HEAD does not descend from the pushed SHA
  (`ESCAPE-1.1`, `ESCAPE-1.2`).
- Store test: a superseded parked run's HEAD is pinned before its worktree is swept
  (`ESCAPE-1.4`).
- Unit test: detach succeeds against a home with no database at all (`ESCAPE-2.1`,
  `ESCAPE-2.3`), and against a checkout whose `porch.repo-id` was already unset
  (`ESCAPE-2.4`, `ESCAPE-2.5`).
- Unit test: the reported outcome distinguishes preserved, purged, and left-behind
  (`ESCAPE-4.1`, `ESCAPE-4.3`).
