# Design: Mandatory Deterministic Floor

Feature code: FLOOR
Status: Implemented
Date: 2026-09-02
Requirements: ./requirements.md

## Context

ROUND (ROAD-6) built a multi-producer round store: `open_round` accepts
`OpenRoundPlan { run_id, producers: Vec<ProducerInvocation> }`, coverage and finding instances key
on `producer_invocation_id`, and `m18_rounds.rs:1366` already proves a floor+judgment round is not
equivalent to a judgment-only one. The run loop never uses that capacity — `porch-run/src/lib.rs:420`
opens with `producers: vec![ONE]`, `:389` fixes `producer_slot: 0`, and `:436` takes `.next()`. An
operator on `engine: agent` therefore runs no deterministic floor at all, and `EngineKind`
(`engine.rs:11-20`) offers no way to say otherwise: selection is one of four exclusive variants.

The deeper defect is not the missing spawn. `applicable_round(db, run_id, bindings,
required_producer_digests)` (`applicability.rs:83`) takes a required set, but
`decision_bindings_for_run` (`applicability.rs:126-143`) *derives* that set by mapping
`producers_for_round` to their equivalence digests. The requirement is reconstructed from whatever
ran, so a judgment-only round declares judgment-only as required and satisfies its own test.
**ARCH-12** is unenforceable through that path no matter how many producers the run loop spawns.
This design therefore treats ROAD-22 as an authorization-soundness change first and a composition
change second.

The binding constraint is that **the required set must be recorded before execution and never
reconstructed from it**, and it must remain expressible when the floor could not be resolved at all
— a requirement with no descriptor, no digest, and no invocation. That rules out the otherwise
obvious approach of widening `round_producers` with a role column and nullable descriptor fields:
that table's `descriptor_json NOT NULL` and `descriptor_equivalence_digest NOT NULL`
(`rounds/schema.rs:40-41`) are exactly what make it trustworthy as an audit record of genuine
resolved invocation plans, and making them nullable would force every reader to ask whether a row
denotes an execution or an intention. Requirements and invocations are separate concerns and get
separate tables.

A second constraint shapes the compatibility work. `v0.2.0`–`v0.2.2` are on crates.io, so an
operator can install a binary that predates this regime at any time, and nothing this feature ships
can make an already-released binary execute new logic. Only constructs that live inside the database
file run for a client regardless of its version. The fence is therefore DB-resident, and rollback to
a pre-protocol-2 binary is defined as unsupported rather than silently permitted.

## Decisions

1. **ROAD-22 is an authorization-soundness problem.** Authorization must prove the mandatory floor
   ran; the required set comes from policy, never reconstructed from the producers present; a
   judgment-only round never authorizes.
2. **The floor requirement is Porch-owned protocol policy**, hardcoded and unconditional — not
   expressible in `.porch.yaml`, `$PORCH_HOME/config.yaml`, or any environment variable. No
   `review.producers` key; MILE-6 is not pre-designed.
3. **A floor-only round may authorize** when `engine: quality` was the deliberately selected shape.
   Floor-only and floor+judgment are distinct recorded shapes; an involuntary loss of the judgment
   producer is `incomplete`, never a downgrade.
4. **Requirements live in a new `round_required_producers` table**, keyed `(round_id,
   requirement_slot)`, immutable after round open. `round_producers` is untouched.
5. **Producers execute sequentially, floor first**, with no cross-feeding of floor output into
   judgment context. A cheap computation-only layer gates the expensive turn.
6. **An unsatisfiable floor fails the run**, never parks it — a parked run exposes `approve` and
   `skip` (`porch-run/src/lib.rs:1316-1322`), which would be a human override of ARCH-12. Recovery is
   `porch rerun --run-id`, which already allocates a fresh run from the recorded tip and intent
   (`porch-run/src/lib.rs:2080-2117`).
7. **The floor resolves through a dedicated resolver** — canonicalized `current_exe()` sibling with
   platform suffix, spawned directly, no PATH fallback on a production path, and never through
   `$PORCH_HOME/bin/review`, `review.bin`, `PORCH_REVIEW_BIN`, or the judgment engine's path.
8. **A run's assurance contract is pinned at first round open** as a canonical digest, and later
   attempts are compared against it before any spawn. Strengthening and weakening are treated
   identically.
9. **Protocol boundary is `protocol_schema_version = 2`**, with `< 2` legacy and never applicable,
   preserved without backfill, and a higher-than-understood version failing closed.
