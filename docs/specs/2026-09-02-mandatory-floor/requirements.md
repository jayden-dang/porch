# Requirements: Mandatory Deterministic Floor

Feature code: FLOOR
Status: Implemented
Date: 2026-09-02

<!--
Rules:
- Feature code: 2-12 chars, A-Z0-9, starts with a letter, unique repo-wide.
  Register it in docs/specs/INDEX.md before use.
- Every acceptance criterion gets a hierarchical ID: FLOOR-<story>.<criterion>.
- Criteria use EARS phrasing.
- Guard requirements protect existing behavior this feature touches.
- IDs are immutable once Status is Approved. Retire a requirement by striking it
  through (~~**FLOOR-N.M**~~ reason) — never renumber.
-->

Implements **ROAD-22** (MILE-2). Respects **ARCH-11**, **ARCH-12**, **ARCH-13**.
Discovery close package: `.skills/FLOOR/close-package.md`.

## 1. The floor runs on every assurance run

**Story:** As a developer pushing to `porch`, I want the deterministic floor to run on my change
even when my configured review engine is a coding agent, so that no push is assured without it.

- **FLOOR-1.1** WHEN an assurance run opens a review round THE SYSTEM SHALL include the
  deterministic floor as a required producer of that round.
- **FLOOR-1.2** THE SYSTEM SHALL derive the floor requirement from Porch-owned protocol policy,
  and SHALL NOT read it from `.porch.yaml`, from `$PORCH_HOME/config.yaml`, or from any
  environment variable.
- **FLOOR-1.3** WHERE the selected judgment engine is distinct from the floor THE SYSTEM SHALL
  compose the round's required set as the resolved floor plus that judgment producer.
- **FLOOR-1.4** WHERE the selected engine is `quality` THE SYSTEM SHALL compose a required set
  containing the floor alone, and that floor-only round SHALL be eligible to authorize a forward.
- **FLOOR-1.5** THE SYSTEM SHALL execute the floor to completion before spawning the judgment
  producer.
- **FLOOR-1.6** IF the floor does not complete its operational, protocol, and coverage obligations
  successfully THEN THE SYSTEM SHALL NOT spawn the judgment producer.
- **FLOOR-1.7** WHEN the floor returns a valid result containing blocking findings THE SYSTEM SHALL
  treat that execution as successful and SHALL still run the judgment producer required by the
  round's recorded assurance shape.
- **FLOOR-1.8** THE SYSTEM SHALL NOT supply the floor's findings or output to the judgment producer
  as context, and the judgment producer's recorded context applications SHALL contain no
  floor-output element.

## 2. Authorization proves the floor ran

**Story:** As an operator auditing a forward, I want authorization to check a requirement recorded
before execution, so that a round cannot satisfy a standard it wrote for itself.

- **FLOOR-2.1** WHEN a review round is opened THE SYSTEM SHALL durably record that round's
  effective required set, one row per requirement, keyed by round and requirement slot.
- **FLOOR-2.2** THE SYSTEM SHALL treat a round's recorded required set as immutable after the round
  is opened, and finalization SHALL record execution outcome without rewriting what the round
  required.
- **FLOOR-2.3** WHEN a requirement is resolved THE SYSTEM SHALL record a reference to a genuine
  recorded producer invocation together with that producer's expected equivalence digest.
- **FLOOR-2.4** IF a requirement is unresolved THEN THE SYSTEM SHALL record neither an invocation
  reference nor an expected equivalence digest, and SHALL record a non-empty reason.
- **FLOOR-2.5** WHEN evaluating whether a round may authorize a forward THE SYSTEM SHALL compare
  against the round's recorded required set, and SHALL NOT reconstruct the requirement from the
  producer invocations present in that round.
- **FLOOR-2.6** WHEN authorizing THE SYSTEM SHALL match every resolved requirement to exactly one
  recorded producer invocation and every such invocation to exactly one resolved requirement.
- **FLOOR-2.7** IF a round carries any unresolved requirement THEN THE SYSTEM SHALL NOT allow that
  round to authorize a forward.
- **FLOOR-2.8** IF a round has zero recorded requirement rows THEN THE SYSTEM SHALL treat it as
  legacy and unrecorded, and SHALL NOT allow it to authorize a forward.
- **FLOOR-2.9** THE SYSTEM SHALL NOT create requirement rows for rounds recorded before this
  feature by backfill or inference.

## 3. An unresolvable floor fails closed and stays recoverable

