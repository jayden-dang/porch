# Tasks: Audit evidence trace

> **For agentic workers:** after plan approval, pick one execute skill —
> `build-in-waves` (continuous dependency-aware scheduler), `build-by-story` (human-gated story review
> units), or `build-inline` (controller implements, no implementer subagents).
> The chosen skill writes `Execution-mode:`. Steps use checkbox (`- [ ]`) syntax
> for tracking.

Feature code: TRACE
Status: In-progress
Date: 2026-09-08
Execution-mode: continuous
Max-concurrency: auto
Requirements: ./requirements.md
Design: ./design.md

**Goal:** Project producer identity, per-path coverage, the finding-to-producer key, and the round's config/protocol pins into the existing audit document, and print producers plus non-completed coverage on default `porch audit`.

**Architecture:** Private `load_producers` / `load_coverage` in `audit.rs` join on `run_id` inside the existing Deferred `build_audit` transaction (same shape as `load_instances`). No `_conn` readers and no `porch-review` dependency; parse `descriptor_json` with a gate-local DTO. Human render is a sibling of `render_phase_tree`. `rounds/mod.rs` and `applicability.rs` stay untouched.

**Tech Stack:** Rust 2024; `rusqlite`; `serde`/`serde_json`; `clap`; `assert_cmd` + `tempfile` integration tests.

## Global Constraints

Source: `docs/agents/project.md` (sha256 `a3f5ba93c4d8ec55de8d1fe30128ac8095d2c016233f6e49801f3664ed0a5400`) plus `AGENTS.md` (sha256 `b0f2a01edc9c16c9db95010b243435404f66863093e43b578816d2c5530dcc24`) **Non-negotiables** and `docs/architecture/INDEX.md` (sha256 `7d9d801f34d79f6493ef69973118ea0fbf324030d010a037eb2f59ce3642ee2b`). No `docs/standards/` or `docs/codebase/` tree; do not invent a parallel SSOT.

- Verify, in order, all must pass before any completion claim: `cargo fmt --all --check` · `cargo check --workspace --all-targets` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace`. `--workspace` is not optional.
- Single file: `cargo test -p porch --test m22_audit_trace` (or `m20_dispo` / `m21_phase`).
- `unsafe_code = "forbid"`. Fix a pedantic warning or `allow` it with a reason.
- Do not add `/// REQ:` or `@TRACE-N.M` to Rust source or test names.
- Integration tests are named for the milestone that introduced them: this feature's is `crates/porch/tests/m22_audit_trace.rs`.
- English only. No network in unit tests. No vendored third-party source. No new crate dependency (`porch-gate` must not depend on `porch-review`).
- **ARCH-1 … ARCH-13** are binding. This feature relies on **ARCH-4**, **ARCH-10**, **ARCH-11**, **ARCH-12**.
- Do not edit `crates/porch-gate/src/rounds/mod.rs` or `crates/porch-gate/src/rounds/applicability.rs`. Do not call `producers_for_round` / `coverage_for_round` from inside `build_audit` (self-deadlock on `Db::conn`).
- The only allowed `crates/porch/src/tui.rs` edit is `assert_eq!(loaded.schema_version, 2)` → `3`. Do not change fetch-on-subscribe or `history_panel_text`.
- Do not invent a `ROAD-N` or claim MILE-2 Closed. Catalog Status stays Draft until the feature ships.
- Team band: **Solo** (roster: Tech Lead — Jayden Đặng). No multi-assignee ceremony.

## File structure

| File | Responsibility |
|---|---|
| `crates/porch-gate/src/audit.rs` | types, private loaders, `build_audit` join, `schema_version` 3, local DTO |
| `crates/porch-gate/src/lib.rs` | re-export `AuditProducer`, `AuditCoverage` |
| `crates/porch/src/main.rs` | `render_evidence_blocks`; `run_human_audit` prints evidence then the tree |
| `crates/porch/src/tui.rs` | one-line `schema_version` assert `2` → `3` |
| `crates/porch/tests/m22_audit_trace.rs` | **new** — `build_audit` + CLI coverage for this feature |
| `crates/porch/tests/m20_dispo.rs` | bump `schema_version` assert `2` → `3` |
| `crates/porch/tests/m21_phase.rs` | bump `schema_version` asserts; prepend evidence lines on default-stdout pins |
| `crates/porch-gate/porch-agent.md` | `schema_version` 3; independent of `PROTOCOL_SCHEMA_VERSION` |
| `docs/usage.md` | same two-counter note |

