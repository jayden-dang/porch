# Architecture: Porch

Status: Active
Date: 2026-08-30

The architecture spine: the small set of INVARIANTS that keep independently-built
features consistent — not a diagram doc. Each invariant is a bold **ARCH-N** ID plus
one imperative rule. Feature `design.md` files cite the ones they rely on as
`Respects: ARCH-N`, and `audit-trace` verifies those citations point at a live
invariant; `inspect-invariants` judges whether a diff actually conforms.

Rules:

- ID grammar: **ARCH-\<n\>**, flat and repo-wide (unique forever, never reuse).
- One rule per invariant. If it needs "and", it is usually two invariants.
- IDs are immutable once relied upon. Retire by strikethrough, never renumber.
- These are **not to be silently reopened in a coding session.** Changing one is a
  deliberate act with an ADR under `docs/adr/`.

Migrated 2026-08-30 from the `## Non-negotiables` section of `AGENTS.md`, which
now points here.

## Invariants

- **ARCH-1** `origin` is never rewritten; the only consent to gate work is `git push porch`.
- **ARCH-2** All gate git operations shell out to the `git` CLI — never `libgit2` / `git2`
  for worktrees, hooks, force-with-lease, or credentialed fetch/push.
- **ARCH-3** Reviewer turns are session-free; a rereview never certifies its own prescription,
  and the reviewer and fixer roles are never collapsed.
- **ARCH-4** Code-executing config (`commands.*`, agent selection, review rules that change
  what runs) is loaded from the trusted default-branch SHA, never from the pushed SHA, and
  fails closed on fetch failure.
- **ARCH-5** Force-push is `--force-with-lease=<ref>:<observed-sha>` and refuses when live
  remote commits were not incorporated; unverifiable safety facts fail closed.
- **ARCH-6** Review auto-fix defaults to off, and findings that would extend scope (schema,
  durable state, on-chain, new subsystem) are `ask-user`.
- **ARCH-7** Deliver babysits PR checks by allowlist only; it never reruns deploy, on-chain
  publish, or spend-money E2E.
- **ARCH-8** Adapter surface is not added because it is easy: day-1 forges are GitHub only,
  day-1 agents are ACP plus one native CLI.
- **ARCH-9** The porch-owned review quality engine is first-party; never vendor or wrap a
  third-party review CLI as that engine.
- **ARCH-10** Crates are use-case slices, not technical layers; do not add a crate per layer.

## Domains

None — all invariants live above. Split into `docs/architecture/<domain>.md` files only
if this list outgrows one page; the `ARCH-N` namespace stays flat across files.
