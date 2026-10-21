# Tasks: Operator voice

> **For agentic workers:** after plan approval, pick one execute skill —
> `build-in-waves` (continuous dependency-aware scheduler), `build-by-story` (human-gated story review
> units), or `build-inline` (controller implements, no implementer subagents).
> The chosen skill writes `Execution-mode:`. Steps use checkbox (`- [ ]`) syntax
> for tracking.

Feature code: VOICE
Status: Approved
Date: 2026-10-21
Execution-mode: continuous
Max-concurrency: auto
Requirements: ./requirements.md
Design: ./design.md

**Goal:** Human porch commands say `rules` and `reader`, while a 0.2.3 `review.engine` still selects the same assurance shape.

**Architecture:** `ReviewConfig::selected` prefers `review.assurance` over `review.engine` and setup writes both. Setup, doctor, status, runs, help, init, attach, and review errors call `voice_phrase`. Agent JSON, the floor sibling line, and daemon-free status stay as they are.

**Tech Stack:** Rust, ratatui, serde_yaml, cargo test

## Global Constraints

`docs/agents/project.md` sha256 `86dd5f1d0ac390763ee83c35c1b7d3093723c8d4c34fae5a016930c6a4acf3ed`. Verify with `cargo fmt --all --check`, `cargo check --workspace --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`. English only. `ARCH-12`: the floor path is the `porch-quality` sibling, never PATH. Team band Solo. No requirement IDs in Rust source or test names.

Retrieval: blast on these paths hits LOOK (`look.rs`, `doctor.rs`), ONEBIN (`doctor.rs`, `setup.rs`), and REVIEW (`home_config.rs`). VOICE has no `tasks.md` OWNS until this file lands, so cluster weight is empty. That is not "no neighbors".

## File Structure

- `crates/porch-review/src/home_config.rs` — `review.assurance`, `selected()`, `voice_phrase`
- `crates/porch-review/src/setup.rs` — `setup_yes` writes both keys; human errors say `rules` / `reader`
- `crates/porch-run/src/lib.rs` — `engine_is_quality` uses `selected()`
- `crates/porch-review/src/plan.rs` — shape read uses `selected()`
- `crates/porch-review/src/agent_review.rs` — shape read uses `selected()`
- `crates/porch/src/setup.rs` — two-block ratatui list
- `crates/porch/src/doctor.rs` — rules/reader lines and which key won
- `crates/porch/src/main.rs` — help and init hints
- `crates/porch/src/tui.rs` — attach shape phrase
- `crates/porch-gate/src/look.rs` — status/runs phrase when an assurance record exists
- `docs/usage.md` — the same words
- `crates/porch/tests/m9_setup.rs` — setup text
- `crates/porch/tests/m8_doctor.rs` — doctor text and sibling line
- `crates/porch/tests/m27_look.rs` — status phrase and daemon-free read
- `crates/porch/tests/m14_agent_run.rs` — agent stdout stays JSON

---

### Task 1: Config selects the shape

**Files:**
- Modify: `crates/porch-review/src/home_config.rs`
- Modify: `crates/porch-review/src/setup.rs`
- Modify: `crates/porch-run/src/lib.rs` (`engine_is_quality` and its caller)
- Modify: `crates/porch-review/src/plan.rs`
- Modify: `crates/porch-review/src/agent_review.rs`
- Test: `crates/porch-review/src/home_config.rs`

**Reuse:** rung 2 — extend `ReviewConfig` in `crates/porch-review/src/home_config.rs` and `setup_yes` in `crates/porch-review/src/setup.rs`

**Interfaces:**
- Consumes: `EngineKind` (`quality`, `agent`, `generic`, `ocr`)
- Produces: `ReviewConfig.assurance: Option<String>`, `ReviewConfig::selected(&self) -> Result<VoiceSelection, Error>`, `voice_phrase(floor_only: bool) -> &'static str`, `VoiceSelection { floor_only: bool, source: VoiceKey, reader: Option<EngineKind> }`, `VoiceKey::{Assurance, Engine}` (not `porch_review::SelectionSource`)

