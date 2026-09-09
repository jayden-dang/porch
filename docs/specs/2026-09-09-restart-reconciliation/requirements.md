# Requirements — restart reconciliation of ambiguous external effects

**Feature code:** RECON
**Roadmap item:** ROAD-8 (MILE-3)
**Goals:** GOAL-1
**Status:** Draft

## Context

ROAD-7 gave the forward boundary a durable **Forward record**: an intent committed
before the pushing command mutates `origin`, and an outcome committed after it and
before the pull request adapter. Nothing reads it. A restart still classifies a stale
`running` run on whether `runs.pr_url` was stored
(`crates/porch-gate/src/rounds/phase.rs:1007`-`:1011`), so a gate killed after a
successful push reports `failed` while its own record proves the branch reached
`origin`.

This feature makes the record a reconciliation input. It owns what a restart concludes
and what it tells the operator. It does not own which SHA may be forwarded — that is
the reopened MILE-3 blocker — and it does not own proving that a real kill produces
these states, which is ROAD-9.

## 1. A restart states what the record proves about `origin`

**Story:** As an operator whose gate died mid-forward, I want porch to tell me whether
my commit is on the shared remote, so that I do not have to inspect `origin` myself to
find out what the gate already knew.

- **RECON-1.1** WHEN reconciling an interrupted run THE SYSTEM SHALL derive a forward
  conclusion for each of that run's `deliver` phase attempts that has at least one
  forward record.
- **RECON-1.2** THE SYSTEM SHALL derive the conclusion from durable local evidence
  only, and SHALL NOT contact `origin` over the network to derive it.
- **RECON-1.3** WHEN a `deliver` attempt has an intent record and a reached-origin
  outcome record THE SYSTEM SHALL conclude that `origin` carries the authorized SHA.
- **RECON-1.4** WHEN a `deliver` attempt has an intent record and no outcome record
  THE SYSTEM SHALL conclude that the forward is undetermined, because the pushing
  command's result was never recorded.
- **RECON-1.5** WHEN a `deliver` attempt has an intent record and a `push_failed`
  outcome record THE SYSTEM SHALL conclude that the forward is undetermined rather
  than that `origin` is unchanged, because a failure reported by the pushing command
  is not evidence about the remote's state.
- **RECON-1.6** WHEN a `deliver` attempt has no forward record THE SYSTEM SHALL
  conclude that no forward was attempted for that attempt.
- **RECON-1.7** THE SYSTEM SHALL NOT assert that a pull request is absent. A
  reached-origin conclusion SHALL state that the pull request state is unrecorded,
  because the durable evidence for a crash before the pull request call and for a
  crash after it and before `set_pr_url` is byte-identical.
- **RECON-1.8** THE SYSTEM SHALL treat `runs.pr_url` as evidence only that a pull
  request was recorded, and never as evidence that a push landed.

## 2. A local discriminator resolves what it can, without the network

**Story:** As an operator, I want porch to use the evidence already on my disk before it
tells me it does not know, so that the undetermined case is as small as it can honestly
be made.

- **RECON-2.1** WHEN a conclusion would be undetermined THE SYSTEM SHALL read the gate
  repository's remote-tracking ref for the run's branch as a discriminator.
- **RECON-2.2** IF that tracking ref resolves to the authorized SHA THEN THE SYSTEM
  SHALL conclude that `origin` carries the authorized SHA, because git writes that ref
  only after the receiving end acknowledged the push.
- **RECON-2.3** IF that tracking ref is absent, unreadable, or resolves to any other
  SHA THEN THE SYSTEM SHALL leave the conclusion undetermined, because the ref is
  one-sided evidence: its absence does not show that the push failed.
- **RECON-2.4** THE SYSTEM SHALL perform that read before opening the reconciliation
  transaction, so that no subprocess runs while the state root's write lock is held.
- **RECON-2.5** THE SYSTEM SHALL NOT make any network call on the daemon startup path.
- **RECON-2.6** IF the gate repository is missing or the read fails THEN THE SYSTEM
  SHALL record an undetermined conclusion and continue reconciling, and SHALL NOT fail
  daemon startup.

## 3. The conclusion is durable and machine-queryable

**Story:** As the next porch process, I want the conclusion stored where I can query it,
so that a later forward can consult it instead of re-deriving it.

- **RECON-3.1** THE SYSTEM SHALL persist each conclusion as an append-only record
  naming the run, the `deliver` attempt, the verdict, the authorized SHA, the target
  ref, and the evidence the verdict rests on.
- **RECON-3.2** THE SYSTEM SHALL persist the conclusion in the same transaction that
  terminalizes the interrupted run's open phase attempts, so a restart cannot leave a
  run reconciled without a conclusion or the reverse.
- **RECON-3.3** THE SYSTEM SHALL NOT update or delete a conclusion record after it is
  committed.
- **RECON-3.4** THE SYSTEM SHALL NOT derive a forward *outcome* record from a
  conclusion, so that `FWDAUTH-3.5` continues to hold: an outcome comes only from
  porch's own command result.
- **RECON-3.5** THE SYSTEM SHALL constrain the verdict vocabulary in application code
  rather than in a database `CHECK`, so that a later verdict can be added without
  rebuilding an append-only table.
- **RECON-3.6** THE SYSTEM SHALL select the attempts needing a conclusion from the
  presence of forward records without a conclusion, and SHALL NOT select them from
  `runs.status`, so that a future writer-protocol upgrade — which terminalizes active
  runs inside `Db::open`, before recovery runs — cannot silently disarm reconciliation.
