# Tasks: Mandatory Deterministic Floor

> **For agentic workers:** after plan approval, pick one execute skill —
> `build-in-waves`, `build-by-story`, or `build-inline`. The chosen skill writes
> `Execution-mode:`.

Feature code: FLOOR
Status: In-progress
Date: 2026-09-02
Execution-mode: continuous
Max-concurrency: auto
Requirements: ./requirements.md
Design: ./design.md

**Goal:** Make authorization prove the deterministic floor ran — a policy-owned required set
recorded per round, a dedicated floor resolver, and a fenced protocol boundary.

**Architecture:** `porch-gate` gains a `rounds::requirements` module owning one additive table
whose CHECK makes resolved/unresolved rows structurally honest, plus a run-level pin and a
DB-resident compatibility fence. `porch-review` gains a `floor` resolver that reaches
`porch-quality` as a canonical sibling of the running executable and never through config.
`porch-run` composes floor-then-judgment sequentially and compares the pin before opening a round.

**Tech Stack:** Rust 2024 workspace; rusqlite 0.32 (`bundled`, **`functions` to be added`**);
`cargo test` / clippy; fake `gh` and PATH fakes in `crates/porch/tests/`.

## Global Constraints

- `AGENTS.md` sha256:`b0f2a01edc9c` — Non-negotiables; English; no network in unit tests; PATH
  fakes; use-case slices; `unsafe_code = "forbid"`.
- `docs/architecture/INDEX.md` sha256:`7d9d801f34d7` — **ARCH-4** (code-executing config is
  trusted), **ARCH-9** (floor is first-party, never a wrapped third-party CLI), **ARCH-10** (no
  crate per layer — the resolver is a module), **ARCH-11** (Porch alone issues outcomes),
  **ARCH-12** (floor always runs, never substitutable), **ARCH-13** (durable authorization before
  any external forward).
- `docs/agents/project.md` sha256:`a3f5ba93c4d8` — verify, in order and all with `--workspace`:
  `cargo fmt --all --check`, `cargo check --workspace --all-targets`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
  `--workspace` is not optional: `default-members = ["crates/porch"]`.
- `CONTEXT.md` sha256:`5b19c396a806` — vocabulary; **Required producer set** and **Assurance
  shape** are the canonical terms.
- Solo band — no fake multi-assignee fields.
- Do **not** put requirement IDs in Rust source or test names.
- No `docs/standards/` tree and no `docs/codebase/` docs exist; `docs/agents/project.md` is the
  fallback SSOT for commands and layout. Not invented elsewhere.

## File Structure

| File | Responsibility |
|---|---|
| `crates/porch-gate/src/rounds/requirements.rs` | **Create** — requirement rows, roles, resolution, canonical required-set digest |
| `crates/porch-review/src/floor.rs` | **Create** — dedicated floor resolver (canonical sibling, no config, no PATH) |
| `crates/porch/tests/m19_floor.rs` | **Create** — end-to-end gate runs over the composed round |
| `crates/porch-gate/src/rounds/schema.rs` | DDL for `round_required_producers`, `round_producer_durations`, additive columns |
| `crates/porch-gate/src/rounds/mod.rs` | `open_round` writes requirements + run pin; `finalize_round` writes durations |
| `crates/porch-gate/src/rounds/applicability.rs` | Authorization reads the recorded set; digest equality; protocol gate |
| `crates/porch-gate/src/db.rs` | Run pin column, `porch_state_meta`, writer function, triggers, upgrade transaction |
| `crates/porch-gate/src/rpc.rs` | `AssuranceRecord` carries assurance shape |
| `crates/porch-review/src/lib.rs` | Declare the `floor` module |
| `crates/porch-run/src/lib.rs` | Compose the required set, sequential spawn, pin compare, failure mapping, status |
| `crates/porch-run/src/agent_run.rs` | `AgentRunSnapshot` carries the extended record |
| `crates/porch/src/main.rs`, `crates/porch/src/tui.rs` | Operator diagnostics and shape display |
| `crates/porch-gate/porch-agent.md` | Headless agent contract documents the shape field |
| `Cargo.toml` | Enable rusqlite `functions` feature |
| `docs/usage.md`, `docs/install.md` | Upgrade, rollback, and recovery guidance |
| `crates/porch-gate/tests/m18_rounds.rs`, `crates/porch/tests/m18_round_identity.rs` | Updated constructors and guard assertions |
| `crates/porch/tests/m3_review.rs`, `m10_agent_review.rs`, `m14_agent_run.rs` | Updated single-spawn expectations |

---

### Task 1: Requirement rows and the canonical digest

**Files:**
- Create: `crates/porch-gate/src/rounds/requirements.rs`
- Modify: `crates/porch-gate/src/rounds/schema.rs`, `crates/porch-gate/src/rounds/mod.rs`
- Test: `crates/porch-gate/tests/m18_rounds.rs`

**Reuse:** rung 7 — new module; digest preimage built on `length_delimited_join` and `sha256_hex`
(`crates/porch-review/src/plan.rs`)

**Interfaces:**
- Produces: `RequirementRow`, `Role`, `Resolution`, `required_set_digest(i64, &[RequirementRow])
  -> String`, `requirements_for_round(&Db, &RoundId) -> Result<Vec<RequirementRow>>`; `OpenRoundPlan`
  gains `requirements: Vec<RequirementSpec>`
- Consumes: `OpenRoundPlan`, `RoundBindings`, `RoundId`

**Depends-on:** none

- [ ] Test: a resolved row without an invocation reference or without a digest is rejected by the
      table; an unresolved row carrying either, or with a blank reason, is rejected.
- [ ] Test: requirement rows are written inside `open_round`'s transaction — a forced failure
      leaves no round and no requirement rows.
- [ ] Test: the digest changes when a slot's role, resolution, or expected digest changes, and does
      **not** change when only `resolution_reason` changes.
- [ ] Run `cargo test -p porch-gate --test m18_rounds` — expect failures on missing table and API.
- [ ] Implement the table with its CHECK and composite FK, the row types, and the
      domain-separated length-delimited digest preimage.
- [ ] Run tests; expect pass. Commit: `feat(porch-gate): record the required producer set per round`

_Requirements: FLOOR-2.1, FLOOR-2.2, FLOOR-2.3, FLOOR-2.4, FLOOR-2.9, FLOOR-5.1, FLOOR-9.3_

---

### Task 2: The floor resolver

**Files:**
- Create: `crates/porch-review/src/floor.rs`
- Modify: `crates/porch-review/src/lib.rs`
- Test: unit tests in `crates/porch-review/src/floor.rs`

**Reuse:** rung 7 — new module built from rung-2 helpers in `crates/porch-review/src/plan.rs`
(`observe_opaque_entrypoint`, `stamp_path`, `composite_artifact_identity`,
`check_artifacts_stable`, `canonicalize_best_effort`)

**Interfaces:**
- Produces: `floor::resolve() -> Result<PreparedInvocation, Error>` — no parameters
- Consumes: `PreparedInvocation`, `InvocationPlan`, `ProducerDescriptor`

**Depends-on:** none

- [ ] Test: with `PORCH_REVIEW_BIN`, `PORCH_REVIEW_AGENT_BIN`, `review.bin`, and a hostile
      `$PORCH_HOME/bin/review` all pointing at a substitute executable, the resolved target is
      still the canonical sibling.
- [ ] Test: a symlinked launch path resolves to the same canonical target on both invocations.
- [ ] Test: when no executable sibling exists the call returns an unresolved outcome with a reason
      and performs no PATH lookup.
- [ ] Test: the recorded canonical path and the content-derived equivalence identity stay
      consistent, and a replaced artifact fails the pre-spawn stability check.
- [ ] Run `cargo test -p porch-review` — expect failure on the missing module.
- [ ] Implement sibling derivation with the platform executable suffix, artifact observation and
      stamping, and a test-only injection seam that is absent from production builds.
- [ ] Run tests; expect pass. Commit: `feat(porch-review): resolve the mandatory floor as a sibling`

_Requirements: FLOOR-1.2, FLOOR-4.1, FLOOR-4.2, FLOOR-4.3, FLOOR-4.4, FLOOR-4.5, FLOOR-8.10, FLOOR-8.11, FLOOR-9.2_

---

### Task 3: Walking skeleton — compose and run floor then judgment

**Files:**
- Create: `crates/porch/tests/m19_floor.rs`
- Modify: `crates/porch-run/src/lib.rs`
- Test: `crates/porch/tests/m19_floor.rs`, `crates/porch/tests/m3_review.rs`,
  `crates/porch/tests/m10_agent_review.rs`, `crates/porch/tests/m14_agent_run.rs`

**Reuse:** rung 2 — extends `open_review_round` and `spawn_review_for_round`
(`crates/porch-run/src/lib.rs`)

**Interfaces:**
- Consumes: `floor::resolve`, `plan::prepare`, `RequirementSpec`, `required_set_digest`
- Produces: composed `OpenRoundPlan` with slot 0 `floor` and optional slot 1 `judgment`

**Depends-on:** Task 1, Task 2

- [ ] Test: on `engine: agent`, one round records a resolved floor requirement and a resolved
      judgment requirement, and the floor's invocation finishes before the judgment spawn starts.
- [ ] Test: on `engine: quality`, the round records the floor alone and still forwards.
- [ ] Test: a floor result carrying blocking findings still runs the judgment producer, and the
      run parks on the merged findings.
- [ ] Test: the judgment producer's recorded context applications contain no floor-output element.
- [ ] Run `cargo test -p porch --test m19_floor` — expect failure on single-producer composition.
- [ ] Implement composition, the sequential spawn, and per-slot context application; update the
      three existing suites that assert one review spawn per phase.
- [ ] Run tests; expect pass. Commit: `feat(porch-run): compose the floor with the judgment layer`

_Requirements: FLOOR-1.1, FLOOR-1.3, FLOOR-1.4, FLOOR-1.5, FLOOR-1.6, FLOOR-1.7, FLOOR-1.8, FLOOR-8.12, FLOOR-8.13, FLOOR-8.14_

---

### Task 4: Authorization reads the recorded set

**Files:**
- Modify: `crates/porch-gate/src/rounds/applicability.rs`
- Test: `crates/porch-gate/tests/m18_rounds.rs`

**Reuse:** rung 2 — extends `applicable_round` and `round_is_applicable`
(`crates/porch-gate/src/rounds/applicability.rs`)

**Interfaces:**
- Consumes: `requirements_for_round`, `RequirementRow`
- Produces: `applicable_round` taking recorded requirements instead of a derived digest list

**Depends-on:** Task 1

- [ ] Test: a round whose producers no longer correspond one-to-one with its resolved requirements
      is not applicable, in both directions of the mismatch.
- [ ] Test: a requirement whose `expected_equivalence_digest` differs from the referenced
      invocation's recorded digest is not applicable, even though the FK is satisfied.
- [ ] Test: any unresolved requirement, and a round with zero requirement rows, never authorize.
- [ ] Test: rounds differing only in selection source or declared engine kind stay applicable, and
      an unavailable producer version still never establishes equivalence.
- [ ] Run `cargo test -p porch-gate --test m18_rounds` — expect failure while the required set is
      still reconstructed.
- [ ] Implement the recorded-set comparison, deleting the derivation in `decision_bindings_for_run`.
- [ ] Run tests; expect pass. Commit: `fix(porch-gate): authorize from the recorded required set`

_Requirements: FLOOR-2.5, FLOOR-2.6, FLOOR-2.7, FLOOR-2.8, FLOOR-8.5, FLOOR-8.6, FLOOR-9.5_

---

### Task 5: An unsatisfiable floor fails closed and reruns

**Files:**
- Modify: `crates/porch-run/src/lib.rs`
- Test: `crates/porch/tests/m19_floor.rs`

**Reuse:** rung 2 — existing `incomplete` finalization paths and `rerun`
(`crates/porch-run/src/lib.rs`)

**Interfaces:**
- Consumes: `floor::resolve` unresolved outcome, `finalize_round`
- Produces: floor-unsatisfied terminal mapping

**Depends-on:** Task 3

- [ ] Test: an unresolvable floor finalizes the round `incomplete` with a reason naming the floor,
      fails the run, and leaves it in no parked phase.
- [ ] Test: floor timeout, non-zero exit, malformed output, coverage shortfall, and artifact
      instability each finalize `incomplete` and never spawn the judgment producer.
- [ ] Test: the failed run and its round survive as readable records, and no branch is forwarded.
- [ ] Test: `porch rerun --run-id` on that run starts a new run that independently resolves the
      floor and carries no approval state forward.
- [ ] Run `cargo test -p porch --test m19_floor` — expect failure.
- [ ] Implement the terminal mapping and the fail-closed forward guard.
- [ ] Run tests; expect pass. Commit: `feat(porch-run): fail closed when the floor is unsatisfiable`

_Requirements: FLOOR-3.1, FLOOR-3.2, FLOOR-3.3, FLOOR-3.5, FLOOR-3.8, FLOOR-3.9, FLOOR-8.22_

---

### Task 6: The run pin and shape mismatch

**Files:**
- Modify: `crates/porch-gate/src/db.rs`, `crates/porch-gate/src/rounds/mod.rs`,
  `crates/porch-run/src/lib.rs`
- Test: `crates/porch-gate/tests/m18_rounds.rs`, `crates/porch/tests/m19_floor.rs`

**Reuse:** rung 2 — `ensure_column` migration helper (`crates/porch-gate/src/db.rs`) and
`open_round`'s immediate transaction

**Interfaces:**
- Produces: `runs.required_set_digest` column,
  `run_required_set_digest(&Db, run_id) -> Result<Option<String>>`
- Consumes: `required_set_digest`

**Depends-on:** Task 1, Task 3

- [ ] Test: the pin is set with the first round in one transaction; a forced failure leaves neither.
- [ ] Test: a second round whose required-set digest matches proceeds; one that differs fails the
      run **before** `open_round`, creating no round.
- [ ] Test: a mismatch is recorded on the run's `review` step with both the pinned and attempted
      digests and shapes, and strengthening is rejected exactly like weakening.
- [ ] Test: a changed producer artifact identity is a mismatch even with unchanged configuration,
      and a missing judgment producer is `incomplete` rather than a floor-only round.
- [ ] Run `cargo test -p porch-gate --test m18_rounds` then `cargo test -p porch --test m19_floor`
      — expect failure (one `--test` per package; cargo rejects a combined `-p`/`--test` set).
- [ ] Implement the column, the `IS NULL`-guarded pin write, the pre-open comparison, and the
      mismatch payload.
- [ ] Run tests; expect pass. Commit: `feat(porch): pin the run assurance contract at first round`

_Requirements: FLOOR-5.2, FLOOR-5.3, FLOOR-5.4, FLOOR-5.5, FLOOR-5.6, FLOOR-5.7, FLOOR-5.8_

---

### Task 7: Protocol 2, legacy rounds, and durations

**Files:**
- Modify: `crates/porch-gate/src/rounds/schema.rs`, `crates/porch-gate/src/rounds/mod.rs`,
  `crates/porch-gate/src/rounds/applicability.rs`, `crates/porch-run/src/lib.rs`
- Test: `crates/porch-gate/tests/m18_rounds.rs`

**Reuse:** rung 2 — existing `protocol_schema_version` column and `finalize_round` transaction

**Interfaces:**
- Produces: `round_producer_durations` table, `review_rounds.review_duration_ms`
- Consumes: `RoundBindings.protocol_schema_version`

**Depends-on:** Task 4

- [ ] Test: rounds opened by this feature record protocol version 2; a version-1 round is never
      applicable and is left byte-for-byte unchanged with no requirement rows invented.
- [ ] Test: a round recording a version above the one understood fails closed.
- [ ] Test: opening an older database applies the additive tables and columns and leaves existing
      rows readable; invocation rows keep non-null descriptor and digest; a second round still
      allocates ordinal 2 under an immediate transaction.
- [ ] Test: finalization writes coverage, instances, terminal state and durations in one
      transaction or none, yields `Stale` on a changed revision, and per-producer plus total
      durations are readable afterwards.
- [ ] Run `cargo test -p porch-gate --test m18_rounds` — expect failure.
- [ ] Implement the version gate, the duration tables, and the finalization writes.
- [ ] Run tests; expect pass. Commit: `feat(porch-gate): protocol 2 rounds with recorded durations`

_Requirements: FLOOR-6.1, FLOOR-6.2, FLOOR-6.3, FLOOR-8.1, FLOOR-8.2, FLOOR-8.3, FLOOR-8.4, FLOOR-8.7, FLOOR-8.9, FLOOR-9.1, FLOOR-9.4_

---

### Task 8: The compatibility fence

**Files:**
- Modify: `crates/porch-gate/src/db.rs`, `Cargo.toml`
- Test: `crates/porch-gate/tests/m18_rounds.rs`, `crates/porch/tests/m19_floor.rs`

**Reuse:** rung 5 — `rusqlite` `create_scalar_function` (already installed; enable the `functions`
feature in `Cargo.toml`)

**Interfaces:**
- Produces: `porch_state_meta` table, `porch_writer_protocol()` SQL function, two `runs` triggers
- Consumes: `Db::open`

**Depends-on:** Task 6, Task 7

- [ ] Test: a connection without the registered function cannot insert a run and cannot write an
      approval; a registered connection can do both.
- [ ] Test: the upgrade transaction installs marker and triggers, fails active legacy runs, and
      clears their undelivered approvals — all present or all absent after a forced mid-upgrade
      failure — and re-running it changes nothing.
- [ ] Test: contending-run listing still counts only pending, running, and parked; daemon startup
      still recovers stale runs and still refuses to serve when recovery fails.
- [ ] Test (integration): a real `0.2.x` binary against an upgraded database cannot create a run
      or approve one.
- [ ] Run `cargo test --workspace` — expect failure.
- [ ] Implement the marker, the function with `SQLITE_UTF8 | SQLITE_DETERMINISTIC |
      SQLITE_INNOCUOUS` and never `SQLITE_DIRECTONLY`, the two triggers, and the atomic upgrade.
- [ ] Run tests; expect pass. Commit: `feat(porch-gate): fence the upgraded state root`

_Requirements: FLOOR-6.4, FLOOR-6.5, FLOOR-8.8, FLOOR-8.21_

---

### Task 9: Operator surface for the assurance shape

**Files:**
- Modify: `crates/porch-gate/src/rpc.rs`, `crates/porch-run/src/lib.rs`,
  `crates/porch-run/src/agent_run.rs`, `crates/porch/src/main.rs`, `crates/porch/src/tui.rs`,
  `crates/porch-gate/porch-agent.md`, `docs/usage.md`, `docs/install.md`
- Test: `crates/porch/tests/m19_floor.rs`, `crates/porch/tests/m18_round_identity.rs`

**Reuse:** rung 2 — extends `AssuranceRecord` (`crates/porch-gate/src/rpc.rs`), the status builder,
and the managed PR attestation block

**Interfaces:**
- Produces: assurance shape on `AssuranceRecord`, carried by `RunSnapshot`, `AgentStatus`,
  `AgentRunSnapshot`
- Consumes: `requirements_for_round`

**Depends-on:** Task 6

- [ ] Test: run status reports the shape for a floor-only and a floor+judgment run; legacy and
      unreviewed records report the shape as absent rather than a fabricated value.
- [ ] Test: the delivered PR attestation states the shape while keeping its existing marker and
      head-SHA semantics.
- [ ] Test: a failed floor-blocked run exposes no response verb at all, and its diagnostics carry a
      copyable `porch rerun --run-id`, plus daemon-restart advice when the cause is resolution.
- [ ] Test: a pin mismatch reports both the pinned and the attempted shape; setup detect, apply and
      verify are unchanged.
- [ ] Run `cargo test -p porch --test m19_floor` — expect failure.
- [ ] Implement the record field and every reader, update the headless contract, and document
      upgrade, rollback and recovery.
- [ ] Run tests; expect pass. Commit: `feat(porch): surface the assurance shape to operators`

_Requirements: FLOOR-3.4, FLOOR-3.6, FLOOR-3.7, FLOOR-7.1, FLOOR-7.2, FLOOR-7.3, FLOOR-7.4, FLOOR-8.15_

---

### Task 10: Park, approval and legacy-serve regression sweep

**Files:**
- Modify: `crates/porch-run/src/lib.rs`
- Test: `crates/porch/tests/m18_round_identity.rs`, `crates/porch/tests/m19_floor.rs`

**Reuse:** rung 2 — existing park, `agent_respond`, and legacy-snapshot serve paths

**Interfaces:**
- Consumes: `AgentResponse`, `AssuranceRecord`
- Produces: none — this task guards existing behavior

**Depends-on:** Task 9

- [ ] Test: a review park still accepts approve, fix, skip and abort, and a compose park still
      offers only respond, skip and abort with its branch taken before the review skip path.
- [ ] Test: approve still records the head SHA, skip still leaves it unrecorded, and a post-fix
      round still leaves the originating `from_sha` unchanged.
- [ ] Test: a pre-round parked run still answers approve, fix, skip, abort, notes and hunk lookup
      through its legacy snapshot.
- [ ] Run `cargo test --workspace` — expect any regression from Tasks 3–9 to surface here.
- [ ] Fix whatever the sweep exposes without weakening the new fail-closed rules.
- [ ] Run the full verify sequence; expect pass. Commit: `test(porch): guard park and legacy serve`

_Requirements: FLOOR-8.16, FLOOR-8.17, FLOOR-8.18, FLOOR-8.19, FLOOR-8.20_
