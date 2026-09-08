# Design: Audit evidence trace

Feature code: TRACE
Status: Implemented
Date: 2026-09-08
Requirements: ./requirements.md

## Context

`build_audit` already assembles a derived **Audit document** from one Deferred
SQLite snapshot (`crates/porch-gate/src/audit.rs`). It loads rounds, finding
instances, authority events, and the phase tree. It never reads
`round_producers` or `round_coverage`, even though those tables exist
(`rounds/schema.rs`) and `&Db` readers `producers_for_round` /
`coverage_for_round` already return them (`rounds/mod.rs`).
`AuditInstance` drops `producer_invocation_id` and `consequence`.
`AuditRound` drops `trusted_config_sha` and `protocol_schema_version`.
The committed row type is `RoundRecord`, not `ReviewRound`.
Default `porch audit` prints `render_phase_tree` only
(`crates/porch/src/main.rs`).

The binding constraint is **one builder, one snapshot, additive wire**
(TRACE-6.1, TRACE-6.5, Cut Released / External compat). Calling
`producers_for_round(&Db)` from inside `build_audit` **deadlocks**: `Db::conn`
is a non-reentrant mutex already held for the Deferred transaction
(`audit.rs:159`, `db.rs:172`). TRACE does not add `_conn` siblings on
`rounds/mod.rs`. It adds two private run-scoped loaders in `audit.rs`, the same
shape as `load_instances` (`audit.rs:251`). That is two SELECTs for the run,
not `2N` per-round queries, and it leaves `rounds/mod.rs` at zero diff.

This is not a new persistence boundary and not a contested interface. The
document type already exists. The render function already exists.
`architect skipped: one obvious shape — extend build_audit and run_human_audit.`
No ADR. Optional system docs (`docs/security/`, `docs/ops/`, `docs/standards/`,
`docs/codebase/`) are absent; consult is a no-op.

Spine this design relies on: **ARCH-4** (expose `trusted_config_sha` as a pin,
do not change load), **ARCH-10** (no new crate), **ARCH-11** (producer rows are
evidence), **ARCH-12** (do not project `round_required_producers` into a
substitutability check; leave `applicability.rs` alone).

Fresh retrieval (neighbors, focus DISPO, terms producer / coverage / audit):
PHASE via both (`audit.rs`, `main.rs`); ROUND via both (`rounds/mod.rs`,
coverage rows); FLOOR via term (producer). OWNS `5/13`. Advisory, not a gate.
Reuse is the existing `load_instances` query shape in `audit.rs`, not a new
`rounds` API and not a neighbor CODE import.

## Decisions

1. **Extend `build_audit`, do not add a second builder.** One snapshot stays the
   only join. New slices are fields on `AuditDocument`.
2. **Private `load_producers` / `load_coverage` on `&Transaction`.** Same
   pattern as `load_instances`. Query by `run_id`, `INNER JOIN review_rounds`,
   `SELECT` the `round_id` column. Do not add `_conn` readers. Do not edit
   `rounds/mod.rs`. Do not call `&Db` readers from inside `build_audit`
   (deadlock).
3. **Parse `descriptor_json` with a porch-gate-local serde DTO**, not
   `porch_review::ProducerDescriptor`. No new crate dependency. The DTO names
   only the descriptor-derived TRACE-1.2 fields. Extra JSON keys are ignored.
   Row columns always project. If the JSON cannot project those fields, mark
   them unavailable with the parse reason, set `AuditAnomaly`, still return
   the document (TRACE-1.6). Do not invent a version string.
4. **`schema_version` 2 → 3.** Additive fields only. New `Vec` fields and the
   four new scalars (`trusted_config_sha`, `protocol_schema_version`,
   `producer_invocation_id`, `consequence`) all carry `#[serde(default)]` so a
   new CLI can deserialize a still-running older daemon. Document that this
   counter is independent of `PROTOCOL_SCHEMA_VERSION` (also 3).
5. **Human render is a sibling of `render_phase_tree`, not a rewrite of it.**
   `run_human_audit` prints producers, then coverage counts with
   non-`completed` paths, then the existing tree. No new flag. Today's live
   mint always stores `reported_version` as unavailable (`not_reported`); the
   producers block prints that reason. `observed_version_identity` is the
   field that currently carries real identity.
6. **TUI does not render the new slices.** `AuditDocument` grows;
   `history_panel_text` stays counts-only. The only allowed `tui.rs` edit is
   the existing unit test's `schema_version` assert (`2` → `3`).
