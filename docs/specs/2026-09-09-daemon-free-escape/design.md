# Design — custody preservation and daemon-free detach

**Feature code:** ESCAPE
**Roadmap item:** ROAD-10 (MILE-4), first wave
**Requirements:** [requirements.md](requirements.md)
**Respects:** ARCH-1, ARCH-2, ARCH-11, ARCH-13

## Invariant citations

- **ARCH-1** — `origin` is never rewritten. Nothing in this wave touches `origin`. The
  pin writes a ref on porch's own bare; detach edits the operator's local git config and
  the bare's hooks. Purge deletes only porch's own state root subtree.
- **ARCH-2** — every ref and worktree operation goes through the git CLI via
  `porch-git`. The pin is `update-ref`; worktree removal is `worktree remove --force`.
  No gitoxide, no direct `.git` writes.
- **ARCH-11** — assurance outcomes are porch's alone. Nothing here derives, writes, or
  reinterprets a review disposition or a forward verdict. Purge deletes verdict rows as
  part of removing a repo, which is not an assurance judgment.
- **ARCH-13** — nothing here forwards to an external system, so there is no
  authorization to bind. Detach removes the mechanism that could forward.

## 1. One owner for worktree custody

The pin lived in `porch-run/src/sync.rs` and was called from one wrapper,
`finish_remove_worktree`, in `porch-run/src/lib.rs`. But `porch-gate`'s daemon also
removes worktrees — on the superseding-push path — and `porch-gate` cannot depend on
`porch-run` (the dependency runs the other way). So the daemon reached for
`porch_git::worktree_remove_force` directly and skipped the pin. That is a layering
consequence, not an oversight that could be fixed locally: any fix that left the pin in
`porch-run` would have had to duplicate it.

Custody moves to **`crates/porch-gate/src/custody.rs`**, which owns three things:

- `recovery_ref_name(run_id)` — the `refs/porch/recover/<run_id>` name.
- `pin_recovery_if_needed(bare, run, wt)` — the pin.
- `finish_remove_worktree(bare, run, wt)` — pin-then-remove, fail-closed.

`porch-run` keeps its public `sync::recovery_ref_name` and its private
`finish_remove_worktree` as one-line delegations, so no caller changes and the published
API is unchanged. The daemon calls `custody::finish_remove_worktree` for superseded
parked runs.

This is what `ESCAPE-1.5` buys: there is no longer a way to remove a run worktree
without going through the pin, because the only function that removes one also pins.

### Why the ancestry gate had to go, not be repaired

The old pin was:

```rust
match porch_git::is_ancestor(wt, &run.sha, &head) {
    Ok(true) => { /* pin */ }
    Ok(false) => Ok(()),   // "nothing worth pinning"
    Err(e) => Err(e.to_string()),
}
```

`run.sha` is the SHA the operator pushed. It is written once by `insert_run` and never
updated — `set_run_shas` writes `head_sha` and `base_sha`, and the rebase path calls
exactly that. So after a rebase that moved commits, `run.sha` is not an ancestor of HEAD,
the gate takes the `Ok(false)` arm, and `finish_remove_worktree` proceeds to delete the
worktree. Every certify correction commit and fixer commit on the rebased chain goes with
it.

Repairing the gate by comparing against `head_sha` instead would narrow the hole without
closing it: `head_sha` tracks the pipeline tip, so it equals HEAD by the time teardown
runs, and the gate would then decline *always*.

The gate cannot be made correct because its premise is wrong. It asks "is this HEAD
descended from what was pushed?" when the question is "does this HEAD exist anywhere
else?" The new rule answers the real question with the only cheap, total test available:
pin whenever HEAD differs from the pushed SHA. A run worktree is created detached at the
pushed SHA and only porch commits into it, so a HEAD that differs is always porch's own
product and always worth a ref. The cost is one `update-ref`; the ancestry check it
replaces was itself a git invocation, so the pin is not more expensive than before.

Consequence accepted: recovery refs now accumulate on rebased runs that previously left
none. `refs/porch/recover/*` is already never swept — `retention::sweep_unreferenced`
handles only `refs/porch/config/*` — so this makes an existing retention gap larger. It
is recorded in tasks as follow-up rather than fixed here, because sweeping recovery refs
is a custody-lifetime decision and this wave's job is to stop losing commits.

## 2. Detach before, and independently of, the database

Old order: canonicalize, read `porch.repo-id`, **open the database**, **refuse if no
row**, remove remote, unset config, neutralize hooks, purge.

Two failures follow from that order. The database open is a *writing* open — it creates
tables, migrates, and runs `install_writer_fence` in an immediate transaction — so it can
block on a wedged daemon's write lock, and it hard-refuses when the state root's writer
protocol is newer than the binary. Either way the operator gets zero detachment: the
remote and hooks are still live. And the two refusals fire before any mutation, so a
checkout whose row was purged by hand cannot be detached by the documented command at
all.

New order: canonicalize, resolve the repo id, **derive** the bare path, remove remote,
unset config, neutralize hooks, then optionally purge.