---

### Task 1: Producer slice and audit schema 3

**Files:**
Create `crates/porch/tests/m22_audit_trace.rs`. Modify `crates/porch-gate/src/audit.rs`, `crates/porch-gate/src/lib.rs`, `crates/porch/tests/m20_dispo.rs` (assert at line 1441), `crates/porch/tests/m21_phase.rs` (asserts at lines 1340 and 1387), `crates/porch/src/tui.rs` (assert at line 1375 only).
**Reuse:** rung 2 — extends `crates/porch-gate/src/audit.rs` `build_audit` / `load_rounds` / `load_instances`
**Interfaces:**
- Consumes: `build_audit(&Db, &str) -> Result<AuditDocument>`, `Db::upsert_repo`, `Db::insert_run` (leave status `pending` so DISPO-6.12 does not occupy `anomaly`), `rounds::open_round`, `OpenRoundPlan`, `RoundBindings`, `ProducerInvocation`, `capture_context_element`, `ContextSource`, `ContextApplication`, `ContextApplicationState`, `sha256_hex`, `context_applicability_digest`. Copy the body of private `open_stale_round` (`crates/porch/tests/m18_round_identity.rs`); do not import it. `inventory_digest` must be SHA-256 of `inventory_bytes`.
- Produces: `AuditProducer { round_id, id, slot, descriptor_equivalence_digest, adapter_kind: AuditText, declared_engine_kind: AuditText, reported_version: AuditReportedVersion, observed_version_identity: AuditObservedIdentity }`. `AuditText` is untagged `String` or `{ unavailable: String }`. `AuditReportedVersion` is `{ unavailable: String }`. `AuditObservedIdentity` is `{ artifact_sha256: String }` or `{ unavailable: String }` (not `AuditText`). `AuditDocument.producers` + `schema_version: 3`; `#[serde(default)]` on `producers` and later scalars; `fn load_producers(tx: &Transaction<'_>, run_id: &str) -> Result<Vec<AuditProducer>>` with the design SQL; gate-local DTO (not `porch_review::ProducerDescriptor`); anomaly `unreadable_producer_descriptor` only when `anomaly` is `None`; `lib.rs` re-exports `AuditProducer`.
**Depends-on:** none
**Steps:**
- [ ] Test: `open_round` with a projectable descriptor (`adapter_kind` `porch_json_cli`, `declared_engine_kind` `quality`, `reported_version` `{ "unavailable": "not_reported" }`, `observed_version_identity` `{ "artifact_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }`) then `build_audit` includes that producer (observed as `artifact_sha256`), `schema_version == 3`, and `producers` ordered by ordinal then slot then id.
- [ ] Test: stub `{"adapter_kind":"porch_json_cli"}` still returns the document, still projects row columns, marks descriptor-derived fields unavailable with the parse reason, and sets `anomaly.code == "unreadable_producer_descriptor"` when no prior anomaly exists. Do not park the run.
- [ ] Test: serde of a v2 document missing `producers` and the four new scalars succeeds (`#[serde(default)]`).
- [ ] Implement types, `load_producers`, `schema_version` 3, re-export; bump the three `schema_version` asserts listed in Files (including `tui.rs` line 1375). Leave `audit_fetch_count` asserts unchanged.
- [ ] Run `cargo test -p porch --test m22_audit_trace --test m20_dispo --test m21_phase`; expect pass. Run `cargo test -p porch --bin porch -- history_view_fetches_audit_once_on_open_not_on_snapshot`; expect pass. Run `cargo test -p porch --test m19_floor --test m18_round_identity` without editing those files; expect pass (authorization coverage reads stay on `applicability.rs`).
- [ ] Commit `feat(audit): project producer invocations onto the audit document`.

