# AGENTS.md

Instructions for anyone (human or agent) working in this repository.

## What porch is

Porch is a **local git gate** written in Rust. Push to a remote named `porch`; a disposable worktree runs independent review and cheap certification; the branch is forwarded and a PR opened only after the inner gate passes. It is not CI, not a deploy system, and not a fork of anything else.

Read **`docs/00-index.md` then `docs/decisions.md`** before changing product shape. Locked decisions are not to be silently reopened in a coding session.

Local implementation notes (gitignored): **`.research/`**. Read that directory before implementing the gate, review adapter, or custody/force-push paths. Do not copy third-party source into this tree. Do not put clone paths or prior-product names into committed files.

## Non-negotiables

- `origin` is never rewritten. Consent is `git push porch`.
- Git operations for the gate **shell out to `git`**. Do not use `libgit2` / `git2` for worktrees, hooks, force-with-lease, or credentialed fetch/push.
- Reviewer turns are **session-free**. The fixer may resume a session. A rereview must not certify its own prescription.
- Code-executing config (`commands.*`, agent selection, review rules that change what runs) is loaded from the **trusted default-branch SHA**, never from the pushed SHA. Fail closed on fetch failure.
- Force-push is `--force-with-lease=<ref>:<observed-sha>` and refuses when live remote commits were not incorporated. Unverifiable safety facts fail closed.
- Review auto-fix default is **off**. Findings that would extend scope (schema, durable state, on-chain, new subsystem) are `ask-user`.
- Deliver babysits **PR checks by allowlist only**. Never rerun deploy, on-chain publish, or spend-money E2E.
- Year-1 review engine is an **external review CLI** composed as a subprocess. Do not reimplement grouping, relocation, or language-rule matching until that is an explicit milestone.
- Day-1 forges: **GitHub only**. Day-1 agents: **ACP + one native CLI**. Do not add adapter surface because it is easy.

## Layout

Virtual Cargo workspace. Slices are use cases, not layers. Do not add a crate per technical layer.

```
Cargo.toml                 # workspace; resolver = "3"; members = ["crates/*"]
crates/porch/              # binary; clap; `porch daemon run` is a fast path
crates/porch-git/          # git CLI wrapper, --git-dir absolute; publish = false
crates/porch-gate/         # M1 slice: init, hooks, admit, notify, sqlite, daemon
# later slices (not M1):
# crates/porch-run/        # worktree, intent, rebase
# crates/porch-review/     # external review CLI adapter
# crates/porch-deliver/    # GitHub PR + allowlisted checks
```

State root: `$PORCH_HOME` (default `~/.porch`).

## Commands (once scaffolded)

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Do not run real LLM / review-CLI network / `gh` network in unit tests. Use PATH fakes and fixtures under `tests/fixtures/`.

## Dogfood

First consumer: **mailgate**. Porch must help that monorepo without replacing its CI. Clone index: [`docs/references.md`](docs/references.md).

| Tree | Role | Clone |
|---|---|---|
| mailgate | Production CI porch must not replace; first dogfood | [../../work/CommandOss/mailgate](../../work/CommandOss/mailgate) |

## Language

Source, comments, commit messages, and docs in this repo are **English**.
