# Tasks — reviewed normalization and binding by equality

**Feature code:** EQUAL
**Roadmap item:** ROAD-23 (MILE-3)
**Requirements:** `requirements.md` · **Design:** `design.md`

Waves are ordered by dependency. Each wave ends green on
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo test --workspace`.

## Wave 1 — certify is verify-only

- [x] **1.1** Remove `maybe_correction_commit` and `refresh_head_sha` from
  `run_certify_phase`. After each of format and lint, fail closed on a dirty tree
  naming the command and the porcelain paths. *(EQUAL-1.1, EQUAL-1.2, EQUAL-1.3,
  EQUAL-1.4)*
- [x] **1.2** Make `porch-fake-format-dirty` a stable overwrite so a rebase-then-certify
  re-run is a no-op. Add `porch-fake-lint-dirty`. *(EQUAL-4.3, EQUAL-4.4)*
- [x] **1.3** `lint_dirty_tree_fails_certify_and_does_not_commit`: lint that writes
  fails certify; log has no `porch: apply lint`. *(EQUAL-4.4)*

## Wave 2 — format at the end of rebase

- [x] **2.1** Keep `cfg.commands` in `run_rebase`. After git rebase / reset /
  already-on-onto, run format; commit on dirt with `commit.gpgsign=false`;
  recompute emptiness; Evidence `format_applied` on the open rebase attempt.
  Format non-zero fails the run. *(EQUAL-2.1 … 2.5, EQUAL-2.8)*
- [x] **2.2** Same helper on the parked-rebase fixer retry, from the pinned
  `trusted_config_sha`. Not on deliver-repair rebase. *(EQUAL-2.6, EQUAL-2.7)*
- [x] **2.3** Retarget `format_dirty_tree_gets_correction_commit` and
  `correction_commit_sets_identity_under_use_config_only` at the rebase-phase
  commit; assert certify did not add a second one. *(EQUAL-4.3)*

## Wave 3 — bind the forward by equality

- [x] **3.1** `authorized_forward_sha`: drop `is_ancestor`; require `head ==
  approved`; return the approved SHA. Invert
  `head_continuity_refuses_a_head_off_the_approved_line` so a descendant is
  refused. *(EQUAL-3.1 … 3.4)*
- [x] **3.2** Convert `tripwire_a_correction_commit_forwards_an_unreviewed_sha`
  into `format_rewrite_is_inside_the_reviewed_range_and_forward_binds_by_equality`.
  *(EQUAL-4.1)*
- [x] **3.3** Confirm empty-diff skip still does not require an approved SHA when
  format is absent or a no-op. *(EQUAL-3.6)*

## Wave 4 — operator-facing words and the ledger

- [x] **4.1** `docs/usage.md`: format runs at rebase and again as a check in
  certify; checking form is a supported choice. Pipeline table. *(EQUAL-5.1,
  EQUAL-5.2)*
- [x] **4.2** `CONTEXT.md` **Certify** and **Forward authorization**. *(EQUAL-5.3)*
- [x] **4.3** Mark FWDAUTH-1.1, 1.2, 1.3, 1.4, 1.6 completed by EQUAL. Register
  EQUAL in `docs/specs/catalog/delivery.md`. Close ROAD-23 and MILE-3 on the roadmap.
- [x] **4.4** FAULT's tripwire section records that the pin became a guard.
