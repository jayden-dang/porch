# Tasks: Durable forward authorization

Feature code: FWDAUTH
Status: Draft
Date: 2026-09-08
Execution-mode: continuous
Requirements: ./requirements.md
Design: ./design.md

**Goal:** Bind a forward to exactly one reviewed commit, and make a crash mid-forward
leave durable local evidence instead of a run that looks like it never tried.

**Architecture:** One append-only `forward_records` table in `porch-gate`'s `rounds/`
module, keyed to the owning `deliver` phase attempt, written through a small
`rounds::forward` module in the shape of the existing `authority_events` and
`phase_events` appenders. In `porch-run`, the lease splits into observe-and-decide then
execute so the intent row commits after the tip observation and before the mutation, and
the outcome row commits from porch's own command result ahead of the post-push
verification. `assert_head_continuity` becomes exact equality.

**Tech Stack:** Rust 1.85+, edition 2024; `rusqlite` over SQLite under `$PORCH_HOME`;
`ulid` for row ids; `clap` for the CLI.

## Global Constraints

Source: `docs/agents/project.md` (sha256 `a3f5ba93c4d8`), `AGENTS.md`
**Non-negotiables** (sha256 `b0f2a01edc9c`), `docs/architecture/INDEX.md`
(sha256 `7d9d801f34d7`), `CONTEXT.md` (sha256 `3b402b6f7d07`). No `docs/standards/` or
`docs/codebase/` tree exists in this repo; do not invent a parallel SSOT.

- **Verify, in order, all must pass before any completion claim:**
  `cargo fmt --all --check` · `cargo check --workspace --all-targets` ·
  `cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace`.
  `--workspace` is not optional — `default-members = ["crates/porch"]` means a bare
  `cargo test` skips `porch-gate`, `porch-git`, and `porch-quality`.
- Single file: `cargo test -p <crate> --test <file-stem>`.
- `unsafe_code = "forbid"`, `clippy::all = deny`, `clippy::pedantic = warn` — fix a
  pedantic warning or `allow` it with a reason; never ignore it.
- **Do not** add `/// REQ:` or `@FWDAUTH-N.M` annotations to Rust source or test names.
  Requirement IDs live in this triad, never in application or test source.
- Integration tests are named for the milestone that introduced them: this feature's is
  `crates/porch/tests/m23_forward_auth.rs`.
- English only. No network in unit tests — `gh` and the remote are PATH fakes and a
  local bare repository. No vendored third-party source.
- **ARCH-1 … ARCH-13** are binding and must not be silently reopened. This feature
  relies on **ARCH-1** (`origin` never rewritten), **ARCH-5** (force-with-lease against
  the observed SHA; unverifiable safety facts fail closed), **ARCH-11** (the forward
  record is porch-owned evidence, never an approval), **ARCH-13** (durable authorization
  and reviewed-input binding precede any external forward).
- **Do not touch the PR-body hidden attestation.** `assemble_scaffold` and
  `attestation_post_compose` in `crates/porch-run/src/deliver.rs` are PRCMP-owned and
  must produce byte-identical content for an identical run (FWDAUTH-7.6).
- **Do not edit `crates/porch-gate/src/rounds/applicability.rs`** — FLOOR's
  authorization reads stay as they are (FWDAUTH-7.5).
- **Do not change restart classification.** `reconcile_one_running` keeps its `pr_url`
  branch; changing it is ROAD-8 (FWDAUTH-7.7).
- Team band: **Solo** (roster: Tech Lead — Jayden Đặng). No multi-assignee ceremony.

## File structure

| File | Responsibility |
|---|---|
| `crates/porch-gate/src/rounds/forward.rs` | **new** — kinds, row type, appenders, readers, the reached-origin predicate |
| `crates/porch-gate/src/rounds/schema.rs` | add the `forward_records` DDL, index, and partial unique index to the create batch |
| `crates/porch-gate/src/rounds/mod.rs` | declare `forward`; re-export its public types; raise `PROTOCOL_SCHEMA_VERSION` to 4 |
| `crates/porch-run/src/deliver.rs` | resolve authorization at the boundary; split the lease; write intent then outcome |
| `crates/porch-run/src/lib.rs` | `assert_head_continuity` becomes exact equality and names both SHAs |
| `crates/porch-gate/tests/m18_rounds.rs` | store-level coverage for the constraints, the index, and the predicate |
| `crates/porch/tests/m23_forward_auth.rs` | **new** — integration coverage across the forward boundary |
| `docs/usage.md` | the protocol-4 upgrade note and its rollback consequence |

---

## Task 1: The forward record exists and refuses dishonest rows

**Files:**
Create `crates/porch-gate/src/rounds/forward.rs`. Modify
`crates/porch-gate/src/rounds/schema.rs`, `crates/porch-gate/src/rounds/mod.rs`.
Test `crates/porch-gate/tests/m18_rounds.rs`.
**Reuse:** rung 2 — extends `crates/porch-gate/src/rounds/`, mirroring `phase_events`
and its `(run_id, seq)` index, and the conditional `CHECK` style of `round_coverage`.
**Interfaces:** Produces `ForwardKind`, `ObservedRemote`, `ForwardIntent`,
`ForwardOutcome`, `ForwardRecordRow`, `forward::append_intent`,
`forward::append_outcome`, `forward::records_for_run`,
`forward::attempt_reached_origin`. Consumes `Db`, `AttemptId`.
**Depends-on:** none
**Steps:**
- [ ] Test: opening an existing database applies the new table and leaves prior rows readable.
- [ ] Test: an `intent` row carrying a `landed_sha`, a `pushed` row without one, and a
      `push_failed` row without a detail are each rejected by the store.
