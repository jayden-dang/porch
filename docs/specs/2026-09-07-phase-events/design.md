# Design: Phase-event history

Feature code: PHASE
Status: Approved
Date: 2026-09-07
Requirements: ./requirements.md

## Context

A run's phase lifecycle today lives in one mutable column, `runs.status`
(`crates/porch-gate/src/db.rs:105-113`), written through a single private funnel
`set_status` (`crates/porch-run/src/lib.rs:116`) that updates the row and publishes a
notification. Beside it sits `step_results` (`db.rs:114-122`) — `(id, run_id, step, status,
error, created_at)`, written through the sibling funnel `record_step` (`lib.rs:122`). That
table is **terminal-only**: one row per finished step, no start timestamp, no attempt
ordinal, no parent. Everything an operator can currently learn about phases is inferred from
it, including `AuditPhase { kind: "step_results_inferred" }` (`audit.rs:194`), the
placeholder DISPO left for this feature.

The constraint that shapes this design is that **the co-write invariant must be enforceable,
not merely conventional** — PHASE-1.8 and PHASE-1.9 are worthless if any call site can write
`runs.status` without its phase event. On a crate already installed from crates.io
(`docs/agents/project.md`: Production, Cut Released), a rule that holds only while everyone
remembers it is a rule that will be broken by the next contributor or by an older binary.
That rules out the cheap alternative — a free-standing `phase_events` appender that call
sites invoke alongside their existing `set_status` — because it leaves two independent writes
that a crash or an omission can separate. The seam has to *own* the status write.

Two facts make this affordable rather than a rewrite. First, DISPO already built this exact
shape: `persist_authority_with_run_effects(db, plan, RunEffects)`
(`rounds/authority.rs:204`) appends an append-only event and applies optional run status,
error, approved-head, and `step_results` rows inside one `TransactionBehavior::Immediate`
transaction, bumping `runs.audit_rev` in the same commit (`authority.rs:306`). `RunEffects`
(`authority.rs:130-135`) is already `{ status, error, approved_head, steps }` — precisely
the co-write payload PHASE needs. Second, most of the write side funnels through two private
functions in `porch-run` — 22 calls go through `set_status` — so redirecting them is
mechanical.

The funnels are not airtight, and that is the design's main scope surprise. A reference
search found **six production status writes and six production step-row writes that bypass
them**, calling `Db::set_run_status` / `Db::insert_step_result` directly:
`deliver.rs:284` (parked, awaiting compose), `:451` (cancelled, agent abort), `:516` and
`:562` (completed), `daemon.rs:285` (cancelled, superseded by new push), and `lib.rs:2161`
(parked); step rows at `deliver.rs:283, 450, 506, 507, 552, 553`. Every one of them is a
real phase transition, so every one must move onto the seam or the co-write invariant is
false on exactly the paths — compose park, agent abort, delivery completion, supersede —
where an operator most needs the record. Narrowing both `Db` methods to `pub(crate)` is what
makes that enforceable rather than remembered.

The read side is likewise smaller than it looked. The frame expected three step-string
lifecycle sites; a full enumeration found four, of which three decide position
(`lib.rs:2177 parked_phase`, `agent_run.rs:287 agent_status_from_snap`,
`tui.rs:146 compose_parked`) and one is display over the retained projection
(`tui.rs:530 render_pipeline`, which `rfind`s each canonical phase name to draw the pipeline
line). The design moves the three; the fourth keeps reading `steps[]`, which remains a
supported wire projection under guard PHASE-7.7.

Spine invariants this design relies on: **ARCH-3** (reviewer turns are session-free; the
fixer and reviewer roles stay distinct), **ARCH-10** (crates are use-case slices — no new
crate), **ARCH-13** (durable authorization and reviewed-input binding precede any external
forward). No decision here contradicts an invariant, so no ADR-or-supersede event arises.

Optional system docs (`docs/security/`, `docs/ops/`, `docs/standards/`, `docs/codebase/`,
`docs/architecture/{system,data,integrations,runtime}.md`) are **absent** in this repo; the
consult is a no-op and no `TB-N` / `THR-N` / `CMP-N` / `SLO-N` is cited anywhere below.

## Decisions

1. **`phase_events` is append-only and mirrors `authority_events`.** Same id/run_id/kind/
   created_at spine, same `CHECK` discipline, same `(run_id, …)` index shape
   (`authority_events_run`, `rounds/schema.rs:167`). No new persistence idiom is invented.
