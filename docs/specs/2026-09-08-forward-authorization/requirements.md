# Requirements: Durable forward authorization

Feature code: FWDAUTH
Status: Implemented
Date: 2026-09-08

Roadmap item: ROAD-7 (MILE-3 — Crash-safe forwarding). Serves GOAL-1, whose outcome
names durable authorization and reviewed-input binding before a forward.

Respects: ARCH-1, ARCH-5, ARCH-11, ARCH-13.

Vocabulary is locked in `CONTEXT.md` — **Deliver**, **Custody**, **Run**, **Park**,
**Phase attempt**, **Assurance protocol**. This feature adds **Forward
authorization** and **Forward record** there; these criteria use those terms and do
not redefine them.

One MILE-3 blocker was resolved in discovery and is recorded on
`docs/roadmap/INDEX.md`: a restart distinguishes an authorized completed push from one
never attempted by reading durable local state. The other — whether an approval
survives HEAD advancing past the reviewed SHA — was completed by EQUAL (ROAD-23);
see *Binding by equality* below. This feature owns the durable record. EQUAL owns the
exact binding. Restart classification is ROAD-8.

## 1. A forward carries only what continuity authorized

**Story:** As an operator, I want the gate to forward a commit continuity has blessed,
resolved once at the forward boundary, so that the record and the push agree.

- **FWDAUTH-1.5** IF no approved SHA is recorded for a run THEN THE SYSTEM SHALL fail
  closed rather than forward.
- **FWDAUTH-1.7** WHEN the forward boundary is reached THE SYSTEM SHALL evaluate
  continuity itself rather than relying on its caller having done so.
- **FWDAUTH-1.8** IF the live HEAD is not on the approved line THEN THE SYSTEM SHALL
  fail closed with an error naming both the live HEAD and the approved SHA.

### Binding by equality — completed by EQUAL (ROAD-23)

These criteria were recorded blocked because certify's correction commit made
equality fail-closed on every dirty `commands.format`. EQUAL moved the mutating
format into rebase, made certify verify-only, and implemented them as originally
worded. The descendant-tolerance paragraph above is historical.

- **FWDAUTH-1.1** WHEN the gate evaluates HEAD continuity THE SYSTEM SHALL
  require the live worktree HEAD to equal the recorded approved SHA.
- **FWDAUTH-1.2** THE SYSTEM SHALL NOT accept a live HEAD that is merely a
  descendant of the approved SHA.
- **FWDAUTH-1.3** IF the live HEAD differs from the approved SHA THEN THE
  SYSTEM SHALL fail closed with an error naming both SHAs.
- **FWDAUTH-1.4** WHEN a forward is performed THE SYSTEM SHALL forward the
  SHA that continuity authorized, and SHALL NOT re-read the worktree HEAD to choose
  what to forward. EQUAL returns the approved SHA after proving equality; a
  `rev-parse` remains as a guard only.
- **FWDAUTH-1.6** THE SYSTEM SHALL leave HEAD movement reachable only
  through the existing phase handoff that revokes the prior approval and re-reviews.

## 2. A forward intent is durable before any external effect

**Story:** As an operator, I want porch to have written down what it is about to push
before it pushes, so that a crash cannot hide an attempt from the record.

- **FWDAUTH-2.1** WHEN a forward attempt is about to mutate `origin` THE SYSTEM SHALL
  durably append a forward-intent record before invoking the pushing command.
- **FWDAUTH-2.2** WHEN appending a forward-intent record THE SYSTEM SHALL include the
  run, the owning `deliver` phase attempt, the target ref name, the authorized SHA,
  and the observed remote tip that the lease was resolved against.
- **FWDAUTH-2.3** THE SYSTEM SHALL commit the forward-intent record to durable storage
  before the pushing command is invoked.
- **FWDAUTH-2.4** IF appending the forward-intent record fails THEN THE SYSTEM SHALL
  NOT invoke the pushing command and SHALL fail the forward closed.
- **FWDAUTH-2.5** THE SYSTEM SHALL NOT update or delete a forward record after it is
  committed.
- **FWDAUTH-2.6** WHEN a forward is refused before any external mutation, on an
  unincorporated remote tip or an unverifiable safety fact, THE SYSTEM SHALL NOT
  append a forward-intent record for that refusal.
- **FWDAUTH-2.7** THE SYSTEM SHALL record at most one forward attempt per `deliver`
  phase attempt, and WHEN a deliver repair succeeds without moving HEAD THE SYSTEM SHALL
  hand the `deliver` phase off to its next attempt rather than forwarding a second time
  under the same one, so that a retry is a new attempt with its own record.

## 3. The forward outcome is durable before the PR call

**Story:** As an operator, I want the record to say whether the push landed, so that a
restart after a crash between the push and the PR does not read the run as never
attempted.

- **FWDAUTH-3.1** WHEN the pushing command reports success THE SYSTEM SHALL durably
  append a forward-outcome record naming the landed SHA, before invoking the pull
  request adapter.
- **FWDAUTH-3.2** WHEN the pushing command reports failure THE SYSTEM SHALL durably
  append a forward-outcome record carrying the failure detail.
- **FWDAUTH-3.3** WHEN appending a forward-outcome record THE SYSTEM SHALL relate it
  to the forward-intent record of the same forward attempt.
- **FWDAUTH-3.4** IF appending a successful forward-outcome record fails THEN THE
  SYSTEM SHALL fail the run closed and SHALL NOT re-invoke the pushing command for
  that attempt.