**Depends-on:** none

- [ ] **Step 1: Write the failing test**

Add `#[cfg(test)] mod tests` in `home_config.rs`. Build `ReviewConfig` values and call `selected`.

- `engine: Some("quality")`, assurance absent → `floor_only == true`, `source == Engine`
- `engine: Some("agent"|"generic"|"ocr")`, assurance absent → `floor_only == false`, `reader` is that kind, `source == Engine`
- both set, assurance `rules-only`, engine `agent` → `floor_only == true`, `source == Assurance`
- both set, assurance `rules+reader`, engine `agent` → `floor_only == false`, `reader == Agent`, `source == Assurance`
- assurance `rules+reader`, engine `quality` or absent → `Err`
- `voice_phrase(true)` is `rules only (floor-only)`; `voice_phrase(false)` is `rules + reader (floor+judgment)`
- missing config file stays `load_home_config` → `Ok(None)`

Run: `cargo test -p porch-review home_config` — expect: compile failure, `selected` missing.

- [ ] **Step 2: Implement**

Parse `review.assurance` as `rules-only` or `rules+reader` only. `selected` uses assurance when present. `rules-only` forces `floor_only` even if engine is `agent`. `rules+reader` with engine missing or `quality` returns `Error::Msg` containing `reader`. `setup_yes` sets `assurance` to `rules-only` or `rules+reader` and still sets `engine` to `kind.as_str()`. `engine_is_quality` in `crates/porch-run/src/lib.rs` calls `selected()` and treats `Err` as a failed selection, not as floor-only. `plan.rs` and `agent_review.rs` do the same. `engine: quality` with no assurance stays floor-only and the floor resolver still runs.

Run: `cargo test -p porch-review home_config` — expect: pass.

- [ ] **Step 3: Commit**

`git commit -m "feat: select assurance shape from review.assurance"`

_Requirements: VOICE-3.1, VOICE-3.2, VOICE-3.3, VOICE-3.4, VOICE-4.3, VOICE-4.4_

---

### Task 2: Setup shows two blocks

**Files:**
- Modify: `crates/porch/src/setup.rs`
- Modify: `crates/porch/src/main.rs`
- Test: `crates/porch/src/setup.rs` (existing `render_to_string` bin tests)
- Test: `crates/porch/tests/m9_setup.rs` (`--engine quality` still parses)

**Reuse:** rung 2 — extend `crates/porch/src/setup.rs`, `doctor.rs`, `tui.rs`, `main.rs`, `crates/porch-gate/src/look.rs`, and the human error strings in `crates/porch-review/src/setup.rs`

**Interfaces:**
- Consumes: `voice_phrase`, `VoiceSelection`, floor sibling path from `floor::state`
- Produces: setup screen text for the two blocks

**Depends-on:** Task 1

- [ ] **Step 1: Write the failing test**

In the existing `render_to_string` tests inside `crates/porch/src/setup.rs`, capture the setup list text.

- The text contains `rules` and the sibling path from the floor resolver.
- The text does not contain `porch-quality` resolved via `which`.
- A claude detection prints that executable path next to `reader`.
- An ocr path whose canonical file name is not `ocr` does not appear.
- The pending line for Quality is `rules only (floor-only)`. The pending line for Agent is `rules + reader (floor+judgment)`.
- `--help` for setup does not use `quality` as the label. `--engine quality` still parses.

Run: `cargo test -p porch --bin porch setup` — expect: failure, old `quality — floor-only` text. Then `cargo test -p porch --test m9_setup` — expect: `--engine quality` still accepted.

- [ ] **Step 2: Implement**

Keep ratatui. Draw a fixed `rules` block and a `reader` block whose rows are detected agent CLIs, "no reader", and legacy `ocr` only when the canonical file name is `ocr`. Do not change `default_engine` order. Confirm still calls `setup_yes` with the same `EngineKind`.