- **RECON-3.7** WHEN a conclusion already exists for a `deliver` attempt THE SYSTEM
  SHALL NOT write a second one for the same evidence, so that repeated restarts are
  idempotent.

## 4. A later conclusion about a terminal run is an append, not a reclassification

**Story:** As an operator whose undetermined run has since been answered, I want the
answer recorded against that run, so that the audit document is not permanently wrong
about it.

This section resolves a three-way tension: reconciliation may not rewrite a terminal
run's outcome, an undetermined forward can only be resolved later, and by the time it is
resolved the run is terminal. Resolved in favour of appending.

- **RECON-4.1** THE SYSTEM SHALL NOT change `runs.status`, the run's recorded outcome,
  or any existing phase event for a run that already has a terminal outcome.
- **RECON-4.2** THE SYSTEM SHALL permit appending a later conclusion record for a
  terminal run's `deliver` attempt, and SHALL define that append as evidence about a
  past forward rather than a reclassification of the run.
- **RECON-4.3** WHERE more than one conclusion exists for a `deliver` attempt THE
  SYSTEM SHALL treat the latest as current and SHALL retain the earlier ones.
- **RECON-4.4** THE SYSTEM SHALL NOT resolve an undetermined conclusion by contacting
  `origin` from the startup path; resolving it is reached only from a later forward for
  that ref, which already observes the remote under its own lease.

## 5. The operator is told what to do

**Story:** As an operator facing an interrupted forward, I want to be told the safe next
action, so that I do not improvise on a shared remote and open a duplicate pull request.

- **RECON-5.1** WHEN a run is reconciled with a reached-origin conclusion THE SYSTEM
  SHALL state in the operator-visible error that the branch carries the authorized SHA
  and that the pull request state is unrecorded.
- **RECON-5.2** WHEN a run is reconciled with any conclusion other than
  no-forward-attempted THE SYSTEM SHALL state the remedy: re-push the branch, and porch
  adopts an existing pull request for that branch rather than opening a second one.
- **RECON-5.3** THE SYSTEM SHALL keep the existing interruption phrase as a prefix of
  that error, so that operators and tests matching on it continue to match.
- **RECON-5.4** THE SYSTEM SHALL NOT introduce a new `runs.status` value, because the
  active-run lookup matches a fixed set of status literals and the audit read path
  renders anything outside `completed`, `failed`, and `cancelled` as non-terminal.

## 6. Compatibility of the state root

- **RECON-6.1** THE SYSTEM SHALL add its table additively and SHALL NOT raise
  `PROTOCOL_SCHEMA_VERSION`, because the upgrade path terminalizes every active run
  inside `Db::open` before recovery runs, which would destroy the very runs this
  feature exists to classify.
- **RECON-6.2** THE SYSTEM SHALL leave the three existing writer triggers unchanged and
  SHALL NOT add a trigger.
- **RECON-6.3** THE SYSTEM SHALL open an older state root without the table by creating
  it, and SHALL leave a state root that already has it unchanged.

## Non-functional

- **RECON-7.1** Reconciliation SHALL add at most one local git read per undetermined
  `deliver` attempt, and no remote read.
- **RECON-7.2** Reconciliation SHALL hold the state root's write lock for one
  `TransactionBehavior::Immediate` transaction per run, as it does today.
- **RECON-7.3** Tests SHALL NOT invoke `gh` or any network.

## Guards — what must keep working

- **RECON-8.1** A run interrupted with no forward record SHALL still reconcile to
  `failed`, unchanged.
- **RECON-8.2** A run interrupted with a stored `pr_url` SHALL still reconcile to
  `ci_monitor_interrupted`, unchanged.
- **RECON-8.3** Every open phase attempt of an interrupted run SHALL still be
  terminalized with outcome `interrupted`.
- **RECON-8.4** `forward_records` — its DDL, its `CHECK` constraints, and its partial
  unique index — SHALL be unchanged.
- **RECON-8.5** `authorized_forward_sha`, `assert_head_continuity`, and certify's
  correction commit SHALL be unchanged; they belong to the open blocker.

## Out of scope

- Resolving the undetermined case by probing `origin`. The next forward for that ref
  observes the remote under its existing lease; making that path write a conclusion is
  a follow-on, not this build.
- A new operator command to force reconciliation. `MILE-4` owns operator escape.
- Retention of `forward_records` and of conclusion records. Both grow without bound.
  A retention module already exists for config refs; extending it to these tables is a
  known deferral, named here so it is not an oversight.
- Proving that a real kill at each boundary produces exactly the states enumerated in
  the design. That is ROAD-9's fault-injection suite, and the design's enumeration is
  what it will assert against.
- A post-push verification failure. That run fails closed on its own path with its own
  error and is not interrupted, so no conclusion is derived for it. The predicate
  `attempt_reached_origin` must not be read as "the forward was clean"; its
  documentation says so.

## Open questions

1. **May an undetermined conclusion remain permanently unresolved?** The roadmap
   assigns discovery of ambiguous external effects to ROAD-8. This specification
   narrows the undetermined set with a local discriminator and defines how a later
   answer is appended, but it does not guarantee that an answer ever arrives: a branch
   never pushed again keeps its undetermined conclusion forever. Accepting that is a
   scope judgement against the roadmap's wording, and it is recorded rather than
   assumed. Owner Jayden.
2. **Should the conclusion reach `porch agent sync`?** A run whose branch landed but
   whose pull request did not is a divergence between the operator's checkout and the
   pipeline, which is what the sync hint exists to show. Left out of this build to keep
   the operator surface to one message; worth deciding before MILE-4.
