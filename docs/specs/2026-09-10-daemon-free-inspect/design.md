# Design — daemon-free inspect

**Feature code:** LOOK
**Status:** Implemented
**Roadmap item:** ROAD-10 (MILE-4), second wave
**Requirements:** [requirements.md](requirements.md)
**Respects:** ARCH-1, ARCH-2, ARCH-10, ARCH-11, ARCH-13

## What this build is

Three production pieces and the surfaces that consume them. No new crate.

1. **`Db::open_read`** — a constructor that cannot create, migrate, or
   Immediate-terminalize. ROAD-24 will call it; this wave does not implement purge.
2. **A recover-ref reader** on the layout-derived bare, git CLI only, plus leftover
   worktree HEADs.
3. **One join** on `porch status` / `porch runs`: **Daemon condition** + custody
   tips + **Forward record**/verdict. Inspect commands do not spawn.

## Invariant citations

- **ARCH-1** — recover remains FF-only of the operator's local branch; inspect
  does not touch `origin`.
- **ARCH-2** — recover-ref listing and leftover-HEAD reads via `git`
  (`for-each-ref` / `rev-parse`).
- **ARCH-10** — `open_read`, the recover-ref reader, and the join live in
  `porch-gate`; operator surfaces stay in `porch` / existing `porch-run` sync.
- **ARCH-11** — a **Forward verdict** is reconciliation evidence, never an
  assurance outcome.
- **ARCH-13** — inspect reads the durable conclusion or derives it with the
  existing pure `classify`; it does not authorize, retry, or append a verdict.

## Options considered and rejected

**New `porch inspect`.** Rejected: `porch status` is already documented as “daemon +
latest”. A third verb while `status` still spawns leaves the first command broken.

**Inspection may spawn “so it can answer”.** Rejected: that is today's
`run_status` (`let _ = ensure_daemon_for_cwd`) and it contradicts GOAL-4.
Spawning on `unreachable` starts recovery, which manufactures the verdict row
the operator asked to see.

**Project only `forward_reconciliations`.** Framing recommended this so inspect
cannot disagree with restart. The advisor showed the GOAL-4 window is *before*
restart: intent-only record, no verdict, daemon dead. Projecting only stored
rows shows nothing in the SIGKILL sliver this wave is supposed to unblock.
**Chosen:** stored row wins (LOOK-3.3); otherwise `classify` as a read
(LOOK-3.4). Never `append_verdict_tx`. Never probe `origin` as a network; the
existing `observed_tracking_sha` is a local `rev-parse`.

**`PRAGMA query_only` after `Db::open`.** Rejected: DDL and Immediate already ran.

**Hang the catalog on `doctor` or `agent sync`.** Doctor is a prerequisite
checker. Sync is recover-or-report for *one* run; its JSON is an agent contract.

## `Db::open_read`

```text
rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
PRAGMA query_only = ON
PRAGMA foreign_keys = ON
busy_timeout ≈ 100ms
```

No `create_dir_all`. Missing file → error, no 0-byte create. No
`journal_mode=WAL`. No writer-protocol UDF (read-only will not fire insert
triggers). An older binary against a newer root degrades on unknown tables
rather than refusing the way a writer does.

WAL: a second process can `SELECT` while the daemon holds `BEGIN IMMEDIATE`.
If `-shm` cannot be created (read-only filesystem after a crash), inspect
reports the DB arm unreadable and still prints git tips and the condition.

## Recover-ref reader

`git --git-dir=<bare> for-each-ref --format=%(objectname)%09%(refname) refs/porch/recover`

Bare: `repos_dir(home).join(format!("{repo_id}.git"))`, same as eject.
Repo id: promote `eject::resolve_repo_id` to `pub`.

Leftover worktrees: each directory under `worktrees_dir(home).join(repo_id)`
whose `rev-parse HEAD` succeeds. Pin declined → worktree kept (`ESCAPE-1.3`);
those HEADs are tips ADR-0005 counts.

Do not scan every `repos/*.git`. Post-eject, identity fails closed with a
named reason (LOOK-3.7).

## Forward projection

Per `deliver` attempt on the inspected repo's runs:

| Situation | `source` | What to show |
|---|---|---|
| Latest `forward_reconciliations` row for the attempt | `stored` | that verdict + evidence + `operator_note` |
| No row, no `terminal` event, records present | `derived` | `classify(records, authorized, tracking)` |
| Terminal event already recorded | omit derived class | the attempt concluded itself; do not contradict it |
| Unknown verdict text | `unknown` | name the string; do not skip |

`pr_state` is `unrecorded` only for `reached_origin`. Never `pr_missing`.

Reuse `rounds::forward::records_for_run`, `rounds::reconcile::{classify, verdicts_for_run, operator_note, observed_tracking_sha}`.

## Surfaces

`crates/porch-gate/src/look.rs` owns the join (`LookReport`). `porch status` and
`porch runs` print it. They do not call `list_runs` / `get_run` RPC for the
inspect body (that would hang on `not-answering` and would spawn today).

`porch status --json` (additive):

```json
{
  "daemon_healthy": false,
  "condition": "unreachable",
  "porch_home": "...",
  "repo_id": "...",
  "database": { "readable": true },
  "recovery_tips": [{ "ref": "refs/porch/recover/<id>", "sha": "...", "run_id": "..." }],
  "worktree_tips": [{ "path": "...", "sha": "...", "run_id": "..." }],
  "forwards": [{ "run_id": "...", "deliver_attempt_id": "...", "source": "derived",
                 "verdict": "indeterminate", "evidence": "intent_only",
                 "ref_name": "...", "authorized_sha": "...", "note": "..." }],
  "latest_run": { "id": "...", "status": "...", "forward": { } }
}
```

Doctor: one check `state database` (ok / warn-unreadable) and, in a porch
clone, `recovery tips: N`. Point at `porch status` in the detail when condition
is not `ready`.

`agent_sync` / `agent_status`: `Db::open_read`. Sync repo id: `resolve_repo_id`.
Do not change `agent sync` JSON keys. Do not change `agent status` default
(latest parked).

## Tests

New `crates/porch/tests/m27_look.rs` for the CLI join and no-spawn guards
(reuse m25's condition induction). Store tests in `crates/porch-gate` next to
`m24_escape` / db unit tests for `open_read`.

Honest tests are listed as LOOK-5.1–5.8. Do not treat
`agent_sync_answers_without_a_daemon` as proving inspect: it already passes on
a writing `Db::open`.

## Out of scope (locked)

ROAD-24; SIGKILL; daemon-not-wedge; recover-ref sweep; daemon-free audit;
lumping `rerun` / `agent run` / TTY attach into “six commands”; a fifth
condition for `SQLITE_BUSY`.