10. **The compatibility fence is F2′** — a persistent `porch_state_meta` minimum-writer marker,
    DB-resident triggers over new-run creation and approval writes, and a friendly `Db::open`
    compatibility check. Compatible connections register a zero-argument SQL function
    (`porch_writer_protocol()`); the triggers compare its value against the stored minimum. A
    persistent trigger cannot reference the TEMP schema, so a temp-table declaration is not viable.
    An old binary lacks the function and its protected writes fail closed.

> **ADR.** Decision 10 passed the three-part gate — hard to reverse (the triggers live inside
> operator databases in the wild), surprising without context (DB-resident enforcement logic in a
> project that otherwise keeps behavior in Rust), and a real trade-off (the only way to fence an
> already-released binary, paid for with logic that outlives the code, blunt error copy, and
> downgrade defined as unsupported). Recorded as
> [ADR 0002](../../adr/0002-database-resident-compatibility-fence.md).
> Decision 7's no-fallback rule was raised and **declined**: it is surprising and a real trade-off,
> but a later binary simply resolves differently and the legacy/version rules absorb the identity
> change, so it fails "hard to reverse". It lives in this design and the code, not an ADR.

Spine invariants this design relies on: **ARCH-11** (Porch alone issues assurance outcomes),
**ARCH-12** (the floor always runs and is never substitutable), **ARCH-13** (durable authorization
and reviewed-input binding precede any external forward), **ARCH-10** (crates are use-case slices —
the floor resolver is a module inside `porch-review`, not a new crate), and **ARCH-4**
(code-executing config is trusted config — the floor is stronger still: not config at all).

## Architecture

Vocabulary for every section below: **module**, **interface**, **implementation**, **seam**.

### 1. `porch-gate::rounds::requirements` — the recorded required set

Satisfies: FLOOR-2.1, FLOOR-2.2, FLOOR-2.3, FLOOR-2.4, FLOOR-2.9, FLOOR-5.1, FLOOR-5.2, FLOOR-9.3
Reuse: rung 7 — new module. Rungs 2–5 were checked: `round_producers` cannot express a requirement
with no descriptor (`schema.rs:40-41` NOT NULL), and no existing table carries per-slot intent.
`length_delimited_join` and `sha256_hex` (`porch-review/src/plan.rs:668,664`) are reused for the
digest preimage rather than reinvented.
Respects: ARCH-11, ARCH-13
Interface: `RequirementRow { slot: i64, role: Role, resolution: Resolution,
expected_equivalence_digest: Option<String>, producer_invocation_id: Option<String>, reason:
Option<String> }`; `required_set_digest(protocol_version, &[RequirementRow]) -> String`;
`requirements_for_round(&Db, &RoundId) -> Result<Vec<RequirementRow>>`. Writes happen only inside
`open_round`'s existing transaction — this module exposes no write entry point of its own.
Depth: if this module vanished, callers would still need to know only that a round has an ordered
list of `(role, resolution, expected digest)` requirements and that they hash to one canonical
digest. The row encoding, the CHECK/FK constraints, the unresolved marker's byte form, and the
length-delimited preimage all stay inside.
Locality: new file `crates/porch-gate/src/rounds/requirements.rs`; `rounds/mod.rs` **extend** (call
it from `open_round`); `rounds/schema.rs` **extend** (DDL); every other neighbor **leave**.

DDL, additive under the existing `CREATE TABLE IF NOT EXISTS` batch:

```sql
CREATE TABLE IF NOT EXISTS round_required_producers (
    round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
    requirement_slot INTEGER NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('floor','judgment')),
    resolution TEXT NOT NULL CHECK (resolution IN ('resolved','unresolved')),
    expected_equivalence_digest TEXT,
    producer_invocation_id TEXT,
    resolution_reason TEXT,
    PRIMARY KEY (round_id, requirement_slot),
    FOREIGN KEY (round_id, producer_invocation_id)
        REFERENCES round_producers(round_id, id),
    CHECK (
        (resolution = 'resolved'
            AND expected_equivalence_digest IS NOT NULL
            AND producer_invocation_id IS NOT NULL)
     OR (resolution = 'unresolved'
            AND expected_equivalence_digest IS NULL
            AND producer_invocation_id IS NULL
            AND resolution_reason IS NOT NULL
            AND length(trim(resolution_reason)) > 0)
    )
);
```

