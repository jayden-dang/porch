# Design: Disposition History

Feature code: DISPO
Status: Approved
Date: 2026-09-06
Requirements: ./requirements.md

## Context

ROUND already stores immutable `finding_instances` (ULID PK, insert at
`finalize_round`, no UPDATE) and projects display `f0…fn` on `get_run`
(`rpc.rs` `status_from_instance`). Review park respond in `porch-run`
writes `review_approved_head_sha`, `step_results`, and run status only —
there is no authority event log. Coverage `authority` is a file-waiver
column; producer `action` is a suggestion. Legacy parks still serve
`findings_json` when `round_for_decision` returns none.

The binding constraint is **instance immutability plus fail-closed
applicability**: operator disposition cannot live on the instance row, and a
respond write must observe the same applicable finalized round and reviewed
HEAD that the operator saw. That rules out adding `current_disposition` on
`finding_instances` (would reopen ROUND) and rules out treating `step_results`
detail strings as the audit log (lossy, not instance-keyed, not fail-closed
on round/HEAD drift).

A second constraint is the live/audit split. `RunSnapshot.state_rev` is
in-memory `EventHub` (`events.rs`); TUI refetches `get_run` on every `State` /
`StreamGap`. Stuffing the join into `get_run` would pull the full log on every
live refresh and make `findings[]` a competing authority. The audit document
is therefore a derived read model on a new RPC, built from one SQLite snapshot
with a **database-backed** watermark.

Fresh retrieval (advisory): catalog now includes DISPO (Draft→Approved card,
ROAD-4). OWNS coverage remains thin (DISPO has no `tasks.md` yet). Neighbors
by term/path: ROUND (instance identity, OOS named ROAD-4), FLOOR (authorize
from recorded required set; park-verb guards), PRCMP (compose abort must not
write disposition events). Spine: **ARCH-3, ARCH-6, ARCH-10, ARCH-11, ARCH-12**.
ARCH-13 is a consumer of this record (MILE-3), not implemented here.

Scan digest: `.skills/DISPO/scan.md`.

## Decisions

1. **Authority events are a new append-only store** (`authority_events` +
   `authority_event_members`), keyed by event ULID and `finding_instance_id`.
   No UPDATE of `finding_instances`. Membership `role` is `context` (bulk
   approve/skip/abort) or `target` (`fix_requested`).
2. **Respond persist uses `BEGIN IMMEDIATE`** like `open_round` /
   `finalize_round`. Git `rev-parse` runs **before** the txn. Inside the
   txn, SQL-only helpers on that `&Transaction` re-read the applicable
   round, round `to_sha`, and `run.head_sha` and compare them to the
   caller-supplied expected round/HEAD. Mismatch rolls back with no status
   change (DISPO-2.6, DISPO-3.7, DISPO-7.1). Existing `&Db` readers
   (`round_for_decision`, `instances_for_round`, `set_run_status`, …) each
   lock the non-reentrant `Db` mutex and **must not** be called while the
   Immediate txn holds it.
3. **`fix_requested` commits before `spawn_and_wait_fixer`.** The fixer is
   not a SQLite writer; "before spawn" is commit-then-spawn, not one
   distributed transaction with the child process.
4. **`--yes` writes a second event** `kind=review_approved`, `actor_kind=porch`,
   `authority_event_id` = the original `fix_requested` id, membership = all
   instances of the **new** applicable round, plus `head_changed=false` when
   HEAD equals the HEAD recorded on that `fix_requested`.
5. **Watermark is `runs.audit_rev` (INTEGER, monotonic per run)** incremented
   in the same Immediate transaction as every authority-event insert **and**
   on `finalize_round` / history bumps so new instances without events still
   advance the as-of token. The document also echoes `review_history_revision`.
   `EventHub.state_rev` is never the audit watermark.
