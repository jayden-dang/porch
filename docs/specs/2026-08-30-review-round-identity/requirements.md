# Requirements: Review Round Identity

Feature code: ROUND
Status: In-progress
Date: 2026-08-30

Roadmap item: ROAD-6 (MILE-2). Serves GOAL-2.
Respects: ARCH-3, ARCH-4, ARCH-10, ARCH-11.

## 1. A review round exists before anything is judged

**Story:** As an operator, I want porch to record what it is about to review before it
reviews it, so that every attempted judgment is on the record even when the attempt fails.

- **ROUND-1.1** WHEN a review invocation is about to begin THE SYSTEM SHALL persist a
  review round record and mint a `review_round_id` for it before invoking the producer.
- **ROUND-1.2** WHEN a review round is opened THE SYSTEM SHALL return its
  `review_round_id` only after the record has committed.
- **ROUND-1.3** IF a review round cannot be durably opened THEN THE SYSTEM SHALL fail the
  review phase without invoking the producer.
- **ROUND-1.4** WHEN a review round is opened THE SYSTEM SHALL record its execution state
  as `running` and its assurance completion as `pending`.
- **ROUND-1.5** THE SYSTEM SHALL record, as the round's input binding, the `from_sha` and
  `to_sha` actually passed to the producer together with the exact reviewed inventory.
- ~~**ROUND-1.6**~~ superseded by ROUND-1.25 and ROUND-1.26: the producer descriptor
  belongs to a producer invocation, not to the round binding.
- ~~**ROUND-1.7**~~ superseded by ROUND-1.27: restated per producer invocation.
- **ROUND-1.8** THE SYSTEM SHALL record, for each text or file review-context element, its
  source state as `absent`, `present`, or `unreadable` with a reason, independently of that
  element's effective digest.
- **ROUND-1.9** WHERE a review-context element is supplied to a producer or protocol layer
  THE SYSTEM SHALL compute its applicability digest from the exact effective bytes supplied
  after the transformation used to build that producer or layer's input.
- **ROUND-1.10** THE SYSTEM SHALL record which producer or protocol layer received each
  review-context element.
- **ROUND-1.11** WHERE a review-context element's readable content exceeds the snapshot
  ceiling THE SYSTEM SHALL retain its digest and record its snapshot as omitted for size,
  without marking the element's source state unreadable.
- **ROUND-1.12** WHEN a round records a `trusted_config_sha` THE SYSTEM SHALL keep that
  commit reachable through a porch-owned git ref for at least as long as the round is
  retained.
- **ROUND-1.13** WHEN a readable text or file review-context element does not exceed the
  snapshot ceiling THE SYSTEM SHALL retain a snapshot of its canonical effective
  representation.
- **ROUND-1.14** WHERE a review-context element is not supplied to a producer or protocol
  layer THE SYSTEM SHALL record it as not applied for that producer or layer, rather than
  treating its source bytes as applied review context.
- **ROUND-1.15** THE SYSTEM SHALL record `intent_source` as audit metadata outside the
  review-context applicability binding.
- **ROUND-1.16** WHEN permanent removal of retained round data leaves no round referencing
  a `trusted_config_sha` THE SYSTEM SHALL remove the porch-owned ref retained solely for
  that commit.
- **ROUND-1.17** WHEN a producer is resolved THE SYSTEM SHALL record one immutable
  invocation plan before the round is opened.
- **ROUND-1.18** WHEN the producer is spawned THE SYSTEM SHALL spawn exactly the absolute
  target and argv recorded in that plan, without re-resolving PATH or configuration.
- **ROUND-1.19** WHERE a porch-owned wrapper is the spawned target THE SYSTEM SHALL derive
  the observed version identity from the wrapper digest, the known backend digest, and the
  effective argv prefix together.
- **ROUND-1.20** THE SYSTEM SHALL NOT derive an observed version identity from a
  porch-owned wrapper's digest alone.
- **ROUND-1.21** WHERE porch can observe only an opaque entrypoint THE SYSTEM SHALL record
  that it observed the entrypoint artifact alone and not its dependency closure.
- **ROUND-1.22** THE SYSTEM SHALL record `reported_version` as audit metadata outside the
  applicability binding, neither establishing nor rescuing descriptor equivalence.
- **ROUND-1.23** THE SYSTEM SHALL base descriptor equivalence on adapter semantics,
  effective argv, observed artifact identity, and the consumed-context declaration.
- **ROUND-1.24** WHERE two invocations differ only in `selection_source` or
  `declared_engine_kind` THE SYSTEM SHALL NOT treat that difference as invalidating
  descriptor equivalence.