_Requirements: TRACE-1.1, TRACE-1.2, TRACE-1.3, TRACE-1.4, TRACE-1.5, TRACE-1.6, TRACE-6.1, TRACE-6.2, TRACE-6.3, TRACE-6.5, TRACE-7.1, TRACE-8.1, TRACE-8.6, TRACE-8.7, TRACE-8.8, TRACE-8.9_

---

### Task 2: Coverage slice

**Files:**
Modify `crates/porch-gate/src/audit.rs`, `crates/porch-gate/src/lib.rs`. Test `crates/porch/tests/m22_audit_trace.rs`.
**Reuse:** rung 2 — extends `crates/porch-gate/src/audit.rs` `build_audit` / `load_rounds` / `load_instances`
**Interfaces:**
- Consumes: Task 1 `build_audit` snapshot; `rounds::finalize_round`, `FinalizeProposal`, `RoundCoverageProposal`, `CoverageState::{Selected, Completed, Failed, Waived}`, `HistoryRevision` from `rounds::read_history` (stale revision → `FinalizeOutcome::Stale`, no rows). Copy private `sample_complete_proposal` (`crates/porch-gate/tests/m18_rounds.rs`); do not import it. Take the invocation id from `producers_for_round` after `open_round` (public; tests may call it — `build_audit` must not). CHECKs (`rounds/schema.rs`): completed needs `completion_evidence`; failed needs `reason`; waived needs `reason` and `authority`. `execution: Finished`, `assurance_completion: Complete`; both `confidence_*` None or both Some.
- Produces: `AuditCoverage { round_id, producer_invocation_id, path, state, reason, authority, completion_evidence }`; `AuditDocument.coverage` with `#[serde(default)]`; `fn load_coverage(tx: &Transaction<'_>, run_id: &str) -> Result<Vec<AuditCoverage>>` sibling join `ORDER BY r.ordinal, p.producer_invocation_id, path`; states stored as `selected` / `completed` / `failed` / `waived`.
**Depends-on:** Task 1
**Steps:**
- [ ] Test: finalize one selected path and one completed path; `build_audit` lists both with stored states and orders by ordinal, invocation id, path.
- [ ] Test: a waived path keeps its stored `reason` and `authority`; a failed path keeps its `reason`.
- [ ] Implement `AuditCoverage`, `load_coverage`, re-export.
- [ ] Run `cargo test -p porch --test m22_audit_trace`; expect pass.
- [ ] Commit `feat(audit): project per-path coverage onto the audit document`.

_Requirements: TRACE-2.1, TRACE-2.2, TRACE-2.3, TRACE-2.4_

---

### Task 3: Finding key and round pins

**Files:**
Modify `crates/porch-gate/src/audit.rs`. Test `crates/porch/tests/m22_audit_trace.rs`.
**Reuse:** rung 2 — extends `crates/porch-gate/src/audit.rs` `build_audit` / `load_rounds` / `load_instances`
**Interfaces:**
- Consumes: Task 2 finalize path; `FindingInstanceProposal` (copy `sample_complete_proposal` instance fields); `RoundBindings.trusted_config_sha` and `protocol_schema_version` (live `PROTOCOL_SCHEMA_VERSION` is `i64` 3). Invocation id from `producers_for_round`. A dangling instance needs raw SQL with `foreign_keys=OFF`; Task 3 only asserts the happy path.
- Produces: `AuditInstance.producer_invocation_id: String` and `consequence: String` (`#[serde(default)]`); `AuditRound.trusted_config_sha: String` and `protocol_schema_version: i64` (`#[serde(default)]`); anomaly `unresolved_producer_invocation` if a finalized instance id is missing from `producers[]` and `anomaly` is `None`. Do not add `provenance_json`, `candidate_key`, `confidence_*`, `inventory_digest`, context, durations, or required-producer rows.
**Depends-on:** Task 1, Task 2
**Steps:**
- [ ] Test: a finalized instance's `producer_invocation_id` equals some `producers[].id` on the same document; `consequence` matches the stored row.
- [ ] Test: each round carries the bindings' `trusted_config_sha` and `protocol_schema_version`; JSON omits inventory/context/duration/required-set keys.
- [ ] Test: instance JSON omits `provenance_json`, `candidate_key`, `confidence_value`, `confidence_kind`.
- [ ] Add the four columns to the two SELECTs; wire the anomaly miss.
- [ ] Run `cargo test -p porch --test m22_audit_trace`; expect pass.
- [ ] Commit `feat(audit): expose finding producer keys and round pins`.