Run: `cargo test -p porch --bin porch setup` and `cargo test -p porch --test m9_setup` — expect: pass.

- [ ] **Step 3: Commit**

`git commit -m "feat: show rules and reader in porch setup"`

_Requirements: VOICE-1.1, VOICE-1.2, VOICE-1.3, VOICE-1.4, VOICE-1.5_

---

### Task 3: Doctor, status, and the other human text

**Files:**
- Modify: `crates/porch/src/doctor.rs`
- Modify: `crates/porch/src/tui.rs`
- Modify: `crates/porch/src/main.rs`
- Modify: `crates/porch-gate/src/look.rs`
- Modify: `crates/porch-review/src/setup.rs`
- Modify: `docs/usage.md`
- Test: `crates/porch/tests/m8_doctor.rs`
- Test: `crates/porch/tests/m27_look.rs`

**Reuse:** rung 2 — extend `crates/porch/src/setup.rs`, `doctor.rs`, `tui.rs`, `main.rs`, `crates/porch-gate/src/look.rs`, and the human error strings in `crates/porch-review/src/setup.rs`

**Interfaces:**
- Consumes: `voice_phrase`, `VoiceSelection.source`
- Produces: none

**Depends-on:** Task 1

- [ ] **Step 1: Write the failing test**

In `m8_doctor.rs`, assert the review line contains `rules only (floor-only)` or `rules + reader (floor+judgment)`, names `assurance` or `engine` as the winning key, and still prints the sibling path and artifact identity. Assert doctor does not spawn a daemon.

`compact_run_row` has no assurance field. In `m27_look.rs`, load the assurance record the gate already stores for that run (the record attach already reads) and assert status text contains `voice_phrase` for its shape. Do not add a table. The test uses the read-only db open and does not call daemon start.

Assert the init hint in `main.rs`, the attach shape line in `tui.rs`, and one setup error in `porch-review/src/setup.rs` by extending the tests that already cover those strings. Each contains `rules` or `reader` and does not use `engine` or `quality` as the label.

Run: `cargo test -p porch --test m8_doctor` and `cargo test -p porch --test m27_look` — expect: missing phrases.

- [ ] **Step 2: Implement**

Doctor review line uses `voice_phrase` and the winning key. Floor line stays `floor::state`. Status and runs append the phrase only when the run already has an assurance record. Attach replaces the visible shape words. Phase name `review` stays. Usage.md uses the same phrases.

Run: `cargo test -p porch --test m8_doctor` and `cargo test -p porch --test m27_look` — expect: pass.

- [ ] **Step 3: Commit**

`git commit -m "feat: use rules and reader on human porch commands"`

_Requirements: VOICE-2.1, VOICE-2.2, VOICE-2.3, VOICE-3.5, VOICE-4.1, VOICE-4.2_

---

### Task 4: Agent JSON stays JSON

**Files:**
- Test: `crates/porch/tests/m14_agent_run.rs`

**Reuse:** rung 1 — these requirements are the existing behavior. No new module.

**Interfaces:**
- Consumes: `emit_agent` stdout JSON (`AgentStatus` at `result.json`, not `AgentCliResult`)
- Produces: none

**Depends-on:** none

- [ ] **Step 1: Write the failing test**

Extend the existing agent-run test that already reads stdout. Assert stdout is one JSON object. Assert its keys match the serde names of the `AgentStatus` value `emit_agent` prints under `result.json`. When a shape string is present, it is `floor-only` or `floor+judgment`, not `rules only`.

Run: `cargo test -p porch --test m14_agent_run` — expect: pass if the contract already holds. If a key was renamed by an earlier task, expect failure.

- [ ] **Step 2: Implement**

Do not edit `emit_agent`. If the test fails because an earlier task changed a key, revert that key.

Run: `cargo test -p porch --test m14_agent_run` — expect: pass.

- [ ] **Step 3: Commit**

`git commit -m "test: keep porch agent results as JSON"`

_Requirements: VOICE-2.4, VOICE-4.5_

---
