# Tasks: Phase-event history

Feature code: PHASE
Status: In-progress
Date: 2026-09-07
Execution-mode: continuous
Requirements: ./requirements.md
Design: ./design.md

**Goal:** Make an append-only `phase_events` log the source of truth for a run's phase
lifecycle, and let an operator read that lifecycle as a tree from `porch audit`.

**Architecture:** Two new tables in `porch-gate`'s `rounds/` module hold phase attempts and
their events. One seam, `phase::persist_phase_transition`, is the only supported way to change
`runs.status` or insert a `step_results` row, co-writing the justifying event in one Immediate
transaction — the same pattern `rounds/authority.rs` already uses for authority events. Live
lifecycle questions and the deliver-repair budget read the log instead of matching step
strings; the audit document's phase slice is rebuilt from it and rendered as text by a new
`porch audit`.

**Tech Stack:** Rust 1.85+, edition 2024; `rusqlite` over SQLite under `$PORCH_HOME`;
`ratatui` for the TUI; `clap` for the CLI.

## Global Constraints

Source: `docs/agents/project.md` (sha256 `a3f5ba93c4d8ec55…`) plus `AGENTS.md`
**Non-negotiables** and `docs/architecture/INDEX.md`. No `docs/standards/` or
`docs/codebase/` tree exists in this repo; do not invent a parallel SSOT.

- **Verify, in order, all must pass before any completion claim:**
  `cargo fmt --all --check` · `cargo check --workspace --all-targets` ·
  `cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace`.
  `--workspace` is not optional — `default-members = ["crates/porch"]` means a bare
  `cargo test` skips `porch-gate`, `porch-git`, and `porch-quality`.
- Single file: `cargo test -p <crate> --test <file-stem>`.
- `unsafe_code = "forbid"`, `clippy::all = deny`, `clippy::pedantic = warn` — fix a pedantic
  warning or `allow` it with a reason; never ignore it.
- **Do not** add `/// REQ:` or `@PHASE-N.M` annotations to Rust source or test names.
  Requirement IDs live in this file and the triad, never in application or test source.
- Integration tests are named for the milestone that introduced them: this feature's is
  `crates/porch/tests/m21_phase.rs`.
- English only. No network in unit tests. No vendored third-party source.
- **ARCH-1 … ARCH-13** are binding and must not be silently reopened. This feature relies on
  **ARCH-3** (fixer stays a nested operation under the suspended review attempt; reviewer and
  fixer roles never collapse), **ARCH-10** (no new crate — everything lands in existing
  use-case slices), **ARCH-13** (durable authorization precedes any external forward).
- **Do not change the PR-body hidden attestation.** `assemble_scaffold` and
  `attestation_post_compose` in `crates/porch-run/src/deliver.rs` keep reading `step_results`
  and must produce byte-identical content for an identical run. That file is owned by the
  PRCMP triad; touching its attestation is out of scope by requirement.
- Team band: **Solo** (roster: Tech Lead — Jayden Đặng). No multi-assignee ceremony.

## File structure