**Story:** As an operator whose daemon cannot find `porch-quality`, I want the run to stop with an
inspectable record and a stated recovery command, so that I fix the cause instead of bypassing it.

- **FLOOR-3.1** IF the floor cannot be resolved, prepared, or spawned THEN THE SYSTEM SHALL finalize
  the round as `incomplete` with a durable reason identifying the floor as the unsatisfied
  requirement.
- **FLOOR-3.2** IF the floor times out, exits unsuccessfully, produces malformed output, falls short
  on coverage, or fails its artifact stability check THEN THE SYSTEM SHALL finalize the round as
  `incomplete`.
- **FLOOR-3.3** WHEN a round is finalized `incomplete` because the floor was unsatisfied THE SYSTEM
  SHALL fail the run closed and SHALL NOT place it in any parked phase.
- **FLOOR-3.4** THE SYSTEM SHALL NOT expose any operator or agent response that authorizes a forward
  for a run whose floor requirement was unsatisfied.
- **FLOOR-3.5** THE SYSTEM SHALL retain the failed run and its `incomplete` round as durable audit
  evidence.
- **FLOOR-3.6** WHEN a run fails because the floor was unsatisfied THE SYSTEM SHALL report a
  copyable `porch rerun --run-id <run-id>` command for that run.
- **FLOOR-3.7** WHEN the recorded cause indicates daemon environment or executable resolution THE
  SYSTEM SHALL advise restarting the daemon before rerunning.
- **FLOOR-3.8** WHEN a run is rerun THE SYSTEM SHALL independently resolve and complete the floor
  requirement for the new run, and SHALL carry no authorization or completion state forward from
  the failed run.
- **FLOOR-3.9** THE SYSTEM SHALL NOT forward a branch externally until a round of a new or retried
  run has completed the required floor successfully.

## 4. The floor cannot be redirected by configuration or environment

**Story:** As a reviewer of Porch's own guarantees, I want the mandated floor to resolve through a
dedicated path, so that nothing an operator writes under `$PORCH_HOME` can substitute a different
program for it.

- **FLOOR-4.1** THE SYSTEM SHALL resolve the floor through a dedicated resolver, and SHALL NOT
  resolve it through `$PORCH_HOME/bin/review`, the `review.bin` setting, the `PORCH_REVIEW_BIN`
  environment variable, or the selected judgment engine's path.
- **FLOOR-4.2** WHEN resolving the floor THE SYSTEM SHALL canonicalize the path of the running
  `porch` executable, derive its sibling `porch-quality` executable using the platform executable
  suffix, and spawn that target directly.
- **FLOOR-4.3** IF an executable canonical sibling cannot be established THEN THE SYSTEM SHALL
  record an unresolved floor requirement, and SHALL NOT fall back to a `PATH` search on a
  production path.
- **FLOOR-4.4** WHEN the floor is resolved THE SYSTEM SHALL observe and hash the resolved artifact,
  record the immutable invocation plan including the canonical path, and re-check artifact
  stability before spawning.
- **FLOOR-4.5** THE SYSTEM SHALL derive the floor's equivalence identity from observed content, and
  SHALL keep the recorded canonical path and that content-based identity consistent with one
  another.

## 5. A run's assurance contract cannot change once pinned

**Story:** As an operator whose review configuration changes between a review and its re-review, I
want the run to stop rather than proceed under a different contract, so that a forward is never
authorized under assurance the run did not begin with.

- **FLOOR-5.1** WHEN the first round of a run is opened THE SYSTEM SHALL pin that run's assurance
  contract as a canonical digest computed over the protocol version and the ordered requirement
  slots, each slot contributing its role and its resolution state, and contributing its expected
  producer-equivalence digest when resolved or a canonical unresolved marker when unresolved; the
  diagnostic reason SHALL NOT contribute to the digest.
- **FLOOR-5.2** THE SYSTEM SHALL set the run pin and insert the first round, its requirement rows,
  and any resolved invocation rows within a single atomic transaction.
- **FLOOR-5.3** WHEN preparing a later review attempt for the same run THE SYSTEM SHALL compute the
  attempted required-set digest under FLOOR-5.1 and compare it against the run pin before spawning
  any producer.
- **FLOOR-5.4** IF a later round's required-set digest differs from the run pin THEN THE SYSTEM
  SHALL fail closed, whether the difference strengthens or weakens the assurance shape.
