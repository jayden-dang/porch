# Requirements: Disposition History

Feature code: DISPO
Status: In-progress
Date: 2026-09-06

Roadmap item: ROAD-4 (MILE-2). Serves GOAL-2.
Respects: ARCH-3, ARCH-6, ARCH-10, ARCH-11, ARCH-12.

Implements **ROAD-4**. The dedicated **Audit document** is introduced here for
the disposition/authority slice; canonical **Phase-event history** (`phase_events`
as source of truth, attempt handoffs, nested operations) is ROAD-5.

## 1. Append-only authority log

**Story:** As an operator, I want every review-level authority change recorded as
an append-only event on a finding instance, so that after a later review round I
can still read what happened to each prior occurrence.

- **DISPO-1.1** THE SYSTEM SHALL persist operator and porch authority actions
  about findings as append-only events keyed by `finding_instance_id`, and SHALL
  NOT update a finalized `finding_instances` row to carry operator disposition.
- **DISPO-1.2** THE SYSTEM SHALL NEVER overwrite a finding instance's producer
  `action` field with an operator or porch authority outcome.
- **DISPO-1.3** THE SYSTEM SHALL treat the event log as the source of truth for
  disposition and authority. Any current-state view SHALL be a derived
  projection, rebuildable from the log, and SHALL NOT become a competing
  authority.
- **DISPO-1.4** AFTER a later review round of the same run THE SYSTEM SHALL
  still serve the disposition/authority events of each prior finding instance
  of that run.
- **DISPO-1.5** THE SYSTEM SHALL NOT persist `successor_instance_id`,
  `predecessor_instance_id`, or any other instance-to-instance lineage edge.
- **DISPO-1.6** THE SYSTEM SHALL NOT transfer disposition, target membership, or
  bulk-event coverage from one finding instance to another through a shared
  fingerprint.
- **DISPO-1.7** THE SYSTEM SHALL NOT use display handles `f0`, `f1`, … as durable
  audit identity for disposition or authority events.
- **DISPO-1.8** THE SYSTEM SHALL NOT treat producer `action` or `round_coverage`
  file `authority` as operator disposition history.

## 2. Bulk approve and skip

**Story:** As an operator, I want review-level approve and skip recorded as one
bulk event with a frozen instance set, so that I can tell decision context from
per-finding disposition.

- **DISPO-2.1** WHEN an operator issues review-level `approve` or `skip` against
  a modern parked review that has an applicable finalized round THE SYSTEM SHALL
  persist exactly one bulk operator-response event and SHALL NOT synthesize
  per-finding disposition events for that response.
- **DISPO-2.2** WHEN that bulk event is persisted THE SYSTEM SHALL freeze every
  `finding_instance_id` from the applicable finalized round at response time as
  membership meaning "included in this decision context," not "individually
  disposed."
- **DISPO-2.3** WHEN the response is `approve` THE SYSTEM SHALL authorize
  continuation, bind the applicable `review_round_id` and the approved HEAD, and
  SHALL CONTINUE TO take the existing certify → deliver path.
- **DISPO-2.4** WHEN the response is `skip` THE SYSTEM SHALL record termination
  without approval, SHALL NOT record an approved HEAD, and SHALL NOT authorize
  continuation.
- **DISPO-2.5** THE SYSTEM SHALL NOT treat a later finding instance that shares
  a fingerprint with a member of an earlier bulk event as covered by that event.
- **DISPO-2.6** IF the applicable round or the reviewed HEAD changed before the
  response is persisted THEN THE SYSTEM SHALL fail closed without writing the
  bulk event and without changing run status.
- **DISPO-2.7** WHILE folding current-state for a finding instance THE SYSTEM
  SHALL distinguish "individually disposed" from "included in a bulk decision
  context."

## 3. Fix requested

**Story:** As an operator, I want `respond fix` to record which durable instances
were sent to the fixer, so the audit trail does not treat that request as those
findings being resolved.

- **DISPO-3.1** WHEN an operator issues review-level `fix` THE SYSTEM SHALL
  persist exactly one append-only `fix_requested` operator-response event.
- **DISPO-3.2** WHEN resolving the fix selection THE SYSTEM SHALL use explicit
  `--findings` if provided, otherwise the default all-blocking selection, at
  response time, then freeze the selected durable `finding_instance_id`s as
  target relations.
