# Review loop (what porch must get right)

This is the heart of the product.

**Sequencing (D9, 2026-08-29):** M3–M9 composed an external review CLI (OCR wrapper, transitional). **M10** made the workflow reviewer a session-free **coding agent**. **M16** (done) adds the porch-owned **`porch-quality`** engine (coverage, relocate, rule packs, grouping, precision bias). Setup prefers `quality` when `porch-quality` is on PATH; otherwise `agent`. OCR remains optional/legacy (`porch setup --engine ocr`).

## When review runs

**Always before push/PR.** Porch phase 3 of 5. The authoring session is biased. Validation is a fresh process in a disposable worktree.

## What review inspects

**In scope**

- Bugs and **wrong results that do not error** (trace at least one concrete input).
- Durable bug-fix claims: is the authorized failure still reachable? Recommend the earliest shared boundary, not another symptom patch.
- Security, performance regressions, breaking changes, weak error handling.
- Authoritative intent (`--intent` / inherited rerun): REQUIRED missing or FORBIDDEN present → **must** `ask-user`, even if risk-clean. Conformance is necessary, **not sufficient** — a change can satisfy intent and still compute a wrong value.
- Test quality: flag new source-content-only assertions; same-pattern tests **in the change’s scope** must be removed or made semantic (not a repo-wide cleanup).
- Simplification = reduce complexity without removing features or changing product behavior.
- Risk: `low|medium|high` + one-sentence rationale.

**Out of scope (explicit)**

- Style, format, lint, compile, type-check (later phases / CI).
- Running the test suite during review.
- Generic “add more tests.”
- Inferring systemic flaws from duplication/shape alone.
- Blocking authorized short-term containment because a later durable fix exists.
- Expanding user scope; turning optional improvements into blockers.
- “This run has no remote branch / PR / CI yet” — stripped after parse (`pipeline-owned-delivery`). External/pre-existing PRs stay in scope.

**Blocking:** any finding `error` or `warning`. Info-only does not park.

**`auto_fix.review` default 0:** even `auto-fix` findings park until a human or `--yes` says so.

## Reviewer vs fixer (do not collapse this)

Incident behind the rule: one fix round authored wrong code **and** the test that blessed it; a **resumed** reviewer session then verified its own prescription.

Rules:

| Role | Session | Shell | Duty |
|---|---|---|---|
| Reviewer | Always cold | **No full-suite tests, no intended edits.** M10 agent reviewer: prefer a no-write invocation; if the CLI cannot drop the shell, file writes fail the review. M16 engine: no shell, no edit. | Judge the tree. Emit findings. |
| Fixer | May resume | Yes, in the worktree | Fix selected findings narrowly; one focused verification of the touched area; **no** full repo test/lint suite |
| Rereview | Always cold | Same as reviewer | Pipeline-authored commits are **unreviewed new code**. Prior findings and fix summaries are **claims**. Tests added in the same fix round are claims. If prior-round machinery **exceeds** the original finding, one `ask-user` to revert to the minimal fix — do not stack ten rounds. |

If a fixer commits and rereview does not complete, persist an **uncertified range** on the branch and feed it to the next run’s initial review. Clear only after a **completed** review whose approved head equals or descends from the range tip. Lint/docs fixer commits should **not** create this range.

Timeout of a review round should **fail the run**, not park: parking a half-applied fix would let deliver commit leftovers.

## What the review engine must provide

- **Coverage:** every reviewable file is in a group and gets a pass or an explicit skip (oversize, unsupported ext). Generic agents skip files; porch must not.
- **Line anchors:** comments land on hunks, with relocate fallback — required if we ever post GitHub review comments; useful even for TUI/`porch agent`.
- **Language rules:** depth no single prompt will match. M16 owns this in a porch engine. Extra porch `path_instructions` are **repo policy** (enclave, contract), not a replacement for language packs.
- **Precision bias:** fewer false alarms. Porch’s gate makes false alarms expensive (they park humans). This matches.
- **Filter:** drop only comments the diff proves wrong. Keep disputed concurrency/safety comments.

Mapping review comments → porch findings (sketch):

| Review comment | Porch finding |
|---|---|
| category bug/security/performance, severity critical/high | `error` or `warning`, `auto-fix` if remedy is local, else `ask-user` |
| maintainability with local refactor | `info` or `warning`, often `no-op` or `auto-fix` |
| style / documentation | usually drop or `no-op` — certify/linters own style; document phase not year-1 |
| suggestion that needs schema/on-chain/new subsystem | `ask-user` regardless of category |

Intent conformance is **porch’s extra pass** (prompt clause and/or a cheap deterministic check). The review CLI may take optional background; it is not authoritative REQUIRED/FORBIDDEN.

Do **not** auto-apply comment `suggestion_code` as patches year 1. Suggestions are evidence for the fixer, not a `git apply` without a rereview.

## Human loop

Park → TUI or `porch agent respond`:

- `approve` — record decline of unfixed findings (advisory history for later rounds).
- `fix --findings …` — fixer, then rereview, then `fix_review` park (unless `--yes` / yolo: one fix round then approve remaining).
- `skip` — step skipped; do not claim a review-approved SHA unless skip was explicit and attested. Skip review without skip deliver fails closed.
- `abort` — cancel run.

`--yes` is standing consent to drive gates unattended. Never the default.

## After review

Record `review_approved_head_sha` only on **completed** review. Every later phase asserts HEAD continuity. Deliver refuses without a binding.
