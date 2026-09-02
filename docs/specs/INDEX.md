# Spec Index

Domain router for the capability catalog. Feature cards live under
`docs/specs/catalog/<domain>.md` — **not** in this file. Register a CODE in the
owning shard before writing `requirements.md`. Codes are 2–12 chars, A-Z0-9,
start with a letter, unique forever (never reuse a retired code).

Agents query via pack `load-subgraph/references/catalog-query.md`; they must not
paste the full catalog into context.

**Roadmap item** on each shard card binds the feature CODE to a live `ROAD-N`
when `docs/roadmap/INDEX.md` exists. Write `—` when there is no roadmap layer.
At most one live CODE may name a given ROAD (`R6`). `specify-behavior` is the
only writer of the **Roadmap item** cell on new features.

`map-features` (dispose) is the writer of **Recognized** cards — rows describing a
capability that exists in the code but has no triad yet (Spec `—`). It never mints a
CODE without explicit confirmation, and never writes an `OBS-*` id into the Code cell.
The `Observation` column on shard cards carries the `OBS-<6hex>` provenance from the
`reconcile-features` run that surfaced the capability; it is audit provenance, not an ID
anything cites.

**Surface roots** are the stable ownership prefixes for the capability. They are what lets
`reconcile-features` classify a changed path as `known-impact` on this CODE instead of
reporting it as unowned. Keep them to at most 3 per card.

| Domain | Scope | Surface roots | Feature catalog |
|---|---|---|---|
| gate | Admit, hooks, daemon/RPC, disposable-worktree execution, git CLI wrapper | `crates/porch-gate/`, `crates/porch-run/`, `crates/porch-git/` | [catalog](./catalog/gate.md) |
| assurance | Review adapter, quality engine, round identity, mandatory deterministic floor | `crates/porch-review/`, `crates/porch-quality/`, `crates/porch-gate/src/rounds/` | [catalog](./catalog/assurance.md) |
| delivery | Branch forward, GitHub PR, allowlisted checks, PR compose | `crates/porch-deliver/`, `crates/porch-run/src/deliver.rs` | [catalog](./catalog/delivery.md) |
| operator | clap entrypoint, doctor, setup, attach TUI, fixer adapter | `crates/porch/`, `crates/porch-agent/` | [catalog](./catalog/operator.md) |
