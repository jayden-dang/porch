# Requirements: Purge refuses over unforwarded custody tips

Feature code: PURGE
Status: Implemented
Date: 2026-09-10

Roadmap item: ROAD-24 (MILE-4 — Escape without the daemon). Completes GOAL-4's
abandon half: when automatic reconciliation cannot complete, the operator can
detach, and destroying this repo's gate state is a *chosen* loss rather than a
silent one.

Respects: ARCH-1, ARCH-2, ARCH-10, ARCH-11, ARCH-13.

Owner ruling: [ADR-0005](../../adr/0005-purge-refuses-over-unforwarded-custody-tips.md).
This wave implements that ruling. It does not reopen it. An advisor pass during
framing recommended that `--abandon` not override the active-run interlock, and
that an unreadable database detach-then-`LeftBehind` even without `--abandon`.
Those options are recorded and rejected in design.md: a crashed daemon leaves
`running` rows, and `stop --force` does not clear them, so an override that
cannot touch active runs traps the operator; detach-first on an unreadable
database destroys LOOK's repo identity (`LOOK-3.7`) while the tips still
exist. Coding details the ADR left incomplete — refuse-before-detach, the
tracking-ref name LOOK already reads, where the abandon record is written, GitDir
ancestry — are locked here from measurement, not taste.

ESCAPE-5.1 forbade adding a refusal. This feature supersedes that SHALL.

Vocabulary is locked in `CONTEXT.md`. These criteria use **Eject**, **Custody**,
**Run**, **Forward record**, **Forward verdict**, **Worktree**. They do not
redefine them.

## 1. `--purge` does not start destroying until loss is proven empty or chosen

**Story:** As an operator, I want `--purge` to refuse while porch still holds
unforwarded **Custody** tips or live **Run**s, so that deleting the bare is a
choice I can still inspect my way out of.

- **PURGE-1.1** WHEN `porch eject --purge` is invoked without `--abandon` THE
  SYSTEM SHALL evaluate the predicate in §2 **before** removing the `porch`
  remote, unsetting `porch.repo-id`, or neutralizing hooks.
- **PURGE-1.2** WHEN that predicate fails THE SYSTEM SHALL NOT mutate the
  checkout or `$PORCH_HOME` for this repo, SHALL print the manifest (§3), and
  SHALL exit 1. `resolve_repo_id` SHALL still succeed afterwards.
- **PURGE-1.3** WHEN there are no unproven tips and no active **Run**s for this
  `repo_id` THE SYSTEM SHALL purge as today (detach, then delete rows and
  files). `--abandon` SHALL NOT be required.
- **PURGE-1.4** `--abandon` SHALL require `--purge`. `--abandon` without
  `--purge` SHALL be an invalid combination (non-zero, no detach).
- **PURGE-1.5** `--abandon` SHALL override both unforwarded tips and the
  active-run interlock. THE SYSTEM SHALL NOT add a `--force` on eject.

## 2. Predicate is over LOOK's tips; origin proof is local; fail closed

**Story:** As an operator with a recover ref for work that never landed on
`origin` and is not in my checkout, I want `--purge` to refuse; as an operator
whose recover ref is behind `refs/remotes/origin/*` on the bare, I want
`--purge` to proceed without `--abandon`.

- **PURGE-2.1** Tips SHALL be exactly LOOK's two classes: `refs/porch/recover/*`
  on the layout-derived bare, plus leftover worktree HEADs under
  `$PORCH_HOME/worktrees/<repo_id>/`. THE SYSTEM SHALL call the promoted LOOK
  listers, not a second implementation.
- **PURGE-2.2** IF the bare exists and listing recover refs fails, OR
  `worktrees/<repo_id>` exists and listing worktree tips fails, THEN THE SYSTEM
  SHALL refuse (cannot prove empty). An absent bare / absent worktree root
  SHALL mean no tips of that class. A leftover worktree directory whose HEAD
  cannot be read SHALL refuse, not be skipped.
- **PURGE-2.3** A tip is **proven** iff (a) it is an ancestor of (or equal to) the
  operator checkout `HEAD` via work-tree `is_ancestor`, or (b) origin-proof
  §2.4 holds. Git errors on (a), including missing objects, SHALL NOT count as
  reachable from the checkout.