6. **RPC method `get_audit` and CLI `porch agent audit`** return the typed
   document. `get_run` / `agent status` unchanged except an optional
   `audit_available: true` boolean on the compact snapshot (additive JSON
   field; old clients ignore it).
7. **Related occurrences are a SELECT** grouping finalized instances by
   `(run_id, fingerprint_version, fingerprint)` with count > 1. No lineage
   table. Groups with a single member are omitted (neutral: no established
   related occurrence).
8. **Until ROAD-5**, `AuditDocument.phase` is
   `{ kind: "step_results_inferred", steps: [...] }` copied from existing
   `step_results` rows, labeled so it is not the phase SoT.
9. **Junction membership, not JSON arrays.** Querying "events for this
   instance" and fail-closed freeze-at-respond both need relational members.
   Empty `members` is allowed in two cases: (a) legacy `review_aborted` with
   `identity_unavailable=1` (no instance rows exist); (b) a modern round
   whose `instances_for_round` is genuinely empty — persist `context`
   membership as the explicit empty set (closes the empty-set Open
   Question: do not omit the event). `fix_requested` still rejects an empty
   **target** set before persist (DISPO-3.5).
10. **No new crate** (ARCH-10). Types live in `porch-gate`; `porch-run` calls
    persist helpers; `porch` CLI/TUI consume RPC.

No ADR: none of these is hard-to-reverse in the ADR-gate sense (additive
tables, additive RPC; `audit_rev` is a nullable-default column). Decision 3
is surprising but cheap to reverse (move the commit) — it stays in this file.

Proposed names that close Open Questions for this design's approval:

| Open question | Proposal |
|---|---|
| Event kinds | `review_approved`, `review_skipped`, `fix_requested`, `review_aborted` |
| Membership role | `context` \| `target` |
| Actor | `actor_kind`: `operator` \| `porch`; `actor_ref` omitted until the identity contract is specified |
| RPC / command | `get_audit` / `porch agent audit [--run-id]` |
| Watermark | `runs.audit_rev` + echoed `review_history_revision` |
| Completeness | `completeness`: `as_of` \| `terminal` (terminal iff run status ∈ {completed, failed, cancelled}) |
| JSON version | `schema_version: 1` on the audit document, independent of status snapshot |

## Architecture

Vocabulary: **module**, **interface**, **implementation**, **seam**.

### 1. `porch-gate::rounds::authority` — append-only authority log