2. **Phase attempts are rows, not a derived view.** A separate `phase_attempts` table holds
   attempt identity and ordinal; `phase_events` references it. Deriving attempts from events
   alone would make the "at most one nonterminal attempt per phase" rule (PHASE-1.6) a scan
   rather than a uniqueness constraint the database enforces.
3. **The transition seam owns the status write.** `phase::persist_phase_transition(db, plan,
   RunEffects)` is the only supported way to change `runs.status` or insert a `step_results`
   row from `porch-run`. `Db::set_run_status` and `Db::insert_step_result` become
   crate-internal to `porch-gate` and are called only by the seam and by recovery.
4. **`RunEffects` is reused, not re-declared.** It already carries exactly the co-write
   payload; PHASE moves it from `authority.rs` to a shared parent module so both event
   families use one type.
5. **Owned unknown resolved — `deliver_repair_attempts` stays as an unused column.**
   Dropping a column in SQLite means a table rebuild of `runs`, which is the riskiest
   possible migration on an installed state root for zero behavioral gain. The column stops
   being written, nothing reads it for enforcement (PHASE-4.4), and it is documented as dead.
   Cheapest safe shape of the three offered.
6. **Owned unknown resolved — the status trigger carries the protocol predicate only.** A
   `BEFORE UPDATE OF status ON runs` trigger cannot assert the co-write: it fires before the
   row changes and cannot see a `phase_events` insert that happens later in the same
   transaction, and reordering so the insert always precedes the update makes the assertion
   vacuous — any writer that inserts any event passes. SQL cannot express "these two writes
   are in the same transaction" from inside one of them. The co-write therefore stays in the
   Rust seam (decision 3), and the trigger does what FLOOR's two triggers do: compare
   `porch_writer_protocol()` against `min_writer_protocol`. This confirms the frame lock
   rather than extending it.
7. **Owned unknown resolved — four step-string sites, three in scope.** Enumerated above;
   `tui.rs:530 render_pipeline` is display over `steps[]` and is deliberately left alone.
8. **The migration reuses FLOOR's fail-forward shape verbatim**, extended to the new
   protocol, and orders trigger creation after its own status writes (see module H).

## Architecture

### A. Phase-event store

Satisfies: PHASE-1.1, PHASE-1.2, PHASE-1.3, PHASE-1.4, PHASE-1.11, PHASE-2.9
Reuse: rung 2 — extends `crates/porch-gate/src/rounds/`, mirroring `authority_events` in
`rounds/schema.rs:149-178` and its migrate path at `schema.rs:180`
Respects: ARCH-10 — a new module inside `porch-gate`, not a new crate
Interface: `phase_attempts` and `phase_events` tables plus row types
`PhaseAttemptRow`, `PhaseEventRow`; readers `attempts_for_run(run_id)`,
`events_for_run(run_id)`, `nonterminal_attempt(run_id, phase)`.
Depth: n/a — extends `rounds/`
Locality: new file `crates/porch-gate/src/rounds/phase.rs`; `rounds/schema.rs` **extend**
(add creates to the existing batch); `rounds/mod.rs` **extend** (module declaration).

`phase_attempts(id, run_id, phase, ordinal, parent_attempt_id, caused_by_attempt_id,
created_at)` with `CHECK (phase IN ('intent','rebase','review','certify','deliver'))` for
top-level rows and a nullable `operation_kind` for nested rows
(`compose`, `fixer`, `deliver_repair`), `UNIQUE (run_id, phase, ordinal)` where
`parent_attempt_id IS NULL`. `phase_events(id, attempt_id, seq, kind, outcome, cause,
created_at)` with `kind IN ('started','terminal','evidence')`, append-only by convention and
never updated (PHASE-1.3). `seq` is a per-run monotonic integer supplying the deterministic
ordering PHASE-2.5 needs. Index `phase_events_run` on `(run_id, seq)` mirroring
`authority_events_run`.

`caused_by_attempt_id` is a column on `phase_attempts` only; nothing joins it to
`finding_instances`, which is how PHASE-2.9 is met — by having no such edge to draw.

### B. Phase transition seam

