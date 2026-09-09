# Tasks — restart reconciliation of ambiguous external effects

**Feature code:** RECON
**Roadmap item:** ROAD-8 (MILE-3)
**Requirements:** `requirements.md` · **Design:** `design.md`

**Tech Stack:** Rust 1.85+, edition 2024; `rusqlite` over SQLite under `$PORCH_HOME`;
`ulid` for row ids; `porch-git` for the local tracking-ref read.

| File | Responsibility |
|---|---|
| `crates/porch-gate/src/rounds/reconcile.rs` | **new** — `Verdict`, `Evidence`, pure `classify`, verdict appender, selection query |
| `crates/porch-gate/src/rounds/schema.rs` | add the `forward_reconciliations` DDL and its indexes to the create batch |
| `crates/porch-gate/src/rounds/mod.rs` | declare `reconcile`; re-export its public types; **no** protocol bump |
| `crates/porch-gate/src/rounds/phase.rs` | two-phase reconciliation: gather and discriminate outside the transaction, append the verdict inside it; conclusion and remedy in `runs.error` |
| `crates/porch-gate/tests/m18_rounds.rs` | the S0-S4 table, the discriminator's one-sidedness, idempotence, selection, message wording |

---

## Task 1: The verdict store and the pure classifier

- [ ] Add the `forward_reconciliations` DDL and both indexes to the create batch in
      `schema.rs`. No `CHECK` on `verdict` (`RECON-3.5`). Leave `forward_records`
      untouched (`RECON-8.4`) and add no trigger (`RECON-6.2`).
- [ ] Confirm `PROTOCOL_SCHEMA_VERSION` is unchanged at 4 (`RECON-6.1`).
- [ ] Create `rounds/reconcile.rs` with `Verdict` and `Evidence`, each with
      `as_str`/`parse`, and a pure `classify(&[ForwardRecordRow], Option<&str>)`
      returning `(Verdict, Evidence)` implementing the S0-S4 table. The discriminator
      argument may only upgrade `Indeterminate` to `ReachedOrigin` and never downgrade
      anything (`RECON-2.2`, `RECON-2.3`).
- [ ] Add `append_verdict_tx`, writing inside a caller-supplied transaction so the
      verdict and the terminalization commit together (`RECON-3.2`), and never updating
      or deleting a row (`RECON-3.3`).
- [ ] Add `attempts_awaiting_verdict`, selecting `deliver` attempts that have at least
      one forward record and no verdict, independent of `runs.status` (`RECON-3.6`,
      `RECON-3.7`).
- [ ] Declare and re-export from `rounds/mod.rs`.
- [ ] Store-level tests: one per S0-S4; the discriminator upgrading S1 and S4 on a
      match and leaving them `indeterminate` on absence and on mismatch; a verdict for
      an attempt whose run status is already `failed`.
- [ ] `cargo test -p porch-gate --test m18_rounds`, then commit.

_Requirements: RECON-1.3, RECON-1.4, RECON-1.5, RECON-1.6, RECON-3.1, RECON-3.3, RECON-3.5, RECON-3.6, RECON-3.7, RECON-6.1, RECON-6.2, RECON-6.3, RECON-8.4_

## Task 2: The local discriminator

- [ ] Add the gate-repository tracking-ref read: resolve
      `refs/remotes/origin/<branch>` in the run's gate bare, returning `Option<String>`
      and mapping every failure to `None` (`RECON-2.6`).
- [ ] Call it only for attempts whose evidence is S1 or S4, and only outside any open
      transaction (`RECON-2.1`, `RECON-2.4`).
- [ ] Assert by construction that no network call is added to the startup path
      (`RECON-2.5`): the read is `rev-parse` against a local `--git-dir`.
- [ ] Tests: a fixture gate bare whose tracking ref matches the authorized SHA upgrades
      the verdict; absent, mismatched, and missing-repository cases leave it
      `indeterminate` and do not error.
- [ ] `cargo test -p porch-gate`, then commit.

_Requirements: RECON-1.2, RECON-2.1, RECON-2.2, RECON-2.3, RECON-2.4, RECON-2.5, RECON-2.6, RECON-7.1_

## Task 3: Reconciliation reads the record and states the remedy

- [ ] Restructure `reconcile_interrupted_with_error` into the two-phase shape: gather
      each stale run's `deliver` attempts and their forward records, resolve the
      discriminator, then call `reconcile_one_running` with the resolved verdicts.
- [ ] Append the verdicts inside the transaction `reconcile_one_running` already opens
      (`RECON-3.2`), keeping the terminalization of every open attempt unchanged
      (`RECON-8.3`).
- [ ] Leave the `failed` / `ci_monitor_interrupted` status rule exactly as it is
      (`RECON-5.4`, `RECON-8.1`, `RECON-8.2`).
- [ ] Compose `runs.error` as the existing interruption phrase, then the conclusion,
      then the remedy. A reached-origin conclusion states the branch carries the
      authorized SHA and that pull request state is **unrecorded**; it must not assert
      absence (`RECON-1.7`, `RECON-5.1`, `RECON-5.2`, `RECON-5.3`).
- [ ] Do not touch `authorized_forward_sha`, `assert_head_continuity`, or
      `certify.rs` (`RECON-8.5`).
- [ ] Tests: the message wording for each verdict, including a negative assertion that
      it contains no claim of pull request absence; idempotence across two passes.
- [ ] `cargo test -p porch-gate --test m18_rounds` and `cargo test -p porch --test
      m21_phase`, then commit.

_Requirements: RECON-1.1, RECON-1.7, RECON-1.8, RECON-3.2, RECON-4.1, RECON-4.2, RECON-4.3, RECON-5.1, RECON-5.2, RECON-5.3, RECON-5.4, RECON-7.2, RECON-8.1, RECON-8.2, RECON-8.3, RECON-8.5_

## Task 4: Documentation and the full gate

- [ ] `CONTEXT.md`: add **Forward reconciliation** and **Forward verdict**, with their
      `_Avoid_` lines, in the style of the existing entries.
- [ ] `docs/usage.md`: state what an operator now sees for an interrupted forward, that
      no protocol bump is involved, and that the remedy is to re-push.
- [ ] `docs/specs/catalog/delivery.md`: move `RECON` from `Draft` to `Implemented`.
- [ ] `docs/roadmap/INDEX.md`: record ROAD-8 as closed under MILE-3; leave the
      correction-commit blocker open.
- [ ] Full gate, with no global `init.defaultBranch` set so hermeticity is what is
      tested: `cargo fmt --all --check`, `cargo check --workspace --all-targets`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`.
- [ ] Commit.

_Requirements: RECON-7.3_