- **PURGE-2.4** Origin-proof SHALL be, without a network probe: a **Forward
  record** outcome `pushed` / `already_current` or a stored **Forward verdict**
  `reached_origin` for that tip's `run_id` whose `authorized_sha` has the tip
  as ancestor-or-equal **on the bare**; else the tip is ancestor-or-equal of
  any `refs/remotes/origin/*` on that bare. Tracking name SHALL be
  `refs/remotes/origin/<branch>` (what `observed_tracking_sha` already reads).
  THE SYSTEM SHALL NOT call `classify` as the purge predicate (`NotAttempted`
  short-circuits before the tracking upgrade; that upgrade is equality, not
  ancestry).
- **PURGE-2.5** `not_attempted`, `indeterminate`, `push_failed`, unknown
  verdict, missing tracking ref, missing objects on the bare SHALL NOT prove
  origin. Fail closed.
- **PURGE-2.6** THE SYSTEM SHALL refuse when this `repo_id` has `active_runs`
  in `pending` / `running` / `parked`, the same set `stop_daemon` uses. Predicate
  and manifest SHALL use `Db::open_read` only.
- **PURGE-2.7** The accepted residual: a commit orphaned by deliver-repair
  rebase (pre-rebase SHA, no tip) SHALL NOT be refused over. GOAL-4 is reachable
  commits.

## 3. Manifest is the refusal output; abandon record is outside the deleted tree

- **PURGE-3.1** THE SYSTEM SHALL NOT add `--dry-run`. The manifest SHALL print
  only on refusal (the human body of that error). A successful clean purge
  SHALL NOT print a manifest.
- **PURGE-3.2** The manifest SHALL list each unproven tip (`ref` or worktree
  path, SHA, `run_id` when known), why it is unproven, active run ids, and a
  remedy naming `porch status`, `porch agent sync --recover`, and
  `porch eject --purge --abandon`.
- **PURGE-3.3** Writer `Db::open` SHALL occur only after the predicate passes or
  `--abandon` is accepted and the abandon file has been written.
- **PURGE-3.4** WHEN `--abandon` proceeds THE SYSTEM SHALL write
  `$PORCH_HOME/abandoned/<repo_id>-<ulid>.json` with repo id, timestamp, tips,
  and active run ids **before** detach or deletion. Write failure SHALL abort
  with no mutation. A later purge SHALL NOT delete this file.
- **PURGE-3.5** WHEN `state.sqlite` exists but `open_read` fails THE SYSTEM SHALL
  refuse `--purge` without `--abandon` (cannot prove origin via records and
  cannot prove no active runs). It SHALL NOT detach. `--purge --abandon` MAY
  then detach and report `LeftBehind` if the writer open or delete cannot
  complete.
- **PURGE-3.6** WHEN `state.sqlite` is absent THE SYSTEM SHALL treat active runs
  and forward records as empty and SHALL apply git-only proofs.

## 4. `Purged` is true only if the bare is gone

**Story:** As an operator, I do not want the word "purged" for a tree whose gate
repository is still on disk.

- **PURGE-4.1** `purge_repo_state` SHALL propagate `remove_dir_all(bare)`
  failure. `purge_or_report` SHALL map it to `LeftBehind`, never `Purged`.
- **PURGE-4.2** ESCAPE-3.3 / 3.4 remain: row delete still precedes destructive
  filesystem steps; row-delete failure SHALL NOT have removed the bare.
- **PURGE-4.3** Detach without `--purge` SHALL remain database-free
  (`ESCAPE-2.1`). PURGE SHALL NOT put `Db::open` on the non-purge path.

## 5. Docs and ESCAPE-5.1

- **PURGE-5.1** This feature SHALL supersede ESCAPE-5.1. The ESCAPE spec SHALL
  record the supersession; the SHALL is not silently ignored.
- **PURGE-5.2** `docs/usage.md` §N and `CONTEXT.md` **Eject** SHALL state:
  `--purge` refuses over unforwarded custody tips and active runs; `--abandon`
  is the override; refusal does not detach; `--purge` remains outside GOAL-4's
  no-loss guarantee.
- **PURGE-5.3** Catalog `PURGE` on `docs/specs/catalog/gate.md`; close ROAD-24
  and MILE-4 when the wave is done. The held `--force` SIGKILL sliver is not a
  MILE-4 member.

## Out of scope

- SIGKILL on `porch daemon stop --force`.
- Making the daemon not wedge.
- Recover-ref sweep (ESCAPE F.1).
- Remaining `repo_id_for` call sites (ADR-0010 / MILE-8).
- Reopening ADR-0005 (committer-filter, `--dry-run` as a mode, refuse on any
  unreachable commit).
- Fail-closed leftover worktree or artifact `remove_dir_all` after rows are
  already gone (named residual).
- Repairing `stop_daemon`'s writer `Db::open`.