- **Bare path by derivation.** `init` writes the bare to
  `$PORCH_HOME/repos/<repo_id>.git` and nothing moves it, so the path is a function of
  the home and the id. Reading it from `repos.bare_path` was the only reason detach
  needed the database.
- **Repo id with a fallback.** `porch.repo-id` first; failing that, the `porch` remote's
  URL, whose last path component is `<repo_id>.git`. This is precisely the state an
  interrupted eject leaves — the unset ran, the remote removal did not — and it is why a
  retry now succeeds instead of reporting "not initialized". Only when both are absent
  does eject error, and then it says "not a porch clone" rather than "run `porch init`",
  because the checkout may never have been one.
- **Purge downgrades, detach does not fail.** Detach has already happened by the time
  purge runs and must not be half-undone, so `purge_or_report` converts every failure
  into `GateState::LeftBehind(reason)` and eject still returns `Ok`. The CLI exits
  non-zero on that variant, so a script notices, but the checkout is detached either way.

### The reported outcome

`EjectResult` gains a `gate_state: GateState` with three variants — `Preserved`,
`Purged`, `LeftBehind(String)` — and keeps `purged: bool` as `gate_state == Purged` so
existing callers and tests are unaffected. The variants exist so that the CLI cannot say
"purged" about state it did not purge (`ESCAPE-4.3`); previously `purged` was simply the
value of the `--purge` flag, which was true even on paths that removed nothing.

## 3. Purge completes, or changes nothing on disk

`Db::delete_repo` pre-deleted `step_results` and `uncertified_pipeline_ranges`, then
deleted `runs`. Those two pre-deletes are the tell: the author was hand-discharging the
non-cascading references that existed at the time. Five more arrived later —
`authority_events`, `phase_attempts`, `phase_events` (M18/M21), `forward_records`
(ROAD-7), `forward_reconciliations` (ROAD-8) — each declaring `REFERENCES runs(id)` with
no `ON DELETE CASCADE`, and `foreign_keys` is `ON`. Every run since M21 writes
`phase_attempts` and `phase_events`, so `DELETE FROM runs` violates an immediate foreign
key on any repo that has run a gate pass.

Adding `ON DELETE CASCADE` retroactively is not available: SQLite cannot alter a foreign
key in place, and the tables are load-bearing across four milestones' worth of state.

So the delete is explicit, in one transaction, with **`PRAGMA defer_foreign_keys = ON`**.
Ordering alone is not sufficient here, and the reason is worth stating because it is the
kind of thing a later reader would try to simplify away:

- `authority_events.authority_event_id` and `phase_attempts.parent_attempt_id` /
  `caused_by_attempt_id` reference their own tables. SQLite checks immediate foreign keys
  after each row within a statement, so a single `DELETE` over a self-referencing set can
  fail mid-statement depending on row order.
- `authority_event_members.finding_instance_id` references `finding_instances`, which has
  no direct link to `runs` — it is reachable only by cascade from `review_rounds`. So the
  cascade fired by deleting `runs` can orphan a row in a table the delete never named.

Deferring moves enforcement to `COMMIT`, where the final state is consistent. It is not a
weakening: a genuinely dangling reference still aborts the commit. The explicit ordering
is kept anyway, children first, so the statement list documents the graph.

Purge is also reordered so the transactional step runs first: read the run list, delete
the rows, *then* remove worktrees, artifacts, and the bare. A purge that cannot complete
now leaves the filesystem untouched, which is what makes `ESCAPE-3.3` true and what makes
the retry in `ESCAPE-2.5` meaningful.

## 4. What this design does not do

No refusal, no `--abandon`, no dry-run manifest. Those are the blocker's to decide, and
`requirements.md` §5 records the recommendation carried forward. This wave deliberately
makes the destructive path *correct and ordered* first, because a refusal layered on a
path that mutates before it can fail — and then cannot complete — protects nothing.

No inspect surface, and no projection of ROAD-8's `indeterminate` verdict. That verdict
currently reaches an operator only as prose appended to `runs.error`, and neither
`sync.rs` nor `doctor.rs` knows forward verdicts exist. It is the MILE-3 → MILE-4 handoff
and the next wave's centerpiece.

## Surface drift

ROAD-10's declared surfaces are `crates/porch-gate/src/eject.rs`,
`crates/porch-run/src/sync.rs`, `crates/porch/src/doctor.rs`. This wave touches
`eject.rs` and `sync.rs`, and additionally `crates/porch-gate/src/custody.rs` (new),
`crates/porch-gate/src/daemon.rs`, `crates/porch-gate/src/db.rs`,
`crates/porch-run/src/lib.rs`, and `crates/porch/src/main.rs`. It does not touch
`doctor.rs`, which belongs to the inspect wave.

The drift is inherent rather than incidental: the custody defect is in the daemon's
sweep path and the purge defect is in the store, and neither is reachable from the three
declared files. Recorded here in the manner ROAD-9's drift was recorded, rather than
absorbed silently.
