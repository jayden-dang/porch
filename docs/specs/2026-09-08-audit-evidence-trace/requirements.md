# Requirements: Audit evidence trace

Feature code: TRACE
Status: In-progress
Date: 2026-09-08

Roadmap item: — (GOAL-2 join; no unbound ROAD under MILE-2; do not invent a slot).
Respects: ARCH-4, ARCH-10, ARCH-11, ARCH-12.

Projects producer identity, per-path coverage, the finding-to-producer key, and
the round's executing-config and protocol pins into the existing **Audit
document**. Does not rewrite DISPO-6.9. Does not claim MILE-2 Closed.

## 1. Producer slice on the audit document

**Story:** As an operator, I want `porch agent audit` JSON to list each producer
invocation with its engine kind and version, so I can name who reviewed a round
without opening SQLite.

- **TRACE-1.1** WHEN building an audit document THE SYSTEM SHALL include a
  `producers` array projected from persisted `round_producers` rows for every
  round on the run.
- **TRACE-1.2** WHEN projecting a producer THE SYSTEM SHALL include `round_id`,
  invocation id (the `round_producers.id`), `slot`, and
  `descriptor_equivalence_digest` from the table row, and SHALL include
  `adapter_kind`, `declared_engine_kind`, `reported_version`, and
  `observed_version_identity` from the stored `descriptor_json` when that JSON
  projects those fields.
- **TRACE-1.3** WHEN `reported_version` or `observed_version_identity` is stored
  as unavailable THE SYSTEM SHALL project that unavailable form with its reason
  and SHALL NOT invent a substitute version string. Live mint today always
  stores `reported_version` as unavailable (`not_reported`); the human render
  SHALL print that reason and SHALL NOT invent a semver string.
- **TRACE-1.4** THE SYSTEM SHALL order `producers` by review-round ordinal, then
  producer `slot`, then invocation id.
- **TRACE-1.5** THE SYSTEM SHALL treat each producer row as evidence of what
  ran (ARCH-11) and SHALL NOT render a producer verdict as a porch approval.
- **TRACE-1.6** IF a stored `descriptor_json` cannot project the
  descriptor-derived fields in TRACE-1.2 THEN THE SYSTEM SHALL still return the
  document, SHALL still project the row columns, SHALL mark those
  descriptor-derived fields unavailable with the parse reason, SHALL set an
  `AuditAnomaly`, and SHALL NOT fail the audit command.

## 2. Coverage slice on the audit document

**Story:** As an operator, I want that same JSON to list coverage state per
changed file per producer, so I can see which paths were completed, failed, or
waived.

- **TRACE-2.1** WHEN building an audit document THE SYSTEM SHALL include a
  `coverage` array projected from persisted `round_coverage` rows for every
  round on the run.
- **TRACE-2.2** WHEN projecting a coverage row THE SYSTEM SHALL include
  `round_id`, `producer_invocation_id`, `path`, `state`, and the stored
  `reason`, `authority`, and `completion_evidence` when present.
- **TRACE-2.3** THE SYSTEM SHALL use coverage `state` values
  `selected`, `completed`, `failed`, and `waived` exactly as stored, and SHALL
  NOT recompute coverage at read time.
- **TRACE-2.4** THE SYSTEM SHALL order `coverage` by review-round ordinal, then
  `producer_invocation_id`, then `path`.

## 3. Finding-to-producer key

**Story:** As an operator, I want each finding instance in the document to name
the producer invocation that emitted it, so I can follow a finding back to its
producer.

- **TRACE-3.1** WHEN projecting a finding instance THE SYSTEM SHALL include
  `producer_invocation_id` from the persisted `finding_instances` row.
- **TRACE-3.2** WHEN projecting a finding instance THE SYSTEM SHALL include
  `consequence` from that same row.
- **TRACE-3.3** WHEN a finalized instance has a `producer_invocation_id` THE
  SYSTEM SHALL use a value that equals some `producers[]` invocation id on the
  same document (same snapshot).
