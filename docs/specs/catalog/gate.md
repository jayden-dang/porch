# Gate catalog

Lifecycle of a pushed ref: admit, hooks, daemon/RPC, disposable-worktree execution, and the git CLI wrapper.

| Code | Feature | Capability | Match terms | Surface roots | Spec | Status | Roadmap item | Observation |
|---|---|---|---|---|---|---|---|---|
| GATE | Gate lifecycle | Accepts a pushed ref and owns run lifecycle and state: admit, hooks, notify, sqlite, daemon/RPC, eject | admit, hook, notify, daemon, eject, custody | `crates/porch-gate/src/`, `crates/porch-gate/tests/` | — | Recognized | — | OBS-95a5d4, OBS-11fc60 |
| RUN | Run execution | Executes one gate run in a disposable worktree: intent, rebase, review, certify, deliver, agent respond | worktree, intent, rebase, certify, respond | `crates/porch-run/src/` | — | Recognized | — | OBS-5df730 |
| GIT | Git wrapper | git CLI wrapper with absolute `--git-dir`; the only place the gate shells out to git | git, force-with-lease, fetch, push, worktree | `crates/porch-git/src/` | — | Recognized | — | OBS-5954b0 |
