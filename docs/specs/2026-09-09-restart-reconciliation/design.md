# Design — restart reconciliation of ambiguous external effects

**Feature code:** RECON
**Roadmap item:** ROAD-8 (MILE-3)
**Requirements:** `requirements.md`

Respects: ARCH-1, ARCH-2, ARCH-5, ARCH-11, ARCH-13

## Context

`reconcile_interrupted_with_error` selects `runs` with `status = 'running'`, terminalizes
each run's open phase attempts, and sets the run's status from one fact:

```
let status = if pr_url.is_some_and(|u| !u.trim().is_empty()) {
    "ci_monitor_interrupted"
} else {
    "failed"
};
```

`crates/porch-gate/src/rounds/phase.rs:1007`-`:1011`. A gate killed after its push
succeeded but before `gh pr create` therefore reports `failed`, while
`forward_records` already holds an intent and a `pushed` outcome for that attempt.

ARCH-13's second clause requires that ambiguous external effects be reconciled before a
retry. ROAD-7 built the evidence. This design reads it.

## The states this classifies

ROAD-9 will assert that a real kill produces these states and no others, so the
enumeration is normative. For one `deliver` phase attempt, the durable forward evidence
is exactly one of:

| State | Durable evidence | What it proves | Verdict |
|---|---|---|---|
| S0 | no forward record | porch never reached the forward boundary | `not_attempted` |
| S1 | intent, no outcome | porch was about to push, or was pushing | `indeterminate` |
| S2 | intent + `pushed` | porch's push command reported success | `reached_origin` |
| S3 | intent + `already_current` | the ref already held the authorized SHA | `reached_origin` |
| S4 | intent + `push_failed` | porch's push command reported failure | `indeterminate` |

S4 is `indeterminate`, not "unchanged". A failure reported by `git push` is a statement
about porch's command, not about the remote's state; treating it as proof the remote is
untouched would be the same category error as reading `pr_url` as proof a push landed
(`RECON-1.5`, `RECON-1.8`).

The one-intent-per-`deliver`-attempt rule means an attempt cannot be in two of these at
once. That rule is upheld by the phase model, not by the classifier: an unchanged-HEAD
deliver repair hands off to the next `deliver` attempt rather than forwarding twice
under the same one.

### Why S2 and S3 cannot be split further

The framing of this work claimed a structural discovery: that a crash after
`append_outcome` but before `gh pr create` is distinguishable from a crash after
`gh pr create` but before `set_pr_url`. It is not. `set_pr_url`
(`crates/porch-run/src/deliver.rs:182`) is the only local write that records a pull
request's existence, and the second crash happens before it. Both leave `intent` +
`pushed` with `pr_url` NULL — byte-identical.

This is the sharpest constraint on the whole feature. The classifier can answer *did
`origin` move?* for both and cannot answer *is there a pull request?* for either. Any
message asserting a pull request is absent would be false in the second case, and false
in the direction that sends the operator to open a duplicate by hand. Hence
`RECON-1.7`: state that the pull request state is unrecorded, never that it is absent.

## Decisions

### The undetermined window is narrowed locally, not by a probe

An `ls-remote` against `origin` would resolve S1 directly. It is rejected on the startup
path, on evidence rather than on principle:

- `porch_git::run` sets no timeout (`crates/porch-git/src/lib.rs:53`-`:68`).
- Recovery runs before the socket binds: `Db::open`, `reconcile_stale`, then
  `recover_stale` at `crates/porch-gate/src/daemon.rs:47`-`:53`, with
  `UnixListener::bind` at `:62`.
- Both recovery calls `return Err`, and the daemon refuses to serve on that error.

So a hung or slow network turns a restart into a dead gate, against GOAL-4. `RECON-2.5`
forbids it outright rather than leaving it to be reintroduced behind a timeout.

What is used instead is a local read of the gate repository's own remote-tracking ref,
`refs/remotes/origin/<branch>`. Git updates that ref only after the receiving end
acknowledges the push, so a match is proof the push landed. It is credential-free,
cannot hang on a network, and needs no new dependency: `porch-gate` already depends on
`porch-git`.

The evidence is **one-sided**. The ref is absent on a fresh gate repository, and
`porch init` can drop it. So a match concludes `reached_origin`; an absence, a mismatch,
or a failed read concludes nothing and the verdict stays `indeterminate`
(`RECON-2.3`). This is why the ref is a discriminator and never an authority.

Reading it is not an assurance outcome, so ARCH-11 is untouched: the fact retrieved is
*porch's own past push was acknowledged*, not a producer's verdict about the change.
ARCH-5 already requires observing the remote before a forward; this observes strictly
less, and locally.

### Where the read happens relative to the transaction

