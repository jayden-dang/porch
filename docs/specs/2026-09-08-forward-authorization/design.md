# Design: Durable forward authorization

Feature code: FWDAUTH
Status: Draft
Date: 2026-09-08
Requirements: ./requirements.md

## Context

A forward today has no durable trace of its own. `run_deliver_phase`
(`crates/porch-run/src/deliver.rs:118`) re-reads the worktree HEAD (`:135`), stores it
on the run, ensures `gh` can run, loads trusted config, and then calls
`lease_push_exact` (`:155`), which mutates `origin`. The next durable write about that
forward is `db.set_pr_url` (`:166`), ten lines and one `gh pr create` later. Between
those two statements the process can die with `origin` already advanced and nothing
local saying so. Startup then reads the single field it has — `reconcile_one_running`
classifies on whether `pr_url` is non-empty (`crates/porch-gate/src/rounds/phase.rs:1002`)
— and marks the run `failed`, which is the state a retry pushes from. That is the
duplicate forward **GOAL-1** forbids.

The second half of the problem is what authorization means. `assert_head_continuity`
(`crates/porch-run/src/lib.rs:1869`) requires `review_approved_head_sha` and then
accepts the live HEAD *or any descendant of it* (`:1881`). The forward pushes the
re-read HEAD, not the approved SHA, so that ancestor branch is a path on which
commits no **Review round** ever saw reach `origin` under an older approval.

A reference search shows the ancestor branch is **dead tolerance**, which is what
makes tightening it affordable rather than a behavior break. Every writer of the
approved SHA binds the *live* HEAD at the moment it writes: a findings-free review
auto-binds (`lib.rs:641`), operator approve binds (`:2317`), the `--yes` path binds
(`:3077`), and a deliver repair that moves HEAD explicitly revokes the binding to
`None` (`:1638`) and hands off to a fresh review before any further forward. So on
every green path the approved SHA already equals HEAD at the continuity check. No test
exercises the descendant branch on purpose — the only continuity unit tests cover a
missing approved SHA and the documented empty-diff no-op (`lib.rs:3556`-`3599`).

Two facts shape the storage decision. First, `porch-gate`'s `rounds/` module already
owns three append-only evidence logs with the same spine — `authority_events`
(`rounds/schema.rs:149`), `phase_events` (`:200`), and the requirement rows FLOOR added
(`round_required_producers`, `:117`) — each with `CHECK` constraints that make a row
structurally honest rather than conventionally honest. This feature invents no new
persistence idiom. Second, a `deliver` **Phase attempt** already exists and is already
minted per re-entry (`lib.rs:530`, `:1696`, `:3215`), so a forward attempt has a
natural durable owner and needs no ordinal of its own.

Spine invariants relied on: **ARCH-1** (`origin` history stays unrewritten),
**ARCH-5** (force-with-lease against the observed SHA, fail closed on unverifiable
safety facts), **ARCH-11** (porch alone issues assurance outcomes; the forward record
is evidence), **ARCH-13** (durable authorization and reviewed-input binding precede any
external forward). No decision here contradicts an invariant, so no ADR arises.

Optional system docs (`docs/security/`, `docs/ops/`, `docs/standards/`,
`docs/codebase/`) are absent in this repo; the consult is a no-op and no `SLO-N` /
`TB-N` / `THR-N` is cited below.

## Decisions

1. **One append-only table, `forward_records`, keyed to the owning `deliver` attempt.**
   It mirrors `phase_events`' spine (`id`, `run_id`, `seq`, `kind`, `created_at`) and
   uses conditional `CHECK`s in the style of `round_coverage` (`schema.rs:74`-`88`) so
   an intent row cannot carry an outcome and an outcome row cannot omit its evidence.
   Two tables were rejected: the read side always wants both halves of one attempt in
   sequence, and a single log keeps the "did this attempt push?" question a single
   scan.

2. **Four kinds, not two.** `intent`, `pushed`, `already_current`, `push_failed`.
   `push_exact_sha` returns `Ok(())` for `PushDecision::UpToDate`
   (`crates/porch-git/src/lib.rs:562`) without moving any object, so collapsing that
   into `pushed` would record a mutation that never happened — and ROAD-8 would read
   it as proof this attempt advanced `origin`. `already_current` says what actually
   happened: the lease observation found the ref already at the authorized SHA. This
   distinction is why `FWDAUTH-3.7` exists.