- **DISPO-3.3** THE SYSTEM SHALL NOT persist display `fN` handles as audit
  identity of those targets.
- **DISPO-3.4** THE SYSTEM SHALL treat a target relation as "requested to be
  sent to this fixer attempt" and SHALL NOT treat it as fixed, resolved,
  accepted, or otherwise terminally disposed.
- **DISPO-3.5** IF the resolved target set is empty THEN THE SYSTEM SHALL reject
  the response with the existing usage failure and SHALL NOT persist
  `fix_requested`.
- **DISPO-3.6** THE SYSTEM SHALL persist `fix_requested` before spawning the
  fixer.
- **DISPO-3.7** IF the applicable round or the reviewed HEAD changed before
  `fix_requested` is persisted THEN THE SYSTEM SHALL fail closed without
  spawning the fixer.

## 4. Post-fix rereview and standing consent

**Story:** As an operator, I want a post-fix rereview to mint fresh identities
and, if I used `--yes`, to show Porch exercising bounded consent on the new
round, so that neither old-round approval nor a silent no-op fixer is confused
with my decision.

- **DISPO-4.1** WHEN rereview runs after a `fix_requested` THE SYSTEM SHALL mint
  a new review round and new finding instances, and SHALL NOT transfer
  disposition, target membership, or bulk coverage by fingerprint.
- **DISPO-4.2** WHEN the fixer process returns `Ok` and `HEAD_after` equals
  `HEAD_before` THE SYSTEM SHALL treat that as a successful nested fixer
  outcome, not as failure or approval, and SHALL still open a fresh review round
  on that unchanged HEAD.
- **DISPO-4.3** WHEN that fresh round is opened THE SYSTEM SHALL execute the
  required producers again and SHALL NOT short-circuit or reuse prior producer
  results solely because the SHA is unchanged.
- **DISPO-4.4** THE SYSTEM SHALL give the new round, producer invocations, and
  finding occurrences fresh identities.
- **DISPO-4.5** THE SYSTEM SHALL permit exactly one rereview for that fix
  response and SHALL NOT automatically loop through repeated no-op fixes.
- **DISPO-4.6** `--yes` SHALL be bounded standing consent attached to the
  original `fix_requested` event, not approval of the old round and not a manual
  approval of unseen findings.
- **DISPO-4.7** WHEN the new round parks and that consent is exercised THE
  SYSTEM SHALL persist a separate bulk `review_approved` event that binds the
  new applicable round, the then-current HEAD (including unchanged HEAD), and
  that round's frozen all-instance set, identifies Porch as executor, and
  references the original `fix_requested` event as the authority source.
- **DISPO-4.8** WHEN the fresh round has no blocking findings THE SYSTEM SHALL
  complete normally without synthesizing a bulk approval event.
- **DISPO-4.9** WHEN `--yes` is exercised after a no-change fixer outcome THE
  SYSTEM SHALL expose on the audit document that the fixer made no HEAD change.

## 5. Review abort

**Story:** As an operator, I want abort at a review park to cancel the run with
a bulk `review_aborted` record, so that it stays distinct from skip and from a
push that superseded the run.

- **DISPO-5.1** WHEN an operator aborts a modern parked review that has an
  applicable finalized round THE SYSTEM SHALL persist one bulk `review_aborted`
  operator-response event binding that `review_round_id`, the reviewed HEAD, and
  the frozen set of every `finding_instance_id` in that round.
- **DISPO-5.2** THE SYSTEM SHALL treat `review_aborted` membership as decision
  context only and SHALL NOT synthesize per-finding `rejected`, `aborted`, or
  other disposition events.
- **DISPO-5.3** WHEN review abort succeeds THE SYSTEM SHALL grant no approval,
  permit no continuation, and terminate the run as `cancelled`.
- **DISPO-5.4** THE SYSTEM SHALL keep `review_aborted` distinct from review
  `skip` and from system cancellation such as `superseded_by_new_push`.
- **DISPO-5.5** THE SYSTEM SHALL persist the abort event and the
  `parked → cancelled` status transition in one transaction. IF any write in
  that transaction fails THEN THE SYSTEM SHALL roll it back, leave the run
  parked, and keep the worktree.
