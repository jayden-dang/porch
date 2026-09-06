# Tasks: Disposition History

> **For agentic workers:** after plan approval, pick one execute skill —
> `build-in-waves`, `build-by-story`, or `build-inline`. The chosen skill writes
> `Execution-mode:`.

Feature code: DISPO
Status: Approved
Date: 2026-09-06
Execution-mode: continuous
Max-concurrency: auto
Requirements: ./requirements.md
Design: ./design.md

**Goal:** Record review-level authority as append-only instance-keyed events and serve a derived JSON audit document as-of a durable watermark.

**Architecture:** `porch-gate::rounds::authority` owns Immediate-txn persist (SQL on `&Transaction`, never nested `&Db` locks). `persist_authority_with_run_effects` writes event + run status/HEAD/steps together. `porch-gate::audit` builds `AuditDocument` on a Deferred snapshot. `porch-run` maps `fN`→instance ULID then persist-before-fixer. RPC `get_audit` / `porch agent audit`; TUI loads audit only when the history view opens.

**Tech Stack:** Rust 2024 workspace; rusqlite; `cargo test -p porch-gate --test m18_rounds`; `cargo test -p porch --test m20_dispo`; PATH fakes, no network.

## Global Constraints

- `AGENTS.md` sha256:`b0f2a01edc9c` — English; no network in unit tests; PATH fakes; use-case slices; `unsafe_code = "forbid"`.
- `docs/architecture/INDEX.md` sha256:`7d9d801f34d7` — **ARCH-3**, **ARCH-6**, **ARCH-10**, **ARCH-11**, **ARCH-12**. Do not implement ARCH-13 here.
- `docs/agents/project.md` sha256:`a3f5ba93c4d8` — `cargo fmt --all --check`; `cargo check --workspace --all-targets`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`. `--workspace` is required.
- `CONTEXT.md` sha256:`51115c3f118f` — **Finding instance**, **Disposition history**, **Bulk operator response**, **Audit document**.
- Solo band — no fake assignees.
- Do **not** put requirement IDs in Rust source or test names.
- No `docs/standards/` / `docs/codebase/` — `docs/agents/project.md` is the command SSOT.

## File Structure

| File | Responsibility |
|---|---|
| `crates/porch-gate/src/rounds/authority.rs` | **Create** — kinds, members, persist + run-effects txn |
| `crates/porch-gate/src/audit.rs` | **Create** — `AuditDocument`, `build_audit` Deferred snapshot |
| `crates/porch/tests/m20_dispo.rs` | **Create** — fail-closed persist, audit JSON, CLI |
| `crates/porch-gate/src/rounds/schema.rs` | DDL `authority_events` / `authority_event_members` |
| `crates/porch-gate/src/rounds/mod.rs` | migrate; bump `audit_rev` in `finalize_round` and `close_interrupted` |
| `crates/porch-gate/src/db.rs` | `ensure_column` `runs.audit_rev` |
| `crates/porch-gate/src/rounds/mod.rs` | `mod authority`; re-export persist types |
| `crates/porch-gate/src/lib.rs` | `mod audit`; re-export `AuditDocument` / `build_audit` |
| `crates/porch-gate/src/rpc.rs` | `get_audit`; optional `audit_available` on snapshot |
| `crates/porch-gate/src/daemon.rs` | dispatch `get_audit` |
| `crates/porch-run/src/lib.rs` | respond persist; `fN`→ULID; persist-before-fixer |
| `crates/porch/src/main.rs` | `porch agent audit` |
| `crates/porch/src/tui.rs` | lazy history view; no audit fetch on `State`/`StreamGap` |
| `crates/porch-gate/porch-agent.md` | document `audit` command |
| `crates/porch-gate/tests/m18_rounds.rs` | persist unit/integration on tempfile |
| `crates/porch/tests/m3_review.rs` `m4_fix.rs` `m13_workflow.rs` `m18_round_identity.rs` `m19_floor.rs` `m17_pr_compose.rs` | guards |

Retrieval: `cluster(DISPO)` has empty OWNS until this file exists (coverage `with_owns` 2/12 for FLOOR/PRCMP). `blast_radius` on mapped `porch-gate`/`porch-run`/`porch` paths overlaps ROUND/FLOOR/PRCMP/GATE — extend those crates, do not extract a new one.

---

### Task 1: Authority tables and fail-closed persist

**Files:** Create `crates/porch-gate/src/rounds/authority.rs`. Modify `crates/porch-gate/src/rounds/schema.rs`, `crates/porch-gate/src/rounds/mod.rs`, `crates/porch-gate/src/db.rs`, `crates/porch-gate/src/lib.rs`. Test `crates/porch-gate/tests/m18_rounds.rs`.

**Reuse:** rung 2 — `Transaction::new_unchecked(..., Immediate)` in `open_round` / `finalize_round`; ULID minting; `ensure_column`.

**Interfaces:**
- Produces: `AuthorityKind`, `MemberRole`, `PersistAuthorityPlan { run_id, kind, expected_round_id, expected_head, live_head, actor_kind, authority_event_id, head_changed, identity_unavailable, members }`, `persist_authority(&Db, PersistAuthorityPlan) -> Result<String, AuthorityError>` (event id is a ULID `String`; `AuthorityStale` on drift). Instance ids on `members` are `finding_instances.id` strings. Txn-scoped SQL only while the mutex is held.
- Consumes: `Db`, `RoundId` (`rounds::RoundId`)

**Depends-on:** none

- [ ] Test: insert `review_approved` with context members; `finding_instances` row bytes unchanged.
- [ ] Test: `expected_round_id` or `live_head` ≠ stored `to_sha`/`head_sha` → `AuthorityStale`, no row.
- [ ] Test: modern empty instance list still inserts the event with zero members; legacy abort uses `identity_unavailable=1` and `review_round_id` NULL.
- [ ] Test: `fN` is never stored on members (ULIDs only).
- [ ] Run `cargo test -p porch-gate --test m18_rounds` — expect missing table/API.
- [ ] Implement DDL, `persist_authority`, bump `audit_rev` on persist and on `finalize_round` / `close_interrupted`.
- [ ] Run tests; expect pass. Commit: `feat(porch-gate): persist append-only authority events`

_Requirements: DISPO-1.1, DISPO-1.2, DISPO-1.3, DISPO-1.5, DISPO-1.7, DISPO-1.8, DISPO-2.1, DISPO-2.2, DISPO-2.5, DISPO-2.6, DISPO-3.1, DISPO-3.3, DISPO-3.4, DISPO-3.7, DISPO-5.1, DISPO-5.2, DISPO-5.4, DISPO-5.7, DISPO-7.1, DISPO-8.1_

---

### Task 2: Persist with run effects

**Files:** Modify `crates/porch-gate/src/rounds/authority.rs`. Test `crates/porch-gate/tests/m18_rounds.rs`.

**Reuse:** rung 2 — extend Task 1 Immediate txn; do not call `Db::set_run_status` / `insert_step_result` / `set_review_approved_head_sha` while the txn holds the mutex.

**Interfaces:**
- Produces: `RunEffects { status, error, approved_head, steps }`, `persist_authority_with_run_effects(&Db, PersistAuthorityPlan, RunEffects) -> Result<EventId, AuthorityError>`
- Consumes: `PersistAuthorityPlan`, `AuthorityError`

**Depends-on:** Task 1

- [ ] Test: abort plan + `cancelled` status commit together; injected write failure leaves parked + no event.
- [ ] Test: approve plan writes `review_approved_head_sha` and review `completed` step in the same txn as the event.
- [ ] Run `cargo test -p porch-gate --test m18_rounds` — expect missing helper.
- [ ] Implement `persist_authority_with_run_effects` SQL on the same `tx`.
- [ ] Run tests; expect pass. Commit: `feat(porch-gate): commit authority events with run effects`

_Requirements: DISPO-5.5, DISPO-7.3_

---

### Task 3: Review approve and skip record bulk events

**Files:** Modify `crates/porch-run/src/lib.rs`. Test `crates/porch/tests/m3_review.rs`, `crates/porch/tests/m20_dispo.rs`.

**Reuse:** rung 2 — `agent_respond_inner` approve/skip arms; `select_findings` unused here; `finish_certify_and_deliver` after successful persist.

**Interfaces:**
- Consumes: `persist_authority_with_run_effects`, `instances_for_round` (map display ordinals to ULIDs **before** the txn), `rev-parse` for `live_head`
- Produces: none (respond JSON unchanged)

**Depends-on:** Task 2

- [ ] Test: approve writes one `review_approved` with every instance as `context`, sets approved HEAD, still certifies.
- [ ] Test: skip writes `review_skipped`, no approved HEAD, certify/deliver skipped.
- [ ] Test: HEAD moved after park → stale, still parked, no event.
- [ ] Run `cargo test -p porch --test m3_review --test m20_dispo` — expect missing events.
- [ ] Wire approve/skip through the helper. Commit: `feat(porch-run): record bulk approve and skip events`

_Requirements: DISPO-2.3, DISPO-2.4, DISPO-8.3, DISPO-8.9_

---

### Task 4: Fix requested before fixer spawn

**Files:** Modify `crates/porch-run/src/lib.rs`. Test `crates/porch/tests/m4_fix.rs`, `crates/porch/tests/m20_dispo.rs`.

**Reuse:** rung 2 — `respond_fix`, `select_findings` (default all-blocking), empty selection usage error.

**Interfaces:**
- Consumes: `persist_authority` then `spawn_and_wait_fixer`
- Produces: stored `fix_requested` `EventId` for Task 5 `--yes`

**Depends-on:** Task 1

- [ ] Test: `--findings` / default blocking freeze `target` ULIDs; no `fN` on members.
- [ ] Test: empty selection still usage-exits and inserts no event.
- [ ] Test: event row exists before fixer binary is invoked; round/HEAD drift does not spawn fixer.
- [ ] Run `cargo test -p porch --test m4_fix --test m20_dispo` — expect missing `fix_requested`.
- [ ] Persist then spawn. Commit: `feat(porch-run): persist fix_requested before fixer spawn`

_Requirements: DISPO-3.2, DISPO-3.5, DISPO-3.6, DISPO-7.2_

---

### Task 5: Post-fix rereview and standing consent

**Files:** Modify `crates/porch-run/src/lib.rs`. Test `crates/porch/tests/m4_fix.rs`, `crates/porch/tests/m20_dispo.rs`.

**Reuse:** rung 2 — `finish_rereview`, session-free `run_review_phase(..., true)`, existing `--yes` complete path.

**Interfaces:**
- Consumes: `fix_requested` EventId, `persist_authority_with_run_effects` for porch `review_approved`
- Produces: new round/instances (existing open/finalize)

**Depends-on:** Task 4, Task 2

- [ ] Test: fixer `Ok` with unchanged HEAD still opens a new round and new instance ids; prior instance events remain; fingerprint does not copy membership.
- [ ] Test: required producers run again on that SHA; no automatic second no-op fix loop.
- [ ] Test: `--yes` after park writes porch `review_approved` citing `fix_requested`, frozen new-round instances, `head_changed=false` when HEAD equal.
- [ ] Test: rereview with no blocking findings completes without a bulk approve event.
- [ ] Run `cargo test -p porch --test m4_fix --test m20_dispo` — expect missing consent event.
- [ ] Implement. Commit: `feat(porch-run): record standing-consent approve on the new round`

_Requirements: DISPO-4.1, DISPO-4.2, DISPO-4.3, DISPO-4.4, DISPO-4.5, DISPO-4.6, DISPO-4.7, DISPO-4.8_

---

### Task 6: Review abort is atomic

**Files:** Modify `crates/porch-run/src/lib.rs`. Test `crates/porch/tests/m3_review.rs`, `crates/porch/tests/m18_round_identity.rs`, `crates/porch/tests/m20_dispo.rs`.

**Reuse:** rung 2 — Task 2 helper; `finish_remove_worktree` **after** commit; legacy `findings_json` parks.

**Interfaces:**
- Consumes: `persist_authority_with_run_effects` (`review_aborted`, `cancelled`)
- Produces: none

**Depends-on:** Task 2

- [ ] Test: modern abort binds round, HEAD, all instance ids as `context`; run `cancelled`; distinct from skip and from `superseded by new push`.
- [ ] Test: txn failure leaves parked + worktree; cleanup runs only after commit.
- [ ] Test: legacy park abort records `identity_unavailable`, no `fN` members.
- [ ] Run `cargo test -p porch --test m3_review --test m18_round_identity --test m20_dispo`.
- [ ] Implement. Commit: `feat(porch-run): record review_aborted with cancelled status`

_Requirements: DISPO-5.3, DISPO-5.6, DISPO-8.8_

---

### Task 7: Audit document builder and RPC

**Files:** Create `crates/porch-gate/src/audit.rs`. Modify `crates/porch-gate/src/lib.rs`, `crates/porch-gate/src/rpc.rs`, `crates/porch-gate/src/daemon.rs`. Test `crates/porch/tests/m20_dispo.rs`.

**Reuse:** rung 2 — `read_history` Deferred `&Transaction` pattern; `rpc_call` / daemon method match. Do not call `&Db` readers inside the snapshot txn.

**Interfaces:**
- Produces: `AuditDocument { schema_version, run_id, run_status, completeness, watermark: { audit_rev, review_history_revision }, rounds, instances, events, related_occurrences, phase, anomaly }`, `build_audit(&Db, &str) -> Result<AuditDocument>`, RPC `get_audit { run_id }`
- Consumes: authority tables, round/instance rows, `step_results`

**Depends-on:** Task 1

- [ ] Test: one snapshot includes committed events/instances; `related_occurrences` groups `(run_id, fingerprint_version, fingerprint)` with len≥2, ordered by round ordinal then instance id; no lineage edges; grouping unused for authorize.
- [ ] Test: parked run returns success with `completeness=as_of`; watermark is `audit_rev` not `state_rev`; inferred `phase.kind=step_results_inferred`.
- [ ] Test: inconsistent parked review (no round, no legacy snapshot) sets structured `anomaly`, still 200-equivalent success.
- [ ] Test: compact `get_run` still has `fN` findings; may set `audit_available`.
- [ ] Test: after `--yes` on a no-change fixer, the document exposes `head_changed=false` on the porch `review_approved` event.
- [ ] Run `cargo test -p porch --test m20_dispo` and `cargo test -p porch --test m18_round_identity`.
- [ ] Implement builder + `get_audit`. Commit: `feat(porch-gate): serve derived audit documents`

_Requirements: DISPO-1.4, DISPO-1.6, DISPO-2.7, DISPO-4.9, DISPO-6.1, DISPO-6.2, DISPO-6.4, DISPO-6.5, DISPO-6.6, DISPO-6.7, DISPO-6.8, DISPO-6.9, DISPO-6.10, DISPO-6.11, DISPO-6.12, DISPO-6.15, DISPO-6.16, DISPO-6.17, DISPO-6.18, DISPO-7.4, DISPO-8.6, DISPO-8.11_

---

### Task 8: Agent CLI and lazy TUI

**Files:** Modify `crates/porch/src/main.rs`, `crates/porch/src/tui.rs`, `crates/porch-gate/porch-agent.md`. Test `crates/porch/tests/m20_dispo.rs`.

**Reuse:** rung 2 — clap `AgentCommand`; TUI subscribe still `get_run` on `State`/`StreamGap`.

**Interfaces:**
- Consumes: `get_audit` / `AuditDocument`
- Produces: `porch agent audit [--run-id]` pretty JSON

**Depends-on:** Task 7

- [ ] Test: `porch agent audit` prints the same builder JSON; status JSON stays compact.
- [ ] Test: subscribe/`apply_snapshot` path does not call `get_audit`; opening the history view calls it once; `MAILBOX_CAP` unchanged (`events.rs`).
- [ ] Run `cargo test -p porch --test m20_dispo`.
- [ ] Implement. Commit: `feat(porch): expose agent audit and lazy TUI history`

_Requirements: DISPO-6.3, DISPO-6.13, DISPO-6.14, DISPO-8.7_

---

### Task 9: Park and floor guards

**Files:** Test `crates/porch/tests/m17_pr_compose.rs`, `crates/porch/tests/m13_workflow.rs`, `crates/porch/tests/m18_round_identity.rs`, `crates/porch/tests/m19_floor.rs`. Modify `crates/porch-run/src/lib.rs` only if a guard fails.

**Reuse:** rung 2 — existing compose/rebase/legacy/floor tests.

**Interfaces:** none new

**Depends-on:** Task 3, Task 4, Task 6

- [ ] Test: compose abort still writes no authority members and does not call `gh` (`m17_pr_compose`).
- [ ] Test: rebase park still rejects approve/skip (`m13_workflow`); `rebase0` is not an authority member.
- [ ] Test: FLOOR authorize still uses the recorded required set (`m19_floor`); notes stay in `finding_notes.json` keyed by `fN` (`m18_round_identity`).
- [ ] Run `cargo test -p porch --test m17_pr_compose --test m13_workflow --test m18_round_identity --test m19_floor`.
- [ ] Fix regressions if any. Commit: `test(porch): guard compose rebase floor and notes after DISPO`

_Requirements: DISPO-8.2, DISPO-8.4, DISPO-8.5, DISPO-8.10, DISPO-8.12_
