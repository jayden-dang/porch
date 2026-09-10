# Requirements: Reviewed normalization and binding by equality

Feature code: EQUAL
Status: Implemented
Date: 2026-09-10

Roadmap item: ROAD-23 (MILE-3 — Crash-safe forwarding). Completes GOAL-1's fourth
checkable property — *no approval outliving its evidence* — which ROAD-7/8/9 left
open because certify's own correction commit advanced HEAD after review approved.

Respects: ARCH-4, ARCH-6, ARCH-13.

Owner ruling: [ADR-0003](../../adr/0003-reviewed-normalization-and-equality-binding.md).
This wave implements that ruling. It does not reopen it. An advisor pass during
framing recommended never mutating at all (check-only, no rebase-phase commit);
that option is recorded and rejected in design.md because it deletes the mutating
step ADR-0003 moved rather than deleted. Coding details the ADR left incomplete —
lint's twin writer, rebase early-returns, which SHA the forward returns, gpgsign on
the new commit — are locked here from measurement, not taste.

Vocabulary is locked in `CONTEXT.md`. These criteria use **Certify**, **Rebase**,
**Review**, **Forward authorization**, **Review round**, **Run**. They do not
redefine them.

## 1. Nothing porch runs after approval may move HEAD

**Story:** As an operator, I want the SHA review approved to be the SHA that can
reach `origin`, so that an approval cannot outlive its evidence.

- **EQUAL-1.1** AFTER a **Review round** has written `review_approved_head_sha` THE
  SYSTEM SHALL NOT create a commit in the disposable **Worktree** from
  `commands.format` or `commands.lint`.
- **EQUAL-1.2** IF `commands.format` or `commands.lint` leaves the worktree dirty
  during **Certify** THEN THE SYSTEM SHALL fail the certify phase, naming the
  command and the dirty paths, and SHALL NOT commit.
- **EQUAL-1.3** THE SYSTEM SHALL delete `maybe_correction_commit` as a certify
  writer. A leftover `porch: apply lint` commit after a format-only move SHALL NOT
  exist; both adapters are verify-only.
- **EQUAL-1.4** `commands.format` SHALL still *run* during **Certify** (so a
  nested fixer that unformats is caught without re-entering rebase). A dirty
  tree after that run is EQUAL-1.2, not a loop.

## 2. Mutating format runs before review, inside the reviewed range

**Story:** As an operator whose repo still has a rewriting `commands.format`, I
want that rewrite to land in `base..head` before **Review** starts, so the first
adoption's reformat is reviewed rather than forwarded unseen.

- **EQUAL-2.1** WHEN the canonical **Rebase** phase completes its git rebase,
  `reset --hard`, or already-on-onto path THE SYSTEM SHALL run `commands.format`
  from the already-resolved trusted config (the object `run_rebase` currently
  discards as `cfg.commands`), pinned at `trusted_config_sha`. Respects: ARCH-4.
- **EQUAL-2.2** THE SYSTEM SHALL run that format on the empty (`HEAD == onto`)
  and ancestor-reset paths as well as after a successful `git rebase`. Emptiness
  of `onto..HEAD` is a *post-format* fact. A format that commits turns a previously
  empty range into a non-empty one and **Review** SHALL run. A format that is a
  no-op leaves FWDAUTH-7.1 in force: skip remaining phases, do not require
  `review_approved_head_sha`.
- **EQUAL-2.3** IF format dirties the tree during rebase THE SYSTEM SHALL commit
  with porch identity (`Porch <porch@example.com>`), `core.hooksPath=/dev/null`,
  `--no-verify`, `-c commit.gpgsign=false`, `git add -A`, subject
  `porch: apply format`, then record `runs.head_sha` as the new tip.
- **EQUAL-2.4** WHEN that commit is made THE SYSTEM SHALL append
  `PhaseTransition::Evidence` with cause `format_applied` on the **already open**
  rebase attempt. THE SYSTEM SHALL NOT call `evidence_open` (that opens a new
  attempt). THE SYSTEM SHALL NOT add a nested `operation_kind` and SHALL NOT
  change DDL. `CREATE TABLE IF NOT EXISTS` would make a widened CHECK a no-op on
  existing state roots.
- **EQUAL-2.5** IF format exits non-zero during rebase THE SYSTEM SHALL fail the
  **Run** (rebase attempt terminal `failed`), not park it. Park remains the
  rebase-*conflict* outcome.
- **EQUAL-2.6** The parked-rebase fixer retry SHALL run the same format helper
  after a successful `git rebase` and before the emptiness check, loading commands
  from the already-pinned `trusted_config_sha`. THE SYSTEM SHALL NOT refresh the
  pin.
- **EQUAL-2.7** THE SYSTEM SHALL NOT run format inside `attempt_merge_conflict_rebase`.
  That path is nested under **Deliver** after approval; a HEAD-changing repair
  already revokes and re-reviews (FWDAUTH-7.4). Formatting there recreates the
  defect.
