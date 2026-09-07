# Gate catalog

Lifecycle of a pushed ref: admit, hooks, daemon/RPC, disposable-worktree execution, and the git CLI wrapper.

| Code | Feature | Capability | Match terms | Surface roots | Spec | Status | Roadmap item | Observation |
|---|---|---|---|---|---|---|---|---|
| GATE | Gate lifecycle | Accepts a pushed ref and owns run lifecycle and state: admit, hooks, notify, sqlite, daemon/RPC, eject | admit, hook, notify, daemon, eject, custody | `crates/porch-gate/src/`, `crates/porch-gate/tests/` | — | Recognized | — | OBS-95a5d4, OBS-11fc60 |
| RUN | Run execution | Executes one gate run in a disposable worktree: intent, rebase, review, certify, deliver, agent respond | worktree, intent, rebase, certify, respond | `crates/porch-run/src/` | — | Recognized | — | OBS-5df730 |
| GIT | Git wrapper | git CLI wrapper with absolute `--git-dir`; the only place the gate shells out to git | git, force-with-lease, fetch, push, worktree | `crates/porch-git/src/`, `crates/porch-git/tests/` | — | Recognized | — | OBS-5954b0, OBS-fb2db9 |
| PHASE | Phase-event history | Append-only `phase_events` as the source of truth for a run's phase lifecycle: top-level attempts and ordinals, nested operations, atomic handoffs, crash evidence, derived repair budget, and the audit document's phase slice rendered by `porch audit` | phase event, phase attempt, nested operation, phase handoff, timeline, co-write seam | `crates/porch-gate/src/`, `crates/porch-run/src/`, `crates/porch/src/` | `../2026-09-07-phase-events/` | Approved | ROAD-5 | — |