- **DISPO-5.6** THE SYSTEM SHALL perform worktree cleanup only after that
  transaction commits. IF cleanup fails or the process dies after commit THEN
  THE SYSTEM SHALL leave the durable cancelled/terminal outcome unchanged.
- **DISPO-5.7** WHEN a parked review has no round identity (legacy snapshot)
  THE SYSTEM SHALL still allow abort, record it at run/review level with audit
  identity explicitly unavailable, and SHALL NOT treat display `fN` values as
  durable instance IDs or synthesize instance membership.

## 6. Audit document

**Story:** As an operator, I want to fetch a JSON audit document for any existing
run without the TUI, so that I can reconstruct disposition history and related
occurrences as of a durable watermark.

- **DISPO-6.1** THE SYSTEM SHALL provide one dedicated typed audit document
  built by a shared server-side audit read path over durable round,
  finding-instance, and disposition-event sources (and, until ROAD-5, an
  explicitly labeled compatibility projection of `step_results` for phase
  display).
- **DISPO-6.2** THE SYSTEM SHALL expose that document through a dedicated
  daemon RPC and a corresponding `porch agent` command that returns
  machine-readable JSON.
- **DISPO-6.3** THE SYSTEM SHALL use one typed audit-document builder for every
  surface: the agent command serializes it as JSON; the human CLI pretty-prints
  it; the TUI renders it in an additive audit/history view. Consumers SHALL NOT
  duplicate audit joins or interpretation rules.
- **DISPO-6.4** `get_run`, `porch agent status`, `findings[]`, and compatibility
  `steps[]` SHALL remain compact live operational state. They MAY advertise
  audit availability and SHALL NOT become the full audit contract or a
  competing authority.
- **DISPO-6.5** THE SYSTEM SHALL treat the audit document as a derived read
  model, not a new source of truth.
- **DISPO-6.6** WHEN building an audit document THE SYSTEM SHALL use one
  consistent SQLite read snapshot (or equivalent) and SHALL include a durable
  database-backed watermark/revision. THE SYSTEM SHALL NOT reuse in-memory
  `EventHub.state_rev` as that watermark.
- **DISPO-6.7** THE SYSTEM SHALL make the document available for every existing
  run, including `running`, `parked`, and terminal runs, using one schema
  across those states.
- **DISPO-6.8** WHEN the run is not terminal THE SYSTEM SHALL label the
  document explicitly as partial/as-of its watermark and observed run status,
  and SHALL NOT expose a final assurance outcome before one exists. Partial
  audit SHALL be a successful RPC/agent response, not an error.
- **DISPO-6.9** THE SYSTEM SHALL include every durable fact committed at that
  snapshot: open/finalized rounds, findings, disposition/authority events, and
  bulk-event memberships. THE SYSTEM SHALL include related-occurrence groups
  built only from fingerprint assignments already persisted at round
  finalization, using the exact key `(run_id, fingerprint_version, fingerprint)`.
- **DISPO-6.10** WHEN grouping related occurrences THE SYSTEM SHALL present an
  equivalence group (`related_occurrences` or equivalent), not a
  predecessor/successor chain; SHALL order occurrences by review-round ordinal
  then durable instance ID; SHALL preserve every occurrence independently; SHALL
  NOT rerun reconciliation, similarity matching, or candidate-key joins at read
  time; SHALL NOT group across runs or fingerprint versions; SHALL expose no
  group across a boundary where conservative reconciliation minted a new
  fingerprint; and SHALL NOT let grouping participate in current-round
  authorization or forward eligibility.
- **DISPO-6.11** THE SYSTEM SHALL NEVER derive a finding-level causal edge from
  a phase-attempt `caused_by_attempt_id` chain.
- **DISPO-6.12** IF the persisted run lifecycle cannot be reconciled with
  durable round/disposition records — for example a parked review with neither
  an applicable round nor an explicit legacy identity-unavailable record — THEN
  THE SYSTEM SHALL surface an explicit consistency anomaly or unknown state
  rather than a silently complete document.
- **DISPO-6.13** WHEN the TUI is used THE SYSTEM SHALL load the audit document
  lazily when the operator opens the audit/history view and SHALL NOT fetch the
  full document on every live `State` event or `StreamGap`.