- **EQUAL-2.8** Unparseable trusted yaml at rebase SHALL continue to mean empty
  commands (today's split). Certify still fail-closes on those bytes. This wave
  SHALL NOT make rebase fail closed on parse just because it now reads
  `commands.format`.

Pre-review format is not `auto_fix.review`. ARCH-6's letter is unchanged. The
first-adoption repo-wide reformat is the cost ADR-0003 accepted; it is inside
the reviewed range and inside `round_coverage`, which is the alternative to
forwarding it unreviewed.

## 3. The forward binds by equality and carries the authorized SHA

**Story:** As an operator, I want the gate to forward exactly the SHA continuity
authorized, so a descendant of the approved SHA is refused rather than silently
pushed.

- **EQUAL-3.1** WHEN the gate evaluates HEAD continuity THE SYSTEM SHALL require
  the live worktree HEAD to equal the recorded approved SHA. (FWDAUTH-1.1)
- **EQUAL-3.2** THE SYSTEM SHALL NOT accept a live HEAD that is merely a
  descendant of the approved SHA. (FWDAUTH-1.2)
- **EQUAL-3.3** IF the live HEAD differs from the approved SHA THEN THE SYSTEM
  SHALL fail closed with an error naming both SHAs. (FWDAUTH-1.3)
- **EQUAL-3.4** WHEN a forward is performed THE SYSTEM SHALL return the approved
  SHA after proving equality, and SHALL NOT return a re-read of worktree HEAD
  as the value to push. A `rev-parse` remains as a *guard*. Returning the
  re-read after equality is the weakened 1.4 ROAD-7 already recorded as not
  meeting the original wording. (FWDAUTH-1.4)
- **EQUAL-3.5** HEAD movement after approval remains reachable only through the
  existing phase handoff that revokes the prior approval and re-reviews
  (fixer, deliver-repair). (FWDAUTH-1.6)
- **EQUAL-3.6** FWDAUTH-7.1 is unchanged: an empty range after format still skips
  remaining phases and SHALL NOT require `review_approved_head_sha`.

## 4. The tripwire becomes two guards

**Story:** As a reader of ROAD-9's suite, I want the test that pinned the bug to
assert the close, so the suite cannot stay green by keeping the bug.

- **EQUAL-4.1** `format_rewrite_is_inside_the_reviewed_range_and_forward_binds_by_equality`
  SHALL replace the ROAD-9 tripwire. Origin's tip SHALL equal
  `review_approved_head_sha`. When format rewrote, origin's log MAY contain
  `porch: apply format` as an ancestor of that SHA, never as a descendant of it.
- **EQUAL-4.2** The unit test that currently asserts a descendant "stays
  forwardable today" SHALL invert: a descendant SHALL be refused, naming both
  SHAs.
- **EQUAL-4.3** `m5_certify::format_dirty_tree_gets_correction_commit` SHALL
  assert the format commit is in `base..head` *before* review, and that certify
  did not add a second `porch: apply format` after approval. The
  `porch-fake-format-dirty` fixture SHALL be idempotent (stable overwrite), so
  the certify re-run is a no-op rather than a second dirt that fail-closes the
  happy path.
- **EQUAL-4.4** A new test SHALL show a lint-dirty tree during certify fails
  certify and does not create `porch: apply lint`.

## 5. Operator-facing words match the phases

**Story:** As an operator writing `.porch.yaml`, I want the docs to say where
`commands.format` runs and that a checking form is a supported choice, so I do
not learn the old "certify will commit your formatter's dirt" behaviour.

- **EQUAL-5.1** `docs/usage.md` SHALL teach that the same `commands.format`
  string runs at the end of rebase (may rewrite) and again in certify (must be a
  no-op). A check-only command (`cargo fmt --check`, `just fmt --check`) is a
  supported choice meaning *refuse dirt at rebase*, not *normalize then review*.
- **EQUAL-5.2** The pipeline table SHALL list format under rebase as well as
  certify, and SHALL stop implying certify commits.
- **EQUAL-5.3** `CONTEXT.md` **Certify** SHALL stay a check phase. **Forward
  authorization** SHALL drop descendant tolerance and state equality.

## Out of scope

- ROAD-15 producer-authority stamping; ROAD-24 purge; ROAD-10 inspect.
- ADR-0004's five-site sweep, except `-c commit.gpgsign=false` on the **new**
  rebase-phase commit (EQUAL-2.3). The three `git rebase` sites and the
  deliver-repair commit stay for that ADR's wave.
- Surfacing the discarded `RequiresNew` reason on `porch agent status` (the
  hardcoded `not_reviewed`). Equality heals the certify-correction case going
  forward; the copy defect is independent.
- Changing `applicable_round_id_tx` / `tip_complete_seed` to ignore SHA
  mismatch. The lookup is the right rule.
- `commands.lint` in rebase; `commands.test`; a sixth canonical phase.
- Dogfood dirty-tree measurement on mailgate/klynt (this tree: 80 commits, 0
  rustfmt dirt; consumers are not in this workspace).

## Open questions

None that block coding. The advisor's check-only option is a design alternative,
not an open question: rejected in design.md against ADR-0003.