7. **`applicability.rs` and `rounds/mod.rs` are not edited** (TRACE-7.1,
   TRACE-8.9). Authorization keeps its own `&Db` coverage read.
8. **No new ROAD-N. No MILE-2 close.** Catalog Status stays Draft until the
   feature ships.

## Architecture

### B. Audit document projection

Satisfies: TRACE-1.1, TRACE-1.2, TRACE-1.3, TRACE-1.4, TRACE-1.5, TRACE-1.6,
TRACE-2.1, TRACE-2.2, TRACE-2.3, TRACE-2.4, TRACE-3.1, TRACE-3.2, TRACE-3.3,
TRACE-3.4, TRACE-4.1, TRACE-4.2, TRACE-4.3, TRACE-6.1, TRACE-6.2, TRACE-6.3,
TRACE-6.4, TRACE-6.5, TRACE-7.1, TRACE-8.1, TRACE-8.2, TRACE-8.3, TRACE-8.7,
TRACE-8.8, TRACE-8.9
Reuse: rung 2 — extends `crates/porch-gate/src/audit.rs` `build_audit` /
`load_rounds` / `load_instances`
Respects: ARCH-4, ARCH-10, ARCH-11, ARCH-12
Surface:
- `AuditDocument` JSON via `get_audit` / `rpc::get_audit_result` / daemon `get_audit` — **replace**: same method, additive fields, `schema_version` 3.
- `run_agent_audit` (`main.rs:409`) — **replace**: both `porch agent audit` and `porch audit --json` call this one function, so TRACE-8.5 byte identity is structural.
- In-repo tests `m20_dispo.rs`, `m21_phase.rs` that deserialize `AuditDocument` or assert `schema_version == 2` — **replace**: `#[serde(default)]` on new vecs and the four new scalars; bump version asserts to `3`.
- `m21_phase.rs` exact `porch audit` stdout pins — **replace**: prepend evidence lines; do not loosen the tree lines.
- `m21_phase.rs:1504` 200-event latency guard — **frozen**: it seeds zero review rounds and does not measure TRACE. No standing product SLO exists.
- TUI `history_audit` (`tui.rs:82`) and `history_panel_text` — **frozen**: stores the document; counts-only render unchanged.
- TUI unit test `schema_version` assert (`tui.rs:1375`) — **replace**: `2` → `3` only. Fetch-count asserts at `tui.rs:1360-1365` stay.
- crates.io / script consumers of `porch agent audit` JSON — **frozen**: additive keys only; TRACE-6.5 is the gate. No dual schema to retire.
- `docs/usage.md`, `crates/porch-gate/porch-agent.md` — **replace**: `schema_version` 3 and the independent protocol counter. `porch-agent.md` is `include_str!` in `skill.rs:13` and reaches an installed skill only on reinstall.
- `producers_for_round` / `coverage_for_round` / `applicability.rs` — **frozen**: not called by TRACE; leave untouched.
Interface: `AuditProducer` and `AuditCoverage`; `AuditDocument.producers` and
`.coverage`; added fields on `AuditRound` and `AuditInstance`. `build_audit`
stays `fn(&Db, &str) -> Result<AuditDocument>`. Private
`load_producers(tx, run_id)` and `load_coverage(tx, run_id)` take
`&Transaction` and return the projected rows (including `round_id` from SQL).
New vecs and the four new scalars use `#[serde(default)]`.
Depth: n/a — extends `audit.rs`
Locality: `audit.rs` **extend**. `lib.rs` **extend**. `rounds/mod.rs` **leave**.
`applicability.rs` **leave**. `Cargo.toml` **leave** (no new dependency).

`load_rounds` SELECT adds `trusted_config_sha`, `protocol_schema_version`.
`load_instances` SELECT adds `producer_invocation_id`, `consequence`.
`load_producers` is one run-scoped join:

```sql
SELECT r.ordinal, p.round_id, p.id, p.slot, p.descriptor_json,
       p.descriptor_equivalence_digest
FROM round_producers p
INNER JOIN review_rounds r ON r.id = p.round_id
WHERE r.run_id = ?1
ORDER BY r.ordinal, p.slot, p.id
```

`UNIQUE (round_id, slot)` makes the invocation-id tiebreak in TRACE-1.4
unreachable inside a round. State the `ORDER BY` anyway.