- **FLOOR-5.5** THE SYSTEM SHALL NOT re-pin an existing run's assurance contract.
- **FLOOR-5.6** THE SYSTEM SHALL treat a changed producer artifact identity as a changed effective
  requirement even when the operator's configuration text is unchanged.
- **FLOOR-5.7** WHEN authorizing a forward THE SYSTEM SHALL confirm that the round's recorded
  assurance contract matches the run pin.
- **FLOOR-5.8** IF the selected judgment producer is missing, fails preparation, or becomes
  unavailable THEN THE SYSTEM SHALL finalize the round as `incomplete`, and SHALL NOT reduce the
  round to a floor-only shape.

## 6. The enforcement regime is legible and fenced

**Story:** As an auditor reading a Porch database, I want each round to state which enforcement
regime produced it, so that floor-enforced forwards are distinguishable from pre-enforcement ones
without inferring from binary versions.

- **FLOOR-6.1** WHEN a round is opened under this feature THE SYSTEM SHALL record its protocol
  schema version as `2`.
- **FLOOR-6.2** WHEN a binary that understands protocol 2 encounters a round recorded at a protocol
  version below 2 THE SYSTEM SHALL treat that round as legacy and never applicable, and SHALL
  preserve it unchanged.
- **FLOOR-6.3** IF a binary encounters a round whose protocol version is greater than the version
  it understands THEN THE SYSTEM SHALL fail closed and SHALL NOT use that round to authorize a
  forward.
- **FLOOR-6.4** WHERE a state root has been upgraded to the mandatory-floor regime THE SYSTEM SHALL
  prevent a binary that does not understand that regime from creating new runs or authorizing
  forwards on that state root.
- **FLOOR-6.5** WHEN a state root is upgraded THE SYSTEM SHALL ensure that runs already active
  under the legacy regime cannot subsequently be approved, requiring a fresh run instead.

## 7. Operators can see which assurance shape ran

**Story:** As an operator reading run status or a delivered pull request, I want the assurance shape
stated, so that I can tell a floor-only assurance from a floor-plus-judgment one without opening the
database.

- **FLOOR-7.1** WHEN reporting the status of a run THE SYSTEM SHALL state the assurance shape
  recorded for that run's round.
- **FLOOR-7.2** WHEN a delivered pull request carries a porch attestation THE SYSTEM SHALL state the
  assurance shape that authorized the forward.
- **FLOOR-7.3** THE SYSTEM SHALL present human-readable assurance-shape labels as presentation data
  only, and SHALL NOT use them as authorization identity.
- **FLOOR-7.4** WHEN a run fails closed because a later round's required-set digest did not match
  the run pin THE SYSTEM SHALL report both the pinned and the attempted assurance shape.

## 8. Existing gate behavior survives the mandate

**Story:** As a maintainer upgrading Porch, I want the shipped gate's existing behavior to keep
working once the floor becomes mandatory, so that adding enforcement does not regress what already
gates pushes.

Files this feature touches, with the behavior guarded in each:

- `crates/porch-gate/src/rounds/schema.rs` — additive table and migration.
- `crates/porch-gate/src/rounds/mod.rs` — round open, ordinal allocation, two-phase finalization.
- `crates/porch-gate/src/rounds/applicability.rs` — equivalence and applicability rules.
- `crates/porch-gate/src/rounds/retention.rs` — trusted-config pin and sweep.
- `crates/porch-gate/src/db.rs` — run rows, status set, rerun lookup.
- `crates/porch-review/src/plan.rs` — descriptor, composite identity, stability check, per-slot
  context applications.
- `crates/porch-review/src/engine.rs` — engine selection and wrapper behavior for judgment CLI
  engines.
- `crates/porch-review/src/lib.rs` — finding contract and blocking classification.
- `crates/porch-review/src/reconcile.rs` — fingerprint matching and multi-producer collapse.
- `crates/porch-review/src/coverage_state.rs` — coverage state derivation.
- `crates/porch-review/src/setup.rs` — setup detect, apply, and verify.
- `crates/porch-run/src/lib.rs` — run orchestration, park, agent responses.
- `crates/porch/src/main.rs`, `crates/porch/src/tui.rs` — operator surfaces.
- `docs/usage.md`, `docs/install.md` — no behavior to guard.

Guards:

- **FLOOR-8.1** (guard) WHEN a database recorded before this feature is opened THE SYSTEM SHALL
  CONTINUE TO apply schema changes additively and leave existing rows readable.