| File | Responsibility |
|---|---|
| `crates/porch-gate/src/rounds/phase.rs` | **new** — phase tables, row types, readers, the transition seam, recovery, budget count |
| `crates/porch-gate/src/rounds/schema.rs` | add `phase_attempts` / `phase_events` creates + index to the migrate batch |
| `crates/porch-gate/src/rounds/mod.rs` | declare `phase`; re-export types; raise `PROTOCOL_SCHEMA_VERSION` |
| `crates/porch-gate/src/rounds/authority.rs` | accept an optional phase transition alongside `RunEffects` |
| `crates/porch-gate/src/db.rs` | narrow status/step writers to `pub(crate)`; fence trigger + migration ordering; startup sweep appends interrupted evidence |
| `crates/porch-gate/src/audit.rs` | rebuild `AuditPhase` from `phase_events`; bump `schema_version` |
| `crates/porch-gate/src/rpc.rs` | add compact `phase` to `RunSnapshot` |
| `crates/porch-gate/src/daemon.rs` | move the supersede status write onto the seam |
| `crates/porch-run/src/lib.rs` | route `set_status` / `record_step` through the seam; `parked_phase` from the log; budget from event count |
| `crates/porch-run/src/deliver.rs` | move ten bypassing writes onto the seam; start nested compose / repair operations |
| `crates/porch-run/src/agent_run.rs` | read `snap.phase` instead of scanning `steps[]` |
| `crates/porch/src/main.rs` | split `porch audit` from `porch agent audit`; render the phase tree; `--json` |
| `crates/porch/src/tui.rs` | `compose_parked` reads `snap.phase` |
| `crates/porch/tests/m21_phase.rs` | **new** — integration coverage for this feature |
| `crates/porch-gate/porch-agent.md`, `docs/usage.md` | correct the `porch audit` description; add the protocol upgrade subsection |

## Task 1: Phase tables exist and an attempt opens durably

**Files:**
Create `crates/porch-gate/src/rounds/phase.rs`. Modify `crates/porch-gate/src/rounds/schema.rs`,
`crates/porch-gate/src/rounds/mod.rs`. Test `crates/porch-gate/tests/m18_rounds.rs`.
**Reuse:** rung 2 — extends `crates/porch-gate/src/rounds/`, mirroring `authority_events` and
its `authority_events_run` index in `rounds/schema.rs`
**Interfaces:** Produces `PhaseAttemptRow`, `PhaseEventRow`, `AttemptId`,
`phase::attempts_for_run`, `phase::events_for_run`, `phase::nonterminal_attempt`. Consumes `Db`.
**Depends-on:** none
**Steps:**
- [ ] Test: opening an existing database applies the new tables and leaves prior rows readable.
- [ ] Test: two nonterminal attempts of one canonical phase on one run are rejected by the store.
- [ ] Implement the two `CREATE TABLE IF NOT EXISTS` statements, the `UNIQUE` constraint, and
      the `phase_events_run` index on `(run_id, seq)`; add row types and the three readers.
- [ ] Run `cargo test -p porch-gate --test m18_rounds`; expect pass.
- [ ] Commit.

_Requirements: PHASE-1.1, PHASE-1.2, PHASE-1.3, PHASE-1.4, PHASE-1.11, PHASE-2.9_

## Task 2: The transition seam co-writes status and step rows

**Files:**
Modify `crates/porch-gate/src/rounds/phase.rs`, `crates/porch-gate/src/db.rs`.
Test `crates/porch-gate/tests/m18_rounds.rs`.
**Reuse:** rung 2 — extends `RunEffects` and the Immediate-transaction pattern of
`persist_authority_with_run_effects` in `crates/porch-gate/src/rounds/authority.rs`
**Interfaces:** Produces `phase::persist_phase_transition(db, PhaseTransition, RunEffects)`,
`PhaseTransition::{Start, Terminal, Evidence}`, `PhaseError`. Consumes `RunEffects`, `Db`.
**Depends-on:** Task 1
**Steps:**
- [ ] Test: a `Start` transition commits the attempt, the `started` event, and the run status
      together; the status is visible only when the event is.
- [ ] Test: a transition whose status write fails leaves no attempt row and no event.
- [ ] Test: a `Terminal` transition appends a new row and never mutates the `started` row.
- [ ] Implement the seam over one `TransactionBehavior::Immediate` transaction, applying
      `RunEffects` and bumping `runs.audit_rev` in the same commit.
- [ ] Narrow `Db::set_run_status` and `Db::insert_step_result` to `pub(crate)`.
- [ ] Run `cargo test -p porch-gate --test m18_rounds`; expect pass.
- [ ] Commit.

_Requirements: PHASE-1.5, PHASE-1.6, PHASE-1.8, PHASE-1.9, PHASE-1.10, PHASE-7.8_