- **ROUND-1.25** THE SYSTEM SHALL record, as the round's review-context binding, the
  source states and per-producer-or-layer applicability digests of the intent and path
  instructions, the `trusted_config_sha`, the protocol schema version, and the fingerprint
  version.
- **ROUND-1.26** THE SYSTEM SHALL record a producer descriptor for each producer invocation
  within a round.
- **ROUND-1.27** WHERE a producer invocation's version cannot be observed THE SYSTEM SHALL
  record it as unavailable with a reason, rather than omitting the field or substituting a
  value.
- **ROUND-1.28** IF a content digest matches a stored blob whose byte length or bytes differ
  THEN THE SYSTEM SHALL fail closed rather than reuse that blob.
- **ROUND-1.29** WHEN retained round data is removed THE SYSTEM SHALL commit the database
  deletion before removing the porch-owned git ref.
- **ROUND-1.30** THE SYSTEM SHALL NOT finalize a round using reconciliation history that
  changed after that history was read.
- **ROUND-1.31** WHEN comparing a recorded round for applicability or reuse THE SYSTEM
  SHALL require the current set of required producer invocations to have a one-to-one
  descriptor-equivalent correspondence with the set recorded for that round.
- **ROUND-1.32** WHEN porch retains an input or output artifact for a producer invocation
  THE SYSTEM SHALL store it under a namespace unique to the run, review round, and producer
  invocation, without overwriting an artifact retained for another invocation.

## 2. A finalized round is all-or-nothing

**Story:** As an operator, I want a completed review to land as one indivisible record, so
that I never read a round that is half-written.

- **ROUND-2.1** WHEN a producer returns THE SYSTEM SHALL persist the round's terminal
  state, its structured coverage, and all of its finding instances in a single transaction.
- **ROUND-2.2** IF any part of finalization fails THEN THE SYSTEM SHALL persist none of
  that finalization and leave the round non-finalized.
- **ROUND-2.3** WHEN finding instances are persisted THE SYSTEM SHALL mint a distinct
  `finding_instance_id` for each occurrence.
- **ROUND-2.4** THE SYSTEM SHALL record the coverage state of each changed file as exactly
  one of `selected`, `completed`, `failed`, or `waived`.
- **ROUND-2.5** WHERE a file's coverage state is `failed` or `waived` THE SYSTEM SHALL
  record the reason for that state.
- **ROUND-2.6** WHERE a file's coverage state is `waived` THE SYSTEM SHALL record the
  authority that waived it.
- **ROUND-2.7** WHERE a file's coverage state is `completed` THE SYSTEM SHALL record the
  producer's completion evidence for that file.
- **ROUND-2.8** WHEN deriving a file's coverage state THE SYSTEM SHALL require an explicit
  completion signal from the producer.
- **ROUND-2.9** THE SYSTEM SHALL NOT infer a `completed` coverage state from a file's
  presence in the producer's output.

## 3. Every finding carries a porch-owned identity and contract

**Story:** As an operator, I want every finding recorded with porch's own identity and a
complete contract, so that my audit trail means the same thing no matter which producer ran.

- ~~**ROUND-3.1**~~ superseded by ROUND-3.16 and ROUND-3.17: the canonical fingerprint is
  assigned during finalization, not during normalization.
- ~~**ROUND-3.2**~~ superseded by ROUND-3.18: restated against the candidate key and the
  canonical fingerprint separately.
- **ROUND-3.3** WHERE a producer supplies its own finding key THE SYSTEM SHALL retain that
  key as provenance.
- **ROUND-3.4** WHERE a producer supplies its own finding key THE SYSTEM SHALL NOT use it
  as, or in place of, the canonical fingerprint.
- **ROUND-3.5** WHEN two findings are classified by the approved reconciliation fixture as
  the same logical issue THE SYSTEM SHALL give them the same fingerprint.
- **ROUND-3.6** WHEN two findings are classified by the approved reconciliation fixture as
  distinct issues THE SYSTEM SHALL give them different fingerprints.
- **ROUND-3.7** THE SYSTEM SHALL give each finding instance a `finding_instance_id` that
  is unique across all rounds.
- **ROUND-3.8** THE SYSTEM SHALL associate each finding instance with exactly one review
  round, recorded under that round's `review_round_id`.
- **ROUND-3.9** WHEN two finding occurrences share a fingerprint THE SYSTEM SHALL still
  record them as separate instances with distinct `finding_instance_id`s.
- **ROUND-3.10** THE SYSTEM SHALL NOT use a fingerprint as the database identity of a
  finding instance.
