# Requirements: Operator voice

Feature code: VOICE
Status: In-progress
Date: 2026-10-21

<!--
Rules:
- Feature code: 2-12 chars, A-Z0-9, starts with a letter, unique repo-wide.
  Register it in docs/specs/INDEX.md before use.
- Every acceptance criterion gets a hierarchical ID: <CODE>-<story>.<criterion>.
- Criteria use EARS phrasing:
    WHEN <event/condition> THE SYSTEM SHALL <behavior>          (event-driven)
    WHILE <state> THE SYSTEM SHALL <behavior>                   (state-driven)
    IF <unwanted condition> THEN THE SYSTEM SHALL <behavior>    (unwanted behavior)
    WHERE <feature is included> THE SYSTEM SHALL <behavior>     (optional feature)
    THE SYSTEM SHALL <behavior>                                 (ubiquitous)
- Guard requirements protect existing behavior this feature touches:
    WHEN <condition> THE SYSTEM SHALL CONTINUE TO <existing behavior>
- IDs are immutable once Status is Approved. Retire a requirement by striking it
  through (~~**CODE-N.M**~~ reason) — never renumber.
-->

## 1. Setup shows rules and reader

**Story:** As an operator, I want `porch setup` to show the always-on rules block and the reader choice before I press Enter, so that I can name the assurance shape without reading source.

- **VOICE-1.1** WHEN `porch setup` draws the engine list THE SYSTEM SHALL show one `rules` block that is not a selectable reviewer and one `reader` block whose choices are the detected agent CLI, no reader (`rules only`), and `ocr` marked `legacy`.
- **VOICE-1.2** WHEN the `rules` block is drawn THE SYSTEM SHALL print the deterministic-floor sibling path from the floor resolver, and SHALL NOT print a `PATH` lookup of `porch-quality`.
- **VOICE-1.3** WHEN a reader choice is drawn for `claude` or `codex` THE SYSTEM SHALL print that executable's path.
- **VOICE-1.4** WHEN the detected `ocr` path canonicalizes to a file whose name is not `ocr` THE SYSTEM SHALL omit that choice.
- **VOICE-1.5** WHEN the operator confirms a choice THE SYSTEM SHALL label the pending shape `rules only (floor-only)` or `rules + reader (floor+judgment)` in the setup UI.

## 2. The same words on every human surface

**Story:** As an operator, I want doctor, status, runs, init, help, attach, and human error text to use `rules` and `reader`, so that I do not learn a second vocabulary after setup.

- **VOICE-2.1** WHEN `porch doctor` prints the floor and the selected review wiring THE SYSTEM SHALL use the words `rules` and `reader` and SHALL include the parenthetical `floor-only` or `floor+judgment` on that same report.
- **VOICE-2.2** WHEN `porch status` or `porch runs` mentions the assurance shape or the review wiring THE SYSTEM SHALL use `rules only (floor-only)` or `rules + reader (floor+judgment)`.
- **VOICE-2.3** WHEN `porch init`, clap help, the attach TUI, or a human error string names the review wiring THE SYSTEM SHALL use `rules` and `reader` and SHALL NOT use `engine` or `quality` as the label for that wiring.
- **VOICE-2.4** THE SYSTEM SHALL keep `porch agent` JSON object keys and the assurance-shape values `floor-only` and `floor+judgment` unchanged.

## 3. Old config still selects the same shape

**Story:** As an operator with a 0.2.3 `$PORCH_HOME/config.yaml`, I want porch to honor `review.engine` until I run setup again, so that an upgrade does not drop my review choice.

- **VOICE-3.1** WHEN `review.assurance` is absent and `review.engine` is `quality` THE SYSTEM SHALL select `rules only (floor-only)`.
- **VOICE-3.2** WHEN `review.assurance` is absent and `review.engine` is `agent`, `generic`, or `ocr` THE SYSTEM SHALL select `rules + reader (floor+judgment)` for that same engine.
- **VOICE-3.3** WHEN both `review.assurance` and `review.engine` are present THE SYSTEM SHALL select from `review.assurance` and ignore `review.engine` for the selection.
- **VOICE-3.4** WHEN `porch setup` writes config THE SYSTEM SHALL set `review.assurance` to `rules-only` or `rules+reader` and SHALL still write `review.engine` as `quality`, `agent`, `generic`, or `ocr` matching that selection.
- **VOICE-3.5** WHEN `porch doctor` reports the selection THE SYSTEM SHALL name which key won: `assurance` or `engine`.

## 4. Behavior this change must not break

**Story:** As an operator, I want status, doctor, and an old config file to keep their current jobs, so that the new words do not change the gate.

Files this change touches: `crates/porch/src/setup.rs`, `crates/porch/src/doctor.rs`, `crates/porch/src/tui.rs`, `crates/porch/src/main.rs`, `crates/porch-gate/src/look.rs`, `crates/porch-review/src/home_config.rs`, `crates/porch-review/src/setup.rs`, `docs/usage.md`.

- **VOICE-4.1** (guard) WHEN `porch doctor` reports the floor THE SYSTEM SHALL CONTINUE TO print the sibling path and the observed artifact identity from the floor resolver, not a `PATH` hit.
- **VOICE-4.2** (guard) WHEN `porch doctor` or `porch status` runs THE SYSTEM SHALL CONTINUE TO report daemon condition without starting a daemon and without writing the state root for the status read.
- **VOICE-4.3** (guard) WHEN `$PORCH_HOME/config.yaml` is missing THE SYSTEM SHALL CONTINUE TO treat operator config as absent.
- **VOICE-4.4** (guard) WHEN `review.engine` is `quality` THE SYSTEM SHALL CONTINUE TO run assurance as floor-only, with the deterministic floor still required.
- **VOICE-4.5** (guard) WHEN `porch agent` prints a result THE SYSTEM SHALL CONTINUE TO write JSON on stdout for that result.

## 5. Quality attributes

**Section-kind:** nfr

**Story:** As a stakeholder, I want measurable quality targets for this feature, so that how-well is not left implicit.

- **Performance:** None — copy and one extra YAML field. No latency target.
- **Security:** None — no new trust boundary. The floor remains the sibling of `porch`, not a `PATH` choice.
- **Reliability:** None — survival of a 0.2.3 config file is VOICE-3.1 through VOICE-3.4, not a separate availability target.
- **Accessibility:** None — no new interactive control. Setup stays on the existing ratatui keys.

## Out of Scope

- A second TUI crate, or full-screen `porch doctor` / `porch status`.
- Renaming `porch agent` JSON keys or the stored values `floor-only` and `floor+judgment`.
- Removing the floor-only shape.
- Vietnamese CLI strings.
- Deleting `review.engine` from config written by setup.
- Changing admit, the five run phases, forward authorization, or which SHA may leave the machine.

## Open Questions

None.
