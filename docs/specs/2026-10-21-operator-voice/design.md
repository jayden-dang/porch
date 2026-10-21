# Design: Operator voice

Feature code: VOICE
Status: Approved
Date: 2026-10-21
Requirements: ./requirements.md

## Context

Human setup lists `quality` beside `agent` and prints `which(porch-quality)` after canonicalize, so a mise shim shows up as the mise binary. Doctor already prints the floor sibling and its artifact identity from `floor::state` (`doctor.rs`). The run selects floor-only only when `review.engine` is `quality` (`porch-run` `engine_is_quality`). Status and runs do not mention that wiring today.

The binding constraint is **ARCH-12**: the floor is the `porch-quality` sibling, never a PATH choice. That rules out drawing `rules` as a detected PATH engine. The other rejected approach is a second TUI crate. Setup and attach already use ratatui. The change is words, the sibling path, and one config field.

Fresh retrieval (catalog neighbors, advisory): **FLOOR** and **ONEBIN** own the resolver and the doctor floor line. **LOOK** owns status and runs. **OPERATOR** owns setup, doctor, and attach. **REVIEW** owns engine selection. VOICE has no `tasks.md` yet, so it has no OWNS paths. Reuse stays on those modules. No new dependency.

## Decisions

1. Human text says `rules` and `reader`. Shape text is `rules only (floor-only)` or `rules + reader (floor+judgment)`.
2. `rules` prints the floor sibling path. A reader prints the claude or codex path. An `ocr` choice is omitted when its canonical file name is not `ocr`.
3. `review.assurance` is `rules-only` or `rules+reader`. When it is present it decides the shape. `review.engine` stays the wire for which reader, and setup still writes it. ADR 0011.
4. This feature does not change `default_engine`. The code still prefers Quality before Agent. The recommended row stays whatever that function returns. The Quality row is only renamed to "no reader". ADR 0007 is not implemented here.
5. `porch agent` JSON keys and the stored shape values stay. Doctor's floor line stays the resolver result.

## Architecture

### Human voice

Satisfies: VOICE-1.1, VOICE-1.2, VOICE-1.3, VOICE-1.4, VOICE-1.5, VOICE-2.1, VOICE-2.2, VOICE-2.3, VOICE-3.5
Reuse: rung 2 — extend `crates/porch/src/setup.rs`, `doctor.rs`, `tui.rs`, `main.rs`, `crates/porch-gate/src/look.rs`, and the human error strings in `crates/porch-review/src/setup.rs`
Respects: ARCH-12
Surface:
- `porch doctor` stdout lines — replace the review wording. The floor sibling line is frozen (VOICE-4.1).
- `porch status` / `porch runs` stdout — replace. Add the voice phrase only on a run that already has an assurance record. Runs without one stay as they are.
- clap help and `porch init` hints — replace `engine` / `quality` labels.
- attach TUI shape line — replace the visible phrase. Phase name `review` stays. That word is the phase, not the wiring.
- `docs/usage.md` — replace the human labels.
Interface: one function `voice_phrase(shape) -> &'static str` returning the two phrases above, plus `rules_path` taken from the existing floor state. Callers do not format the parentheses themselves.
Depth: n/a — extends the existing command and TUI modules
Locality: extend `crates/porch/src/setup.rs` into two blocks (`rules` fixed, `reader` choices include "no reader"). Extend doctor, look, help, init, attach, and the operator-facing errors in `porch-review/src/setup.rs` to call `voice_phrase`. Leave `emit_agent`.

Setup's reader list uses the engines already detected, except Quality is the "no reader" row and is not labeled `quality`. Confirm still calls `setup_yes` with the same `EngineKind`. Headless `porch setup --yes` and `--engine quality` keep the wire value `quality`. Only the help text loses the word as a label.

### Config selection

Satisfies: VOICE-3.1, VOICE-3.2, VOICE-3.3, VOICE-3.4, VOICE-4.4
Reuse: rung 2 — extend `ReviewConfig` in `crates/porch-review/src/home_config.rs` and `setup_yes` in `crates/porch-review/src/setup.rs`
Respects: ARCH-12
Surface:
- persisted `review.engine` — frozen. Values stay `quality`, `agent`, `generic`, `ocr`. Setup keeps writing them (VOICE-3.4). External readers of that field need no edit.
- `engine_kind()` callers (`porch-run` selection, `plan.rs`, `agent_review.rs`, setup verify) — replace the read so a present `review.assurance` decides floor-only versus floor-plus-judgment before `engine` is consulted.
- new `review.assurance` — no old reader. Setup writes it.
Interface: `ReviewConfig::selected()` returns the shape (`floor-only` or `floor+judgment`) and, when the shape has a reader, the existing `EngineKind` that is not Quality. `rules-only` forces floor-only even if `engine` is `agent`. `rules+reader` forces floor-plus-judgment. If that value is set and `engine` is missing or `quality`, selection fails closed with a human `reader` error instead of inventing a producer.
Depth: n/a — extends `ReviewConfig`
Locality: extend `home_config.rs` so `engine_kind()` and every current caller (`porch-run` selection, `plan.rs`, `agent_review.rs`, doctor, and setup verify) see the assurance winner. Leave floor resolution in `floor.rs`.

### Untouched contracts

Satisfies: VOICE-2.4, VOICE-4.1, VOICE-4.2, VOICE-4.3, VOICE-4.5
Reuse: rung 1 — these requirements are the existing behavior. No new module.
Surface:
- `porch agent` JSON — frozen. `emit_agent` is not edited.
- floor sibling path and artifact identity on doctor — frozen.
- daemon condition and `look` open-read — frozen. No `ensure_daemon` on doctor or status.
- missing `config.yaml` — frozen as `Ok(None)`.
Interface: none added.
Depth: n/a — extends nothing
Locality: leave `emit_agent`, `floor::state`, `daemon_condition`, and the missing-file branch of `load_home_config`.

## Seams for testing

| Seam | Kind | Covers |
|---|---|---|
| `ReviewConfig::selected` and `setup_yes` write | unit | VOICE-3.1, VOICE-3.2, VOICE-3.3, VOICE-3.4, VOICE-4.3, VOICE-4.4 |
| `voice_phrase` plus doctor and setup rendered lines | unit | VOICE-1.1, VOICE-1.2, VOICE-1.3, VOICE-1.4, VOICE-1.5, VOICE-2.1, VOICE-2.3, VOICE-3.5, VOICE-4.1 |
| `porch status` / `porch runs` text when a run has an assurance record | integration | VOICE-2.2, VOICE-4.2 |
| `porch agent` stdout | unit | VOICE-2.4, VOICE-4.5 |

## Coverage check

Every ID is in exactly one Satisfies line. VOICE-1.1 through VOICE-1.5, VOICE-2.1, VOICE-2.2, VOICE-2.3, and VOICE-3.5 are Human voice. VOICE-3.1 through VOICE-3.4 and VOICE-4.4 are Config selection. VOICE-2.4 and VOICE-4.1 through VOICE-4.3 and VOICE-4.5 are Untouched contracts. None left unmapped.
