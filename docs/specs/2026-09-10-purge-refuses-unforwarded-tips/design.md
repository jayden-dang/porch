# Design — purge refuses over unforwarded custody tips

**Feature code:** PURGE
**Status:** Implemented
**Roadmap item:** ROAD-24 (MILE-4)
**Requirements:** [requirements.md](requirements.md)
**Respects:** ARCH-1, ARCH-2, ARCH-10, ARCH-11, ARCH-13

## What this build is

Four production pieces. No new crate.

1. **A read-only predicate** over LOOK's custody tips plus `active_runs`, run
   before any **Eject** mutation.
2. **`--abandon`**, clap-tied to `--purge`, writes a durable record under
   `$PORCH_HOME/abandoned/` then proceeds.
3. **A refusal manifest** printed only when the predicate fails — not a
   `--dry-run`.
4. **`remove_dir_all(bare)` failure propagates**, so `Purged` cannot be
   reported over a surviving gate repository.

## Invariant citations

- **ARCH-1** — purge/abandon do not rewrite `origin`; origin proof is local
  tracking plus porch-owned **Forward record** / **Forward verdict**, never a
  fetch or push.
- **ARCH-2** — tip listing via `for_each_ref` / `rev_parse`; ancestry via git
  CLI (`-C` checkout, `--git-dir` bare). No libgit2.
- **ARCH-10** — predicate in `porch-gate`; CLI flag in `porch`; GitDir
  ancestor helper in `porch-git`.
- **ARCH-11** — a **Forward verdict** remains reconciliation evidence, not an
  assurance outcome. Using `reached_origin` as origin-proof for a local delete
  does not approve a review.
- **ARCH-13** — no network probe of `origin`; no `append_verdict_tx` on this
  path.

## Framing vs advisor

Framing and the advisor agreed: refuse before detach; tracking ref is
`refs/remotes/origin/*` (ADR-0005's `refs/porch/remotes/origin/<branch>` is a
prose error against `observed_tracking_sha`); `--purge` on a clean tree still
purges; `--abandon` without `--purge` is invalid; checkout reachability is
evaluated in the checkout (missing objects are not proven); LOOK's fail-soft
listers must be wrapped for purge; `GateState` stays three post-detach
states; `indeterminate` is not origin proof.

They disagreed on three coding details:

**`--abandon` vs active runs.** The advisor would split the override:
`--abandon` consents only to losing enumerated inactive tips; active runs
would need a separate stop. Rejected: a crashed daemon leaves `running` rows,
and `porch daemon stop --force` does not clear them, so a purge that cannot
override those rows never completes on the longest-serving dead homes. ADR
names `--abandon` as the single explicit override. Chosen: it overrides both.
The operator is told the run ids in the manifest.

**Unreadable DB without `--abandon`.** The advisor would detach then
`LeftBehind` (ESCAPE-2.3). Rejected: after detach, `resolve_repo_id` fails
and LOOK cannot list the tips that still exist. Chosen: refuse, stay
attached. `--purge --abandon` keeps ESCAPE-2.3 (detach then `LeftBehind` if
the writer cannot finish).

**Feature code.** The advisor preferred `ABANDON` (ROAD-10's remaining
verb). Rejected: that names the override. `PURGE` is the path this wave
changes; ESCAPE already shipped purge-completes-or-leaves.

**Fail-closed every filesystem delete.** The advisor would fail a PR that only
repairs the bare. Rejected: rows are already gone when worktree deletion
runs; failing closed mid-fs after `delete_repo` leaves a worse split than
today. ADR named the bare lie. Residual stays named.

## Order of operations

```text
canonicalize, resolve_repo_id, derive bare
if !purge:
  detach; Preserved
else:
  list tips (LOOK listers, fail closed on git/fs error)
  if state.sqlite exists:
    open_read → active_runs(this repo); forward facts
    open_read failure → refuse (unless abandon)
  else:
    active runs and records are empty
  evaluate per-tip (checkout | origin-proof); active-run interlock
  if blocked && !abandon: Err(manifest)              // no mutation
  if abandon: write abandoned/<repo_id>-<ulid>.json // fail → no mutation
  Db::open (writer); re-check active_runs if !abandon
  detach (remote, config, hooks)
  purge_repo_state; bare remove_dir_all must propagate
```