Satisfies: DISPO-1.1, DISPO-1.2, DISPO-1.3, DISPO-1.5, DISPO-1.7, DISPO-1.8, DISPO-2.1, DISPO-2.2, DISPO-2.5, DISPO-2.6, DISPO-3.1, DISPO-3.3, DISPO-3.4, DISPO-3.7, DISPO-4.7, DISPO-5.1, DISPO-5.2, DISPO-5.4, DISPO-5.7, DISPO-7.1, DISPO-8.1
Reuse: rung 2 — extend `rounds` migrate/`BEGIN IMMEDIATE` / ULID minting (`mod.rs` `open_round` / `finalize_round`). New tables because `finding_instances` is insert-only and `step_results` is not instance-keyed. Rung 7 only for the new module file, not a new crate.
Respects: ARCH-11, ARCH-10
Surface:
- `finding_instances` rows — **frozen** (ROUND contract; this change does not UPDATE them; workaround: append events)
- `runs.review_approved_head_sha` writers (`porch-run` approve/`--yes`) — **compat** (keep writing the column for FLOOR/authorize/status; follow-up ROAD-5/MILE-3 may treat events as the authority SoT)
- `runs.findings_json` legacy parks — **frozen** (legacy abort records `identity_unavailable` rather than synthesizing members)
Interface: `AuthorityKind { ReviewApproved, ReviewSkipped, FixRequested, ReviewAborted }`; `MemberRole { Context, Target }`; `PersistAuthorityPlan { run_id, kind, expected_round_id: Option<RoundId>, expected_head: Option<String>, live_head: Option<String>, actor_kind, authority_event_id: Option<EventId>, head_changed: Option<bool>, identity_unavailable: bool, members: Vec<(FindingInstanceId, MemberRole)> }`. `persist_authority(db, plan) -> Result<EventId>` and `persist_authority_with_run_effects(db, plan, RunEffects { status, error, approved_head, steps }) -> Result<EventId>`: one mutex lock, `Transaction::new_unchecked(..., Immediate)`, **txn-scoped SQL only** (no `&Db` readers/writers). Guard: stored applicable round id and round `to_sha` / `runs.head_sha` must match `expected_round_id` / `expected_head`; `live_head` (from `rev-parse` before the txn) must match `expected_head` for kinds that bind HEAD. Then insert event+members, apply optional run effects (status, `review_approved_head_sha`, `step_results`) on the same `tx`, `audit_rev += 1`, commit. `AuthorityStale` if the guard fails — no inserts, no status change. Txn-scoped helpers (private): `applicable_round_id_tx`, `instances_for_round_tx`. Public `&Db` `events_for_run` remains for tests outside a persist txn.
Depth: if this module vanished, callers would still need "append one typed event with expected vs live round/HEAD, actor, and a frozen set of instance ids with role context|target, fail-closed when those drifted, optionally with run status/step writes in the same txn." Table names, CHECKs, and ULID minting stay inside.
Locality: new `crates/porch-gate/src/rounds/authority.rs`; `schema.rs` **extend** (DDL); `mod.rs` **extend** (migrate + re-export); `finalize_round` **extend** and `close_interrupted` **extend** (`audit_rev += 1` beside `review_history_revision`); other round readers **leave**.

DDL (additive `CREATE TABLE IF NOT EXISTS` in `ROUND_DDL` / migrate):

```sql
ALTER TABLE runs ADD COLUMN audit_rev INTEGER NOT NULL DEFAULT 0;
-- via ensure_column, matching required_set_digest

CREATE TABLE IF NOT EXISTS authority_events (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    kind TEXT NOT NULL CHECK (kind IN (
        'review_approved', 'review_skipped', 'fix_requested', 'review_aborted'
    )),
    review_round_id TEXT REFERENCES review_rounds(id),
    reviewed_head TEXT,
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('operator', 'porch')),
    authority_event_id TEXT REFERENCES authority_events(id),
    head_changed INTEGER CHECK (head_changed IN (0, 1)),
    identity_unavailable INTEGER NOT NULL DEFAULT 0 CHECK (identity_unavailable IN (0, 1)),
    created_at TEXT NOT NULL,
    CHECK (
        (identity_unavailable = 1 AND review_round_id IS NULL)
        OR (identity_unavailable = 0)
    )
);
CREATE INDEX authority_events_run ON authority_events(run_id, created_at, id);

CREATE TABLE IF NOT EXISTS authority_event_members (
    event_id TEXT NOT NULL REFERENCES authority_events(id) ON DELETE CASCADE,
    finding_instance_id TEXT NOT NULL REFERENCES finding_instances(id),
    role TEXT NOT NULL CHECK (role IN ('context', 'target')),
    PRIMARY KEY (event_id, finding_instance_id, role)
);
CREATE INDEX authority_event_members_instance
    ON authority_event_members(finding_instance_id);
```

Guard inside the Immediate txn when `identity_unavailable = 0` (SQL on `&tx`,
not `round_for_decision(&Db)`):

1. Recompute applicable finished/complete round for `run_id` (same predicates
   as `applicable_round_for_run`, inlined on `tx`). It must equal
   `expected_round_id`.
2. That round's `to_sha` and `runs.head_sha` must equal `expected_head`.
3. For kinds that bind HEAD (`review_approved`, `review_aborted`,
   `fix_requested`): `live_head` (caller `rev-parse` **before** the txn) must
   equal `expected_head`. `review_skipped` does not bind HEAD (`expected_head`
   / `live_head` may be NULL).