- **ROUND-3.11** WHEN the fingerprint algorithm or its version changes THE SYSTEM SHALL
  leave previously recorded fingerprints unchanged.
- **ROUND-3.12** THE SYSTEM SHALL record, for each finding instance, the criterion it
  violates, its evidence, its consequence, its action, its producer provenance, and its
  canonical fingerprint.
- **ROUND-3.13** WHERE a producer supplies a confidence THE SYSTEM SHALL record it typed by
  that producer's epistemology.
- **ROUND-3.14** THE SYSTEM SHALL NOT record a model-style confidence for a finding
  produced by a deterministic producer.
- **ROUND-3.15** WHERE a producer supplies no confidence THE SYSTEM SHALL record the
  finding instance without one.
- **ROUND-3.16** WHEN findings are normalized THE SYSTEM SHALL compute a
  producer-independent candidate key for each finding from the fingerprint version, the
  canonical path identity, the porch-normalized criterion, and the structural anchor
  candidates.
- **ROUND-3.17** WHEN a round is finalized THE SYSTEM SHALL assign each finding instance a
  canonical fingerprint, reusing a prior round's fingerprint only where reconciliation
  establishes exactly one match under the approved rules.
- **ROUND-3.18** THE SYSTEM SHALL derive both the candidate key and the canonical
  fingerprint for findings from both first-party producers without either producer
  supplying one.
- **ROUND-3.19** IF correspondence to a prior finding is ambiguous THEN THE SYSTEM SHALL
  assign a distinct new fingerprint rather than merging or inheriting identity.
- **ROUND-3.20** WHEN a round containing findings from more than one producer is finalized
  THE SYSTEM SHALL reconcile those findings within that round under the approved rules.
- **ROUND-3.21** WHERE a first-party producer supplies a rule identity with a registered
  porch mapping THE SYSTEM SHALL derive the finding's porch-normalized criterion from that
  mapping.
- **ROUND-3.22** THE SYSTEM SHALL record the `fingerprint_version` in force with every
  finding instance as well as with the round binding.
- **ROUND-3.23** WHEN a round is finalized THE SYSTEM SHALL reconcile its findings against
  finding instances from prior rounds of the same run under the approved rules.

## 4. Every round ends in a state that says what happened

**Story:** As an operator, I want every review — completed, failed, timed out, or killed —
to end in a state that says whether it may authorize my change, so that a gap in my audit
trail never has to be inferred and an unfinished judgment never speaks for a finished one.

- **ROUND-4.1** WHEN a producer returns valid output whose coverage meets the required
  states THE SYSTEM SHALL finalize the round with execution state `finished` and assurance
  completion `complete`.
- **ROUND-4.2** WHEN a producer invocation times out and the timeout is handled THE SYSTEM
  SHALL finalize the round with execution state `finished` and assurance completion
  `incomplete`.
- **ROUND-4.3** IF a producer exits unsuccessfully THEN THE SYSTEM SHALL finalize the round
  with execution state `finished` and assurance completion `incomplete`.
- **ROUND-4.4** IF a producer's output is malformed or cannot be normalized THEN THE SYSTEM
  SHALL finalize the round with execution state `finished` and assurance completion
  `incomplete`.
- **ROUND-4.5** IF the round's coverage falls short of the required states THEN THE SYSTEM
  SHALL finalize the round with execution state `finished` and assurance completion
  `incomplete`.
- **ROUND-4.6** IF the process or daemon dies during a review THEN THE SYSTEM SHALL leave
  that round durably recorded with execution state `running` and assurance completion
  `pending`.
- **ROUND-4.7** WHEN the daemon starts THE SYSTEM SHALL reconcile every round left
  `running` / `pending` to execution state `interrupted` and assurance completion
  `incomplete`.
- **ROUND-4.8** WHEN a round is reconciled to `interrupted` THE SYSTEM SHALL leave it
  without finding instances and without an approval.
- **ROUND-4.9** WHERE a complete round's findings include blocking findings THE SYSTEM
  SHALL still record its assurance completion as `complete`.
- **ROUND-4.10** THE SYSTEM SHALL treat a recorded round as immutable: a later change of
  input or review context SHALL NOT alter it.
- **ROUND-4.11** WHEN the current input binding or review-context binding differs from a
  recorded round's THE SYSTEM SHALL treat that round as inapplicable to authorize the
  current change.
- **ROUND-4.12** THE SYSTEM SHALL NOT authorize a change from a round whose assurance
  completion is `pending` or `incomplete`, whose execution state is `interrupted`, which is
  not finalized, or whose coverage fell short of the required states.
