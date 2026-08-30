# Spec Index

Feature-code registry: every requirements.md registers its code here before use.
Codes are 2-12 chars, A-Z0-9, start with a letter, unique forever (never reuse a
retired code).

**Roadmap item** binds this feature CODE (delivery unit) to the `ROAD-N` program **slot** it
implements, when the project has a `docs/roadmap/INDEX.md`. Write `—` when there is no
roadmap layer, or when this work was not planned as a roadmap item. At most one live CODE
may name a given ROAD (`R6`). The column is what lets `refresh-roadmap-status` join plan to
spec; `specify-behavior` is the only writer of the Roadmap item and Spec cells.

`map-features` (dispose) is the writer of **Recognized** cards — rows describing a
capability that exists in the code but has no triad yet (Spec `—`). It never mints a
CODE without explicit confirmation, and never writes an `OBS-*` id into the Code cell.
The `Observation` column carries the `OBS-<6hex>` provenance from the
`reconcile-features` run that surfaced the capability; it is audit provenance, not an ID
anything cites.

**Surface roots** are the stable ownership prefixes for the capability. They are what lets
`reconcile-features` classify a changed path as `known-impact` on this CODE instead of
reporting it as unowned. Keep them to at most 3 per card.

This **flat** table is the default. Agents query it (see pack
`load-subgraph/references/catalog-query.md`); they must not assume it stays small
enough to paste whole into context. Optional later scale-out: replace this table
with a Domain router + `docs/specs/catalog/<domain>.md` shards — not required at
bootstrap.

| Code | Feature | Capability | Match terms | Surface roots | Spec | Status | Roadmap item | Observation |
|---|---|---|---|---|---|---|---|---|
| GATE | Gate lifecycle | Accepts a pushed ref and owns run lifecycle and state: admit, hooks, notify, sqlite, daemon/RPC, eject | admit, hook, notify, daemon, eject, custody | `crates/porch-gate/src/`, `crates/porch-gate/tests/` | — | Recognized | — | OBS-95a5d4, OBS-11fc60 |
| RUN | Run execution | Executes one gate run in a disposable worktree: intent, rebase, review, certify, deliver, agent respond | worktree, intent, rebase, certify, respond | `crates/porch-run/src/` | — | Recognized | — | OBS-5df730 |
| REVIEW | Review adapter | Selects the review engine (coding-agent turn, quality, or CLI) and emits JSON findings | review, engine, findings, adapter, reviewer | `crates/porch-review/src/` | — | Recognized | — | OBS-208976 |
| QUALITY | Quality engine | First-party review quality engine: diff, rules, grouping, relocation, coverage | quality, rules, grouping, relocation, coverage, diff | `crates/porch-quality/src/`, `tests/fixtures/quality/` | — | Recognized | — | OBS-9edece, OBS-dc9cb3 |
| OPERATOR | Operator surface | clap entrypoint, doctor, setup, and the attach TUI | cli, doctor, setup, tui, attach | `crates/porch/src/` | — | Recognized | — | OBS-0861ba |
| DELIVER | Delivery | Forwards the certified branch, opens the GitHub PR, babysits allowlisted checks | pr, forward, checks, allowlist, github | `crates/porch-deliver/src/` | — | Recognized | — | OBS-75c657 |
| AGENT | Fixer adapter | Native fixer CLI adapter | agent, fixer, cli-adapter | `crates/porch-agent/src/` | — | Recognized | — | OBS-26ed45 |
| GIT | Git wrapper | git CLI wrapper with absolute `--git-dir`; the only place the gate shells out to git | git, force-with-lease, fetch, push, worktree | `crates/porch-git/src/` | — | Recognized | — | OBS-5954b0 |