4. `context` members must equal the instance ids of that round at txn time
   (empty list is a valid freeze). `target` members must equal the caller-
   supplied set (non-empty; empty targets never reach persist).

Mismatch → `Err(AuthorityStale)` without inserts. Mapping `fN` → instance id
happens in `porch-run` using the same ordinal as `status_from_instance`,
**before** the txn.

`--yes`: caller passes `actor_kind=porch`, `authority_event_id=fix_requested_id`,
`kind=review_approved`, `head_changed` from comparing current live HEAD to the
`expected_head` stored on that `fix_requested` row. No bulk event when the new
round has no blocking findings (DISPO-4.8) — `porch-run` does not call persist.

### 2. `porch-run` respond orchestration

Satisfies: DISPO-2.3, DISPO-2.4, DISPO-3.2, DISPO-3.5, DISPO-3.6, DISPO-4.1, DISPO-4.2, DISPO-4.3, DISPO-4.4, DISPO-4.5, DISPO-4.6, DISPO-4.8, DISPO-5.3, DISPO-5.5, DISPO-5.6, DISPO-7.2, DISPO-7.3, DISPO-8.2, DISPO-8.3, DISPO-8.4, DISPO-8.5, DISPO-8.8, DISPO-8.9, DISPO-8.10, DISPO-8.12
Reuse: rung 2 — extend `agent_respond_inner` / `respond_fix` / `finish_rereview`. Existing certify→deliver (`finish_certify_and_deliver`), skip tails, `--findings` default-blocking (`select_findings`), empty-selection usage error, compose/rebase verb filters stay.
Respects: ARCH-3, ARCH-6
Surface:
- `agent_respond` JSON (D12) — **frozen** (status shape after respond stays compact `AgentStatus`; audit is a different command)
- `porch-agent.md` verb table — **extend** (document that events are recorded; verbs unchanged)
- m3_review / m4_fix / m18_round_identity respond tests — **extend** (assert new event rows; existing SHA/status assertions stay)
- compose abort `deliver.rs` — **leave** (no disposition writes; DISPO-8.4 / 8.10)
- rebase `rebase0` JSON — **leave** (DISPO-8.5)
- `finding_notes.json` — **leave** (DISPO-8.12)
Interface: `agent_respond` signature unchanged. Internals call
`persist_authority_with_run_effects` (never `Db::set_run_status` /
`insert_step_result` / `set_review_approved_head_sha` while a persist txn is
open). `respond_fix`: that helper commits `fix_requested` → **then**
`spawn_and_wait_fixer`. Review abort: helper writes event + `cancelled` in
one Immediate txn, then `finish_remove_worktree`. Approve/skip: helper writes
event + approved HEAD (approve only) + step_results + status in that same
txn. Git `rev-parse` is outside the txn and supplies `live_head`.
Depth: n/a — extends `porch-run` respond.
Locality: `crates/porch-run/src/lib.rs` **extend**; `deliver.rs` **leave**; `porch-agent.md` **extend**.

Sequence — review `fix` (DISPO-3.6, DISPO-4.*):

1. Resolve targets (display ids → instance ULIDs). Empty → existing usage error, no persist.
2. `persist_authority(fix_requested, targets, reviewed_head)`.
3. Spawn fixer (unchanged). `Ok` + same HEAD still rereviews (DISPO-4.2–4.4).
4. Session-free rereview (existing `run_review_phase(..., after_fix=true)`).
5. If parked and `--yes`: persist `review_approved` (porch, cite fix event) then existing complete path. If no blocking: complete, no bulk event.
6. Exactly one rereview per this `fix_requested` (existing `finish_rereview` already does one; do not loop on no-op HEAD).