Satisfies: PHASE-1.5, PHASE-1.6, PHASE-1.8, PHASE-1.9, PHASE-1.10, PHASE-2.1, PHASE-2.2,
PHASE-2.3, PHASE-2.4, PHASE-2.5, PHASE-2.6, PHASE-2.7, PHASE-2.8, PHASE-7.8
Reuse: rung 2 — extends `RunEffects` and the Immediate-transaction pattern of
`persist_authority_with_run_effects` (`rounds/authority.rs:204`, `authority.rs:130-135`)
Respects: ARCH-3 — fixer execution is modelled as a nested operation under the suspended
`review` attempt, never as a role that certifies its own prescription; ARCH-13 — the handoff
that revokes and rebinds an approval commits atomically with its events
Surface:
- `crates/porch-run/src/lib.rs:116 set_status` (22 call sites) — **replace**: becomes a thin
  wrapper that requires a `PhaseTransition`; every caller supplies one.
- `crates/porch-run/src/lib.rs:122 record_step` — **replace**: folded into the seam's
  `RunEffects.steps`.
- Production status writes that bypass `set_status` — **replace**, each onto the seam:
  `crates/porch-run/src/deliver.rs:284` (parked / awaiting compose),
  `deliver.rs:451` (cancelled / agent abort), `deliver.rs:516` and `deliver.rs:562`
  (completed), `crates/porch-gate/src/daemon.rs:285` (cancelled / superseded by new push),
  `crates/porch-run/src/lib.rs:2161` (parked).
- Production step-row writes that bypass `record_step` — **replace**, each folded into the
  owning transition's `RunEffects.steps`: `deliver.rs:283, 450, 506, 507, 552, 553`.
- `crates/porch-gate/src/db.rs:559 set_run_status` — **replace**: visibility narrowed to
  `pub(crate)`; only the seam and module C call it. This narrowing is what forces the twelve
  bypassing writes above to be found at compile time rather than by review.
- `crates/porch-gate/src/db.rs:845 insert_step_result` — **replace**: same narrowing.
- Test helpers calling both methods directly (`daemon.rs:412-413`, `tui.rs:1160`,
  `tui.rs:1302`, `service.rs:515`, `db.rs:1151`, `db.rs:1158`, `deliver.rs:990`,
  `deliver.rs:992`) — **replace**: they seed fixtures and move to the seam or to a
  `#[cfg(test)]` constructor; none is a production path.
- `crates/porch-gate/src/rounds/authority.rs:204` `RunEffects` users (DISPO's approve /
  skip / fix / abort paths) — **compat**: they keep calling
  `persist_authority_with_run_effects`, which gains an optional phase-transition argument;
  removed when authority and phase writes are unified, tracked as follow-up in
  `## Follow-ups`.
- `crates/porch-gate/src/rpc.rs:688` `steps[]` on the wire — **frozen**: an agent-facing
  contract this change does not alter; discharged by building around it — `step_results`
  keeps being written, now inside the seam's transaction.
- PR-body hidden attestation (`deliver.rs:311`, `deliver.rs:577`) — **frozen**: content is
  published to `origin` and is out of scope by requirement; discharged by leaving both
  builders reading `step_results` untouched.
Interface: `phase::persist_phase_transition(db, plan: PhaseTransition, effects: RunEffects)
-> Result<AttemptId, PhaseError>`, where `PhaseTransition` is one of `Start { phase }`,
`Terminal { attempt, outcome, cause }`, `Handoff { from, to_phase, outcome, cause }`,
`NestedStart { parent, kind }`, `NestedTerminal { attempt, outcome, cause }`, or
`Evidence { attempt, cause }`.
Depth: n/a — extends `rounds/authority.rs`'s pattern
Locality: `rounds/phase.rs` **extend**; `porch-run/src/lib.rs` **extend** (both funnels plus
the bypassing write at :2161); `porch-run/src/deliver.rs` **extend** (nested compose and
repair starts, plus its ten bypassing writes); `porch-gate/src/daemon.rs` **extend** (the
supersede path); neighbor `rounds/authority.rs` **extend** — one optional parameter, no
restructuring.

One `Immediate` transaction per call: validate, insert attempt row when starting, append the
event, apply `RunEffects` (status, error, approved-head, step rows), bump `audit_rev`, commit.
Any failure returns before commit, so PHASE-1.10 is the transaction's own semantics rather
than compensating logic. `Handoff` performs PHASE-2.4's four writes in order and assigns
consecutive `seq` values, satisfying PHASE-2.5. `NestedStart` rejects a parent that already
has a terminal event (PHASE-2.3) and refuses a canonical phase name as `operation_kind`
(PHASE-2.2). `Start` refuses when a nonterminal attempt for that phase exists (PHASE-1.6),
which the `UNIQUE` index also enforces underneath.