`load_coverage` is the sibling join, `ORDER BY r.ordinal, p.producer_invocation_id, path`.
`PRIMARY KEY (producer_invocation_id, path)` makes that total within a producer.

Parse `descriptor_json` with the gate-local DTO. Copy
`descriptor_equivalence_digest` from the column. Do not recompute it.
`applicability.rs` omits `declared_engine_kind` and `reported_version` from its
preimage; the audit slice still emits those fields from JSON when present.

TRACE-3.3 is a schema foreign key (`schema.rs:108-109`) plus `NOT NULL`.
`m22_audit_trace` still asserts every instance id resolves in the same
document. A miss sets `AuditAnomaly` and still returns the document; it is
not a TRACE-1.6 case (that ID is descriptor parse only). Migration briefly
sets `foreign_keys = OFF` (`schema.rs:252`); that window is why the test
exists.

Do not load context elements, applications, durations, required-producer
rows, or `content_blobs`.

### C. Human `porch audit` render

Satisfies: TRACE-5.1, TRACE-5.2, TRACE-5.3, TRACE-5.4, TRACE-8.4, TRACE-8.5,
TRACE-8.6
Reuse: rung 2 — extends `run_human_audit` / `render_phase_tree` in
`crates/porch/src/main.rs`
Surface:
- `run_human_audit` (`main.rs:385`) — **replace**: prints evidence then the tree.
- `run_agent_audit` (`main.rs:409`) — **frozen** here: JSON path is section B. Named so TRACE-8.5 is visible.
- `render_phase_tree` (`main.rs:350`) — **frozen**: signature and tree format unchanged; still called last.
- clap split — **frozen**: no new flag.
- `m21_phase.rs` default-stdout pins — **replace** (same rows as section B).
- TUI `history_panel_text` and `tui.rs:1360-1365` — **frozen**: counts-only; subscribe must not call `get_audit`.
- TUI unit test `schema_version` assert (`tui.rs:1375`) — **replace** (same one-line edit as section B; one verb).
Interface: `fn render_evidence_blocks(doc: &AuditDocument) -> String` plus the
existing `render_phase_tree`. `run_human_audit` concatenates evidence then tree.
Depth: n/a — extends `main.rs`
Locality: `main.rs` **extend**. `tui.rs` **extend** (one assert). `porch-agent.md` /
`usage.md` **replace** (same files as section B; one verb).

Evidence text, in order:

1. Producers block: one line per `AuditProducer` (round, slot, declared
   engine kind, reported-version unavailable reason, observed identity).
2. Per-round coverage: counts per state; enumerate `path` plus reason/authority
   only when state is `selected`, `failed`, or `waived`.
3. Unchanged phase tree.

Colour stays optional and must remain unambiguous with colour off.
No persistence in this module.

## Seams for testing

| Seam | Kind | Covers |
|---|---|---|
| `build_audit` (`m22_audit_trace` + existing `m20_dispo` / `m21_phase`) | integration | TRACE-1.1–1.6, TRACE-2.1–2.4, TRACE-3.1–3.4, TRACE-4.1–4.3, TRACE-6.1–6.3, TRACE-6.5, TRACE-8.1–8.3, TRACE-8.7, TRACE-8.8 |
| `porch audit` / `porch audit --json` / `porch agent audit` (`m22_audit_trace`) | integration | TRACE-5.1–5.4, TRACE-6.4, TRACE-8.4, TRACE-8.5 |
| `tui.rs:1360-1365` existing unit (fetch counts stay; `schema_version` assert → 3) | unit | TRACE-8.6 |
| existing floor/round authorize tests (`m19_floor`, `m18_round_identity`) | integration | TRACE-7.1, TRACE-8.9 |

New seams: none. `render_evidence_blocks` is covered through the CLI row, same
as `render_phase_tree` today. TRACE-3.3 gets its own assert inside the
`build_audit` row.

## Coverage check

Every TRACE ID appears in exactly one `Satisfies:` line.

| ID | Section |
|---|---|
| TRACE-1.1–1.6 | B |
| TRACE-2.1–2.4 | B |
| TRACE-3.1–3.4 | B |
| TRACE-4.1–4.3 | B |
| TRACE-5.1–5.4 | C |
| TRACE-6.1–6.5 | B |
| TRACE-7.1 | B |
| TRACE-8.1–8.3, 8.7–8.9 | B |
| TRACE-8.4–8.6 | C |

Deliberately unmapped: none.

UI design: omitted. No Satisfies ID is delivered through a browser surface.