`GateState` is unchanged: `Preserved` / `Purged` / `LeftBehind`. Refusal is
`Error::Other` with the manifest as the message, returned **before** detach.
`run_eject` never prints `ejected repo` on that path.

## Tips and origin proof

LOOK's `recovery_tips` / `worktree_tips` stay fail-soft for inspect. Purge
calls strict wrappers:

| Layout | Strict result |
|---|---|
| Bare absent | no recover tips |
| Bare exists, `for_each_ref` fails | refuse |
| `worktrees/<repo_id>` absent | no worktree tips |
| Root exists, `read_dir` fails | refuse |
| A leftover directory's HEAD cannot be read | refuse |

Per tip, **proven** if either:

1. **Checkout.** `porch_git::is_ancestor(work, tip_sha, HEAD)` is `Ok(true)`.
   `Ok(false)` and `Err` (missing object) are not proven. Do not evaluate this
   leg on the bare: the bare may not contain a newer checkout HEAD.
2. **Origin, on the bare**, using new `porch_git::is_ancestor` for `GitDir`
   (`--git-dir`, same 0/1/other contract as the work-tree helper):
   - For the tip's `run_id`, any **Forward record** `pushed` / `already_current`
     or stored **Forward verdict** `reached_origin` whose `authorized_sha` has
     the tip as ancestor-or-equal.
   - Else the tip is ancestor-or-equal of **any** `refs/remotes/origin/*` on
     that bare (renames / later forwards). Direction: tip ancestor of the
     landed SHA, not the reverse — a leftover worktree *ahead* of a prior
     forward is unforwarded work.

Do not call `classify`. Its `NotAttempted` return before the tracking-ref
upgrade is exactly the pre-ROAD-7 state ADR-0005 forbids refusing, and its
upgrade is equality, which would refuse after origin moved past the tip.

ADR-0005's `refs/porch/remotes/origin/<branch>` is not created. Nothing writes
it.

## Abandon record

Path: `$PORCH_HOME/abandoned/<repo_id>-<ulid>.json` — not under `repos/`,
`worktrees/`, `runs/`, or `logs/` (`logs/` is truncated on daemon spawn).
ULID in the name so a later re-init + abandon cannot overwrite. Payload:

```json
{
  "repo_id": "...",
  "abandoned_at": "<unix seconds>",
  "tips": [{ "kind": "recover"|"worktree", "ref": "...", "path": "...", "sha": "...", "run_id": "..." }],
  "active_run_ids": ["..."]
}
```

Written whenever `--abandon` proceeds, including a clean tree (empty arrays):
the file is the durable proof the flag was used. Never deleted by a later
purge.

## Active runs

Same status set as `stop_daemon`: `pending` / `running` / `parked`, scoped to
this `repo_id`. Predicate uses `open_read`. After the writer open, re-query
`active_runs` for this repo; if now non-empty and not abandoning, return `Err`
while still attached. That narrows the TOCTOU; it does not close it against a
`notify_push` that inserts after the re-check. Residual named.

`--abandon` overrides this interlock. There is no second flag.

## Bare delete

`purge_repo_state` currently:

```text
runs_for_repo → delete_repo → best-effort worktrees/artifacts/wt_root/sweep
→ let _ = remove_dir_all(bare); Ok(())
```

The last line becomes `remove_dir_all(bare)?` when the bare exists. Worktree
and artifact removals stay best-effort (ESCAPE-3.4 already ran the
transactional delete first).

## Tests

`crates/porch/tests/m28_purge.rs` for the CLI join (refuse, abandon, clean
purge, clap combo, still-attached after refuse). Store tests in
`crates/porch-gate/tests/m28_purge.rs` plus eject unit tests for the bare-delete
lie. GitDir ancestor cases in `crates/porch-git/tests/m1_git_dir.rs`. Do not
pile ROAD-24 into `m24_escape.rs` / `m27_look.rs`.

Honest tests are listed in tasks.md wave 5. Do not treat LOOK's empty-on-error
listers as proving purge enumeration.

## Drive-by

Collapse the duplicated `resolve_repo_id` docstring (ESCAPE paragraph + LOOK
paragraph stacked) when `eject.rs` is touched.

## Out of scope (locked)

SIGKILL; daemon-not-wedge; recover-ref sweep; remaining `repo_id_for` sites;
reopening ADR-0005; unifying `classify`'s equality upgrade with purge ancestry;
a `--json` eject flag.