The `CHECK` is what makes FLOOR-2.3 and FLOOR-2.4 structural rather than conventional: a resolved
row cannot exist without a real invocation reference, and an unresolved row cannot smuggle in a
digest or omit its reason. The composite FK reuses `round_producers`' existing `UNIQUE (round_id,
id)` (`schema.rs:43`), so a requirement can only point at an invocation of its own round.

Exactly one `role = 'floor'` row per round is enforced by the open-round API and its tests rather
than by a partial unique index, keeping the DDL portable across the SQLite versions operators have.

**Digest preimage** (FLOOR-5.1). Domain-separated and length-delimited via
`length_delimited_join`, over: the literal `porch.required_set.v1`, the protocol version, then for
each slot in ascending `requirement_slot` order its `role`, its `resolution`, and either the
`expected_equivalence_digest` when resolved or the constant marker `unresolved` when not.
`resolution_reason` is excluded — it is diagnostic text and must not perturb identity.

**Run pin persistence** (FLOOR-5.2). The pin is a nullable `runs.required_set_digest TEXT` column,
added with the established additive helper (`ensure_column(&conn, "runs", "required_set_digest",
"TEXT")`, precedent `db.rs:129-146`); NULL means "not yet pinned", which is the correct state for
every run created before this feature and for a run whose first round has not opened.

Read/write API on `porch-gate::rounds`:

- `run_required_set_digest(&Db, run_id) -> Result<Option<String>>` — the read used by the
  pre-open comparison.
- The write is **not** a public function. `open_round` performs
  `UPDATE runs SET required_set_digest = ?1 WHERE id = ?2 AND required_set_digest IS NULL`
  inside its existing `TransactionBehavior::Immediate` transaction (`rounds/mod.rs:558`), in the
  same statement sequence that inserts the round, the `round_producers` rows, and the
  `round_required_producers` rows. The `IS NULL` predicate makes FLOOR-5.5 structural: a second
  write cannot re-pin, and the affected-row count of `0` on a run that already carries a pin is
  itself the mismatch signal. There is no setter a caller could invoke out of band.

### 2. `porch-gate::rounds` core — round open, finalization, retention

Satisfies: FLOOR-6.1, FLOOR-8.2, FLOOR-8.3, FLOOR-8.4, FLOOR-8.7, FLOOR-8.9, FLOOR-9.4
Reuse: rung 2 — extends `open_round` (`rounds/mod.rs:556`) and `finalize_round` (`:935`), both
already running under `TransactionBehavior::Immediate` (`:558`, `:943`).
Respects: ARCH-13
Surface:
- `OpenRoundPlan` (in-repo, `porch-run/src/lib.rs:420` sole caller) — **replace**: gains
  `requirements: Vec<RequirementSpec>` and `run_pin: Option<String>`; the single caller migrates in
  this change.
- `RoundBindings.protocol_schema_version` (in-repo, `porch-run`) — **replace**: callers pass `2`.
- `review_rounds` rows written by prior versions (persisted) — **frozen**: this design does not
  alter or backfill them; FLOOR-2.9 and FLOOR-6.2 discharge the contract by leaving them untouched
  and treating them as never-applicable.
- `crates/porch-gate/tests/m18_rounds.rs` (tests, `:63`, `:1009`) — **replace**: constructs
  `OpenRoundPlan` directly; updated with the new fields in this change.
- `crates/porch/tests/m18_round_identity.rs` (tests, `:836`) — **replace**: also constructs
  `OpenRoundPlan` directly; updated in this change.
Interface: unchanged names — `open_round(&Db, &OpenRoundPlan, &RoundBindings) -> Result<RoundId>`,
`finalize_round(&Db, &RoundId, &FinalizeProposal, HistoryRevision) -> Result<FinalizeOutcome>`.
Depth: n/a — extends `porch-gate::rounds`.
Locality: `rounds/mod.rs` **extend**; `rounds/schema.rs` **extend**; `rounds/retention.rs`
**leave** (its pin/sweep contract is guarded, not changed).

`open_round` gains, inside its existing immediate transaction and in this order: insert the round,
insert `round_producers` rows for resolved invocations, insert `round_required_producers` rows, and
set the run pin when it is currently NULL. One transaction covers all four, satisfying FLOOR-5.2 and
FLOOR-9.3 — a kill at any point leaves no round rather than a half-opened one. FLOOR-8.2's guarantee
is preserved and sharpened: invocation rows are still committed before the producer is spawned and
still denote a resolved invocation *plan*.

`finalize_round` is unchanged in shape and continues to write coverage, finding instances, and
terminal state in one transaction or none, returning `Stale` (`:967`) when the run's review-history
revision moved between phases. It never touches requirement rows — FLOOR-2.2.

**Duration storage** (backing FLOOR-9.1, satisfied at §6). `round_producers` is not modified.
Per-producer timing goes in a new additive table, and the round total on `review_rounds` via the
`ensure_column` helper:

```sql
CREATE TABLE IF NOT EXISTS round_producer_durations (
    round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
    producer_invocation_id TEXT NOT NULL,
    duration_ms INTEGER NOT NULL CHECK (duration_ms >= 0),
    PRIMARY KEY (round_id, producer_invocation_id),
    FOREIGN KEY (round_id, producer_invocation_id)
        REFERENCES round_producers(round_id, id)
);
```

plus `ensure_column(&conn, "review_rounds", "review_duration_ms", "INTEGER")` for the round total.
Both are written during `finalize_round`'s existing transaction, so a round either records its
durations with its terminal state or records neither. A producer that never ran has no row, which
is distinguishable from a producer that ran in zero measurable time (`duration_ms = 0`).

### 3. `porch-gate::rounds::applicability` — authorization from the record

Satisfies: FLOOR-2.5, FLOOR-2.6, FLOOR-2.7, FLOOR-2.8, FLOOR-5.7, FLOOR-6.2, FLOOR-6.3, FLOOR-8.5,
FLOOR-8.6, FLOOR-8.20, FLOOR-9.5
Reuse: rung 2 — extends `applicable_round` (`applicability.rs:83`) and
`round_is_applicable` (`:92`); the equivalence-digest machinery is reused unchanged.
Respects: ARCH-11, ARCH-12, ARCH-13
Surface:
- `decision_bindings_for_run` (`applicability.rs:126`, private, sole caller
  `applicable_round_for_run` `:116`) — **replace**: stops deriving `required` from
  `producers_for_round` and reads `requirements_for_round` instead. This is the defect fix.
- `applicable_round`'s `required_producer_digests: &[String]` (in-repo; callers
  `applicable_round_for_run` `:117` and nine call sites in `m18_rounds.rs`) — **replace**: takes the
  recorded requirement rows so the bijection and the unresolved check are decidable in one place.
- `rounds::applicable_round` re-export (`rounds/mod.rs:8`, crate-public) — **replace**: the
  re-export follows the changed signature in this change; no external crate consumes it.
- `rpc.rs:198` serve path (in-repo) — **compat**: keeps serving legacy parked runs through the
  existing legacy-snapshot fallback (FLOOR-8.20); no follow-up removal is owed because the legacy
  path is a permanent read-only affordance, not a migration shim.
Interface: `applicable_round_for_run(&Db, &RunRow) -> Result<Applicability>` — unchanged signature,
corrected semantics.
Depth: n/a — extends `porch-gate::rounds::applicability`.
Locality: `applicability.rs` **extend**; `rpc.rs` **leave**.

Applicability now requires, in order: the round's `protocol_schema_version` is exactly the version
this binary understands (`< 2` → not applicable, FLOOR-6.2; `> known` → fail closed, FLOOR-6.3);
the round has at least one requirement row (zero → legacy, FLOOR-2.8); no requirement is
`unresolved` (FLOOR-2.7); every resolved requirement maps to exactly one recorded invocation and
back, **and each requirement's `expected_equivalence_digest` equals that referenced invocation's
recorded `descriptor_equivalence_digest`** (FLOOR-2.6) — the composite FK proves same-round
ownership only, never that the invocation is the one the requirement demanded, so the digest
comparison is an explicit check and not a constraint side effect; the round's recomputed required-set digest equals the run pin (FLOOR-5.7); and
finally the existing bindings and equivalence checks, unchanged — including the exclusion of
`selection_source` and `declared_engine_kind` from the digest (`applicability.rs:47`, FLOOR-8.5) and
the per-invocation ULID nonce that makes an unavailable version incapable of matching (`:48,60`,
FLOOR-8.6).

### 4. `porch-gate::db` — the compatibility fence (F2′)

Satisfies: FLOOR-6.4, FLOOR-6.5, FLOOR-8.1, FLOOR-8.8, FLOOR-8.21
Reuse: rung 5 — `rusqlite`'s `create_scalar_function` (already-installed dependency; the workspace
must enable its `functions` feature, currently `features = ["bundled"]` only in `Cargo.toml:25`).
Confirmed signature: `create_scalar_function(fn_name, n_arg, flags: FunctionFlags, x_func)`.
Respects: ARCH-13
Surface:
- `Db::open` (`db.rs:87`; every binary path — daemon, CLI, hooks, rerun, doctor, eject) —
  **replace**: registers `porch_writer_protocol()` and performs the compatibility check on open.
- `runs` / approval rows in existing operator databases (persisted) — **frozen**: the upgrade
  transaction does not rewrite history; it terminalizes active legacy runs and invalidates
  un-delivered legacy approval state, which FLOOR-6.5 mandates. Discharged as a requirement-mandated
  change, gated on the operator's own act of upgrading — there is no external subscriber to consent.
- Released `0.2.x` binaries (external readers) — **frozen**: their behavior cannot be changed. The
  design builds around the contract by fencing at the database instead, and by defining continued
  gating on an upgraded state root as unsupported.
Interface: `Db::open(&Path) -> Result<Db>` — unchanged signature; fails with a stated incompatibility
message rather than silently proceeding.
Depth: n/a — extends `porch-gate::db`.
Locality: `db.rs` **extend**; `daemon.rs` **leave** (startup recovery contract guarded, FLOOR-8.21).

Mechanism:

- `porch_state_meta(min_writer_protocol INTEGER NOT NULL)` — one row, persistent.
- Every compatible connection registers `porch_writer_protocol()` at `Db::open`, returning the
  binary's protocol constant, with
  `FunctionFlags::SQLITE_UTF8 | SQLITE_DETERMINISTIC | SQLITE_INNOCUOUS`.
  **Flag policy.** `SQLITE_INNOCUOUS` is present in rusqlite 0.32.1
  (`functions.rs:387`, `0x0000_0020_0000`), as is its opposite `SQLITE_DIRECTONLY` (`:383`).
  SQLite refuses a non-innocuous application-defined function inside a trigger body whenever the
  schema is treated as untrusted, so the function **must** be flagged `SQLITE_INNOCUOUS` and must
  **not** be flagged `SQLITE_DIRECTONLY`. The flag is honest here: the function reads no database
  state, takes no argument, performs no I/O, and returns a compile-time constant.
  **`trusted_schema` policy.** Porch does not set `PRAGMA trusted_schema` in either direction and
  does not rely on its value. Flagging the function innocuous makes the triggers behave identically
  whether the setting is on or off, and whether the database is opened by porch or by an operator's
  `sqlite3` shell — leaving the pragma alone is what keeps the fence independent of how the
  connection was configured.
- Two persistent triggers, `BEFORE INSERT ON runs` and `BEFORE UPDATE OF review_approved_head_sha
  ON runs`, each `RAISE(ABORT, …)` when `porch_writer_protocol()` is absent or below
  `min_writer_protocol`. The two cover run creation and approval — the surfaces identified in the
  write-path audit as reachable by an old binary. A binary without the function makes the trigger
  body fail, which aborts the write: absence fails closed.
- `Db::open` additionally performs the friendly check, so a compatible-but-too-old *new* binary
  reports a readable message instead of a raw SQLite trigger error.

**The upgrade is one atomic transaction** (FLOOR-6.5). Within a single immediate transaction, in
this order:

1. Install the marker table and its row, and the two triggers
   (`CREATE TABLE IF NOT EXISTS`, `CREATE TRIGGER IF NOT EXISTS`).
2. **Terminalize active legacy runs** — every run whose status is in `('pending','running','parked')`
   and which has no protocol-2 round becomes `failed`, with a stated reason naming the upgrade. This
   is what stops a run admitted under the legacy regime from being approved afterwards.
3. **Invalidate undelivered legacy approval state** — clear `runs.review_approved_head_sha`
   (`db.rs:135`) for those same runs, so an approval recorded before the upgrade but not yet
   delivered cannot authorize a forward under the new regime.

All three commit together or not at all: a kill mid-upgrade must never leave the triggers installed
while legacy runs remain approvable, nor legacy approvals cleared without the fence in place. The
transaction is idempotent — re-running it on an already-upgraded root matches zero rows in steps 2
and 3 and no-ops in step 1 — which matters because `Db::open` is on every binary path and will
attempt it on each start (`ensure_column` precedent, `db.rs:129-146`, preserving FLOOR-8.1).

`active_runs`' status set (`db.rs:421`, `:485`) is unchanged, preserving FLOOR-8.8 — the
terminalization moves legacy runs out of that set rather than redefining it.

### 5. `porch-review::floor` — the dedicated resolver

Satisfies: FLOOR-1.2, FLOOR-4.1, FLOOR-4.2, FLOOR-4.3, FLOOR-4.4, FLOOR-4.5, FLOOR-8.10,
FLOOR-8.11, FLOOR-9.2
Reuse: rung 7 — new module, but built almost entirely from rung-2 parts already in
`porch-review/src/plan.rs`: `observe_opaque_entrypoint` (`:488`), `stamp_path` (`:631`),
`composite_artifact_identity` (`:223`), `check_artifacts_stable` (`:204`),
`canonicalize_best_effort` (`:680`), `sha256_hex` (`:664`). New code is confined to sibling
resolution. It cannot reuse `plan::prepare` (`:178`), whose every branch routes through
`review_bin` / `agent_bin` / the wrapper — precisely the paths FLOOR-4.1 forbids.
Respects: ARCH-12, ARCH-10 (module inside `porch-review`, not a new crate), ARCH-9 (the floor
is the first-party quality engine — never a vendored or wrapped third-party review CLI), ARCH-4
Interface: `floor::resolve() -> Result<PreparedInvocation, Error>` — no parameters: taking a path,
a home, or a config handle would reintroduce the redirection this module exists to prevent.
Depth: if this module vanished, callers would still need to know only that the floor yields a
`PreparedInvocation` whose target is fixed by the installation. Sibling derivation, platform suffix,
canonicalization, artifact observation, and stamp capture all stay inside.
Locality: new file `crates/porch-review/src/floor.rs`; `lib.rs` **extend** (module declaration);
`plan.rs` **leave** — its wrapper and agent paths continue to serve the judgment producer
unchanged, which is exactly what FLOOR-8.10 and FLOOR-8.11 guard.

Resolution: `current_exe()` → canonicalize → take parent → join `porch-quality` plus
`std::env::consts::EXE_SUFFIX` → verify it exists and is executable. Failure at any step yields an
unresolved floor requirement carrying the reason, never a PATH search. The resolved artifact is
observed and stamped at resolve time; `check_artifacts_stable` re-runs before spawn (FLOOR-4.4).
The canonical path is recorded in the descriptor for observation and stability checking; equivalence
identity comes from observed content via `composite_artifact_identity`, and the resolver keeps the
two consistent (FLOOR-4.5).

Development and test builds inject the target through a compile-time or test-only seam rather than a
production environment override — an env-var escape hatch would defeat FLOOR-9.2 and is excluded.

### 6. `porch-run` — composition, ordering, and the pin

Satisfies: FLOOR-1.1, FLOOR-1.3, FLOOR-1.4, FLOOR-1.5, FLOOR-1.6, FLOOR-1.7, FLOOR-1.8, FLOOR-3.1,
FLOOR-3.2, FLOOR-3.3, FLOOR-3.5, FLOOR-3.8, FLOOR-3.9, FLOOR-5.3, FLOOR-5.4, FLOOR-5.5, FLOOR-5.6,
FLOOR-5.8, FLOOR-8.12, FLOOR-8.13, FLOOR-8.14, FLOOR-8.16, FLOOR-8.17, FLOOR-8.18, FLOOR-8.19,
FLOOR-8.22, FLOOR-9.1
Reuse: rung 2 — extends `open_review_round` (`porch-run/src/lib.rs:328`) and
`spawn_review_for_round` (`:460`); `rerun` (`:2080`) is reused unchanged.
Respects: ARCH-11, ARCH-12, ARCH-13, ARCH-3 (reviewer turns stay session-free; no floor output
enters the judgment turn)
Surface:
- `open_review_round` / `spawn_review_for_round` (private, in-repo) — **replace**: single-producer
  assumptions at `:389`, `:420`, `:436` migrate in this change.
- `crates/porch/tests/m3_review.rs`, `m10_agent_review.rs`, `m14_agent_run.rs` (tests asserting one
  review spawn per phase) — **replace**: updated to expect the composed round in this change.
Interface: unchanged from the caller's view — the review phase still returns a `ReviewOutcome`.
Depth: n/a — extends `porch-run`.
Locality: `porch-run/src/lib.rs` **extend**; `deliver.rs` **leave** (the forward boundary keeps
reading authorization state rather than gaining its own floor logic); `certify.rs` **leave**.

Sequence for one review attempt:

1. Resolve the floor (`floor::resolve`) and, when the selected engine is distinct from it, the
   judgment producer (`plan::prepare`). Both resolutions complete before anything is opened —
   preparation failure is distinguished from runtime failure here.
2. Compose the requirement list: slot 0 `floor`, slot 1 `judgment` when distinct (FLOOR-1.3); when
   the engine is `quality`, the floor is the only slot (FLOOR-1.4).
3. Compute the required-set digest and compare against the run pin when one exists (FLOOR-5.3).
   **The comparison happens before `open_round`.** On mismatch no round is created at all — there is
   no `incomplete` round for this case, because a round that was never opened cannot be finalized —
   and the run fails closed without spawning (FLOOR-5.4, FLOOR-5.5).
4. `open_round` atomically, setting the pin when it is NULL.
5. Spawn the floor. On success, spawn the judgment producer with its own declared context only
   (FLOOR-1.8). A valid floor result carrying blocking findings is a success and does not stop the
   judgment producer (FLOOR-1.7); a floor fault does (FLOOR-1.6).
6. Normalize, reconcile, finalize. Record each producer's duration alongside the round's total
   (FLOOR-9.1).

**Mismatch record** (closing the previously open representation question). A pin mismatch is
recorded as a `step_results` row (`db.rs:112-120`) for the `review` step with `status = 'failed'`
and a stable JSON `error` payload naming both shapes, plus the same payload on `runs.error`:

```json
{ "kind": "assurance_shape_mismatch",
  "pinned_digest": "<hex>",   "pinned_shape": "floor+judgment",
  "attempted_digest": "<hex>", "attempted_shape": "floor-only" }