### C. Crash recovery

Satisfies: PHASE-1.7, PHASE-8.3
Reuse: rung 2 — extends the existing startup sweep at
`crates/porch-gate/src/db.rs:522-538`, which already marks stale runs
`ci_monitor_interrupted` or `failed`
Interface: `phase::reconcile_interrupted(db) -> Result<usize>`, called from the same startup
path as the existing sweep.
Depth: n/a — extends the startup sweep
Locality: `rounds/phase.rs` **extend**; `db.rs` startup sweep **extend** — the sweep gains a
phase-event append per run it terminalises, in the sweep's own transaction.

For each run the sweep touches, every nonterminal attempt gets a terminal event carrying
interrupted evidence, written in the same transaction as the status change the sweep already
makes — so the co-write invariant holds on the recovery path too.

### D. Lifecycle read path

Satisfies: PHASE-3.1, PHASE-3.2, PHASE-3.3, PHASE-3.4, PHASE-3.5, PHASE-7.9, PHASE-7.12,
PHASE-7.15
Reuse: rung 2 — extends `RunSnapshot` (`rpc.rs:398-405`), following the precedent of
`audit_available`, the compact server-filled field DISPO added
Surface:
- `crates/porch-run/src/lib.rs:2177 parked_phase` — **replace**: reads
  `nonterminal_attempt`; the `"review"` fallback is deleted (PHASE-3.3).
- `crates/porch-run/src/agent_run.rs:287 agent_status_from_snap` — **replace**: the
  `s.status == "parked" || error.contains("park") || step == "review" || step == "rebase"`
  heuristic is deleted in favour of `snap.phase`.
- `crates/porch/src/tui.rs:146 compose_parked` — **replace**: reads `snap.phase`.
- `crates/porch/src/tui.rs:530 render_pipeline` — **compat**: keeps `rfind`ing over
  `steps[]`; it displays per-phase last-known status rather than deciding position, and
  `steps[]` remains a supported projection. No follow-up: this is a permanent, correct use.
- `porch agent status` JSON shape — **frozen**: agent-facing contract; discharged by adding
  no field to it, only changing how its existing `phase` value is computed.
Interface: `RunSnapshot.phase: Option<PhaseView>` — `{ phase, ordinal, operation }`, or
`None` when unavailable.
Depth: n/a — extends `RunSnapshot`
Locality: `rpc.rs` **extend**; the three decision sites **extend** (each shrinks to a field
read); no extraction — there is no shared helper worth pulling out once each site is one line.

### E. Deliver-repair budget

Satisfies: PHASE-4.1, PHASE-4.2, PHASE-4.3, PHASE-4.4, PHASE-8.4
Reuse: rung 2 — extends module A's readers; the enforcement point stays where it is
(`crates/porch-run/src/lib.rs:1247-1252`)
Respects: ARCH-13 — the bound on repair attempts is a bound on forward attempts
Surface:
- `crates/porch-gate/src/db.rs:727 increment_deliver_repair_attempts` — **replace**: no
  longer called; removed.
- `runs.deliver_repair_attempts` column and `RunRow` field (`db.rs:54`, mapped `db.rs:1025`)
  — **frozen**: a persisted column on an installed state root. Discharged by building around
  it: the column is left in place, stops being written, and is documented dead (decision 5).
  Not read by enforcement, so PHASE-4.4 holds regardless of its residual values.
Interface: `phase::repair_attempts_started(db, run_id) -> Result<u32>`
Depth: n/a — extends module A
Locality: `rounds/phase.rs` **extend**; `porch-run/src/lib.rs` **extend** at the existing
budget check; neighbor `db.rs` **leave** apart from deleting the increment helper.

### F. Audit document phase slice

Satisfies: PHASE-5.1, PHASE-5.6, PHASE-5.7, PHASE-6.4, PHASE-7.5, PHASE-7.6, PHASE-8.1
Reuse: rung 2 — extends `crates/porch-gate/src/audit.rs`, replacing the body of the
`AuditPhase` placeholder at `audit.rs:28-42, 194`
Surface:
- `AuditDocument.phase` (`audit.rs:117`) — **replace**: `kind` changes from
  `"step_results_inferred"` to `"phase_events"` or `"unavailable"`, and the payload becomes
  an attempt tree. `AuditDocument.schema_version` (`audit.rs:102`) is bumped in the same
  change.
