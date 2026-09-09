# Tasks — fault injection across the forward boundary

**Feature code:** FAULT
**Roadmap item:** ROAD-9 (MILE-3)
**Requirements:** `requirements.md` · **Design:** `design.md`

Waves are ordered by dependency. Each wave ends green on
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace`.

## Wave 1 — the instrument

- [x] **1.1** Add `GIT_BIN_ENV` and `git_bin()` to `porch-git`, and route the six
  `Command::new("git")` sites through it. *(FAULT-4.1, FAULT-4.2)*
- [x] **1.2** Report the resolved git binary in `porch doctor`. *(FAULT-4.6)*
- [x] **1.3** Unit-test that the override is honoured and that the default is `git`.
  *(FAULT-4.1)*

## Wave 2 — the ROAD-8 correction

- [x] **2.1** Add the no-`terminal`-phase-event clause to `attempts_awaiting_verdict`.
  *(FAULT-5.1)*
- [x] **2.2** Stop filtering resolved verdicts through the `status = 'running'` list;
  append a conclusion for every selected attempt, in the run's reconciliation transaction
  where one exists and in its own transaction otherwise, touching nothing else.
  *(FAULT-5.2, FAULT-5.4)*
- [x] **2.3** Store test: a conclusion is **persisted** for an interrupted attempt whose
  run status is not `running`. Replaces the existing selector-only assertion, which
  passes today for the wrong reason. *(FAULT-5.6)*
- [x] **2.4** Store test: a terminalized `deliver` attempt receives no conclusion.
  *(FAULT-5.3)*
- [x] **2.5** Store test: reconciliation leaves nothing awaiting a conclusion.
  *(FAULT-5.5)*
- [x] **2.6** Record the correction in `RECON`'s requirements so the ledger shows what
  `RECON-3.6` did and did not deliver. *(requirements `§5`)*

## Wave 3 — the harness

- [x] **3.1** New `crates/porch/tests/m23_forward_fault.rs`: fixture with a real bare
  `origin`, PATH fakes for review and `gh`, its own default branch, identity, and
  signing. *(FAULT-6.2, FAULT-6.6)*
- [x] **3.2** The git shim, with pass-through default and the three firing modes,
  bounded sleeps, and fire-once state. *(FAULT-4.4, FAULT-4.5, FAULT-6.5)*
- [x] **3.3** Restart helper: wait for the old pid to be gone, restart without the shim,
  assert the pid changed, then rely on the startup barrier rather than polling.
  *(FAULT-6.3, FAULT-6.4)*
- [x] **3.4** Ground-truth readers: `origin`'s ref for a branch, the gate's tracking ref,
  the run's forward records, and its conclusions. *(FAULT-2.1, FAULT-2.2)*

## Wave 4 — the scenarios

- [x] **4.1** `forward_landed_then_killed_reconciles_to_reached_origin`, including the
  exactly-one-conclusion assertion. *(FAULT-1.1, FAULT-1.2, FAULT-2.3, FAULT-2.4)*
- [x] **4.2** `killed_before_the_push_reconciles_to_indeterminate`. *(FAULT-2.5)*
- [x] **4.3** `killed_before_intent_leaves_no_conclusion`. *(FAULT-2.6)*
- [x] **4.4** `refused_push_records_failure_and_leaves_origin_unchanged`. *(FAULT-2.7)*
- [x] **4.5** `retry_after_reached_origin_adopts_the_pull_request`.
  *(FAULT-3.1, FAULT-3.2, FAULT-3.3)*
- [x] **4.6** `two_restarts_leave_one_conclusion`. *(FAULT-3.4)*

## Wave 5 — the blocker tripwire

- [x] **5.1** Pin that after a correction commit the forwarded SHA is not the reviewed
  SHA, documented as a tripwire that changes when the blocker is decided.
  *(FAULT-8.1, FAULT-8.2)*

## Wave 6 — record and docs

- [x] **6.1** Register `FAULT` in `docs/specs/catalog/delivery.md`.
- [x] **6.2** Correct the stale claim at `docs/usage.md` that nothing reads the forward
  record at restart — it contradicts the section directly beneath it.
- [x] **6.3** Close ROAD-9 in `docs/roadmap/INDEX.md`; leave MILE-3 committed and name
  GOAL-1's unmet third clause as the reason.
- [x] **6.4** Glossary entry for the fault-injection instrument if `CONTEXT.md` needs
  one.

## Verification

- [x] `cargo fmt --all --check`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `cargo test --workspace`

## Notes for the reviewer

**ROAD-8 never reached `main`.** PR #8 was stacked on PR #7's branch and merged into it
*after* that branch had already merged to `main`, so its three commits landed on a branch
nobody merges from. They are cherry-picked onto this branch unchanged as the first three
commits; `crates/porch-gate/src/rounds/reconcile.rs` and the RECON specs are ROAD-8's, not
ROAD-9's.

**Two production changes sit outside this item's declared surface** (`crates/porch/tests/`),
both recorded in `requirements.md` rather than absorbed silently: the `PORCH_GIT_BIN`
override (`§4`), and the verdict-write correction (`§5`), which is a defect in ROAD-8 that
framing found and that no kill-point suite would have found, because it is a defect in
what reconciliation writes rather than in what it concludes.

**MILE-3 does not close here.** GOAL-1 names four checkable properties; this suite
discharges three. The fourth — *no approval outliving its evidence* — is the open
head-continuity blocker, and `FAULT-8.3` refuses to write the test that would freeze the
undecided answer.