```

This keeps the two failure families structurally distinct and separately assertable: an unsatisfiable
floor produces an `incomplete` **round**, while a pin mismatch produces **no round** and a failed
**step**. Both fail the run closed; only the former has a round to point at. FLOOR-7.4's operator
report reads its two shapes from this payload.

An unsatisfied floor finalizes the round `incomplete` with its reason and fails the run without
parking it (FLOOR-3.1, FLOOR-3.2, FLOOR-3.3). Reconciliation, coverage, blocking classification and
park behavior are untouched (FLOOR-8.12, FLOOR-8.13, FLOOR-8.14), as are the review park's response
verbs and the compose-park branch ordering (FLOOR-8.16, FLOOR-8.17).

### 7. `porch` operator surface — shape, diagnostics, attestation

Satisfies: FLOOR-3.4, FLOOR-3.6, FLOOR-3.7, FLOOR-7.1, FLOOR-7.2, FLOOR-7.3, FLOOR-7.4, FLOOR-8.15
Reuse: rung 2 — extends the RPC status builder (`porch-run/src/lib.rs:2005-2030`), the TUI
(`porch/src/tui.rs`), and the attestation block already merged into PR bodies by `porch-deliver`.
Respects: ARCH-11
Surface: assurance shape gets **one** operator-facing seam — the existing `AssuranceRecord`
(`porch-gate/src/rpc.rs:38`), a serde-tagged enum already carrying `review_round_id` and
`audit_identity` through every operator path. Extending it, rather than adding a field to each
consumer, is what keeps the shape from drifting between surfaces. Full reader inventory:
- `AssuranceRecord` (`rpc.rs:38`, three variants `Round` / `LegacySnapshot` / `None`) —
  **replace**: gains the assurance-shape field. `LegacySnapshot` and `None` report the shape as
  absent rather than fabricating one.
- `RunSnapshot` (`rpc.rs:272`) — **replace**: carries the extended record; no separate shape field.
- `AgentStatus` (`porch-run/src/lib.rs:1333`) — **replace**: same, via the record it already holds.
- `AgentRunSnapshot` (`porch-run/src/agent_run.rs:326`) — **replace**: same.
- `crates/porch/src/tui.rs` — **replace**: renders the shape from the record.
- `crates/porch-gate/porch-agent.md` (headless agent contract, external readers follow it) —
  **replace**: documents the new field in this change.
- Headless-agent contract tests and `m18_round_identity.rs` assertions over the serialized record
  (tests) — **replace**: updated in this change.
- PR attestation block (external readers — people and tooling reading delivered PRs) — **frozen**:
  the existing `porch-attestation` marker and `head_sha` semantics are preserved; the shape line is
  *added* within the managed block rather than altering what is already emitted.
Interface: assurance shape surfaces as a presentation label derived from the recorded requirement
roles, carried on `AssuranceRecord`; it is never an authorization input (FLOOR-7.3).
Depth: n/a — extends the operator surface.
Locality: `porch/src/main.rs`, `porch/src/tui.rs`, `porch-run/src/lib.rs` status builder **extend**;
`porch-review/src/setup.rs` **leave** (detect/apply/verify guarded, FLOOR-8.15).

A run whose floor requirement was unsatisfied is `failed`, and a failed run exposes no response
verbs at all — FLOOR-3.4 is discharged structurally rather than by filtering an action list.
Diagnostics print the copyable `porch rerun --run-id <ULID>` (FLOOR-3.6) and, when the recorded
reason indicates daemon environment or executable resolution, advise restarting the daemon
(FLOOR-3.7). A pin mismatch reports both the pinned and the attempted shape (FLOOR-7.4).

Exact copy for these messages is an Open Question, owned and gated to this slice.

## Seams for testing

Existing seams carry almost everything; exactly one new seam is introduced.

| Seam | Kind | Covers |
|---|---|---|
| `rounds::open_round` / `requirements_for_round` / `required_set_digest` | unit (`porch-gate/tests/m18_rounds.rs`) | FLOOR-2.1–2.4, 2.9, 5.1, 5.2, 6.1, 8.2, 8.3, 8.9, 9.3 |
| `rounds::finalize_round` | unit (`m18_rounds.rs`) | FLOOR-8.4, 9.4 |
| `rounds::applicable_round` / `applicable_round_for_run` | unit (`m18_rounds.rs`) | FLOOR-2.5–2.8, 5.7, 6.2, 6.3, 8.5, 8.6, 9.5 |
| `rounds::retention::pin_trusted_config` / `sweep_unreferenced` | unit (`m18_rounds.rs`) | FLOOR-8.7 |
| `Db::open` + `runs` triggers | unit (`porch-gate` tests) | FLOOR-6.4, 6.5, 8.1, 8.8 |
| **`porch_review::floor::resolve`** *(new seam)* | unit (`porch-review`) | FLOOR-1.2, 4.1–4.5, 8.10, 8.11, 9.2 |
| `plan::prepare` | unit (`porch-review`) | FLOOR-8.10, 8.11 (judgment paths unchanged) |
| `reconcile` / `coverage_state` | unit (`porch-review`) | FLOOR-8.13, 8.14 |
| `porch` gate run end-to-end (`crates/porch/tests/m19_floor.rs`, new file) | integration | FLOOR-1.1, 1.3–1.8, 3.1–3.3, 3.5, 3.6, 3.7, 3.8, 3.9, 5.3–5.6, 5.8, 7.1–7.4, 9.1 |
| existing `m3_review` / `m10_agent_review` / `m14_agent_run` / `m18_round_identity` | integration | FLOOR-8.12, 8.16–8.22, 8.15 |
| real `0.2.x` binary against an upgraded database | integration | FLOOR-6.4, 6.5 (fence proof) |

## Coverage check

Every requirement ID appears in exactly one `Satisfies:` line:

| Story | IDs | Section |
|---|---|---|
| 1 | 1.1, 1.3–1.8 | §6 |
| 1 | 1.2 | §5 |
| 2 | 2.1–2.4, 2.9 | §1 |
| 2 | 2.5–2.8 | §3 |
| 3 | 3.1–3.3, 3.5, 3.8, 3.9 | §6 |
| 3 | 3.4, 3.6, 3.7 | §7 |
| 4 | 4.1–4.5 | §5 |
| 5 | 5.1, 5.2 | §1 |
| 5 | 5.3–5.6, 5.8 | §6 |
| 5 | 5.7 | §3 |
| 6 | 6.1 | §2 |
| 6 | 6.2, 6.3 | §3 |
| 6 | 6.4, 6.5 | §4 |
| 7 | 7.1–7.4 | §7 |
| 8 | 8.1, 8.8, 8.21 | §4 |
| 8 | 8.2, 8.3, 8.4, 8.7, 8.9 | §2 |
| 8 | 8.5, 8.6, 8.20 | §3 |
| 8 | 8.10, 8.11 | §5 |
| 8 | 8.12–8.14, 8.16–8.19, 8.22 | §6 |
| 8 | 8.15 | §7 |
| 9 | 9.1 | §6 |
| 9 | 9.2 | §5 |
| 9 | 9.3 | §1 |
| 9 | 9.4 | §2 |
| 9 | 9.5 | §3 |

Deliberately unmapped: none.

**UI design section:** deliberately absent. The Step-2b predicate does not hold — no `Satisfies:` ID
is delivered through a browser-rendered surface. Porch ships a CLI and a terminal TUI, and
`docs/agents/project.md` records "Frontend: — no web surface" and "Browser E2E: none".
