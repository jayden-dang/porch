# Tasks: Review Round Identity

Feature code: ROUND
Status: Approved
Date: 2026-08-30
Execution-mode: unset
Requirements: ./requirements.md
Design: ./design.md

**Goal:** Every review becomes a durable, auditable round record with porch-owned finding identity.

**Architecture:** `porch-gate` gains a `rounds` module owning seven additive tables, ULID identity,
and a two-phase finalization keyed on a per-run `review_history_revision`. `porch-review` gains
stateless plan/identity/reconcile/coverage modules that mint nothing. `porch-run` orchestrates:
resolve plan → open round → spawn → normalize → reconcile → finalize.

**Tech Stack:** Rust 1.85 / edition 2024, rusqlite (WAL, foreign_keys ON), ulid, sha2, serde_json.

## Global Constraints

Sources (canonical; do not restate rules elsewhere):

- `docs/agents/project.md` — verify commands, posture, team. sha256:a3f5ba93c4d8
- `AGENTS.md` (**Non-negotiables**) — engineering rules. sha256:b0f2a01edc9c
- `docs/architecture/INDEX.md` — ARCH-1…ARCH-13. sha256:7d9d801f34d7

Every task inherits:

- Verify with `--workspace`: `cargo fmt --all --check`, `cargo check --workspace --all-targets`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- **ARCH-2** git via the CLI · **ARCH-3** session-free producer turns · **ARCH-4** repository-controlled
  executing config from the trusted SHA · **ARCH-6** forced `ask-user` preserved · **ARCH-10** use-case
  slices · **ARCH-11** porch alone issues outcomes · **ARCH-12** floor never substituted.
- No network in unit tests; PATH fakes and `tests/fixtures/`. English only. Glossary terms per
  `CONTEXT.md`. Integration tests named `crates/<slice>/tests/m<N>_<topic>.rs`.
- Requirement IDs live in task footers only — never in production or test source.
- **Concurrent occupancy:** feature `PRCMP` is `In-progress` and its plan modifies
  `crates/porch-gate/src/db.rs`, `crates/porch-run/src/lib.rs`, and `crates/porch/src/tui.rs`.
  Coordinate before editing those three; do not rebase over its work blindly.

## File structure

Create:

- `crates/porch-gate/src/rounds/mod.rs` — round store API: open, finalize, read_history, lookup
- `crates/porch-gate/src/rounds/schema.rs` — DDL and additive migration for seven tables
- `crates/porch-gate/src/rounds/applicability.rs` — the applicability tuple and producer-set match
- `crates/porch-gate/src/rounds/retention.rs` — trusted-config ref pin and sweep
- `crates/porch-review/src/plan.rs` — one-shot resolution, descriptor, effective context
- `crates/porch-review/src/identity.rs` — candidate key, criterion mapping, finding contract
- `crates/porch-review/src/reconcile.rs` — pure matcher over caller-supplied history
- `crates/porch-review/src/coverage_state.rs` — per-file coverage state derivation
- `tests/fixtures/reconcile/1/` — normative corpus, seven families, `MANIFEST.json`
- `crates/porch-gate/tests/m18_rounds.rs` — store, migration, applicability, retention
- `crates/porch/tests/m18_round_identity.rs` — end-to-end round lifecycle and legacy fallback

Modify:

- `crates/porch-gate/src/db.rs` — migration calls, `review_history_revision` column
- `crates/porch-gate/src/daemon.rs` — stale-round reconciliation at startup
- `crates/porch-gate/src/executor.rs` — widen the `RunExecutor` recovery contract
- `crates/porch-gate/src/rpc.rs` — `assurance_record`, legacy decode, hunk lookup
- `crates/porch-gate/src/eject.rs` — ref removal ordered after row deletion
- `crates/porch-run/src/lib.rs` — orchestration, terminal states, artifact namespace
- `crates/porch-review/src/lib.rs` — wire plan, identity, coverage; stop resolving at spawn
- `crates/porch-review/src/agent_review.rs` — per-invocation artifact paths
- `crates/porch-quality/src/lib.rs`, `crates/porch-quality/src/rules.rs` — expose `rule_id`
- `crates/porch/src/tui.rs` — legacy-snapshot marker
- `docs/usage.md`, `docs/install.md` — upgrade guidance

## Task 1: Round tables exist and a round opens durably