Before it. `reconcile_one_running` opens a `TransactionBehavior::Immediate` transaction
and holds the state root's write lock for its duration; spawning `git` inside it would
hold that lock across a subprocess. So the classifier is two-phase: gather evidence and
resolve the discriminator with no transaction open, then commit the terminalization and
the verdict together (`RECON-2.4`, `RECON-3.2`).

This also settles the ordering M2 raises: classification needs the **gate repository**,
not the worktree. `recover_stale` removes leftover worktrees; it does not remove the gate
bare. Nothing in this design reads a worktree, so worktree cleanup and classification are
independent.

### The verdict is a new additive table, with no vocabulary CHECK

The verdict cannot be a new `ForwardKind` on `forward_records`. `FWDAUTH-3.4` forbids
deriving a forward outcome from anything but porch's own command result, and a verdict
partly rests on a tracking ref. It also cannot be a `phase_events` free-text row,
because `RECON-3.6` needs it machine-queryable to find attempts still lacking a verdict.

So: `forward_reconciliations`, keyed to the `deliver` attempt, append-only.

The verdict column carries **no `CHECK` constraint**, and this is deliberate.
`forward_records` bakes its kind vocabulary into a `CHECK`
(`crates/porch-gate/src/rounds/schema.rs:218`) inside a `CREATE TABLE IF NOT EXISTS`.
That combination is a trap: on a state root that already has the table, editing the DDL
string does nothing, so adding a kind means rebuilding a table whose own requirement says
rows are never rewritten. Repeating that pattern here would leave the same wall for the
next verdict. The vocabulary is enforced in Rust by `Verdict`, which is where a
compile-time exhaustiveness check is available anyway (`RECON-3.5`).

### No protocol bump

`PROTOCOL_SCHEMA_VERSION` stays 4 (`RECON-6.1`). Beyond the ordinary cost of a fence
bump, a bump here is actively self-defeating:
`fail_forward_active_runs_for_protocol_upgrade` sets `status = 'failed'` and NULLs
`review_approved_head_sha` for every active run, and it runs inside `Db::open`
(`crates/porch-gate/src/db.rs:1056`-`:1065`, called from `:165`). `Db::open` is
`daemon.rs:47`, *before* `reconcile_stale` at `:48`. A protocol-5 upgrade would
terminalize exactly the mid-forward `running` rows this feature exists to classify,
before the classifier ever ran.

The table is created additively by the existing `migrate` batch, so an older state root
gains it and a current one is untouched.

### Candidates are selected from the record, not from `runs.status`

The same fail-forward ordering is why `RECON-3.6` selects work from forward records
lacking a verdict rather than from `runs.status`. Status-based selection is correct today
and would be silently disarmed by the next bump — the runs would already read `failed`
by the time reconciliation looked. Selecting on the record's own shape is
status-independent and idempotent, which also gives `RECON-3.7` for free.

### A later verdict about a terminal run is an append

Three requirements would otherwise contradict each other: reconciliation must not
reclassify a terminal run; an `indeterminate` forward can only be resolved by a later
observation; and by the time that observation happens the run is terminal.

Resolved by distinguishing the two writes. A **reclassification** changes
`runs.status`, the run's outcome, or an existing phase event — forbidden (`RECON-4.1`).
An **append** adds a later verdict row about a past forward attempt, retaining the
earlier row; the audit read path takes the latest as current (`RECON-4.2`,
`RECON-4.3`). The run's outcome is never rewritten, and an operator whose
`indeterminate` run is later answered sees the answer.

Writing that later verdict is not in this build. The path that can produce it is the next
forward for the same ref, which observes the remote under its own lease and is therefore
where the observation is already paid for (`RECON-4.4`). This design defines the shape so
that follow-on is an append and not a migration. The honest consequence, recorded as an
open question rather than buried: a branch never pushed again keeps its `indeterminate`
verdict forever.

### The operator surface is a message, not a status

No new `runs.status` value (`RECON-5.4`). The active-run lookup matches a fixed set of
literals (`crates/porch/src/main.rs:719`, `:846`), and the audit read path renders
anything outside `completed|failed|cancelled` as non-terminal
(`crates/porch/src/audit.rs:246`-`:250`) while its anomaly branch fires only for
`parked|running`. A new value would make the run un-addressable and produce a
permanently non-terminal audit document with no anomaly recorded —
`ci_monitor_interrupted` already has that shape, and this feature should not add a
second instance of it.

The conclusion travels in `runs.error`, which reconciliation already writes, keeping the
existing phrase as a prefix so operators and tests matching on it still match
(`RECON-5.3`; `crates/porch/tests/m21_phase.rs` asserts containment, not equality).