- **FWDAUTH-3.5** THE SYSTEM SHALL derive the forward-outcome record from the result
  of its own pushing command, and SHALL NOT read remote state to decide what the
  outcome record says.
- **FWDAUTH-3.6** WHEN a run enters `deliver` more than once THE SYSTEM SHALL record
  each forward attempt separately under its own `deliver` phase attempt.
- **FWDAUTH-3.7** WHEN the lease observation finds the target ref already at the
  authorized SHA THE SYSTEM SHALL append a forward-outcome record naming that landed
  SHA in a form distinguishable from a record of porch's own mutating push.

## 4. The record is a reconciliation input

**Story:** As the author of restart reconciliation, I want an unambiguous local record
to read, so that discovery does not have to begin by guessing.

- **FWDAUTH-4.1** THE SYSTEM SHALL expose the forward records for a run in a
  deterministic order.
- **FWDAUTH-4.2** WHEN forward records exist for a run THE SYSTEM SHALL make an
  authorized attempt whose push completed distinguishable from an authorized attempt
  that never invoked the pushing command, using those records alone.
- **FWDAUTH-4.3** THE SYSTEM SHALL treat the forward record as porch-owned evidence
  and SHALL NOT present it as an assurance outcome or an approval.

## 5. Compatibility of the state root

**Story:** As an operator upgrading an installed porch, I want the new record to
appear without hand-editing my database.

- **FWDAUTH-5.1** WHEN porch opens an existing state root THE SYSTEM SHALL create the
  forward-record storage additively, leaving prior rows readable.
- **FWDAUTH-5.2** THE SYSTEM SHALL raise the writer-protocol minimum exactly once for
  this feature.
- **FWDAUTH-5.3** THE SYSTEM SHALL document the upgrade and its rollback consequence
  for operators.

## Non-functional

- **Reliability:** no service-level objective layer exists for this local CLI; the
  reliability posture is the fail-closed rule in section 1 and the ordering rules in
  sections 2 and 3. Accepted risk, signer Jayden.
- **Performance:** **FWDAUTH-6.1** THE SYSTEM SHALL add no additional remote
  round-trip to a forward attempt beyond those the lease already performs.

## Guards — must keep working

- **FWDAUTH-7.1** (guard) WHEN a rebase produces an empty diff and the remaining
  phases are skipped THE SYSTEM SHALL CONTINUE TO complete without requiring a
  recorded approved SHA.
- **FWDAUTH-7.2** (guard) WHEN forwarding THE SYSTEM SHALL CONTINUE TO push with
  `--force-with-lease` against the observed remote SHA, SHALL CONTINUE TO refuse when
  live remote commits were not incorporated, and SHALL CONTINUE TO verify the ref
  after the push (ARCH-5).
- **FWDAUTH-7.3** (guard) THE SYSTEM SHALL CONTINUE TO leave `origin` history
  unrewritten (ARCH-1).
- **FWDAUTH-7.4** (guard) WHEN a deliver repair changes HEAD THE SYSTEM SHALL CONTINUE
  TO revoke the prior approval, re-review, and reach the next forward through the
  existing phase handoff.
- **FWDAUTH-7.5** (guard) WHEN authorization compares a round's required producer set
  THE SYSTEM SHALL CONTINUE TO use the recorded required set and SHALL CONTINUE TO
  require the deterministic floor (ARCH-12). This feature SHALL NOT edit
  `rounds/applicability.rs`.
- **FWDAUTH-7.6** (guard) THE SYSTEM SHALL CONTINUE TO produce a byte-identical
  hidden PR attestation for an identical run; this feature SHALL NOT change
  `assemble_scaffold` or `attestation_post_compose` content.
- **FWDAUTH-7.7** (guard) WHEN a stale `running` run is reconciled at daemon startup
  THE SYSTEM SHALL CONTINUE TO classify it exactly as it does today; changing that
  classification is ROAD-8.

## Out of Scope

- Restart reconciliation of ambiguous external effects, and any change to
  `reconcile_one_running` or the `pr_url` classification — ROAD-8.
- The fault-injection suite across the forward boundary — ROAD-9.
- Probing `origin` to discover what happened — ROAD-8 owns discovery.
- Projecting forward records onto the audit document or the TUI.
- Treating authority events, rather than the approved-SHA column, as the continuity
  source of truth.
- Closing MILE-3, and the three audit-document slices deferred on MILE-2.
- Inventing a `ROAD-N`.

## Open Questions

1. **May certify's correction commit be forwarded without re-review?** Certify runs
   `commands.format` and `commands.lint` after review approves and commits the result,
   so the forwarded tree can differ from the reviewed tree. Today it is forwarded on
   the descendant tolerance. The candidate answers are: re-review it through the
   existing revoke-and-handoff idiom the deliver-repair path already uses; declare
   porch's own deterministic correction exempt and record it as such; or forbid
   tree-mutating certify commands after approval. Each has a different blast radius,
   and the re-review answer needs a termination rule for a nondeterministic formatter.
   Owner Jayden. This blocks FWDAUTH-1.1 … FWDAUTH-1.4 and FWDAUTH-1.6, and it
   reopens the MILE-3 blocker recorded on `docs/roadmap/INDEX.md`.
2. The residual window between a completed push and its outcome write is owned by
   ROAD-8 and recorded as an owned unknown on `docs/roadmap/INDEX.md`; this feature
   must not invent the probe that closes it.