## Task 3: Nested operations and atomic handoffs

**Files:**
Modify `crates/porch-gate/src/rounds/phase.rs`. Test `crates/porch-gate/tests/m18_rounds.rs`.
**Reuse:** rung 2 — extends the seam from Task 2
**Interfaces:** Produces `PhaseTransition::{NestedStart, NestedTerminal, Handoff}`. Consumes
`AttemptId`.
**Depends-on:** Task 2
**Steps:**
- [ ] Test: a nested start under a terminated parent is refused.
- [ ] Test: a handoff writes the old terminal and the new `started` at consecutive `seq` values
      and sets `caused_by_attempt_id`.
- [ ] Test: a canonical phase name is refused as a nested `operation_kind`.
- [ ] Test: a review attempt yields at most one successor; a failed nested op yields none.
- [ ] Implement the three variants with their validation.
- [ ] Run `cargo test -p porch-gate --test m18_rounds`; expect pass.
- [ ] Commit.

_Requirements: PHASE-2.1, PHASE-2.2, PHASE-2.3, PHASE-2.4, PHASE-2.5, PHASE-2.6, PHASE-2.7, PHASE-2.8_

## Task 4: Every production write moves onto the seam

**Files:**
Modify `crates/porch-run/src/lib.rs`, `crates/porch-run/src/deliver.rs`,
`crates/porch-gate/src/daemon.rs`, `crates/porch-gate/src/rounds/authority.rs`.
Test `crates/porch/tests/m21_phase.rs`, `crates/porch/tests/m6_deliver.rs`.
**Reuse:** rung 2 — extends the `set_status` and `record_step` funnels in
`crates/porch-run/src/lib.rs` and the seam from Task 3
**Interfaces:** Consumes `phase::persist_phase_transition`. Produces no new public names.
**Depends-on:** Task 3
**Steps:**
- [ ] Test: parking for compose, cancelling on agent abort, completing delivery, and
      superseding by a new push each leave a matching phase event beside the status.
- [ ] Route `set_status` and `record_step` through the seam; add an optional phase transition
      to `persist_authority_with_run_effects`.
- [ ] Move the six bypassing status writes and six bypassing step writes onto the seam:
      `deliver.rs` awaiting-compose park, agent-abort cancel, and both completions;
      `daemon.rs` supersede; `lib.rs` park; and the six `deliver.rs` compose/deliver step rows.
- [ ] Migrate direct-writer test helpers to the seam or a `#[cfg(test)]` constructor so the
      narrowed visibility from Task 2 compiles.
- [ ] Run `cargo test --workspace`; expect pass.
- [ ] Commit.

_Requirements: PHASE-1.8, PHASE-1.9_

## Task 5: Crash recovery appends interrupted evidence

**Files:**
Modify `crates/porch-gate/src/rounds/phase.rs`, `crates/porch-gate/src/db.rs`.
Test `crates/porch/tests/m21_phase.rs`.
**Reuse:** rung 2 — extends the existing startup stale-run sweep in `crates/porch-gate/src/db.rs`
**Interfaces:** Produces `phase::reconcile_interrupted(db)`. Consumes the seam.
**Depends-on:** Task 4
**Steps:**
- [ ] Test: a run killed mid-phase has, after restart, exactly one nonterminal attempt per
      canonical phase and a `runs.status` consistent with the log.
- [ ] Test: the interrupted terminal is appended in the same transaction as the sweep's status
      change.
- [ ] Implement `reconcile_interrupted` and call it from the startup sweep.
- [ ] Run `cargo test --workspace`; expect pass.
- [ ] Commit.

_Requirements: PHASE-1.7, PHASE-8.3_

## Task 6: Lifecycle questions read the log