**Files:** Create `crates/porch-gate/src/rounds/schema.rs`, `crates/porch-gate/src/rounds/mod.rs`,
`crates/porch-gate/tests/m18_rounds.rs`. Modify `crates/porch-gate/src/db.rs`.
**Reuse:** rung 2 — extends `porch-gate::db` (`ensure_column`, `unchecked_transaction`, ULID minting)
**Interfaces:** Produces `rounds::open_round(plan, bindings) -> RoundId`, `RoundId`, `ExecutionState`,
`AssuranceCompletion`. Consumes `Db`.
**Depends-on:** none
**Steps:**
- [ ] Test: opening an old database applies the new tables and leaves existing rows readable.
- [ ] Test: `open_round` returns an id only after commit; a second open allocates ordinal 2.
- [ ] Test: a blob whose stored bytes disagree with its digest is refused.
- [ ] Implement the seven tables, CHECKs, indexes, and the `review_history_revision` column.
- [ ] Implement `open_round` under `BEGIN IMMEDIATE` with ordinal allocation.
- [ ] Run `cargo test --workspace`; expect pass. Commit.

_Requirements: ROUND-1.1, ROUND-1.2, ROUND-1.4, ROUND-1.5, ROUND-1.25, ROUND-1.26, ROUND-1.27, ROUND-1.28, ROUND-4.10, ROUND-5.5, ROUND-6.1, ROUND-6.2, ROUND-7.2_

## Task 2: Context elements bind what each layer received

**Files:** Modify `crates/porch-gate/src/rounds/mod.rs`, `crates/porch-gate/src/rounds/schema.rs`.
Test `crates/porch-gate/tests/m18_rounds.rs`.
**Reuse:** rung 2 — extends the round store from Task 1
**Interfaces:** Consumes `RoundId`. Produces `ContextElement`, `ContextApplication`, `SourceState`,
`SnapshotState`.
**Depends-on:** Task 1
**Steps:**
- [ ] Test: an absent element and a present-but-empty element record different source states.
- [ ] Test: an element over the 256 KiB ceiling stores its digest with snapshot omitted, source unchanged.
- [ ] Test: an element not supplied to a layer records `not_applied` with no effective digest.
- [ ] Implement element and application persistence with content-addressed snapshots.
- [ ] Run tests; expect pass. Commit.

_Requirements: ROUND-1.8, ROUND-1.9, ROUND-1.10, ROUND-1.11, ROUND-1.13, ROUND-1.14, ROUND-1.15_

## Task 3: One immutable invocation plan describes what will run

**Files:** Create `crates/porch-review/src/plan.rs`. Modify `crates/porch-review/src/lib.rs`,
`crates/porch-review/src/agent_review.rs`.
**Reuse:** rung 2 — extends `review_bin()` and `EngineKind`
**Interfaces:** Produces `plan::prepare(opts) -> PreparedInvocation`, `InvocationPlan`,
`ProducerDescriptor`. Consumes `EngineKind`.
**Depends-on:** none
**Steps:**
- [ ] Test: resolution happens once; the spawn uses the recorded absolute target and argv.
- [ ] Test: a wrapper's identity spans wrapper, backend, and argv — not the wrapper digest alone.
- [ ] Test: an unobservable version records `unavailable` with a reason, never a substitute.
- [ ] Test: an opaque entrypoint records that only the entrypoint was observed.
- [ ] Implement `prepare`, the descriptor, composite identity, and the post-spawn stability check.
- [ ] Run tests; expect pass. Commit.

_Requirements: ROUND-1.17, ROUND-1.18, ROUND-1.19, ROUND-1.20, ROUND-1.21, ROUND-1.22_

## Task 4: Findings carry a porch-owned contract and candidate key

**Files:** Create `crates/porch-review/src/identity.rs`. Modify `crates/porch-review/src/lib.rs`,
`crates/porch-quality/src/lib.rs`, `crates/porch-quality/src/rules.rs`.
**Reuse:** rung 2 — extends the map/normalize pass and `CommentOut`
**Interfaces:** Produces `identity::derive(finding, mapping) -> CandidateKey`, enriched `Finding`.
Consumes `ReviewComment`.
**Depends-on:** none
**Steps:**
- [ ] Test: a quality finding's `rule_id` reaches the output and maps to a canonical criterion.
- [ ] Test: a producer key is retained as provenance and never becomes the identity.
- [ ] Test: a deterministic producer's finding carries no model-style confidence.
- [ ] Test: scope-extending findings keep their forced `ask-user` action.
- [ ] Implement `rule_id` exposure, criterion mapping, anchor fallback, and the candidate key.
- [ ] Run tests; expect pass. Commit.

_Requirements: ROUND-3.3, ROUND-3.4, ROUND-3.12, ROUND-3.13, ROUND-3.14, ROUND-3.15, ROUND-3.16, ROUND-3.18, ROUND-3.21, ROUND-6.11, ROUND-6.12_

## Task 5: Coverage states are derived, never inferred