The message states the remedy, not only the diagnosis (`RECON-5.2`). The remedy is
already idempotent and needs no new mechanism: re-push, and `find_open_pr`
(`crates/porch-run/src/deliver.rs:254`) adopts an existing pull request for the branch
rather than opening a second one. Without that sentence the build would ship a better
diagnosis and the same stuck operator.

## Data model

```sql
CREATE TABLE IF NOT EXISTS forward_reconciliations (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    deliver_attempt_id TEXT NOT NULL REFERENCES phase_attempts(id),
    seq INTEGER NOT NULL,
    verdict TEXT NOT NULL,
    ref_name TEXT NOT NULL,
    authorized_sha TEXT NOT NULL,
    evidence TEXT NOT NULL,
    observed_tracking_sha TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS forward_reconciliations_run
    ON forward_reconciliations(run_id, seq);
CREATE INDEX IF NOT EXISTS forward_reconciliations_attempt
    ON forward_reconciliations(deliver_attempt_id, seq);
```

`verdict` carries no `CHECK`, by the decision above. `evidence` names which of S0-S4 the
verdict rests on, so the audit document can state the ground rather than only the
conclusion. `observed_tracking_sha` is non-NULL only where the discriminator was read
and resolved.

## Components

### `crates/porch-gate/src/rounds/reconcile.rs` — new

`Verdict` (`NotAttempted`, `ReachedOrigin`, `Indeterminate`) with `as_str`/`parse`, and
`Evidence` naming S0-S4. `classify(records) -> (Verdict, Evidence)` is pure over a
`deliver` attempt's forward records, so the state table above is directly testable
without a daemon, a repository, or a push. `append_verdict_tx` writes inside the caller's
transaction. `attempts_awaiting_verdict` implements `RECON-3.6`.

Respects: ARCH-11, ARCH-13

### `crates/porch-gate/src/rounds/phase.rs`

`reconcile_interrupted_with_error` gains the two-phase shape: for each stale run, gather
its `deliver` attempts' forward records and resolve the discriminator outside any
transaction, then pass the resolved verdicts into `reconcile_one_running`, which appends
them in the transaction it already opens. The existing status rule is unchanged
(`RECON-8.1`, `RECON-8.2`); only `runs.error` gains the conclusion and the remedy.

Respects: ARCH-13

### `crates/porch-gate/src/rounds/schema.rs`

The DDL above, added to the existing create batch. No change to `forward_records`
(`RECON-8.4`), no new trigger (`RECON-6.2`), no protocol bump (`RECON-6.1`).

## Rejected alternatives

- **`ls-remote` on the startup path.** Rejected on the three verified facts above: no
  timeout, recovery before the socket binds, and a recovery error refusing to serve.
- **A new `ForwardKind` for the verdict.** Contradicts `FWDAUTH-3.4`, and would need an
  append-only table rebuilt to widen a `CHECK`.
- **A new `runs.status` value.** Makes the run un-addressable by the active-run lookup
  and produces a permanently non-terminal audit document.
- **Bumping the protocol.** Destroys the runs being classified, before classification.
- **Selecting candidates on `runs.status`.** Correct today, silently disarmed by the
  next bump.
- **Trusting the tracking ref in both directions.** Its absence is not evidence of a
  failed push; `porch init`'s `git remote remove` drops tracking refs.

## Risks

- **A confident false claim about the shared remote.** The one failure mode worse than
  today's unhelpful `failed`. It arrives through wording, not logic: the classifier is
  right about the question it answers and any message implying a pull request is absent
  is wrong about the question it does not. No test can catch it, because `gh` network in
  tests is forbidden. `RECON-1.7` is the guard, and the wording is asserted in tests as
  a string containment.
- **The tracking ref read misused as authority.** Guarded by `RECON-2.3` and by
  `classify` taking the discriminator as an `Option` that can only upgrade
  `Indeterminate` to `ReachedOrigin`, never downgrade.
- **Unbounded growth** of `forward_records` and `forward_reconciliations`, named as a
  deferral in the requirements rather than left implicit.

## Test strategy

Store-level, in `crates/porch-gate/tests/m18_rounds.rs`, since `classify` is pure and the
transaction path is exercisable against a fixture state root:

- the S0-S4 table, one case each, over hand-built forward records;
- the discriminator upgrading S1 and S4 to `reached_origin` on a match, and leaving them
  `indeterminate` on absence, mismatch, and read failure;
- terminalization and verdict committing together, and the existing `failed` /
  `ci_monitor_interrupted` status rule unchanged;
- idempotence across two reconciliation passes;
- selection finding an attempt whose run status is already `failed`, proving
  `RECON-3.6`;
- the operator message containing the interruption prefix, the remedy, and no assertion
  of pull request absence.

`crates/porch/tests/m21_phase.rs` keeps its existing containment assertions as the
regression guard on `RECON-5.3`. End-to-end proof that a real kill produces these states
is ROAD-9.