Review abort (DISPO-5.5–5.6): one Immediate txn writes event + `parked→cancelled`. Worktree removal after commit. Rollback leaves parked + worktree. CLI respond still does not publish EventHub (scan); TUI already polls `get_run` after respond — **leave** that refresh.

### 3. `porch-gate::audit` — derived audit document

Satisfies: DISPO-1.4, DISPO-1.6, DISPO-2.7, DISPO-4.9, DISPO-6.1, DISPO-6.4, DISPO-6.5, DISPO-6.6, DISPO-6.7, DISPO-6.8, DISPO-6.9, DISPO-6.10, DISPO-6.11, DISPO-6.12, DISPO-6.15, DISPO-6.16, DISPO-6.17, DISPO-6.18, DISPO-7.4, DISPO-8.11
Reuse: rung 2 — pattern from `read_history` (Deferred `&Transaction`, inlined
SQL). Do **not** call public `&Db` readers (`instances_for_round`,
`step_results_for_run`, `resolve_run_assurance`) while that snapshot txn
holds the mutex. New builder module because no existing type joins events ×
instances × fingerprints.
Respects: ARCH-11
Surface:
- `get_run` `RunSnapshot` — **compat** (`audit_available: true` additive; full tree not added here; follow-up none required if clients ignore unknown fields)
- `StatusFindingDto` / `findings[]` — **frozen** (display `fN` contract; audit document carries instance ULIDs separately)
- `step_results` / snapshot `steps[]` — **compat** (copied into `phase.kind=step_results_inferred` until ROAD-5 replaces it)
Interface: `AuditDocument { schema_version: u32, run_id, run_status, completeness, watermark: { audit_rev, review_history_revision }, rounds, instances, events, related_occurrences, phase, anomaly }`; `build_audit(db, run_id) -> Result<AuditDocument>` takes one mutex lock, `TransactionBehavior::Deferred`, and reads with txn-scoped SQL (same shape as `read_history`). `related_occurrences`: groups of instance ids sharing `(run_id, fingerprint_version, fingerprint)` with len≥2, ordered by round ordinal then instance id. `completeness=terminal` iff `run.status` ∈ {completed,failed,cancelled}; else `as_of`. `anomaly` set when parked/running review has neither applicable round nor legacy `identity_unavailable` abort/event and no `findings_json` — structured `{ code, detail }`, document still returned (success). Pagination v1: none; the agent command returns the whole document. If a later bound is added, cursors MUST embed the watermark pair (DISPO-6.16).
Depth: if this module vanished, callers would still need "given a run id, a versioned JSON object as-of `(audit_rev, review_history_revision)` containing rounds, instances, authority events with members, fingerprint groups, and an inferred phase list." SQL and grouping stay inside.
Locality: new `crates/porch-gate/src/audit.rs`; `lib.rs` **extend** (re-export); extract txn-scoped reads here rather than calling `&Db` readers; round/db public APIs **leave**.

`caused_by_attempt_id` is not present until ROAD-5; DISPO-6.11 is satisfied by **not emitting** finding-level causal edges.

### 4. RPC, agent CLI, TUI consumer

Satisfies: DISPO-6.2, DISPO-6.3, DISPO-6.13, DISPO-6.14, DISPO-8.6, DISPO-8.7
Reuse: rung 2 — `rpc_call` / daemon `match req.method`; clap `AgentCommand` enum. New method/command, not a new transport.
Respects: ARCH-8 (no new forge/agent protocol — still the existing daemon JSON-RPC + `porch agent`)
Surface:
- daemon method table (`daemon.rs` `handle_connection`) — **extend** (`get_audit`)
- `porch agent status` / D12 JSON — **compat** (optional `audit_available`; compact fields frozen)
- TUI subscribe loop — **leave** (still `get_run` on State/Gap; MUST NOT call `get_audit` there)
- EventHub / MAILBOX_CAP / sticky-gap — **frozen** (DISPO-8.7)
Interface: RPC `get_audit { run_id }` → `AuditDocument`. CLI `porch agent audit [--run-id]` prints pretty JSON (same builder). Human `porch audit` is an alias to that pretty print if we want a non-agent entry; v1 can be agent-only plus `porch agent audit` documented in `porch-agent.md`. TUI: new key opens history view, one `get_audit` per open, not on live events.
Depth: n/a — extends rpc/cli/tui.
Locality: `rpc.rs` **extend**; `daemon.rs` **extend**; `porch/src/main.rs` **extend**; `tui.rs` **extend** (lazy view only); `events.rs` **leave**.