- `crates/porch/src/tui.rs:82 history_audit` — **compat**: the panel keeps rendering counts
  and watermark from the document and ignores the new tree; removed when a TUI tree view is
  specified, which requirements place out of scope.
- `porch agent audit` JSON consumers — **frozen**: an agent-facing contract. Discharged by
  the requirement mandating the change (PHASE-5.1) and by the `schema_version` bump being the
  declared signal; no external subscriber exists outside this repo, so the gate is the
  version field rather than a third party's agreement.
Interface: `AuditPhase { kind, attempts: Vec<AuditAttempt>, steps: Vec<AuditStep> }`;
`AuditAttempt { id, phase, ordinal, operation, parent_id, caused_by_id, started_at,
terminal, cause, children }`.
Depth: n/a — extends `audit.rs`
Locality: `audit.rs` **extend** — the existing single-snapshot build (`audit.rs:133`,
`TransactionBehavior::Deferred`) gains two queries; no new module.

`steps` is retained inside `AuditPhase` and is now rebuilt from `phase_events` rather than
copied from `step_results`, which is what makes the projection rebuildable in the sense the
glossary requires. A run with no `phase_events` rows yields `kind: "unavailable"` with an
empty tree (PHASE-6.4) — never a synthesised one. PHASE-8.1's latency target is met by the
`phase_events_run` index from module A; the plan assertion is a test at the seam.

### G. `porch audit` tree renderer

Satisfies: PHASE-5.2, PHASE-5.3, PHASE-5.4, PHASE-5.5, PHASE-7.13, PHASE-8.5
Reuse: rung 7 — new code. No renderer exists: `porch audit` and `porch agent audit` are the
same match arm (`crates/porch/src/main.rs:319-332`) emitting
`serde_json::to_string_pretty`.
Surface:
- `porch audit` stdout — **replace**: default output becomes the human tree; `--json`
  restores the previous bytes.
- `crates/porch-gate/porch-agent.md:78` and `docs/usage.md:187,214` — **replace**: all three
  describe `porch audit` as a JSON alias and are corrected in this change.
- `porch agent audit` stdout — **frozen**: machine contract; discharged by leaving that arm
  untouched (PHASE-7.13).
Interface: `fn render_phase_tree(doc: &AuditDocument) -> String` — one function, document in,
text out.
Depth: if this module vanished, callers would still need the audit document's attempt tree
and the rule that a child renders indented under its parent — that is, they keep an ordering
and an indentation rule, not the traversal, the outcome vocabulary, or the unavailability
copy. The interface is a pure function of a value they already hold.
Locality: new function in `crates/porch/src/main.rs`; the shared match arm at `main.rs:319`
**extract** — split into `Command::Audit` (renders) and `AgentCommand::Audit` (emits JSON),
which is the smallest change that lets the two diverge.

Rendering is pure text: outcome, nesting depth, and unavailability are carried by indentation
and words, never by colour, which is how PHASE-8.5 is met — there is no colour to remove.

### H. Protocol bump, writer fence, and migration

Satisfies: PHASE-6.1, PHASE-6.2, PHASE-6.3, PHASE-6.5, PHASE-6.6, PHASE-6.7, PHASE-7.1,
PHASE-7.2, PHASE-7.3, PHASE-7.4, PHASE-8.2
Reuse: rung 2 — extends `install_writer_fence` (`db.rs:1074-1116`) and
`reject_stale_writer` (`db.rs:1120-1134`); `PROTOCOL_SCHEMA_VERSION` in `rounds/mod.rs`
Surface:
- `porch_state_meta.min_writer_protocol` — **replace**: raised to the new protocol.
- Existing `porch_runs_writer_insert` / `porch_runs_writer_approve` triggers — **frozen**:
  in force and unchanged (PHASE-7.3); discharged by adding a third trigger beside them rather
  than editing either.
- Pre-PHASE binaries reading an upgraded state root — **frozen**: rollback is unsupported by
  documented policy (`docs/usage.md:311-313`); discharged by building around it — the
  documented escape is restoring a pre-upgrade `$PORCH_HOME` backup, and this change adds a
  protocol-3 subsection saying so in the protocol-2 shape.
Interface: unchanged — the fence installs itself on first open of a state root.
Depth: n/a — extends the fence installer
Locality: `db.rs` **extend**; `rounds/mod.rs` **extend** (version constant); `docs/usage.md`
and `porch-agent.md` **extend**.