3. **The lease splits into observe-and-decide, then execute.** `lease_push_exact`
   currently interleaves the pre-push observation, the incorporate check, the refusal,
   the push, and the post-push verification. The intent record has to land between the
   observation (it records the observed tip) and the mutation (it must precede it), so
   the function becomes `observe_forward_lease` returning the tip and the decision, and
   `execute_forward_push` performing the push and the post-push verification. Refusal
   stays inside the observe half, which is what keeps `FWDAUTH-2.6` true without a
   special case: a refused forward returns before any intent is written.

4. **The outcome is recorded from our own command's result, before the verification
   read.** The dangerous window is between the mutation and the record, so the
   `pushed` row is appended as soon as `push_exact_sha` returns success — ahead of the
   post-push `ls-remote`. A verification mismatch then fails the run closed with a
   `pushed` row already durable, which is the honest record: we did push. Recording
   after the verification would reintroduce a window in which `origin` moved and
   nothing local said so.

5. **The forward boundary re-derives its own authorization instead of trusting the
   caller.** `run_deliver_phase` resolves the authorized SHA and asserts equality with
   the live HEAD itself, rather than relying on the four upstream
   `assert_head_continuity` call sites (`lib.rs:1490`, `:1516`, `:1694`, `:3194`,
   `:3214`). Callers keep their checks; the boundary stops depending on them.

6. **`assert_head_continuity` becomes exact equality.** The ancestor branch is removed
   rather than left unused, because a tolerance that no path needs is a tolerance the
   next contributor will rely on. This also tightens the certify boundary, which is
   inert for the reason in Context: HEAD already equals the approved SHA there.

7. **The scaffold keeps reading the re-read HEAD.** `assemble_scaffold` and
   `write_compose_packet` are PRCMP-owned and their hidden attestation must stay
   byte-identical (`FWDAUTH-7.6`). Since the boundary has already proved HEAD equals
   the authorized SHA, feeding the push from the authorized value and the scaffold from
   the existing `head_sha` variable changes no byte while making the push's input
   explicit.

8. **Restart classification is untouched.** `reconcile_one_running` keeps its `pr_url`
   branch (`FWDAUTH-7.7`). ROAD-7 supplies the record; ROAD-8 changes the reader. The
   table is therefore write-and-read-back only in this feature: readers exist and are
   tested, but nothing in production behavior yet depends on them.

## Data model

Added to `rounds/schema.rs`'s create batch:

```sql
CREATE TABLE IF NOT EXISTS forward_records (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    deliver_attempt_id TEXT NOT NULL REFERENCES phase_attempts(id),
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('intent','pushed','already_current','push_failed')),
    ref_name TEXT NOT NULL,
    authorized_sha TEXT NOT NULL,
    remote_state TEXT CHECK (remote_state IN ('absent','present')),
    observed_remote_tip TEXT,
    landed_sha TEXT,
    detail TEXT,
    created_at TEXT NOT NULL,
    CHECK (
        kind <> 'intent'
        OR (
            remote_state IS NOT NULL
            AND ((remote_state = 'present') = (observed_remote_tip IS NOT NULL))
            AND landed_sha IS NULL
        )
    ),
    CHECK (
        kind = 'intent'
        OR (remote_state IS NULL AND observed_remote_tip IS NULL)
    ),
    CHECK (kind NOT IN ('pushed','already_current') OR landed_sha IS NOT NULL),
    CHECK (kind <> 'push_failed' OR detail IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS forward_records_run
    ON forward_records(run_id, seq);
CREATE UNIQUE INDEX IF NOT EXISTS forward_records_attempt_intent
    ON forward_records(deliver_attempt_id)
    WHERE kind = 'intent';
```

