# Requirements: Phase-event history

Feature code: PHASE
Status: In-progress
Date: 2026-09-07

Roadmap item: ROAD-5 (MILE-2 — Auditable assurance record). Serves GOAL-2, whose
outcome names phase events explicitly. Frame confirmed 2026-09-07; the seven locked
decisions behind these criteria are in `.skills/PHASE/close-package.md`.

Vocabulary is already locked in `CONTEXT.md` — **Phase-event history**, **Phase
attempt**, **Nested operation**, **Phase handoff**, **Audit read path**, **Audit
document**. These criteria use those terms and do not redefine them.

## 1. A run's phase lifecycle is durably recorded

**Story:** As an operator, I want every phase a run entered and left to be recorded as it
happens, so that I can reconstruct what the gate did without trusting a live process.

- **PHASE-1.1** WHEN a run enters one of the canonical top-level phases (`intent`, `rebase`,
  `review`, `certify`, `deliver`) THE SYSTEM SHALL append a `started` phase event carrying a
  new phase-attempt identity and that phase's next attempt ordinal, before the phase's work
  begins.
- **PHASE-1.2** WHEN a phase attempt reaches a terminal outcome THE SYSTEM SHALL append a
  separate terminal phase event for that attempt.
- **PHASE-1.3** THE SYSTEM SHALL NOT update or delete a `phase_events` row after it is
  committed.
- **PHASE-1.4** WHEN a run re-enters a canonical top-level phase THE SYSTEM SHALL mint a new
  phase attempt carrying the next ordinal for that phase on that run.
- **PHASE-1.5** WHILE a phase attempt is parked THE SYSTEM SHALL leave that attempt
  nonterminal until an operator response either resumes it or produces its terminal outcome.
- **PHASE-1.6** THE SYSTEM SHALL NOT hold two attempts of the same canonical top-level phase
  nonterminal on one run at the same time.
- **PHASE-1.7** WHEN the daemon starts and finds a phase attempt that is nonterminal with no
  live execution THE SYSTEM SHALL append interrupted evidence as that attempt's terminal.
- **PHASE-1.8** WHEN a `runs.status` value is written THE SYSTEM SHALL write it in the same
  database transaction as the phase event that justifies it.
- **PHASE-1.9** WHEN a `step_results` row is written THE SYSTEM SHALL write it in the same
  database transaction as the phase event it projects.
- **PHASE-1.10** IF any write in a phase transition fails THEN THE SYSTEM SHALL roll back the
  whole transaction, leaving `runs.status`, `step_results`, and `phase_events` unchanged.
- **PHASE-1.11** THE SYSTEM SHALL reconstruct a run's phase timeline from `phase_events`
  alone, without the live event stream.

## 2. Nested work and successions are recorded with their structure

**Story:** As an operator, I want compose, fixer, and deliver-repair work to appear under the
phase that owns it, so that I can see what contained what and what caused what.

- **PHASE-2.1** WHEN a nested operation (`compose`, fixer execution, `deliver_repair`) begins
  THE SYSTEM SHALL append a `started` event for it carrying the `parent_attempt_id` of the
  owning phase attempt, before that work begins.
- **PHASE-2.2** THE SYSTEM SHALL NOT record `compose`, fixer execution, or `deliver_repair` as
  a canonical top-level phase.
- **PHASE-2.3** IF a nested operation would start after its parent attempt already has a
  terminal event THEN THE SYSTEM SHALL refuse to start it.
- **PHASE-2.4** WHEN one top-level phase attempt succeeds another THE SYSTEM SHALL, in one
  transaction, append the old attempt's terminal, allocate the next ordinal, append the new
  attempt's `started`, and set the new attempt's `caused_by_attempt_id` to the old attempt.
- **PHASE-2.5** WHEN a phase handoff commits THE SYSTEM SHALL place the old attempt's terminal
  and the new attempt's `started` at consecutive sequence positions.
- **PHASE-2.6** WHEN a fixer nested operation ends with an outcome that permits rereview THE
  SYSTEM SHALL create at most one successor review attempt from that review attempt.
- **PHASE-2.7** IF a fixer nested operation fails or is interrupted THEN THE SYSTEM SHALL
  append terminals for that nested operation and for its parent review attempt, and SHALL
  create no successor attempt.