**Files:**
Modify `crates/porch-gate/src/rpc.rs`, `crates/porch-run/src/lib.rs`,
`crates/porch-run/src/agent_run.rs`, `crates/porch/src/tui.rs`.
Test `crates/porch/tests/m21_phase.rs`.
**Reuse:** rung 2 — extends `RunSnapshot`, following the `audit_available` precedent in
`crates/porch-gate/src/rpc.rs`
**Interfaces:** Produces `RunSnapshot.phase: Option<PhaseView>`. Consumes
`phase::nonterminal_attempt`.
**Depends-on:** Task 5
**Steps:**
- [ ] Test: a parked run reports its phase from the log, and a parked run with no nonterminal
      attempt reports unavailable rather than `review`.
- [ ] Test: the agent status JSON keeps its shape while its phase value now comes from the
      snapshot field.
- [ ] Fill `RunSnapshot.phase` server-side; rewrite `parked_phase`, `agent_status_from_snap`,
      and `compose_parked` to read it; delete the `"review"` fallback and the step-string scans.
- [ ] Run `cargo test --workspace`; expect pass.
- [ ] Commit.

_Requirements: PHASE-3.1, PHASE-3.2, PHASE-3.3, PHASE-3.4, PHASE-3.5, PHASE-7.9, PHASE-7.12, PHASE-7.15_

## Task 7: The deliver-repair budget counts started events

**Files:**
Modify `crates/porch-gate/src/rounds/phase.rs`, `crates/porch-run/src/lib.rs`,
`crates/porch-gate/src/db.rs`. Test `crates/porch/tests/m6_repair.rs`,
`crates/porch/tests/m21_phase.rs`.
**Reuse:** rung 2 — extends the Task 1 readers; the budget check stays where it is in
`crates/porch-run/src/lib.rs`
**Interfaces:** Produces `phase::repair_attempts_started(db, run_id)`. Consumes the phase store.
**Depends-on:** Task 6
**Steps:**
- [ ] Test: a run that has started the budgeted number of nested repairs is refused another and
      terminals with an explicit budget-exhausted cause.
- [ ] Test: a kill between counting a repair and finishing it never yields more than the budget
      of started repairs after restart.
- [ ] Implement the count; switch the budget check to it; delete
      `Db::increment_deliver_repair_attempts` and stop writing the column.
- [ ] Retarget `m6_repair` assertions from `run.deliver_repair_attempts` to the derived count.
- [ ] Run `cargo test --workspace`; expect pass.
- [ ] Commit.

_Requirements: PHASE-4.1, PHASE-4.2, PHASE-4.3, PHASE-4.4, PHASE-8.4_

## Task 8: The audit document carries the phase tree

**Files:**
Modify `crates/porch-gate/src/audit.rs`. Test `crates/porch/tests/m21_phase.rs`,
`crates/porch/tests/m20_dispo.rs`.
**Reuse:** rung 2 — extends `crates/porch-gate/src/audit.rs`, replacing the
`step_results_inferred` placeholder
**Interfaces:** Produces `AuditAttempt`, `AuditPhase { kind, attempts, steps }`. Consumes
`phase::attempts_for_run`, `phase::events_for_run`.
**Depends-on:** Task 7
**Steps:**
- [ ] Test: a run with attempts yields `kind` naming the event source and a nested tree; a run
      with no phase rows yields an explicitly unavailable slice.
- [ ] Test: `steps` inside the slice is rebuilt from phase events and still matches what the
      run recorded.
- [ ] Test: a document built while the run is active is labelled partial as-of its watermark.
- [ ] Test: building a document for a run holding 200 phase events stays under 100 ms and the
      query plan is index-backed.
- [ ] Implement the slice inside the existing single-snapshot build; bump `schema_version`.
- [ ] Run `cargo test --workspace`; expect pass.
- [ ] Commit.

_Requirements: PHASE-5.1, PHASE-5.6, PHASE-5.7, PHASE-6.4, PHASE-7.5, PHASE-7.6, PHASE-8.1_

## Task 9: `porch audit` renders the tree