**Files:** Create `crates/porch-review/src/coverage_state.rs`. Modify `crates/porch-review/src/lib.rs`.
**Reuse:** rung 2 — extends `assert_coverage` and the existing derivation
**Interfaces:** Produces `coverage_state::derive_states(changed, output) -> Vec<CoverageEntry>`.
**Depends-on:** none
**Steps:**
- [ ] Test: a path present in output without a completion signal is not `completed`.
- [ ] Test: `failed` and `waived` carry reasons; `waived` carries authority; `completed` carries evidence.
- [ ] Test: a changed file missing without a skip still fails the review closed.
- [ ] Implement state derivation over the producer manifest.
- [ ] Run tests; expect pass. Commit.

_Requirements: ROUND-2.4, ROUND-2.5, ROUND-2.6, ROUND-2.7, ROUND-2.8, ROUND-2.9, ROUND-6.10_

## Task 6: Reconciliation matches conservatively against a normative corpus

**Files:** Create `crates/porch-review/src/reconcile.rs`, `tests/fixtures/reconcile/1/`.
Modify `crates/porch-review/src/lib.rs`.
**Reuse:** rung 7 — none; no existing code matches finding sets across rounds
**Interfaces:** Produces `reconcile(current, history) -> Proposal`, `History`, `Proposal`.
Consumes `CandidateKey`.
**Depends-on:** Task 4
**Steps:**
- [ ] Write the seven fixture families with expectations as mappings, plus `MANIFEST.json`.
- [ ] Test: moved code and a rewritten message reuse the prior fingerprint.
- [ ] Test: two distinct issues sharing a candidate key receive different fingerprints.
- [ ] Test: multi-producer duplicates collapse only on a common non-empty range intersection.
- [ ] Test: ambiguity mints; a prior instance nothing claims simply disappears.
- [ ] Implement matching, minting with the instance disambiguator, and the version-boundary rule.
- [ ] Run tests; expect pass. Commit.

_Requirements: ROUND-3.5, ROUND-3.6, ROUND-3.9, ROUND-3.10, ROUND-3.11, ROUND-3.19, ROUND-3.20, ROUND-3.23_

## Task 7: Finalization is atomic and revision-guarded

**Files:** Modify `crates/porch-gate/src/rounds/mod.rs`. Test `crates/porch-gate/tests/m18_rounds.rs`.
**Reuse:** rung 2 — extends the round store from Task 1
**Interfaces:** Produces `read_history(run_id) -> (HistoryRevision, Vec<StoredPriorInstance>)`,
`finalize_round(round_id, proposal, seen_revision) -> Finalized | Stale`.
**Depends-on:** Task 1, Task 2
**Steps:**
- [ ] Test: coverage, instances, and terminal state land in one transaction or not at all.
- [ ] Test: a revision changed between phases yields `Stale` and no durable finalization.
- [ ] Test: each instance gets a distinct id; instances sharing a fingerprint stay separate rows.
- [ ] Test: a contention-free finalization commits exactly two writes beyond the pre-ROUND path.
- [ ] Implement two-phase finalization, revision increment, and the three-retry bound.
- [ ] Run tests; expect pass. Commit.

_Requirements: ROUND-1.30, ROUND-2.1, ROUND-2.2, ROUND-2.3, ROUND-3.7, ROUND-3.8, ROUND-3.17, ROUND-3.22, ROUND-7.4, ROUND-7.5_

## Task 8: A round says whether it may authorize the current change

**Files:** Create `crates/porch-gate/src/rounds/applicability.rs`. Test `crates/porch-gate/tests/m18_rounds.rs`.
**Reuse:** rung 2 — extends the round store's stored digests
**Interfaces:** Produces `applicable_round(run_id, bindings, required) -> Applicable | RequiresNew`.
**Depends-on:** Task 7
**Steps:**
- [ ] Test: a pending, incomplete, interrupted, or under-covered round never authorizes.
- [ ] Test: differing only in `selection_source` or `declared_engine_kind` stays applicable.
- [ ] Test: an unavailable producer version never establishes equivalence.
- [ ] Test: a floor-plus-judgment round is not equivalent to a judgment-only round.
- [ ] Implement the applicability tuple and multiset producer-set correspondence.
- [ ] Run tests; expect pass. Commit.

_Requirements: ROUND-1.23, ROUND-1.24, ROUND-1.31, ROUND-4.11, ROUND-4.12, ROUND-4.13, ROUND-4.14_

## Task 9: The review phase drives the round lifecycle end to end