- **PHASE-2.8** WHEN `AllowlistFailed` or `MergeConflicting` occurs THE SYSTEM SHALL record it
  as nonterminal repairable-failure evidence on the active `deliver` attempt rather than as
  that attempt's terminal.
- **PHASE-2.9** THE SYSTEM SHALL treat `caused_by_attempt_id` as phase-attempt causality only,
  and SHALL NOT derive any finding-level causal edge from it.

## 3. Lifecycle questions are answered from the log

**Story:** As an operator, I want the gate to decide where a run is from the phase log, so
that what I am shown and what the gate acts on cannot disagree.

- **PHASE-3.1** WHEN the system determines which phase a parked run is parked in THE SYSTEM
  SHALL derive that answer from the run's nonterminal phase attempt in `phase_events`.
- **PHASE-3.2** THE SYSTEM SHALL NOT derive a run's phase position by matching step strings or
  statuses in `step_results`.
- **PHASE-3.3** IF a parked run has no nonterminal phase attempt THEN THE SYSTEM SHALL report
  its phase as unavailable rather than defaulting to `review`.
- **PHASE-3.4** WHEN a run snapshot is built THE SYSTEM SHALL include a compact phase field
  derived server-side from `phase_events`.
- **PHASE-3.5** WHEN the attach TUI or the agent CLI needs a run's parked phase THE SYSTEM
  SHALL read that snapshot field rather than re-deriving the phase from `steps[]`.

## 4. The deliver-repair budget is counted from the log

**Story:** As an operator, I want the repair budget enforced from the same record I can read,
so that a crash cannot buy a run an extra forward attempt.

- **PHASE-4.1** WHEN the system decides whether a deliver repair may start THE SYSTEM SHALL
  compute attempts used by counting that run's nested `deliver_repair` `started` events.
- **PHASE-4.2** IF that count has reached the deliver-repair budget THEN THE SYSTEM SHALL
  refuse to start another nested repair operation.
- **PHASE-4.3** WHEN a repair is refused for a reached budget THE SYSTEM SHALL append terminals
  for the current `deliver` attempt and the run carrying an explicit budget-exhausted cause.
- **PHASE-4.4** THE SYSTEM SHALL NOT read `deliver_repair_attempts` when enforcing the
  deliver-repair budget.

## 5. An operator can read the phase tree

**Story:** As an operator diagnosing a run that went wrong, I want to read its phase tree in
my terminal, so that I can see which attempt failed and what ran after it without a JSON tool.

- **PHASE-5.1** WHEN the audit document is built THE SYSTEM SHALL populate its phase slice
  from `phase_events`.
- **PHASE-5.2** WHEN `porch audit` runs THE SYSTEM SHALL print a human-readable phase tree on
  stdout.
- **PHASE-5.3** THE SYSTEM SHALL render each top-level attempt with its canonical phase, its
  ordinal, and its terminal outcome.
- **PHASE-5.4** THE SYSTEM SHALL render each nested operation beneath the phase attempt that
  contains it.
- **PHASE-5.5** WHERE `--json` is passed to `porch audit` THE SYSTEM SHALL emit the same
  document JSON that `porch agent audit` emits.
- **PHASE-5.6** THE SYSTEM SHALL build every phase rendering from the audit document returned
  by the shared audit read path, without re-deriving ordering, nesting, or containment in the
  rendering code.
- **PHASE-5.7** WHEN a run's phase tree is rendered while the run is still active THE SYSTEM
  SHALL label the document partial as-of its watermark.

## 6. An existing installation upgrades safely

**Story:** As an operator upgrading porch on a machine with existing runs, I want the upgrade
to tell me exactly what it did to them, so that I am never shown a phase history that was
invented for me.

- **PHASE-6.1** WHEN a state root is first opened by a binary implementing this protocol THE
  SYSTEM SHALL raise the minimum writer protocol recorded in that state root.
- **PHASE-6.2** WHEN that upgrade transaction runs THE SYSTEM SHALL terminal every run whose
  status is `pending`, `running`, or `parked`, recording an explicit phase-events upgrade cause
  in `runs.error`.
- **PHASE-6.3** THE SYSTEM SHALL NOT synthesize phase attempts, ordinals, timestamps, or
  nesting for runs that predate this protocol.