- **FLOOR-8.2** (guard) WHEN a producer invocation is recorded THE SYSTEM SHALL CONTINUE TO store it
  with a non-null descriptor and a non-null equivalence digest, so that every recorded invocation
  denotes a genuine resolved invocation plan; invocation rows are committed before the producer is
  spawned.
- **FLOOR-8.3** (guard) WHEN a second round is opened for a run THE SYSTEM SHALL CONTINUE TO
  allocate the next ordinal under an immediate transaction.
- **FLOOR-8.4** (guard) WHEN a round is finalized THE SYSTEM SHALL CONTINUE TO write coverage,
  finding instances, and terminal state in one transaction or not at all, and SHALL CONTINUE TO
  yield a stale outcome without durable finalization when the run's review history revision changed
  between phases.
- **FLOOR-8.5** (guard) WHEN two candidate rounds differ only in selection source or declared engine
  kind THE SYSTEM SHALL CONTINUE TO treat them as applicable to one another.
- **FLOOR-8.6** (guard) IF a producer's version is unobservable THEN THE SYSTEM SHALL CONTINUE TO
  record it as unavailable with a reason and SHALL CONTINUE TO refuse to establish equivalence on
  it.
- **FLOOR-8.7** (guard) WHEN a round pins a trusted configuration commit THE SYSTEM SHALL CONTINUE
  TO keep that commit reachable while a round references it, and SHALL CONTINUE TO remove the
  reference only after the last referencing round is gone.
- **FLOOR-8.8** (guard) WHEN listing runs that contend for a branch THE SYSTEM SHALL CONTINUE TO
  count only runs whose status is pending, running, or parked.
- **FLOOR-8.9** (guard) WHEN a review context element is supplied to one producer and not another
  THE SYSTEM SHALL CONTINUE TO record the application state per producer slot.
- **FLOOR-8.10** (guard) WHERE the selected judgment producer is a CLI engine distinct from the
  mandatory floor THE SYSTEM SHALL CONTINUE TO invoke that producer through its existing wrapper and
  argv contract; this guard does not apply to the mandatory floor, whose resolution is governed by
  FLOOR-4.1 and FLOOR-4.2.
- **FLOOR-8.11** (guard) WHEN the selected engine is `agent` THE SYSTEM SHALL CONTINUE TO invoke it
  as a session-free agent turn with no review wrapper.
- **FLOOR-8.12** (guard) WHEN a finding's severity is error or warning, or its action is ask-user,
  THE SYSTEM SHALL CONTINUE TO treat it as blocking and park the run, and SHALL CONTINUE TO leave
  the run unparked for informational findings alone.
- **FLOOR-8.13** (guard) WHEN candidate findings from distinct producers describe the same issue THE
  SYSTEM SHALL CONTINUE TO collapse them only on a common non-empty range intersection.
- **FLOOR-8.14** (guard) WHEN a changed file is absent from producer output without a skip signal
  THE SYSTEM SHALL CONTINUE TO fail the review closed.
- **FLOOR-8.15** (guard) WHEN `porch setup` runs THE SYSTEM SHALL CONTINUE TO detect engines, write
  the operator configuration, and verify an existing installation.
- **FLOOR-8.16** (guard) WHEN a run parks on blocking findings THE SYSTEM SHALL CONTINUE TO accept
  approve, fix, skip, and abort responses for that park.
- **FLOOR-8.17** (guard) WHEN a run is parked in the compose phase THE SYSTEM SHALL CONTINUE TO
  offer only respond, skip, and abort, and SHALL CONTINUE TO branch on the compose phase before the
  review skip path.
- **FLOOR-8.18** (guard) WHEN an operator approves a parked review THE SYSTEM SHALL CONTINUE TO
  record the head SHA, and WHEN they skip it SHALL CONTINUE TO leave that SHA unrecorded.
- **FLOOR-8.19** (guard) WHEN a post-fix review round is opened THE SYSTEM SHALL CONTINUE TO leave
  the run's originating `from_sha` unchanged.
- **FLOOR-8.20** (guard) WHEN a parked run predates round records THE SYSTEM SHALL CONTINUE TO
  answer approve, fix, skip, abort, notes, and hunk lookup through its legacy snapshot.
- **FLOOR-8.21** (guard) WHEN the daemon starts THE SYSTEM SHALL CONTINUE TO recover stale runs and
  SHALL CONTINUE TO refuse to serve when recovery fails.
- **FLOOR-8.22** (guard) WHEN `porch rerun` is invoked THE SYSTEM SHALL CONTINUE TO allocate a new
  run identifier and start a fresh run from the prior run's recorded tip and intent.