**Ordering, which is the whole risk here.** The installer's own fail-forward
`UPDATE runs SET status = 'failed' …` (`db.rs:1102-1115`) runs in the same transaction that
creates the triggers. The new `BEFORE UPDATE OF status ON runs` trigger would fire on that
statement. The design orders the transaction: raise `min_writer_protocol`, run the
fail-forward status writes, **then** create the status trigger, then commit. The fail-forward
writes therefore complete before the trigger exists (PHASE-6.6), and on an already-fenced
root the trigger is created idempotently by `CREATE TRIGGER IF NOT EXISTS` after the same
sweep. The trigger's predicate is `porch_writer_protocol() < (SELECT min_writer_protocol FROM
porch_state_meta)` — identical to the two existing triggers (decision 6).

PHASE-6.3 is met by omission: the migration writes no `phase_events` rows for any existing
run, so nothing is synthesised.

### I. Retained projections (infrastructure — no Satisfies of its own beyond guards)

Satisfies: PHASE-7.7, PHASE-7.10, PHASE-7.11, PHASE-7.14
Reuse: rung 1 — does not need to exist; these are behaviours the design deliberately does not
touch, recorded so the omission is visible rather than accidental
Interface: unchanged
Depth: n/a — no new code
Locality: `rpc.rs`, `deliver.rs`, `tui.rs` **leave**.

`steps[]` stays compact on the wire; both attestation builders keep reading `step_results`;
the history panel keeps its lazy on-open fetch. The only change reaching any of them is that
`step_results` rows are now written inside the seam's transaction, which changes when they
are committed, not what they contain.

## Follow-ups

- Unify `persist_authority_with_run_effects` and `persist_phase_transition` onto one
  transaction core, removing the optional phase-transition parameter added to the former
  (module B, `compat` row).
- Render the phase tree in the TUI history panel, removing that panel's `compat` disposition
  (module F). Out of scope by requirement.

## Seams for testing

| Seam | Kind | Covers |
|---|---|---|
| `phase::persist_phase_transition` | unit | PHASE-1.5, 1.6, 1.8, 1.9, 1.10, 2.1–2.8, 7.8 |
| `phase_attempts` / `phase_events` schema + readers | unit | PHASE-1.1, 1.2, 1.3, 1.4, 1.11, 2.9 |
| `phase::reconcile_interrupted` + startup sweep | integration | PHASE-1.7, 8.3 |
| `RunSnapshot.phase` via `rpc::get_run` | integration | PHASE-3.1–3.5, 7.9, 7.12, 7.15 |
| `phase::repair_attempts_started` + budget check | integration | PHASE-4.1–4.4, 8.4 |
| `porch_gate::get_audit` | integration | PHASE-5.1, 5.6, 5.7, 6.4, 7.5, 7.6, 8.1 |
| `render_phase_tree` | unit | PHASE-5.2, 5.3, 5.4, 5.5, 8.5 |
| `porch audit` / `porch agent audit` CLI | e2e | PHASE-7.13 |
| `Db::open` fence + migration | integration | PHASE-6.1, 6.2, 6.3, 6.5, 6.6, 6.7, 7.1–7.4, 8.2 |
| existing `steps[]`, attestation, history-panel tests | integration | PHASE-7.7, 7.10, 7.11, 7.14 |

New seams: two (`persist_phase_transition`, `render_phase_tree`). Every other row is an
existing boundary.

## Coverage check

All 63 requirement IDs appear in exactly one `Satisfies:` line:

| Module | IDs | Count |
|---|---|---|
| A | 1.1, 1.2, 1.3, 1.4, 1.11, 2.9 | 6 |
| B | 1.5, 1.6, 1.8, 1.9, 1.10, 2.1–2.8, 7.8 | 14 |
| C | 1.7, 8.3 | 2 |
| D | 3.1–3.5, 7.9, 7.12, 7.15 | 8 |
| E | 4.1–4.4, 8.4 | 5 |
| F | 5.1, 5.6, 5.7, 6.4, 7.5, 7.6, 8.1 | 7 |
| G | 5.2, 5.3, 5.4, 5.5, 7.13, 8.5 | 6 |
| H | 6.1, 6.2, 6.3, 6.5, 6.6, 6.7, 7.1–7.4, 8.2 | 11 |
| I | 7.7, 7.10, 7.11, 7.14 | 4 |
| **Total** | | **63** |

Deliberately unmapped: none.