- **PHASE-6.4** WHEN an audit document is built for a run that has no `phase_events` rows THE
  SYSTEM SHALL report its phase slice as explicitly unavailable.
- **PHASE-6.5** WHEN the writer fence is installed or raised THE SYSTEM SHALL add a
  `BEFORE UPDATE OF status ON runs` trigger that aborts the write when the writing binary's
  protocol is below the state root minimum.
- **PHASE-6.6** WHEN the upgrade transaction installs that trigger THE SYSTEM SHALL complete
  its own fail-forward status writes in the same transaction without aborting.
- **PHASE-6.7** IF a binary whose protocol is below the state root minimum attempts to update
  `runs.status` THEN THE SYSTEM SHALL abort that write at the database.

## 7. Preserved behavior

**Story:** As an operator already using porch, I want everything that worked before the phase
log to keep working after it, so that adopting an audit improvement costs me no capability.

Files this feature touches, and what each must continue to do:

- `crates/porch-gate/src/db.rs`
  - **PHASE-7.1** (guard) WHEN a repo is detached THE SYSTEM SHALL CONTINUE TO delete that
    repo's `step_results` rows.
  - **PHASE-7.2** (guard) WHEN a state root is opened THE SYSTEM SHALL CONTINUE TO refuse a
    binary whose writer protocol is below the recorded minimum.
  - **PHASE-7.3** (guard) WHEN the protocol-2 fence is already installed THE SYSTEM SHALL
    CONTINUE TO leave its existing `runs` insert and `review_approved_head_sha` triggers in
    force.
- `crates/porch-gate/src/rounds/schema.rs`, `crates/porch-gate/src/rounds/mod.rs`
  - **PHASE-7.4** (guard) WHEN the round and authority tables are migrated THE SYSTEM SHALL
    CONTINUE TO leave existing round, finding-instance, and authority-event rows readable.
- `crates/porch-gate/src/audit.rs`
  - **PHASE-7.5** (guard) WHEN an audit document is built THE SYSTEM SHALL CONTINUE TO carry
    its rounds, instances, events, related-occurrence groups, completeness, and durable
    watermark unchanged.
  - **PHASE-7.6** (guard) WHEN a lifecycle inconsistency is found THE SYSTEM SHALL CONTINUE TO
    report it as an explicit anomaly rather than a silently complete document.
- `crates/porch-gate/src/rpc.rs`
  - **PHASE-7.7** (guard) WHEN a run snapshot is served THE SYSTEM SHALL CONTINUE TO carry
    `steps[]`, findings, and `audit_available` as compact live state, and SHALL NOT carry the
    full audit contract.
- `crates/porch-run/src/lib.rs`
  - **PHASE-7.8** (guard) WHEN a run progresses through its phases THE SYSTEM SHALL CONTINUE TO
    record the same `step_results` step names and statuses it records today.
  - **PHASE-7.9** (guard) WHEN a run is parked THE SYSTEM SHALL CONTINUE TO accept the same
    operator responses (approve, fix, skip, abort, note) for that park.
- `crates/porch-run/src/deliver.rs`
  - **PHASE-7.10** (guard) WHEN a PR body is composed THE SYSTEM SHALL CONTINUE TO build its
    hidden attestation from `step_results` and SHALL CONTINUE TO produce byte-identical
    attestation content for an identical run.
  - **PHASE-7.11** (guard) WHEN compose resolves THE SYSTEM SHALL CONTINUE TO exclude the
    parked compose row from the post-compose attestation.
- `crates/porch-run/src/agent_run.rs`
  - **PHASE-7.12** (guard) WHEN `porch agent status` is emitted THE SYSTEM SHALL CONTINUE TO
    emit its existing compact JSON shape.
- `crates/porch/src/main.rs`
  - **PHASE-7.13** (guard) WHEN `porch agent audit` runs THE SYSTEM SHALL CONTINUE TO emit
    pretty-printed audit-document JSON on stdout.
- `crates/porch/src/tui.rs`
  - **PHASE-7.14** (guard) WHEN the history panel is opened THE SYSTEM SHALL CONTINUE TO fetch
    the audit document lazily on open and SHALL CONTINUE NOT TO fetch it on the subscribe path.
  - **PHASE-7.15** (guard) WHEN a compose park is active THE SYSTEM SHALL CONTINUE TO offer
    only the compose-appropriate key actions.