- **DISPO-6.14** Live subscribe events SHALL remain refresh notifications only
  and SHALL NOT carry or replace the durable audit record.
- **DISPO-6.15** Repeated reads MAY return later watermarks as the run
  progresses; consumers SHALL NOT treat a partial document as final.
- **DISPO-6.16** IF pagination is used THE SYSTEM SHALL bind every page/cursor
  of one assembled document to the same durable watermark. The agent command
  MAY assemble pages into one complete JSON document for headless consumers.
- **DISPO-6.17** Ordering throughout the document SHALL be deterministic.
- **DISPO-6.18** UNTIL ROAD-5 ships THE SYSTEM SHALL label any phase timeline
  taken from `step_results` as a compatibility/inferred projection, not as the
  phase-event source of truth.

## 7. Quality attributes

**Section-kind:** nfr

**Story:** As a stakeholder, I want measurable quality targets for this feature, so that how-well is not left implicit.

- **Performance:** None — no standing product metric or SLO for audit-document
  latency exists (`docs/product/metrics.md` absent, `docs/ops/reliability.md`
  absent). Size bounds and pagination are Open Questions; no ceiling is
  invented here.
  <!-- Consult: metrics.md absent, reliability.md absent — no-op. -->
- **Security:** **DISPO-7.1** THE SYSTEM SHALL fail closed (no event, no status
  change, no fixer spawn) when the applicable round or reviewed HEAD does not
  match the parked review at persist time — verified by tests that mutate round
  applicability or HEAD between park and respond. No TB/THR identifier is
  cited (`docs/security/threat-model.md` absent).
  <!-- Consult: threat-model.md absent — no-op; grounded in frame-change lock. -->
- **Reliability:** **DISPO-7.2** THE SYSTEM SHALL commit `fix_requested` before
  the fixer process is spawned, so a crash cannot leave an unrecorded fixer
  side effect without a durable request event — verified by asserting the event
  row exists before spawn in the success path and by crash-before-spawn leaving
  no fixer process. **DISPO-7.3** THE SYSTEM SHALL commit `review_aborted` and
  `parked → cancelled` atomically, and SHALL CONTINUE TO leave the run parked
  with its worktree if that transaction rolls back — verified by injecting a
  write failure inside the transaction. **DISPO-7.4** THE SYSTEM SHALL keep the
  audit watermark durable in the database so it survives daemon restart —
  verified by fetching the same watermark after restart for an unchanged
  snapshot. No SLO identifier is cited (`docs/ops/reliability.md` absent).
  <!-- Consult: reliability.md absent — no-op; grounded in frame-change locks. -->
- **Accessibility:** None — the primary CUJ is headless JSON; the TUI view is
  additive. No standing accessibility conformance target exists
  (`docs/standards/accessibility.md` absent).

## 8. Guards

Touched files: `crates/porch-gate/src/rounds/schema.rs`,
`crates/porch-gate/src/rounds/mod.rs`, `crates/porch-gate/src/db.rs`,
`crates/porch-gate/src/rpc.rs`, `crates/porch-gate/src/events.rs`,
`crates/porch-gate/src/notes.rs`, `crates/porch-run/src/lib.rs`,
`crates/porch-run/src/deliver.rs`, `crates/porch/src/main.rs`,
`crates/porch/src/tui.rs`, `crates/porch-gate/porch-agent.md`.

- **DISPO-8.1** (guard) WHEN a review round is finalized THE SYSTEM SHALL
  CONTINUE TO leave `finding_instances` rows immutable (no update/delete of
  instance content).
- **DISPO-8.2** (guard) WHEN authorization compares a round's required producer
  set THE SYSTEM SHALL CONTINUE TO use the recorded required set and SHALL
  CONTINUE TO require the deterministic floor.
- **DISPO-8.3** (guard) WHEN a run is parked in the review phase THE SYSTEM
  SHALL CONTINUE TO accept `approve`, `skip`, `abort`, and `fix` with the
  existing `--findings` / `--yes` CLI shape.
- **DISPO-8.4** (guard) WHEN a run is parked in the compose phase THE SYSTEM
  SHALL CONTINUE TO accept respond, skip, and abort, and SHALL CONTINUE NOT TO
  write finding-instance or disposition events for compose abort.