_Requirements: TRACE-3.1, TRACE-3.2, TRACE-3.3, TRACE-3.4, TRACE-4.1, TRACE-4.2, TRACE-4.3, TRACE-8.2, TRACE-8.3_

---

### Task 4: Human porch audit render and docs

**Files:**
Modify `crates/porch/src/main.rs`, `crates/porch-gate/porch-agent.md`, `docs/usage.md`, `crates/porch/tests/m21_phase.rs` (default-stdout pins). Test `crates/porch/tests/m22_audit_trace.rs`.
**Reuse:** rung 2 — extends `run_human_audit` / `render_phase_tree` in `crates/porch/src/main.rs`
**Interfaces:**
- Consumes: `AuditDocument.producers`, `.coverage`, `.phase`; `run_human_audit` (`main.rs` line 385); `run_agent_audit` (`main.rs` line 409); `render_phase_tree` (`main.rs` line 350) — signature frozen. CLI calls `get_audit` over the Unix socket — copy `start_daemon_on_home` / `kill_daemon` from `crates/porch/tests/m21_phase.rs` and seed `state.sqlite` under the same `PORCH_HOME`.
- Produces: `fn render_evidence_blocks(doc: &AuditDocument) -> String`. `run_human_audit` prints it then `render_phase_tree` (no new clap flag; about string `main.rs` line 106 may mention producers). Always print both headers. Line grammar:
  - `producers:` then one line `  r{ordinal} s{slot} adapter={text} engine={text} reported=unavailable:{reason} observed={artifact_sha256:hex|unavailable:reason}`.
  - `coverage:` then per round `  round {ordinal} selected=N completed=N failed=N waived=N` and, only for `selected`/`failed`/`waived`, `    {state} {path}` plus ` reason={reason}` / ` authority={authority}` when present.
  - Empty slices print the two headers only. `m21_phase.rs` pins that seed no review rounds (`["deliver #1 started", "  compose #1 started"]` and `["phase: unavailable"]`) gain `producers:` and `coverage:` above the tree; do not loosen tree lines.
- Docs **replace** (one verb): `porch-agent.md` (~76–80) and the audit paragraph; `docs/usage.md` (~187–189) plus a sentence that audit `schema_version` 3 is independent of `PROTOCOL_SCHEMA_VERSION` (also 3). `porch-agent.md` is `include_str!` in `skill.rs`.
**Depends-on:** Task 1, Task 2
**Steps:**
- [ ] Test: with daemon up, default `porch audit` prints the producers block, then coverage counts with a selected path enumerated, then the existing phase tree; no new flag (`Audit { run_id, json }` stays).
- [ ] Test: `porch audit --json` stdout bytes equal `porch agent audit` for the same run (existing `m21_phase.rs` `porch_audit_json_matches_agent_audit_bytes` stays).
- [ ] Implement `render_evidence_blocks`; prepend the two empty headers on `m21_phase.rs` default-stdout pins without loosening tree lines.
- [ ] Replace the two-counter note in `docs/usage.md` and `porch-agent.md`.
- [ ] Run `cargo test -p porch --test m22_audit_trace --test m21_phase`; expect pass.
- [ ] Commit `feat(audit): render producers and coverage on porch audit`.

_Requirements: TRACE-5.1, TRACE-5.2, TRACE-5.3, TRACE-5.4, TRACE-6.4, TRACE-8.4, TRACE-8.5_
