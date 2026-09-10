# 5. `eject --purge` refuses over unforwarded custody tips

`CONTEXT.md` places purge outside GOAL-4's no-loss guarantee, and that stands: a refusal exists
to make the loss *chosen*, not to extend the guarantee. `ESCAPE` carried a predicate forward for
ratification — refuse when a porch-authored commit is neither reachable from the checkout nor
recorded as having reached `origin` — having discriminated it against "refuse on any unreachable
commit", which fires on the benign post-success case and would train the override. That
discrimination holds, and the predicate is adopted with four corrections that discovery measured.

**It is expressed over tips, not commits.** Not because authorship is undecidable — the two
porch-identity sites do pin `porch@example.com`, and git preserves the author across a rebase —
but because the deliver-repair rebase relabels the *committer* to the operator, so a committer
filter under-counts porch's own commits precisely after a repair. Under-counting is the failure a
refusal exists to prevent. The tips are `refs/porch/recover/*` on the bare plus the HEAD of any
worktree a declined pin left behind, and both are readable with no daemon and no database.

**The reached-`origin` leg falls back to the bare's own tracking ref.** Reading only
`forward_records` refuses every run that predates the table, and ROAD-8's classifier cannot
rescue them: it returns `NotAttempted` for a run with no intent record *before* the tracking-ref
upgrade can run (`crates/porch-gate/src/rounds/reconcile.rs:130-144`). Because the pre-`ESCAPE`
pin gated on ancestry, the common fast-forward case did pin a recovery ref and has no forward
record, so the longest-serving state roots are exactly the ones that would refuse. The purge
predicate therefore reads the forward record first and then
`refs/porch/remotes/origin/<branch>` by ancestry, which `CONTEXT.md`'s **Forward verdict** entry
already ratifies as an evidence class.

**The manifest is the refusal path's output, not a `--dry-run` mode.** `Db::open` takes an
IMMEDIATE transaction and, on a protocol bump, terminalizes active runs, so a dry run would mutate
what it claims to describe. Until a read-only open exists — work the daemon-free inspect wave
owns — the manifest is printed when the refusal fires, with `--abandon` as the single explicit
override and the abandoned tips and run ids written outside the deleted tree.

**Purge refuses on active runs, the way `stop_daemon` already does.** `delete_repo` deletes rows
for runs of any status, after which the live run thread's `run_by_id` returns `None` and its
cleanup falls through to the unpinned removal path — so a predicate evaluated at T is invalidated
at T+ε by the run it did not know about, and the invalidation destroys the commits unpinned.

One shipped defect is repaired in the same wave, because a refusal bolted onto a path that lies
about its own outcome is worse than no refusal: `purge_repo_state` discards the failure of
`remove_dir_all(bare)` and still reports `Purged`, violating `ESCAPE-4.3`.

The accepted residual is that a commit orphaned by a repair rebase — the pre-rebase SHA, replaced
by a rewritten copy — has no tip and is not refused over. Its content survives in the copy, and
GOAL-4 promises every *reachable* porch-authored commit, so this is loss of a name rather than
loss of work.