- **TRACE-3.4** THE SYSTEM SHALL NOT add `provenance_json`, `candidate_key`,
  `confidence_value`, or `confidence_kind` to the audit instance projection.

## 4. Round executing-config and protocol pins

**Story:** As an operator, I want each round in the document to show the trusted
config SHA and protocol schema version, so I can see which executing-config pin
and writer protocol the round ran under.

- **TRACE-4.1** WHEN projecting a review round THE SYSTEM SHALL include
  `trusted_config_sha` and `protocol_schema_version` from the persisted
  `review_rounds` row.
- **TRACE-4.2** THE SYSTEM SHALL present `trusted_config_sha` as a read-only
  pin (ARCH-4) and SHALL NOT change where code-executing config is loaded.
- **TRACE-4.3** THE SYSTEM SHALL NOT add `inventory_digest`,
  `round_context_elements`, `round_context_applications`,
  `round_producer_durations`, or `round_required_producers` to the document.

## 5. Human porch audit render

**Story:** As an operator, I want `porch audit` with no flags to print producers
and non-completed coverage paths plus the phase tree, so I can answer which
producer, which version, and which files were covered from the default command.

- **TRACE-5.1** WHEN `porch audit` runs without `--json` THE SYSTEM SHALL print
  a producers block, then per-round coverage-state counts that enumerate `path`
  (and stored `reason` / `authority` when present) only for states
  `selected`, `failed`, and `waived`, then the existing phase tree.
- **TRACE-5.2** WHEN `porch audit --json` or `porch agent audit` runs THE SYSTEM
  SHALL emit the full audit document, including the new slices, as JSON.
- **TRACE-5.3** THE SYSTEM SHALL NOT add a new CLI flag for this feature.
- **TRACE-5.4** THE SYSTEM SHALL keep `porch audit` and `porch agent audit` as
  distinct commands.

## 6. Shared builder and version

**Story:** As an operator, I want one document version from every audit surface,
so a script and a human read cannot disagree about what ran.

- **TRACE-6.1** WHEN assembling producers, coverage, thickened instances, or
  thickened rounds THE SYSTEM SHALL use the same single SQLite read snapshot
  already used by `build_audit`.
- **TRACE-6.2** THE SYSTEM SHALL read those rows through connection- or
  transaction-scoped helpers and SHALL NOT open a second `Db` snapshot to
  load them.
- **TRACE-6.3** WHEN the document includes the new slices THE SYSTEM SHALL set
  audit `schema_version` to `3`.
- **TRACE-6.4** THE SYSTEM SHALL treat audit `schema_version` and
  `PROTOCOL_SCHEMA_VERSION` as independent counters, even when both read `3`,
  and SHALL say so in `docs/usage.md`.
- **TRACE-6.5** THE SYSTEM SHALL add fields only. THE SYSTEM SHALL NOT rename,
  remove, or re-nest any existing `AuditDocument` field.

## 7. Quality attributes

**Section-kind:** nfr

**Story:** As a stakeholder, I want measurable quality targets for this feature, so that how-well is not left implicit.

- **Performance:** None — no standing product metric or SLO for audit-document
  latency exists (`docs/product/metrics.md` absent, `docs/ops/reliability.md`
  absent). This feature adds two SELECT joins over already-bounded round tables.
  <!-- Consult: metrics.md absent, reliability.md absent — no-op. -->
- **Security:** None — this feature is a derived local read of rows the operator
  already owns under `$PORCH_HOME`. It does not cross a new authn/authz or
  tenant boundary. No TB/THR identifier is cited (`docs/security/threat-model.md`
  absent).
  <!-- Consult: threat-model.md absent — no-op. -->
- **Reliability:** **TRACE-7.1** THE SYSTEM SHALL leave authorization's coverage
  and required-set reads in `applicability.rs` untouched, so a projection bug
  cannot change forward eligibility — verified by existing floor/round
  authorization tests remaining green without edits to those assertions.
  <!-- Consult: reliability.md absent — no-op; grounded in close-package guard. -->
- **Accessibility:** None — the primary CUJ is CLI text and JSON. No standing
  accessibility conformance target exists (`docs/standards/accessibility.md`
  absent).