`seq` is allocated per run as `MAX(seq) + 1` inside the writing transaction, the same
way the phase log orders its events, giving `FWDAUTH-4.1` a deterministic order without
depending on `created_at` resolution. The partial unique index enforces one intent per
`deliver` attempt, which is what makes `FWDAUTH-3.6` structural: a second forward in the
same run arrives under a new attempt or not at all.

**Durability boundary.** The state root runs WAL with SQLite's default synchronous level
(`db.rs:94`). A committed record therefore survives process crash, kill, and daemon
restart — the failure modes **GOAL-1** names — but not host or power loss, which
`docs/product/vision.md` already excludes from porch's guarantees. This is stated rather
than fixed: raising `synchronous` would cost every gate write for a mode the vision does
not cover.

## Components

### `crates/porch-gate/src/rounds/forward.rs` — new

Owns the row type, the kinds, the three appenders, and the readers.

```rust
pub enum ForwardKind { Intent, Pushed, AlreadyCurrent, PushFailed }
pub enum ObservedRemote { Absent, Present(String) }

pub struct ForwardIntent<'a> {
    pub run_id: &'a str,
    pub deliver_attempt_id: &'a AttemptId,
    pub ref_name: &'a str,
    pub authorized_sha: &'a str,
    pub observed: ObservedRemote,
}

pub fn append_intent(db: &Db, intent: &ForwardIntent<'_>) -> Result<String>;
pub fn append_outcome(db: &Db, spec: &ForwardOutcome<'_>) -> Result<String>;
pub fn records_for_run(db: &Db, run_id: &str) -> Result<Vec<ForwardRecordRow>>;
pub fn attempt_reached_origin(db: &Db, deliver_attempt_id: &AttemptId) -> Result<bool>;
```

`append_outcome` refuses a kind that is not an outcome, and refuses an outcome whose
`deliver_attempt_id` has no committed intent — that refusal is what makes
`FWDAUTH-3.3` a property of the store rather than a convention of its caller.
`attempt_reached_origin` is the reconciliation predicate `FWDAUTH-4.2` requires; it is
exercised by tests here and consumed by ROAD-8.

Each appender runs one `TransactionBehavior::Immediate` transaction, matching
`persist_authority_with_run_effects` (`rounds/authority.rs:204`). No `RunEffects`
co-write: a forward record justifies no status change by itself, and inventing one
would put this feature on PHASE's seam without cause.

Respects: ARCH-11, ARCH-13

### `crates/porch-gate/src/rounds/mod.rs`, `schema.rs`, `db.rs`

Declare the module, re-export the public types, add the DDL to the create batch, and
raise `PROTOCOL_SCHEMA_VERSION` once (`FWDAUTH-5.2`). The three existing `runs` writer
triggers — insert, `UPDATE OF status`, `UPDATE OF review_approved_head_sha`
(`db.rs:1073`-`1083`) — are left exactly as they are; ADR-0002's fence keeps working
because the bump flows through `porch_state_meta` the same way FLOOR's did.

Respects: ARCH-13

### `crates/porch-run/src/deliver.rs`

`run_deliver_phase` gains the authorization resolution and the three-step forward:

```rust
let head_sha = porch_git::rev_parse_c(wt, "HEAD")?;
let authorized = authorized_forward_sha(db, run_id, &head_sha)?;
db.set_run_shas(run_id, Some(&head_sha), None)?;
// … gh + trusted config unchanged …
let attempt = open_deliver_attempt(db, run_id)?;
let lease = observe_forward_lease(bare, &refname, &authorized, run.base_sha.as_deref())?;
forward::append_intent(db, &ForwardIntent { … })?;
match execute_forward_push(bare, &refname, &authorized, &lease.decision) {
    Ok(()) => forward::append_outcome(db, &landed_outcome(&lease, &authorized, …))?,
    Err(e) => {
        forward::append_outcome(db, &failed_outcome(&e, …))?;
        return Err(e);
    }
}
```

`authorized_forward_sha` reads `review_approved_head_sha`, fails closed when it is
absent (`FWDAUTH-1.5`), and fails closed naming both SHAs when it differs from the live
HEAD (`FWDAUTH-1.3`). `observe_forward_lease` keeps the `RefuseIncorporate` and
unverifiable-incorporate failures ahead of any intent write (`FWDAUTH-2.6`,
`FWDAUTH-7.2`) and performs exactly the two remote reads the lease already performed
(`FWDAUTH-6.1`). `execute_forward_push` keeps the post-push equality check.