- `crates/porch-gate/porch-agent.md`, `docs/usage.md` — documentation only; no behavior to
  guard beyond the accuracy corrections this feature requires.
- `crates/porch/tests/m21_phase.rs` (new) — no behavior to guard.

## 8. Quality attributes

**Section-kind:** nfr

**Story:** As a stakeholder, I want measurable quality targets for this feature, so that
how-well is not left implicit.

System-doc consult outcome: `docs/product/metrics.md`, `docs/ops/reliability.md`,
`docs/security/threat-model.md`, and `docs/standards/accessibility.md` are all **absent** in
this repo, so the consult is a no-op for every attribute. Targets below come from the
confirmed frame-change locks or from domain judgment, and cite **no** greppable SLO/TB/THR
IDs, because none are defined in an Approved doc.

- **Performance:** **PHASE-8.1** WHEN an audit document is built for a run holding up to 200
  phase events THE SYSTEM SHALL return that document in under 100 ms on a warm database —
  verified by a benchmark test over a seeded run, plus a test asserting the phase-events query
  plan is index-backed rather than a full table scan. (Scope is document build, not
  `porch audit` render time.)
- **Security:** **PHASE-8.2** WHEN a binary whose writer protocol is below the state root
  minimum attempts to insert a run, update `review_approved_head_sha`, or update `runs.status`
  THE SYSTEM SHALL abort that write inside the database regardless of any check in that binary
  — verified by a test that writes through a simulated under-protocol connection.
- **Reliability:** **PHASE-8.3** WHEN the process is killed at any point between a phase
  event's append and its transition's other writes THE SYSTEM SHALL, on restart, present
  exactly one nonterminal attempt per canonical phase for that run with `runs.status`
  consistent with the log — verified by fault-injection tests that kill at each write boundary
  of a phase transition.
- **Reliability:** **PHASE-8.4** WHEN the process is killed between counting a deliver repair
  and completing it THE SYSTEM SHALL, on restart, never permit more than the deliver-repair
  budget of nested repair operations to have started for that run — verified by a
  fault-injection test.
- **Accessibility:** **PHASE-8.5** WHEN `porch audit` renders a phase tree THE SYSTEM SHALL
  convey attempt outcome, nesting, and unavailability through text alone, and SHALL NOT encode
  any of them by colour alone — verified by asserting the rendering is unambiguous with colour
  disabled.

## Out of Scope

- Backfilling, synthesizing, or inferring phase attempts, ordinals, timestamps, or nesting for
  runs that predate this protocol.
- Changing the hidden attestation bytes written into a PR body; any `gh` call that closes,
  drafts, edits, or otherwise mutates a PR.
- Rewiring the attestation builders (`assemble_scaffold`, `attestation_post_compose`) or the
  wire `steps[]` projection onto `phase_events`.
- Rendering the phase tree in the attach TUI, and any scroll/collapse rework of the fixed
  history pane.
- Pruning, retention, or archival of `phase_events`; bounding total `$PORCH_HOME` growth.
- New parked phase verbs, collapsing the fixer and reviewer roles, or a sixth canonical
  top-level phase.
- MILE-3 work: durable authorization before external forward, restart reconciliation of
  ambiguous forwards, and fault injection across the forward boundary.
- Storing finding-instance lineage edges, or deriving finding-level causality from
  `caused_by_attempt_id`.
- Replacing `get_run` or `porch agent status` with the full audit document.

## Open Questions

Owned unknowns carried from the confirmed close package. None may be resolved by guessing.

- Cheapest safe migration shape for the `deliver_repair_attempts` column — drop it in this
  protocol bump, leave it unused, or merely stop writing it — Jayden — at design-solution —
  forbid-guess (cấm đoán). Enforcement must not read it under any of the three.
- Whether a `BEFORE UPDATE OF status ON runs` trigger can safely assert the co-write invariant
  in SQL without false aborts — Jayden — at design-solution — forbid-guess (cấm đoán). The
  frame lock is a protocol-only predicate; do not assume SQL can express more.
- Full enumeration of remaining step-string control-flow sites beyond `parked_phase`,
  `compose_parked`, and `agent_status_from_snap` — Jayden — before tasks.md — forbid-guess
  (cấm đoán). Enumerate by reading the code; do not assume those three are exhaustive.