- **ROUND-4.13** WHEN no applicable round exists for the current change THE SYSTEM SHALL
  require a new round before that change can be authorized.
- **ROUND-4.14** WHERE a producer version is recorded as unavailable THE SYSTEM SHALL NOT
  treat that producer descriptor as establishing equivalence with a descriptor from another
  review invocation for applicability or reuse.

_Note (non-normative): ROUND-4.14 governs equivalence only. An unavailable producer version
does not by itself make the current round `incomplete` — that policy is a MILE-6 decision
(see Out of Scope)._

## 5. Records written before this feature stay usable

**Story:** As an operator upgrading with work in flight, I want my existing parked run to
still be answerable, so that adopting audit identity costs me nothing in progress.

- **ROUND-5.1** WHEN a parked decision is backed by a finalized, applicable round THE
  SYSTEM SHALL serve the status and respond paths from that round's finding instances.
- **ROUND-5.2** WHEN a parked decision predates round identity THE SYSTEM SHALL fall back
  to `runs.findings_json` and label the result a legacy snapshot whose audit identity is
  unavailable.
- **ROUND-5.3** THE SYSTEM SHALL NOT synthesize a round, an input binding, a
  review-context binding, a producer descriptor, a coverage state, a fingerprint, or an
  identity for a legacy record.
- **ROUND-5.4** WHEN a round is finalized THE SYSTEM SHALL NOT write `runs.findings_json`.
- **ROUND-5.5** THE SYSTEM SHALL introduce its storage as an additive migration that
  leaves existing rows readable by the upgraded binary.

## 6. The gate behaves exactly as before

**Story:** As an operator, I want the push-to-park-to-deliver loop to behave as it did
before I upgraded, so that audit identity is added underneath me and not in front of me.

Files this feature touches, and what each must keep doing:

- **ROUND-6.1** (guard) `crates/porch-gate/src/db.rs` — WHEN an existing database is opened
  THE SYSTEM SHALL CONTINUE TO apply additive column migrations and read rows written by an
  earlier version.
- **ROUND-6.2** (guard) `crates/porch-gate/src/db.rs` — WHEN runs are queried THE SYSTEM
  SHALL CONTINUE TO report `pending`, `running`, and `parked` runs as active, and to return
  the latest parked run for a repository.
- **ROUND-6.3** (guard) `crates/porch-gate/src/daemon.rs` — WHEN the daemon starts THE
  SYSTEM SHALL CONTINUE TO recover stale runs and to refuse to serve when that recovery
  fails.
- **ROUND-6.4** (guard) `crates/porch-gate/src/daemon.rs` — WHEN a finding hunk is
  requested THE SYSTEM SHALL CONTINUE TO return a size-capped snippet read from the run's
  worktree, and to error when no worktree exists.
- **ROUND-6.5** (guard) `crates/porch-gate/src/rpc.rs` — WHEN a run snapshot is requested
  THE SYSTEM SHALL CONTINUE TO expose that run's findings to the TUI and to
  `porch agent status`.
- **ROUND-6.6** (guard) `crates/porch-gate/src/executor.rs` — WHEN the daemon starts THE
  SYSTEM SHALL CONTINUE TO invoke stale-run recovery through the `RunExecutor` contract
  rather than bypassing it.
- **ROUND-6.7** (guard) `crates/porch-run/src/lib.rs` — WHEN a review round produces
  blocking findings THE SYSTEM SHALL CONTINUE TO park the run at the review phase.
- **ROUND-6.8** (guard) `crates/porch-run/src/lib.rs` — WHEN the operator approves a parked
  review THE SYSTEM SHALL CONTINUE TO record the approved head SHA.
- **ROUND-6.14** (guard) `crates/porch-run/src/lib.rs` — WHEN the operator skips a parked
  review THE SYSTEM SHALL CONTINUE TO leave the approved head SHA unrecorded.
- **ROUND-6.9** (guard) `crates/porch-run/src/lib.rs` — WHEN a review follows a fix THE
  SYSTEM SHALL CONTINUE TO resolve the reviewed `from_sha` from the uncertified pipeline
  range as it does today.
- **ROUND-6.10** (guard) `crates/porch-review/src/lib.rs` — WHEN a producer's output omits
  a changed file without a skip THE SYSTEM SHALL CONTINUE TO fail the review closed.
- **ROUND-6.11** (guard) `crates/porch-review/src/lib.rs` — WHEN producer output is
  normalized THE SYSTEM SHALL CONTINUE TO map severity and category into porch's own
  severity and action, forcing `ask-user` on scope-extending findings.
