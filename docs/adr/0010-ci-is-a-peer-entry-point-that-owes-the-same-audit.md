# 10. CI is a peer entry point, and it owes the same audit guarantee

MILE-8's blocker asked whether CI mode is a supported peer entry point or a fallback only. The
case for "fallback" rested on a peer entry point requiring an amendment to ARCH-1 — "the only
consent to gate work is `git push porch`" — and that case does not survive the code. `porch
rerun` already enqueues a fresh gate run by inserting a run row and asking the daemon to start
it, with no push anywhere in the path, and that run can forward to `origin`. More simply: a CI
job that runs `git push porch` against its own checkout satisfies ARCH-1 verbatim. There is no
reading under which CI needs the invariant moved, so **ARCH-1 is not amended and CI is a peer
entry point.**

What the blocker did not ask is the question that actually decides the milestone. The assurance
record lives in SQLite under `$PORCH_HOME`, which dies with the runner, while GOAL-2 promises an
operator can trace any outcome to its reviewed range, producer and version, per-file coverage,
findings, disposition and authority events, and phase events. **A CI-entered run owes that
guarantee in full, and porch owes a durable, self-describing audit export to discharge it.** An
archived `porch audit --json` is not sufficient on its own: `CONTEXT.md` is explicit that a
nonterminal audit document is partial and that the durable tables are the source of truth, so a
snapshot taken at an arbitrary moment of a job discharges nothing. MILE-8 ships the export or it
does not ship.

One prerequisite is a defect rather than a design question, and it should be fixed before the
milestone starts because it is not CI-specific. `repo_id` is already an indirection —
`init` prefers the `porch.repo-id` git-config value and writes it back, falling back to a hash of
the canonicalized work-tree path — but only `init` and `eject` honour it. Every other call site
recomputes the path hash, so presetting the key or moving an initialized checkout makes `porch
rerun` compute an identity `init` never stored and report no prior run for the branch. Routing
every call site through one resolver both fixes that and gives a CI runner a stable identity with
no further change.

The remaining couplings are named so the milestone is scoped honestly rather than discovered
mid-wave: hooks carry a baked `PORCH_HOME`; one daemon per state root, spawned detached by
`porch agent`, must be reaped when a job ends; the bare's `origin` and its credentials are copied
from the operator's clone at `init` with no injected-token path; porch-authored commits are
reachable only through `refs/porch/recover/*` on that bare; and a blocking finding parks, so a CI
job must act through `porch agent respond` with the same authority as an operator. None of these
is an invariant. GOAL-1's reconciliation of a crashed forward is the one guarantee that a truly
ephemeral runner cannot honour, and MILE-8 states that as a bounded exemption rather than
pretending otherwise.