## Seams for testing

Prefer existing `m18_rounds` / `m3_review` / `m4_fix` / `m18_round_identity` / `m19_floor`. One new integration file `crates/porch/tests/m20_dispo.rs` for the audit document and fail-closed persist. Unit tests in `porch-gate` for grouping SQL and persist guards (PATH-free, sqlite tempfile — same as `m18_rounds.rs`).

| Seam | Kind | Covers |
|---|---|---|
| `persist_authority` + sqlite tempfile (`m18_rounds` style) | integration | DISPO-1.1, DISPO-1.2, DISPO-1.3, DISPO-1.5, DISPO-1.7, DISPO-1.8, DISPO-2.1, DISPO-2.2, DISPO-2.5, DISPO-2.6, DISPO-3.1, DISPO-3.2, DISPO-3.3, DISPO-3.4, DISPO-3.5, DISPO-3.7, DISPO-5.1, DISPO-5.2, DISPO-5.4, DISPO-5.7, DISPO-7.1, DISPO-8.1 |
| `agent_respond` review approve/skip (`m3_review` + m20) | integration | DISPO-2.3, DISPO-2.4, DISPO-8.3, DISPO-8.9 |
| `respond_fix` persist-before-spawn (`m4_fix` + m20) | integration | DISPO-3.6, DISPO-4.2, DISPO-4.3, DISPO-4.4, DISPO-4.5, DISPO-4.6, DISPO-4.7, DISPO-4.8, DISPO-7.2, DISPO-8.3 |
| review abort txn (`m3_review` / m20) | integration | DISPO-5.3, DISPO-5.5, DISPO-5.6, DISPO-7.3 |
| `build_audit` / `get_audit` (`m20_dispo`) | integration | DISPO-1.4, DISPO-1.6, DISPO-2.7, DISPO-4.9, DISPO-6.1–6.12, DISPO-6.15–6.18, DISPO-7.4, DISPO-8.11 |
| `porch agent audit` CLI (`m20_dispo`) | integration | DISPO-6.2, DISPO-6.3 |
| TUI does not fetch audit on State (`m8_operator` or unit) | integration | DISPO-6.13, DISPO-6.14, DISPO-8.7 |
| `get_run` / status compact (`m18_round_identity`, `m19_floor`) | integration | DISPO-8.6 |
| compose/rebase/legacy parks (`m17_pr_compose`, `m18_round_identity`) | integration | DISPO-8.4, DISPO-8.5, DISPO-8.8, DISPO-8.10, DISPO-8.12 |
| rereview identities (`m4_fix`, `m18_round_identity`) | integration | DISPO-4.1 |
| FLOOR authorize unchanged (`m19_floor`) | integration | DISPO-8.2 |

DISPO-8.2 is a guard-only seam on existing FLOOR tests (no new FLOOR logic).

## Coverage check

Every DISPO-* ID appears in exactly one `Satisfies:` line (modules 1–4). Guards DISPO-8.2, DISPO-8.4, DISPO-8.5, DISPO-8.8, DISPO-8.10, DISPO-8.12 are mapped to respond/legacy seams that **leave** those paths unchanged. No ID is unmapped.

UI design: deleted — no browser-rendered surface.

Reuse lines: module 1 rung 2 + new file; 2–4 extend existing. No new third-party crate.