- **ROUND-6.12** (guard) `crates/porch-quality/src/` — WHEN the quality engine runs THE
  SYSTEM SHALL CONTINUE TO satisfy the existing argv and JSON contract for callers that do
  not read any newly added field.
- **ROUND-6.13** (guard) `crates/porch/src/tui.rs` — WHILE a run is parked THE SYSTEM SHALL
  CONTINUE TO render its findings and enable the operator actions.
- **ROUND-6.15** (guard) `crates/porch-gate/src/id.rs` — WHEN a repository id is derived
  for a working tree THE SYSTEM SHALL CONTINUE TO return the same stable value for the same
  absolute path.

## 7. Quality attributes

**Section-kind:** nfr

**Story:** As a stakeholder, I want measurable quality targets for this feature, so that
how-well is not left implicit.

- **Performance:** ~~**ROUND-7.1**~~ superseded by ROUND-7.4 through ROUND-7.6: durable open,
  terminal recording, and bounded contention cannot satisfy one transaction bound on every
  terminal path.
- **Performance:** **ROUND-7.4** WHEN a review phase runs without history contention THE SYSTEM
  SHALL add at most two committed write transactions beyond the pre-ROUND path: one durable
  round-open transaction and one terminal-finalization transaction — verified by a test counting
  committed write transactions on each terminal path.
- **Performance:** **ROUND-7.5** WHEN finalization observes stale reconciliation history THE
  SYSTEM SHALL attempt at most three additional write transactions, each leaving no durable
  finalization when its revision check fails — verified by a test that mutates history between
  phases.
- **Performance:** **ROUND-7.6** IF the process dies after round open and before finalization
  THEN THE SYSTEM SHALL use at most one later committed write transaction to reconcile that round
  during startup — verified by a fault-injection test.
- **Security:** **ROUND-7.2** THE SYSTEM SHALL write round records only within
  `$PORCH_HOME`, under the same ownership and permissions as the existing database —
  verified by an integration test asserting that no round-storage artifact is created
  outside `$PORCH_HOME`. (No
  Approved threat model exists; no `TB-N` / `THR-N` is cited.)
- **Reliability:** **ROUND-7.3** WHEN the process is killed at any boundary between round
  open and finalization THE SYSTEM SHALL leave the database in a state that restart
  reconciles to `interrupted` / `incomplete` with no partial finalization — verified by
  fault-injection tests at each boundary. (No Approved reliability doc exists; no `SLO-N`
  is cited.)
- **Accessibility:** None — porch is a terminal CLI and daemon, and this feature adds no
  new interactive surface; the TUI keeps its existing rendering and key handling.

## Out of Scope

- Backfilling legacy `findings_json` into synthesized rounds.
- Dual-writing `findings_json` alongside round storage, or any compatibility projection for
  older binaries; downgrade after a new-version run is unsupported.
- Producer-issued porch identities of any kind.
- Breaking changes to the external producer contract. Backward-compatible enrichment of
  first-party `agent` and `quality` output, or of the internal normalized contract, is in
  scope.
- MILE-6 decisions: the external producer bar, its transport, SARIF placement, and the
  policy consequence of unavailable external-producer metadata.
- MILE-3's reconciliation of ambiguous forwarding effects. This feature reconciles review
  round records only.
- The operator-facing audit read path and per-finding disposition history (ROAD-4), and
  phase start/end events (ROAD-5).

## Open Questions

Owned unknowns carried from the frame-change close package. Forbid-guess (cấm đoán) — an
agent or developer must not invent these; they are resolved in design.

- Fingerprint inputs, normalization rules, hashing algorithm, collision policy, and
  version-transition mechanism — Jayden — due at design-solution — forbid-guess.
- Whether each review-context element is stored as a readable snapshot, a digest, or both,
  and the canonical serialization and digest algorithms — Jayden — due at design-solution —
  forbid-guess.
- The exact producer-descriptor fields, and how an executable or harness version is
  observed — Jayden — due at design-solution — forbid-guess.
- Physical table layout, table count, and transaction implementation — Jayden — due at
  design-solution — forbid-guess.
- The reconciliation fixture corpus: moved code, rewritten messages, path changes,
  collisions, disappearing findings, and findings emitted by multiple producers — Jayden —
  due at design-solution — forbid-guess.
- Whether `review_round_id` is passed into producer logs or artifact paths for correlation
  — Jayden — due at design-solution — forbid-guess.
- Legacy labeling in CLI and TUI output, and upgrade guidance on parked runs and
  `$PORCH_HOME` backup — Jayden — due at design-solution — forbid-guess.