Respects: ARCH-1, ARCH-5, ARCH-13

### `crates/porch-run/src/lib.rs`

`assert_head_continuity` drops the `is_ancestor` branch and reports both SHAs on
mismatch. Nothing else in the file changes: the repair loop's revoke-and-rereview
(`:1638`-`:1696`) is already the only route by which a moved HEAD becomes forwardable,
which is exactly what `FWDAUTH-1.6` and `FWDAUTH-7.4` require.

Respects: ARCH-13

### `crates/porch/tests/m23_forward_auth.rs` — new

Integration coverage over a real gate run with PATH fakes for `gh`, following
`m6_deliver.rs`. Named for the milestone that introduced it, per `AGENTS.md`.

## Ordering and the crash windows

| Instant | Durable state | What a restart can conclude |
|---|---|---|
| before observe | nothing for this attempt | never attempted |
| after refuse | nothing for this attempt | never attempted; run failed closed |
| after intent, before push | `intent` | authorized and about to push; `origin` may be untouched |
| after push, before outcome | `intent` | **residual** — ROAD-8 must probe |
| after outcome | `intent` + `pushed` | authorized and pushed; do not re-push |
| after PR | `intent` + `pushed` + `pr_url` | as today, plus the push evidence |

The row that closes the wide window is `pushed`; the row that narrows the remaining one
to a single statement is `intent`. The residual line is the owned unknown carried to
ROAD-8 — this design does not close it and must not pretend to.

## Reuse

- rung 2 — extends `crates/porch-gate/src/rounds/` with a table and module in the shape
  of `authority_events` / `phase_events` and their `(run_id, seq)` index.
- rung 2 — reuses `AttemptId` and the existing per-re-entry `deliver` attempt as the
  forward attempt's identity rather than minting a parallel ordinal.
- rung 1 — reuses `porch_git::{ls_remote_sha, remote_commits_incorporated,
  resolve_push_decision, push_exact_sha, RemoteTip, PushDecision}` unchanged; the split
  happens in `porch-run`, so `ARCH-2` and the lease semantics are untouched.
- rung 1 — reuses `TransactionBehavior::Immediate` and the `porch_state_meta` fence.

## Alternatives rejected

- **Record forward evidence as a `phase_events` `evidence` row.** No protocol bump and
  no new table, but the authorized SHA, observed tip, and landed SHA would live in a
  free-text `cause`. `FWDAUTH-4.2` asks a machine to distinguish two states; parsing
  prose is not that.
- **Write the intent inside `porch-git`'s push helper.** Puts state-root knowledge in
  the git plumbing crate, which `AGENTS.md` keeps as shared plumbing and not a slice
  (**ARCH-10**).
- **Probe `origin` at the forward boundary to self-heal.** Reads authorization out of
  remote state, which **ARCH-11** forbids, and it is ROAD-8's job.
- **Keep the descendant tolerance and record a copy condition.** Rejected in discovery
  and recorded on the roadmap: it leaves an approval covering commits no round reviewed.

## Risks

- An undiscovered path where HEAD legitimately advances after approve now fails closed
  instead of forwarding. Accepted in the close package; the mitigation is that the
  refusal names both SHAs, so the path identifies itself.
- The protocol bump kills active runs on upgrade, as every prior bump has. Documented in
  `docs/usage.md` per `FWDAUTH-5.3`.

## Test strategy

Store-level tests in `crates/porch-gate/tests/m18_rounds.rs` for the `CHECK`
constraints, the one-intent-per-attempt index, the outcome-without-intent refusal, and
`attempt_reached_origin`. Unit tests in `porch-run` for `authorized_forward_sha`'s two
fail-closed paths. Integration tests in `crates/porch/tests/m23_forward_auth.rs` for a
green forward writing both rows in order, a refused forward writing none, and a
descendant HEAD refusing with both SHAs named. No network: `gh` and the remote are the
existing PATH fakes and a local bare repository.
