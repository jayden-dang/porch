# 7. The judgment layer is never dropped implicitly

Installing `porch` puts a `porch-quality` executable on `PATH`, and `default_engine` prefers
`Quality` over `Agent` unconditionally, so `porch setup --yes` selects floor-only on a machine
that has a coding agent — reproduced with one `PATH` entry as the only difference between
`"engine": "agent"` and `"engine": "quality"`. Porch's own installation artifact is thereby read
as evidence of an operator's choice, and the judgment layer is dropped with nothing in the output
saying so. `CONTEXT.md` settles the principle: the judgment layer is optional, but never
**implicit**.

**`default_engine` prefers a judgment engine: Agent, then Quality, then Generic, then Ocr.**
Quality stays ahead of Generic deliberately — `Generic` detects any executable named `review` on
`PATH`, which setup's own copy calls legacy, and ranking an unknown `PATH` binary above the
first-party engine would invert ARCH-9. The `detected.len() == 1` early return is kept, so a bare
machine with only porch still gets floor-only and zero-config headless setup keeps working.

**Selecting floor-only without being asked always warns.** The warning attaches to the outcome —
no engine named and `Quality` chosen — not to the ordering branch, because the early return fires
first and the bare machine is the case where the assurance shape is actually reduced.

Refusing to guess at all was rejected: `porch setup --yes` is the documented headless path and
the remedy `doctor` prints, so a `--yes` that then demands a flag is self-contradictory and would
break the agent-driven install.

The recorded trap does not bind this fix. `detect_engines` does feed both the default and the
explicit `--engine` lookup, but the flip lives in the *ordering* inside `default_engine`, which
only the default path reaches; the explicit path takes its backend from the detection list
directly, with the floor-sibling fallback behind it. So the second MILE-5 blocker is answered
without touching detection, which is what `ONEBIN` fixed the explicit path alone to preserve.

`tripwire_engine_selection_depends_on_whether_porchs_bindir_is_on_path` is retargeted rather than
deleted: after this change a bare machine still behaves differently depending on `PATH`, so the
tripwire's premise stands and only its expected value moves. The TUI's preselected row moves with
it, since the wizard shares `default_engine` — a benefit, not a cost, because the wizard shows
both engines and the operator can still choose either.