## 9. Quality attributes

**Section-kind:** nfr

**Story:** As a stakeholder, I want measurable quality targets for this feature, so that how-well is
not left implicit.

- **Performance:** **FLOOR-9.1** WHEN an assurance run executes the floor THE SYSTEM SHALL record
  that producer's execution duration alongside the round's total review duration — verified by
  reading both durations from the round record after a run. The acceptable ceiling for the floor's
  contribution to review wall-clock is an Open Question resolved by dogfood measurement; no target
  is asserted here.
  <!-- Consult: docs/product/metrics.md absent, docs/ops/reliability.md absent — no-op; no standing
  latency metric or SLO to cite, and no number invented. -->
- **Security:** **FLOOR-9.2** THE SYSTEM SHALL ensure that no value of `.porch.yaml`,
  `$PORCH_HOME/config.yaml`, `PORCH_REVIEW_BIN`, `review.bin`, or any other environment variable
  causes a program other than the resolved canonical sibling to be spawned as the floor — verified
  by tests that set each of those inputs to a substitute executable and assert the floor's recorded
  invocation target is unchanged. This guarantee covers configuration and environment only; it does
  not extend to an owner who replaces the installed `porch` or `porch-quality` binaries.
  <!-- Consult: docs/security/threat-model.md absent — no-op; grounded in the frame-change lock, no
  TB/THR identifier invented. -->
- **Reliability:** **FLOOR-9.3** THE SYSTEM SHALL commit a round's opening — the run pin, its
  requirement rows, and any resolved invocation rows — atomically, so that a process killed at any
  point leaves either no durable record of that round or a complete opening — verified by fault
  injection at the round-open boundary. **FLOOR-9.4** THE SYSTEM SHALL commit a round's finalization
  — coverage, finding instances, and terminal state — atomically, and every durable intermediate
  protocol state SHALL be classified fail-closed by startup recovery — verified by fault injection
  at the finalization boundary and by asserting the recovered classification of each intermediate
  state. **FLOOR-9.5** THE SYSTEM SHALL fail closed on every path where the floor requirement cannot
  be established or verified — verified by tests asserting no forward occurs for unresolved,
  mismatched, legacy, or unknown-version rounds.
  <!-- Consult: docs/ops/reliability.md absent — no-op; grounded in frame-change locks, no SLO
  identifier invented. -->
- **Accessibility:** None — Porch ships a CLI and a terminal TUI with no web surface, and the repo
  declares no standing accessibility conformance target.

## Out of Scope

- Consolidating the quality engine into the `porch` binary, running the floor in-process, or any
  compatibility shim for the separate executable — that is ROAD-12 in MILE-5.
- Repository-declared external producer policy, a `review.producers` key, or any machine-checkable
  bar for external judgment producers — that is MILE-6.
- Changing the `round_producers` table.
- Introducing a new parked phase, a `retry` response verb, or in-place resumption of a floor-blocked
  run.
- Concurrent execution of the floor and judgment producers.
- Any configuration or environment override that makes the floor optional.
- Reworking the **Review** glossary entry in `CONTEXT.md` so the judgment layer reads as
  optional-but-explicit; agreed as a consequence of this feature but carried as documentation work,
  not a behavioral requirement here.
- Recording MILE-7 dogfood effectiveness metrics; this feature records durations only.

## Open Questions

- Concrete compatibility-fence mechanism satisfying FLOOR-6.4 and FLOOR-6.5 — a minimum-writer
  protocol marker, database constraints or triggers, a startup compatibility check, or a
  combination — owner Jayden — due before `design.md` approval, after an audit of every write and
  authorization path — forbid-guess (cấm đoán): must not be invented ahead of that audit.
- Acceptable ceiling for the floor's contribution to review wall-clock, and the trigger that would
  reopen concurrency — owner Jayden — due at the first dogfood run under MILE-7 — forbid-guess
  (cấm đoán): no target or SLO identifier may be invented.
- Mismatch-record representation for FLOOR-5.4: an `incomplete` round carrying the attempted
  requirement set, versus a durable failed-run event only — owner Jayden — due during `design.md` —
  forbid-guess (cấm đoán).
- Exact operator error, status, and rollback copy for FLOOR-3.6, FLOOR-3.7, and FLOOR-7.4 — owner
  Jayden — due during the operator-surface slice — forbid-guess (cấm đoán).
