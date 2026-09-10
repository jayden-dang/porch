# 3. Porch's own correction is reviewed, not exempted, and the forward binds by equality

GOAL-1's fourth checkable property — no approval outliving its evidence — was defeated by porch
itself. Review approves a SHA; certify then runs `commands.format` and `commands.lint`, and
`maybe_correction_commit` commits any dirt with `git add -A` and no path filter
(`crates/porch-run/src/certify.rs:283-317`), so HEAD advances past the reviewed SHA after
approval. The forward accepts any descendant (`crates/porch-run/src/lib.rs:1907`), so `origin`
receives a commit no reviewer saw, whose content is not even bounded by the reviewed path set,
and the hidden PR attestation names it as `head_sha` beside `review: completed`. Discovery also
measured a consequence nobody had recorded: because the applicable-round lookup keys on
`to_sha == runs.head_sha` (`crates/porch-gate/src/rounds/authority.rs:453-461`), the drift makes
`porch agent status` report `not_reviewed` and `porch audit` raise `inconsistent_review_state`
on a run that was reviewed and approved — a live defect inside MILE-2, which is `Closed`.

**Certify never commits.** After approval, a tree the adapter left dirty is a certify failure
naming the command and the paths, not a commit. **The mutating step moves earlier instead of
being deleted:** `commands.format` runs inside the rebase phase, before review, so its output
lands in `base..head`, inside the reviewed range, inside `round_coverage`, and inside the
inventory digest. `run_rebase` already resolves the trusted config and discards `cfg.commands`
(`crates/porch-run/src/lib.rs:1952-1958`), so the command is available at that point under the
same `trusted_config_sha`, and ARCH-4 is satisfied unchanged. The normalization is recorded as
`PhaseTransition::Evidence` on the open rebase attempt, which needs no DDL because
`phase_events.kind` already admits `evidence`; it must *not* become a nested operation, because
`phase_attempts.operation_kind` bakes its `CHECK` inside `CREATE TABLE IF NOT EXISTS`. With no
writer left that moves HEAD after approval, the forward binds by equality and the descendant
tolerance is deleted, completing FWDAUTH-1.1 … 1.4 and 1.6 as originally written.

Three alternatives were rejected on measured grounds. Re-review through the existing
revoke-and-handoff idiom costs a *full* re-review, and CONTEXT.md's bulk-response rule then
re-parks the operator on findings they already approved — a trained-override generator. Declaring
the correction exempt cannot be written as an authority event after the fact, because
`guard_applicable` would return `Stale`, and it narrows GOAL-1's "evidence" by definition rather
than discharging it. Bounded-provenance binding — accept a descendant whose commits are
porch-authored and touch only reviewed paths — fails because "porch-authored" is an unpinned
identity string in a worktree with no isolation boundary, and because porch already binds by
equality in `rounds/applicability.rs`, so it would make the authorization layer and the forward
boundary give two answers about one tree.

The accepted cost is adoption: the first run on a repo that has never been formatted puts a
repo-wide reformat into the reviewed diff. That cost is transient and self-extinguishing — the
formatting reaches `origin` through that PR — whereas a bare refusal would recur on every push,
and today's behaviour pushes that same reformat unreviewed. Two obligations follow. The dirty-tree
rate must be measured on the dogfood consumers before the wave lands, with the pure-git loop that
needs no porch code; on porch's own last 80 commits it was zero. And `docs/usage.md`, which
teaches `format: just fmt`, must teach the checking form, since converting a mutating command is
cheaper for an operator than deleting `commands.format` and losing certification entirely.
