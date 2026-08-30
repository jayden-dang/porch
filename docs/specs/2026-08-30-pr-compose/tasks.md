# Tasks: PR Compose

> **For agentic workers:** after plan approval, pick one execute skill —
> `build-in-waves`, `build-by-story`, or `build-inline`.
> The chosen skill writes `Execution-mode:`.

Feature code: PRCMP
Status: In-progress
Date: 2026-08-30
Execution-mode: continuous
Max-concurrency: auto
Requirements: ./requirements.md
Design: ./design.md

**Goal:** After lease-push, open a scaffold GitHub PR without self-review theater, park compose for the Agent to author title/body from a packet, then finish deliver.

**Architecture:** Extend `porch-deliver` scaffold/merge/title helpers; extend `run_deliver_phase` to scaffold→park `compose`→resume; branch `agent_respond` so compose skip continues deliver (never review-skip). Packet at `$PORCH_HOME/runs/<run_id>/compose-packet.json`.

**Tech Stack:** Rust 2024 workspace; `cargo test` / clippy; fake `gh` in `crates/porch/tests/`.

## Global Constraints

- `AGENTS.md` sha256:`b0f2a01edc9c16c9db95010b243435404f66863093e43b578816d2c5530dcc24` — English; no network in unit tests; PATH fakes; use-case slices.
- `docs/architecture/INDEX.md` sha256:`7d9d801f34d79f6493ef69973118ea0fbf324030d010a037eb2f59ce3642ee2b` — ARCH-1,3,4,5,7,8,11,13 as in design `Respects:`.
- `docs/agents/project.md` sha256:`a3f5ba93c4d8ec55de8d1fe30128ac8095d2c016233f6e49801f3664ed0a5400` — verify: `cargo fmt --all --check`, `cargo check --workspace --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- `CONTEXT.md` sha256:`cc9a4368aeec025ac9b9a8e9b4ad0780787ff545d7c59d2d5a3abf2f09f94ad4` — update Park to include `compose` in Task 7.
- Solo band — no fake multi-assignee fields.
- Integration tests: `crates/porch/tests/m17_pr_compose.rs` (milestone naming).
- Do not put requirement IDs in Rust source or test names.

## File Structure

| Path | Responsibility |
|---|---|
| `crates/porch-deliver/src/lib.rs` | Scaffold/default body, merge markers, title helpers, `edit_pr_title`, keep attestation+redact |
| `crates/porch-git/src/lib.rs` | Reuse `show_path_at` for template blobs (thin wrapper only if needed) |
| `crates/porch-run/src/deliver.rs` | Push → scaffold → write packet → park compose → resume/watch |
| `crates/porch-run/src/lib.rs` | Pipeline pause on compose park; `agent_respond` compose branch; status fields |
| `crates/porch-gate/src/db.rs` | Persist `pr_title_written` (or equivalent) if required by title heuristic |
| `crates/porch/src/main.rs` | `porch agent respond` compose flags (`--body-file`, `--title`) |
| `crates/porch/src/tui.rs` | Minimal compose-park display (skip/abort hint) |
| `crates/porch-gate/porch-agent.md` | Document compose park + packet |
| `CONTEXT.md` | Park includes `compose` |
| `crates/porch/tests/m14_agent_run.rs` | Replace theater body assertions |
| `crates/porch/tests/m17_pr_compose.rs` | Integration: scaffold, park, respond/skip/abort |

---

### Task 1: Scaffold body without theater

**Files:**
- Modify: `crates/porch-deliver/src/lib.rs`
- Test: unit tests in same crate (extend existing `build_pr_body` / redact tests)

**Reuse:** rung 2 — extend `build_pr_body`, `redact_home_paths`, attestation append in `crates/porch-deliver/src/lib.rs`

**Interfaces:**
- Consumes: `Attestation`, existing redact helper
- Produces: `build_scaffold_body(...)`, `merge_porch_managed(...)`, default skeleton with `porch-managed` markers; theater `build_pr_body` layout removed or redirected

**Depends-on:** none

- [ ] Write failing units: default scaffold has Summary/Why/How tested/Links; no visible Review/Certify/Pipeline; managed markers present; attestation HTML still appended; redact still applies.
- [ ] Run `cargo test -p porch-deliver` — expect fail on missing APIs / old sections.
- [ ] Implement scaffold + merge helpers per design; keep marker `porch-attestation` + `head_sha`.
- [ ] Run `cargo test -p porch-deliver` — expect pass.
- [ ] Commit: `feat(porch-deliver): scaffold PR body without gate theater`

_Requirements: PRCMP-1.2, PRCMP-1.3, PRCMP-1.4, PRCMP-1.5, PRCMP-5.3, PRCMP-6.1_

---

### Task 2: Trusted-SHA PR template load

**Files:**
- Modify: `crates/porch-deliver/src/lib.rs` (and/or thin call via `porch-git`)
- Modify: `crates/porch-git/src/lib.rs` only if wrapper needed around `show_path_at`
- Test: units with temp bare + committed template paths

**Reuse:** rung 2 — `show_path_at` in `crates/porch-git/src/lib.rs`

**Interfaces:**
- Consumes: `show_path_at` / git show at trusted SHA
- Produces: `load_pr_template(bare, trusted_sha) -> TemplateBytes` (design name; includes which path/source won) with pick order from design §8

**Depends-on:** Task 1

- [ ] Failing test: template at `.github/pull_request_template.md` on trusted SHA becomes managed interior; missing → porch default.
- [ ] Run focused deliver/git tests — expect fail.
- [ ] Implement pick order; never read feature tip alone.
- [ ] Pass + commit: `feat(porch-deliver): load PR template from trusted SHA`

_Requirements: PRCMP-2.1, PRCMP-2.2, PRCMP-2.3, PRCMP-2.4_

---

### Task 3: Deterministic title + porch-managed title rules

**Files:**
- Modify: `crates/porch-deliver/src/lib.rs` (`deterministic_pr_title`, `is_porch_managed_title`, **new** `edit_pr_title`)
- Modify: `crates/porch-gate/src/db.rs` if storing `pr_title_written`
- Test: units in `porch-deliver`

**Reuse:** rung 2 — replace `pr_title`; new `edit_pr_title` via `gh pr edit --title`

**Interfaces:**
- Consumes: intent / commit subject inputs from caller
- Produces: replace `pr_title` with `deterministic_pr_title` (keep `pr_title` only as a thin deprecated wrapper if call sites need one symbol); `is_porch_managed_title`; **new** `edit_pr_title`

**Depends-on:** Task 1

- [ ] Failing tests: title not solely `porch: {branch}` when intent present; managed detection per design §8; human title not classified managed.
- [ ] Implement + wire `edit_pr_title`; update deliver callers to the one chosen title symbol.
- [ ] Pass + commit: `feat(porch-deliver): deterministic and managed PR titles`

_Requirements: PRCMP-5.1, PRCMP-5.2_

---

### Task 4: Deliver scaffold then park compose

**Files:**
- Modify: `crates/porch-run/src/deliver.rs`
- Modify: `crates/porch-run/src/lib.rs` (pipeline handles deliver parked / compose step)
- Test: `crates/porch/tests/m17_pr_compose.rs` (create); update `m14_agent_run.rs` body expectations

**Reuse:** rung 2 — extend `run_deliver_phase`; `run_artifact_dir` for packet path

**Interfaces:**
- Consumes: Task 1–3 builders; `create_pr` / `edit_pr_body` / lease-push
- Produces: packet at `$PORCH_HOME/runs/<run_id>/compose-packet.json`; step `compose`+`parked` (this is the **only** parked row that drives `parked_phase` — never `deliver`+`parked`); `DeliverOutcome::ParkedCompose` mapped to pipeline `PhaseLoop::Parked` without recording deliver completed

**Depends-on:** Task 1, Task 2, Task 3

- [ ] Failing integration: after certify path with fake gh, PR body is scaffold (no theater), run `parked` with phase `compose`, packet file exists with required keys.
- [ ] Implement push→scaffold→packet→`compose` parked; if open PR exists and title still porch-managed, `edit_pr_title` as well as body merge; do not watch checks yet.
- [ ] On deliver repair / later redeliver of an already-composed tip: refresh porch-managed body (+ title if managed) and **do not** re-enter compose park.
- [ ] Update m14 assertions away from Intent/Review/Certify theater.
- [ ] Pass `cargo test -p porch --test m17_pr_compose` (and m14 as touched).
- [ ] Commit: `feat(porch-run): scaffold PR and park compose`

_Requirements: PRCMP-1.1, PRCMP-3.1, PRCMP-3.2, PRCMP-3.3, PRCMP-3.5, PRCMP-5.4, PRCMP-7.1, PRCMP-7.2_

---

### Task 5: Compose respond / skip / abort

**Files:**
- Modify: `crates/porch-run/src/lib.rs` (`agent_respond` compose branch **before** review Skip)
- Modify: `crates/porch-run/src/deliver.rs` (`resume_deliver_after_compose`)
- Modify: `crates/porch/src/main.rs` (`--body-file`, `--title`)
- Test: `crates/porch/tests/m17_pr_compose.rs`

**Reuse:** rung 2 — extend `agent_respond` / `AgentStatus`; branch on `parked_phase == "compose"`

**Interfaces:**
- Consumes: packet path, merge helpers, `edit_pr_body` / `edit_pr_title`
- Produces: status fields `pr_url`, `compose_packet_path`, `allowed_actions`; compose skip continues watch path
- CLI: keep `porch agent respond <verb>`; when `phase=compose`, accept `respond skip|abort` and `respond --body-file <path> [--title <str>]` (body respond implies apply prose; no separate compose verb). Reject `approve`/`fix` on compose with usage error.

**Depends-on:** Task 4

- [ ] Failing tests: respond merges body + refreshes attestation; skip leaves scaffold, completes deliver (does **not** hit review Skip arm); abort fails run and leaves PR open; invalid theater body rejected and stays parked; approve/fix on compose → usage error.
- [ ] Implement respond/skip/abort + CLI flags; branch `parked_phase == "compose"` **before** review Skip; refresh attestation on skip/respond per design.
- [ ] Pass + commit: `feat(porch): agent compose respond skip abort`

_Requirements: PRCMP-3.4, PRCMP-4.1, PRCMP-4.2, PRCMP-4.3, PRCMP-4.4, PRCMP-6.2, PRCMP-7.5, PRCMP-7.6_

---

### Task 6: Allowlist watch after compose resolves

**Files:**
- Modify: `crates/porch-run/src/deliver.rs` (watch only after compose resolve)
- Test: `crates/porch/tests/m17_pr_compose.rs` and/or extend `m6_deliver.rs` pattern

**Reuse:** rung 2 — existing `maybe_watch` / allowlist helpers

**Interfaces:**
- Consumes: resume path from Task 5
- Produces: watch runs only post-compose

**Depends-on:** Task 5

- [ ] Failing test: with `watch_checks` set, no check poll while `phase=compose`; after skip/respond, allowlisted watch runs.
- [ ] Implement ordering; keep rerun_transient ignored.
- [ ] Pass + commit: `feat(porch-run): babysit checks after compose`

_Requirements: PRCMP-7.3, PRCMP-7.3a, PRCMP-7.4_

---

### Task 7: Operator docs surface (skill, glossary, TUI hint)

**Files:**
- Modify: `CONTEXT.md` (Park includes compose)
- Modify: `crates/porch-gate/porch-agent.md`
- Modify: `crates/porch/src/tui.rs` (minimal compose park hint)
- Test: light unit/snapshot or doc-linked integration already covering status JSON

**Reuse:** rung 2 — existing TUI park patterns; skill markdown embed

**Interfaces:**
- Consumes: status `phase=compose` fields from Task 5
- Produces: documented Agent loop; glossary term Park updated

**Depends-on:** Task 5

- [ ] Update CONTEXT Park; skill documents packet path + respond/skip/abort; TUI shows compose park actions.
- [ ] `cargo test -p porch --test m17_pr_compose` still green; `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Commit: `docs(porch): compose park operator surface`

_Requirements: PRCMP-3.4, PRCMP-7.5_ (docs/ops surface for already-tested behavior)

---

## Coverage check

All PRCMP-* IDs appear in a task footer above. Seam table from design covered by Tasks 1–6 units/integrations.