**Files:**
Modify `crates/porch/src/main.rs`, `crates/porch-gate/porch-agent.md`, `docs/usage.md`.
Test `crates/porch/tests/m21_phase.rs`.
**Reuse:** rung 7 — new code; no renderer exists, both audit commands share one match arm in
`crates/porch/src/main.rs`
**Interfaces:** Produces `render_phase_tree(&AuditDocument) -> String` and a `--json` flag on
`porch audit`. Consumes `AuditDocument`.
**Depends-on:** Task 8
**Steps:**
- [ ] Test: `porch audit` prints each top-level attempt with phase, ordinal, and outcome, and
      each nested operation indented beneath its parent.
- [ ] Test: outcome, nesting, and unavailability stay unambiguous with colour disabled.
- [ ] Test: `porch audit --json` and `porch agent audit` emit identical bytes.
- [ ] Split the shared match arm; implement the renderer as a pure function of the document.
- [ ] Correct both docs: `porch audit` is no longer a JSON alias.
- [ ] Run `cargo test --workspace`; expect pass.
- [ ] Commit.

_Requirements: PHASE-5.2, PHASE-5.3, PHASE-5.4, PHASE-5.5, PHASE-7.13, PHASE-8.5_

## Task 10: Protocol bump, status trigger, and upgrade

**Files:**
Modify `crates/porch-gate/src/db.rs`, `crates/porch-gate/src/rounds/mod.rs`, `docs/usage.md`.
Test `crates/porch/tests/m21_phase.rs`, `crates/porch-gate/tests/m18_rounds.rs`.
**Reuse:** rung 2 — extends `install_writer_fence` and `reject_stale_writer` in
`crates/porch-gate/src/db.rs`
**Interfaces:** Consumes `PROTOCOL_SCHEMA_VERSION`. Produces no new public names.
**Depends-on:** Task 9
**Steps:**
- [ ] Test: upgrading a state root terminals every `pending` / `running` / `parked` run with an
      explicit cause in `runs.error`, and writes no phase rows for pre-existing runs.
- [ ] Test: the upgrade's own fail-forward status writes complete on both a fresh root and an
      already-fenced one, without the trigger it installs aborting them.
- [ ] Test: an under-protocol writer is aborted by the database when updating `runs.status`.
- [ ] Raise the protocol constant; order the transaction as protocol → fail-forward writes →
      create the status trigger → commit, using the existing protocol-only predicate.
- [ ] Add the upgrade subsection to `docs/usage.md` in the protocol-2 shape.
- [ ] Run `cargo test --workspace`; expect pass.
- [ ] Commit.

_Requirements: PHASE-6.1, PHASE-6.2, PHASE-6.3, PHASE-6.5, PHASE-6.6, PHASE-6.7, PHASE-7.1, PHASE-7.2, PHASE-7.3, PHASE-7.4, PHASE-8.2_

## Task 11: Untouched behaviour stays untouched

**Files:**
Test `crates/porch/tests/m17_pr_compose.rs`, `crates/porch/tests/m6_deliver.rs`,
`crates/porch/tests/m13_workflow.rs`, `crates/porch/tests/m20_dispo.rs`.
Modify `crates/porch-run/src/deliver.rs` only if a guard fails.
**Reuse:** rung 1 — does not need to exist as new code; these behaviours are deliberately not
changed, and this task proves it
**Interfaces:** Consumes the existing attestation, snapshot, and history-panel surfaces.
Produces nothing.
**Depends-on:** Task 10
**Steps:**
- [ ] Test: an identical run produces byte-identical PR attestation content before and after
      this feature, with the parked compose row still excluded post-compose.
- [ ] Test: the run snapshot still carries `steps[]`, findings, and `audit_available` as compact
      live state, without the full audit contract.
- [ ] Test: the history panel still fetches lazily on open and never on the subscribe path.
- [ ] Run `cargo test --workspace`; expect pass.
- [ ] Commit.

_Requirements: PHASE-7.7, PHASE-7.10, PHASE-7.11, PHASE-7.14_
