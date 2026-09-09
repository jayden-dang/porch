# Tasks — custody preservation and daemon-free detach

**Feature code:** ESCAPE
**Roadmap item:** ROAD-10 (MILE-4), first wave
**Requirements:** `requirements.md` · **Design:** `design.md`

Waves are ordered by dependency. Each wave ends green on
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace`.

## Wave 1 — custody has one owner

- [x] **1.1** Add `crates/porch-gate/src/custody.rs` with `recovery_ref_name`,
  `pin_recovery_if_needed`, and `finish_remove_worktree`; export all three.
  *(ESCAPE-1.5)*
- [x] **1.2** Drop the ancestry gate: pin whenever the worktree HEAD differs from the
  pushed SHA. *(ESCAPE-1.1, ESCAPE-1.2)*
- [x] **1.3** Delegate `porch-run`'s `sync::recovery_ref_name` and
  `finish_remove_worktree` to `porch-gate`, keeping the published API unchanged.
  *(ESCAPE-1.5)*
- [x] **1.4** Route the daemon's superseded-parked-run sweep through
  `custody::finish_remove_worktree`, carrying the whole `RunRow` so the pin has the
  pushed SHA. *(ESCAPE-1.4)*

## Wave 2 — purge completes or changes nothing

- [x] **2.1** Delete every row referencing this repo's runs in `Db::delete_repo`, in one
  transaction, children first, with `defer_foreign_keys` for the self-referencing and
  cascade-reachable cases. *(ESCAPE-3.1, ESCAPE-3.2)*
- [x] **2.2** Reorder `purge_repo_state` so the transactional row delete precedes every
  destructive filesystem step. *(ESCAPE-3.3, ESCAPE-3.4)*

## Wave 3 — detach survives the state it escapes

- [x] **3.1** Derive the bare path from `$PORCH_HOME` and the repo id; stop reading
  `repos.bare_path` on the detach path. *(ESCAPE-2.2)*
- [x] **3.2** Remove the database open and both refusals from the detach path.
  *(ESCAPE-2.1, ESCAPE-2.3)*
- [x] **3.3** Fall back to the `porch` remote's URL when `porch.repo-id` is already
  unset. *(ESCAPE-2.4, ESCAPE-2.5)*
- [x] **3.4** Add `GateState` and report `Preserved` / `Purged` / `LeftBehind(reason)`;
  derive `purged` from it rather than from the flag. *(ESCAPE-4.1, ESCAPE-4.2,
  ESCAPE-4.3)*
- [x] **3.5** Exit non-zero on `LeftBehind` so a script notices, while still reporting
  the checkout as detached. *(ESCAPE-4.2)*

## Wave 4 — proof

- [x] **4.1** Store test: `delete_repo` succeeds on a repo carrying a run, a phase
  attempt, phase events, an authority event, a forward record, and a reconciliation row;
  assert every table is empty for that repo afterwards. This is the regression test for
  the shipped defect and it fails before wave 2. *(ESCAPE-3.1, ESCAPE-3.2)*
- [x] **4.2** Store test: the pin fires when the worktree HEAD does not descend from the
  pushed SHA — the rebase case the old gate declined. *(ESCAPE-1.1, ESCAPE-1.2)*
- [x] **4.3** Store test: a failed pin keeps the worktree. *(ESCAPE-1.3)*
- [x] **4.4** Integration test: a parked run superseded by a second push to the same
  branch has its HEAD pinned before the worktree is swept. *(ESCAPE-1.4)*
- [x] **4.5** Unit test: detach succeeds against a home with no database at all, and
  reports `LeftBehind` when asked to purge. *(ESCAPE-2.1, ESCAPE-2.3, ESCAPE-4.1)*
- [x] **4.6** Unit test: detach succeeds when `porch.repo-id` is already unset but the
  `porch` remote remains, and a second eject from the fully detached state still
  succeeds. *(ESCAPE-2.4, ESCAPE-2.5)*
- [x] **4.7** Unit test: eject errors only when the checkout is not a porch clone at all.
  *(ESCAPE-2.4)*
- [x] **4.8** Unit test: purge that cannot delete rows leaves the bare intact.
  *(ESCAPE-3.3)*

## Wave 5 — the record

- [x] **5.1** Register `ESCAPE` in `docs/specs/catalog/`.
- [x] **5.2** Correct `docs/usage.md`: `--purge` carries the hazard note `CONTEXT.md`
  already implies, and eject's three outcomes are documented.
- [x] **5.3** Update `CONTEXT.md`'s **Eject** entry so "escape hatch" is true of the
  code — detach no longer requires the database.
- [x] **5.4** Record on the roadmap that ROAD-10's first wave shipped, that MILE-4 stays
  `Planned`, and carry the purge-policy recommendation onto the blocker so the owner
  ratifies once, in the open.

## Follow-up, not this wave

- **F.1** `refs/porch/recover/*` is never swept. `retention::sweep_unreferenced` handles
  only `refs/porch/config/*`. Wave 1 makes recovery refs accumulate on rebased runs that
  previously left none, so the existing gap is now larger. Sweeping them is a
  custody-lifetime decision — how long does porch owe the operator a commit it authored?
  — and belongs with the inspect wave, which is where custody becomes visible.
- **F.2** The daemon-free **inspect** surface, and projecting ROAD-8's `indeterminate`
  verdict into it. Framing found the verdict reaches an operator only as prose appended
  to `runs.error`, and that no surface ROAD-10 names knows forward verdicts exist.
- **F.3** `rpc_call` sets no read timeout, so a daemon wedged on the database mutex makes
  every RPC client hang — and `health` answers without touching the database, so
  `porch status` reports `daemon_healthy=true` while nothing can make progress. Needed
  before *wedged* can be a reported state.
- **F.4** `porch status` spawns a daemon before printing a health value it read *before*
  spawning. An inspection command that mutates the state it reports on is a defect
  against GOAL-4, on the inspect surface.
- **F.5** `porch doctor` tells the operator the daemon is "started on `porch init` /
  first push notify". The push hook execs `porch daemon notify-push`, which does a
  best-effort RPC and logs a warning when it fails; it does not spawn. The advice
  misdirects in exactly the scenario MILE-4 serves.
- **F.6** The purge refusal and explicit-abandon policy — MILE-4's blocker, owned by
  Jayden. Recommendation recorded in `requirements.md` §5.
- **F.7** Porch's machine-authored commits inherit the operator's commit-signing
  configuration. `certify.rs` and the deliver-repair path both commit with
  `-c core.hooksPath=/dev/null -c user.email=… -c user.name=… --no-verify` — porch
  deliberately neutralizes hooks and pins its own identity because these commits are
  porch's, not the operator's — but they do not pass `--no-gpg-sign`. On a host with
  `commit.gpgsign = true` the daemon therefore invokes the operator's signing program
  from inside a run, with no timeout, which is one way the gate wedges. It is not an
  obvious bug: a remote that requires signed commits would reject an unsigned
  correction commit, so this is a trade-off with an owner rather than a fix. Recorded
  here, alongside the wedge-state work, rather than decided in a coding session.
- **F.8** Test fixtures inherit ambient git configuration, and commit signing is the
  expensive one. With `commit.gpgsign = true` on the host, every `git commit` and
  `git rebase` in a fixture calls the operator's signing helper, which stalls. Measured
  on this wave: the new integration fixture went from a stable 220 ms to an intermittent
  30 s stall inside `git rebase`, and `porch-gate --lib` swung between 0.19 s and 21.8 s
  across five runs. Isolating the two fixtures this wave touches — `m24_escape.rs` and
  `eject.rs`'s test module — put `porch-gate --lib` at 0.19–0.20 s across six
  consecutive runs with no stalls, and the daemon test that had been the target's
  standing flake stopped failing.

  That is strong evidence the suite's long-standing load-sensitive flakes are largely
  this: a five-second `wait_for_health` starved by a concurrent signing stall looks
  exactly like a load flake. Only `crates/porch/tests/m23_forward_fault.rs` isolated git
  config before this wave. Doing it everywhere is a cross-cutting change to roughly
  twenty test files and belongs on its own rather than inside a feature wave.