## 8. Guards

Touched files: `crates/porch-gate/src/audit.rs`,
`crates/porch-gate/src/lib.rs`, `crates/porch/src/main.rs`,
`crates/porch-gate/porch-agent.md`, `docs/usage.md`, and the single
`schema_version` assert in `crates/porch/src/tui.rs`.

Expected additional test file: `crates/porch/tests/m22_audit_trace.rs`.
`crates/porch-gate/src/rounds/mod.rs` and `applicability.rs` are not in the
write set. `porch-agent.md` is `include_str!` in `skill.rs` and reaches an
installed skill only on reinstall.

- **TRACE-8.1** (guard) WHEN building an audit document THE SYSTEM SHALL
  CONTINUE TO emit existing fields `schema_version`, `run_id`, `run_status`,
  `completeness`, `watermark`, `rounds`, `instances`, `events`,
  `related_occurrences`, `phase`, and `anomaly` with their current names and
  nesting.
- **TRACE-8.2** (guard) WHEN projecting a round THE SYSTEM SHALL CONTINUE TO
  emit `id`, `ordinal`, `from_sha`, `to_sha`, `execution`,
  `assurance_completion`, and `finalized_at`.
- **TRACE-8.3** (guard) WHEN projecting a finding instance THE SYSTEM SHALL
  CONTINUE TO emit `id`, `round_id`, `fingerprint`, `fingerprint_version`,
  `path`, `criterion_id`, `evidence`, `severity`, and `action`.
- **TRACE-8.4** (guard) WHEN `porch audit` runs without `--json` THE SYSTEM
  SHALL CONTINUE TO print the phase tree produced from `doc.phase`.
- **TRACE-8.5** (guard) WHEN `porch audit --json` runs THE SYSTEM SHALL
  CONTINUE TO emit the same JSON bytes as `porch agent audit` for the same
  snapshot (one builder, including the new additive fields).
- **TRACE-8.6** (guard) WHEN the TUI is used THE SYSTEM SHALL CONTINUE TO load
  the audit document lazily when the history view opens and SHALL NOT fetch it
  on every live `State` or `StreamGap` (DISPO-6.13). The live assertion is
  `crates/porch/src/tui.rs` (`audit_fetch_count == 0` after gap/snapshot).
  This feature SHALL NOT change that fetch behavior or `history_panel_text`.
  The same test's `schema_version` assert MAY move from `2` to `3`; no other
  edit to that file is allowed.
- **TRACE-8.7** (guard) WHEN a parked or running review has neither an
  applicable round, legacy findings, nor an identity-unavailable authority
  event THE SYSTEM SHALL CONTINUE TO surface an explicit anomaly rather than a
  silently complete document (DISPO-6.12).
- **TRACE-8.8** (guard) WHEN grouping related occurrences THE SYSTEM SHALL
  CONTINUE TO use the persisted fingerprint key only and SHALL NOT rerun
  reconciliation at read time (DISPO-6.10).
- **TRACE-8.9** (guard) WHEN authorization compares a round's required producer
  set THE SYSTEM SHALL CONTINUE TO use the recorded required set and SHALL
  CONTINUE TO require the deterministic floor (ARCH-12). This feature SHALL
  NOT edit `rounds/mod.rs` or `applicability.rs`.

## Out of Scope

- Rewriting DISPO-6.9's SHALL text.
- New tables, columns, or durable writes.
- Read-time reconciliation, similarity matching, or candidate-key joins.
- Finding-level causal edges from phase attempts.
- Changing `get_run`, `porch agent status`, `findings[]`, or `steps[]`.
- Collapsing `porch audit` into `porch agent audit`.
- A second read snapshot.
- Inventing a `ROAD-N`.
- Claiming MILE-2 Closed.
- TUI rendering of producer or coverage slices (deferred, unscheduled).
- An inventory-digest or `content_blobs` read path (deferred, unscheduled).
- Projecting `round_required_producers` (FLOOR's authorization proof).

## Open Questions

None.