**Files:** Modify `crates/porch-run/src/lib.rs`, `crates/porch-review/src/agent_review.rs`.
Test `crates/porch/tests/m18_round_identity.rs`.
**Reuse:** rung 2 — extends `run_review_phase` and `resolve_review_from`
**Interfaces:** Consumes `plan::prepare`, `open_round`, `reconcile`, `finalize_round`.
**Depends-on:** Task 3, Task 5, Task 6, Task 7
**Steps:**
- [ ] Test: a failed round open aborts before any producer is spawned.
- [ ] Test: timeout, unsuccessful exit, malformed output, and coverage shortfall each finalize
      `finished`/`incomplete` with distinct reasons; a clean run finalizes `complete`.
- [ ] Test: blocking findings still park the run and still finalize `complete`.
- [ ] Test: two rounds of one run keep separate artifacts under their invocation namespaces.
- [ ] Test: approve records the head SHA; skip leaves it unrecorded; post-fix `from_sha` is unchanged.
- [ ] Implement the sequence, terminal-state mapping, and per-invocation artifact paths.
- [ ] Run tests; expect pass. Commit.

_Requirements: ROUND-1.3, ROUND-1.32, ROUND-4.1, ROUND-4.2, ROUND-4.3, ROUND-4.4, ROUND-4.5, ROUND-4.6, ROUND-4.9, ROUND-6.7, ROUND-6.8, ROUND-6.9, ROUND-6.14_

## Task 10: A killed review is reconciled at startup

**Files:** Modify `crates/porch-gate/src/daemon.rs`, `crates/porch-gate/src/executor.rs`,
`crates/porch-gate/src/rounds/mod.rs`. Test `crates/porch/tests/m18_round_identity.rs`.
**Reuse:** rung 2 — extends `recover_stale`
**Interfaces:** Consumes `RunExecutor::recover_stale`. Produces `rounds::reconcile_stale`.
**Depends-on:** Task 7
**Steps:**
- [ ] Test: a process killed at each boundary leaves a round restart reconciles to
      `interrupted`/`incomplete`, with no instances and no approval.
- [ ] Test: reconciliation uses at most one committed write per stale round.
- [ ] Test: startup still recovers stale runs and still refuses to serve when recovery fails.
- [ ] Implement stale-round reconciliation behind the existing recovery contract.
- [ ] Run tests; expect pass. Commit.

_Requirements: ROUND-4.7, ROUND-4.8, ROUND-6.3, ROUND-6.6, ROUND-7.3, ROUND-7.6_

## Task 11: Trusted-config commits stay reachable while a round needs them

**Files:** Create `crates/porch-gate/src/rounds/retention.rs`. Modify `crates/porch-gate/src/eject.rs`.
Test `crates/porch-gate/tests/m18_rounds.rs`.
**Reuse:** rung 2 — `porch_git` CLI plumbing; `refs/porch/recover/<run_id>` namespace pattern
**Interfaces:** Produces `retention::pin_trusted_config(bare, sha)`, `retention::sweep_unreferenced(bare)`.
**Depends-on:** Task 1
**Steps:**
- [ ] Test: opening a round pins its trusted commit; the ref survives a gc-style prune.
- [ ] Test: removing the last referencing round removes the ref, and row deletion commits first.
- [ ] Implement pin, sweep, and the ordered purge path.
- [ ] Run tests; expect pass. Commit.

_Requirements: ROUND-1.12, ROUND-1.16, ROUND-1.29_

## Task 12: Operators read rounds, and legacy runs still answer

**Files:** Modify `crates/porch-gate/src/rpc.rs`, `crates/porch/src/tui.rs`,
`crates/porch-gate/src/id.rs`, `docs/usage.md`, `docs/install.md`.
Test `crates/porch/tests/m18_round_identity.rs`.
**Reuse:** rung 2 — extends the snapshot builder and `get_finding_hunk_result`
**Interfaces:** Produces `assurance_record`, `LegacyFindingDto`, `StatusFindingDto`.
**Depends-on:** Task 7, Task 8
**Steps:**
- [ ] Test: a parked decision backed by an applicable round serves findings from that round.
- [ ] Test: a pre-migration parked run answers approve/fix/skip/abort, notes, and hunk lookup,
      labelled `legacy_snapshot`; an unreviewed run reports `none`.
- [ ] Test: legacy rows decode through their own DTO with no enriched field defaulted in.
- [ ] Test: `repo_id_for` returns the same value for the same absolute path.
- [ ] Implement the record variants, both DTOs, the TUI marker, and the upgrade guidance.
- [ ] Run tests; expect pass. Commit.

_Requirements: ROUND-5.1, ROUND-5.2, ROUND-5.3, ROUND-5.4, ROUND-6.4, ROUND-6.5, ROUND-6.13, ROUND-6.15_