- [ ] Test: a second `intent` for one `deliver` attempt is rejected; a second under a new
      attempt is accepted.
- [ ] Test: `append_outcome` refuses an outcome whose attempt has no committed intent.
- [ ] Test: `records_for_run` returns rows in `seq` order; `attempt_reached_origin` is
      false after intent alone and true after `pushed` or `already_current`.
- [ ] Implement the DDL, the two indexes, the row types, the appenders in one Immediate
      transaction each with `MAX(seq) + 1` allocation, and the readers.
- [ ] Raise `PROTOCOL_SCHEMA_VERSION` to 4; leave the three `runs` writer triggers alone.
- [ ] Run `cargo test -p porch-gate --test m18_rounds`; expect pass.
- [ ] Commit.

_Requirements: FWDAUTH-2.2, FWDAUTH-2.5, FWDAUTH-3.3, FWDAUTH-3.6, FWDAUTH-4.1, FWDAUTH-4.2, FWDAUTH-4.3, FWDAUTH-5.1, FWDAUTH-5.2_

## Task 2: Authorization binds exactly one commit

**Files:**
Modify `crates/porch-run/src/lib.rs`, `crates/porch-run/src/deliver.rs`.
Test `crates/porch-run/src/lib.rs` (in-crate `continuity_tests`).
**Reuse:** rung 1 — reuses the existing `review_approved_head_sha` column and
`porch_git::rev_parse_c`; no new persistence.
**Interfaces:** Produces `authorized_forward_sha`. Consumes `Db`, `porch_git`.
**Depends-on:** none
**Steps:**
- [ ] Test: continuity fails closed when no approved SHA is recorded (existing test stays green).
- [ ] Test: continuity fails closed when the live HEAD is a descendant of the approved
      SHA, and the error names both SHAs.
- [ ] Test: continuity passes when the live HEAD equals the approved SHA.
- [ ] Remove the `is_ancestor` branch from `assert_head_continuity` and report both SHAs
      on mismatch.
- [ ] Add `authorized_forward_sha` at the forward boundary so it does not depend on the
      caller having checked.
- [ ] Run `cargo test -p porch-run`; expect pass.
- [ ] Commit.

_Requirements: FWDAUTH-1.1, FWDAUTH-1.2, FWDAUTH-1.3, FWDAUTH-1.5, FWDAUTH-1.6, FWDAUTH-7.1, FWDAUTH-7.4_

## Task 3: The lease splits so intent precedes the mutation

**Files:**
Modify `crates/porch-run/src/deliver.rs`.
Test `crates/porch/tests/m23_forward_auth.rs` (created here).
**Reuse:** rung 1 — reuses `porch_git::{ls_remote_sha, remote_commits_incorporated,
resolve_push_decision, push_exact_sha, RemoteTip, PushDecision}` unchanged; the split is
local to `porch-run`, so `ARCH-2` and the lease semantics are untouched.
**Interfaces:** Produces `observe_forward_lease`, `execute_forward_push`,
`ForwardLease`. Consumes `forward::append_intent`, `forward::append_outcome`.
**Depends-on:** Task 1, Task 2
**Steps:**
- [ ] Test: a green forward writes `intent` then `pushed`, in that `seq` order, both
      under the same `deliver` attempt, with the observed tip recorded on the intent.
- [ ] Test: a forward refused for unincorporated remote commits writes no record at all
      and still fails closed with the existing message.
- [ ] Test: a forward whose remote ref is already at the authorized SHA writes
      `already_current`, not `pushed`.
- [ ] Test: the run's PR body is byte-identical to the pre-change output for an
      identical run.
- [ ] Split `lease_push_exact`; keep the refusal and the unverifiable-incorporate failure
      inside the observe half; keep the post-push equality check in the execute half.
- [ ] Write intent between observe and execute; write the outcome from the command result
      before the verification read; write `push_failed` on the error path and propagate.
- [ ] Run `cargo test -p porch --test m23_forward_auth --test m6_deliver --test m6_repair`; expect pass.
- [ ] Commit.

_Requirements: FWDAUTH-1.4, FWDAUTH-2.1, FWDAUTH-2.3, FWDAUTH-2.4, FWDAUTH-2.6, FWDAUTH-3.1, FWDAUTH-3.2, FWDAUTH-3.4, FWDAUTH-3.5, FWDAUTH-3.7, FWDAUTH-6.1, FWDAUTH-7.2, FWDAUTH-7.3, FWDAUTH-7.6_

## Task 4: The upgrade is documented and the suite is green

**Files:**
Modify `docs/usage.md`. Test: the whole workspace.
**Reuse:** rung 1 — extends the existing protocol-upgrade subsection written for
protocol 3.
**Interfaces:** none
**Depends-on:** Task 1, Task 2, Task 3
**Steps:**
- [ ] Add the protocol-4 note: an older binary refuses this state root, active runs are
      failed forward on upgrade, and the rollback consequence.
- [ ] Confirm `reconcile_one_running` is unchanged and `applicability.rs` is untouched.
- [ ] Run `cargo fmt --all --check` · `cargo check --workspace --all-targets` ·
      `cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace`;
      expect all pass.
- [ ] Mark the triad `Implemented` and the catalog card `Implemented`.
- [ ] Commit.

_Requirements: FWDAUTH-5.3, FWDAUTH-7.5, FWDAUTH-7.7_