- **DISPO-8.5** (guard) WHEN a run is parked in the rebase phase THE SYSTEM
  SHALL CONTINUE TO accept `fix` and `abort` only, and SHALL CONTINUE NOT TO
  treat `rebase0` fixer input as a durable finding instance.
- **DISPO-8.6** (guard) WHEN `porch agent status` or `get_run` is served THE
  SYSTEM SHALL CONTINUE TO return the compact live snapshot including
  `findings[]` display handles and `assurance_record`.
- **DISPO-8.7** (guard) WHEN subscribe clients receive `State` or `StreamGap`
  THE SYSTEM SHALL CONTINUE TO treat those events as refresh notifications
  (mailbox cap and sticky-gap unchanged).
- **DISPO-8.8** (guard) WHEN a parked run predates round records THE SYSTEM
  SHALL CONTINUE TO answer approve, fix, skip, abort, notes, and hunk lookup
  through its legacy snapshot.
- **DISPO-8.9** (guard) WHEN an operator approves a parked review THE SYSTEM
  SHALL CONTINUE TO record the head SHA, and WHEN they skip it SHALL CONTINUE
  TO leave that SHA unrecorded.
- **DISPO-8.10** (guard) WHEN compose abort succeeds THE SYSTEM SHALL CONTINUE
  TO leave the GitHub PR open and SHALL CONTINUE NOT TO call `gh` to close,
  draft, edit, or otherwise mutate it.
- **DISPO-8.11** (guard) `crates/porch-gate/src/notes.rs` — no behavior to guard
  beyond keeping `finding_notes.json` as non-audit notes keyed by display `fN`,
  not as disposition history. **DISPO-8.12** (guard) THE SYSTEM SHALL CONTINUE
  TO store per-finding notes in `finding_notes.json` and SHALL NOT migrate them
  into the disposition event log.

## Out of Scope

- Canonical `phase_events` source of truth, phase-attempt identity/ordinals,
  nested compose/fixer/`deliver_repair` operations, atomic phase handoff
  sequence positions, deliver-repair succession, compose-abort phase tree, and
  rebase-abort phase terminals — ROAD-5.
- Rebuilding `step_results` / `steps[]` from `phase_events` as dual
  representation — ROAD-5.
- MILE-3 durable authorization before external forward, restart reconciliation
  of ambiguous forwards, and fault injection across the forward boundary.
- MILE-6 external producers, SARIF, and the producer bar.
- Storing instance lineage edges, rerunning reconciliation at audit-read time,
  or using related-occurrence groups for authorization or forward eligibility.
- Closing, drafting, or editing GitHub PRs on abort.
- Migrating `finding_notes.json` into the disposition log.
- Replacing `porch agent status` / `get_run` with the full audit document.
- Using `EventHub.state_rev` as the audit watermark.
- New parked phase verbs or collapsing fixer and reviewer roles (ARCH-3).

## Open Questions

- Event, outcome, cause, and completeness field names for bulk operator-response
  events and the audit document — Jayden — due at design-solution — forbid-guess
  (cấm đoán).
- Dedicated daemon RPC and `porch agent` command names — Jayden — due at
  design-solution — forbid-guess (cấm đoán).
- Audit JSON schema and independent versioning from the compact status
  contract — Jayden — due at design-solution — forbid-guess (cấm đoán).
- Durable database-backed watermark implementation — Jayden — due at
  design-solution — forbid-guess (cấm đoán).
- Pagination and cursor design (same watermark per assembled document) —
  Jayden — due at design-solution — forbid-guess (cấm đoán).
- Actor/authority identity contract beyond Porch-as-executor for `--yes` and
  the locked cause strings — Jayden — due at design-solution — forbid-guess
  (cấm đoán).
- Whether immutable non-authoritative `reconciled_from` provenance will ever be
  required — Jayden — due only if a concrete audit need appears — forbid-guess
  (cấm đoán).
- Status-snapshot `audit_available` / reference shape — Jayden — due at
  design-solution — forbid-guess (cấm đoán).
- Human CLI pretty-print and TUI copy, including `--yes` after a no-change
  fixer — Jayden — due at design-solution — forbid-guess (cấm đoán).

Empty modern approve/skip membership is closed in `design.md` Decision 9
(explicit empty context set). Empty `fix_requested` remains rejected
(DISPO-3.5).
