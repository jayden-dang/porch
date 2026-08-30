# AGENTS.md

Instructions for anyone (human or agent) working in this repository.

## What porch is

Porch is a **local git gate** written in Rust. Push to a remote named `porch`; a disposable worktree runs independent review and cheap certification; the branch is forwarded and a PR opened only after the inner gate passes. It is not CI, not a deploy system, and not a fork of anything else.

Operator docs: **`docs/install.md`** and **`docs/usage.md`**. Product shape for contributors lives in this file. Locked decisions are not to be silently reopened in a coding session.

Local implementation notes (gitignored): **`.research/`**. Read that directory before implementing the gate, review adapter, or custody/force-push paths. Do not copy third-party source into this tree. Do not put clone paths or prior-product names into committed files.

## Non-negotiables

The invariant spine lives in **[docs/architecture/INDEX.md](docs/architecture/INDEX.md)**
as **ARCH-1 … ARCH-13** — `origin` never rewritten, git via the CLI only, session-free
reviewer turns, config from the trusted default-branch SHA, fail-closed
force-with-lease, auto-fix off, allowlisted check reruns, bounded adapter surface,
first-party quality engine, use-case slices, porch-only assurance outcomes, a
mandatory deterministic floor, and durable authorization before any external forward.

They are **not to be silently reopened in a coding session.** A `design.md` that relies
on one cites it as `Respects: ARCH-N`; `audit-trace` checks the citation resolves and
`inspect-invariants` judges whether the diff conforms. Changing an invariant is a
deliberate act with an ADR under `docs/adr/`.

Engineering rules that are not invariants live in this file: English-only
(**Language**), no network in unit tests (**Commands**), no vendored third-party source
and no clone paths in committed files (**What porch is**), plus:

- Anything `cargo fmt` and `cargo clippy` already enforce is not restated. The workspace
  sets `unsafe_code = "forbid"`, `clippy::all = deny`, and `clippy::pedantic = warn` —
  fix a pedantic warning or `allow` it with a reason; do not ignore it.
- Integration test files are named for the milestone that introduced them:
  `crates/<slice>/tests/m<N>_<topic>.rs`.
- Domain terms follow **[CONTEXT.md](CONTEXT.md)** — use the glossary term, not a
  paraphrase.

## Layout

Virtual Cargo workspace. Slices are use cases, not layers. Do not add a crate per technical layer.

```
Cargo.toml                 # workspace; resolver = "3"; members = ["crates/*"] (slices published for cargo install)
crates/porch/              # binary; clap; `porch daemon run` is a fast path
crates/porch-git/          # git CLI wrapper, --git-dir absolute
crates/porch-gate/         # init, hooks, admit, notify, sqlite, daemon (+ RunExecutor inject)
crates/porch-run/          # worktree, intent, rebase, review, certify, deliver, agent respond
crates/porch-review/       # review adapter (agent / quality / CLI; PATH fake in tests)
crates/porch-quality/      # M16 porch-owned review quality engine (`porch-quality` bin)
crates/porch-agent/        # native fixer CLI adapter (PATH fake in tests)
crates/porch-deliver/      # GitHub PR + allowlisted checks (`gh`)
```

State root: `$PORCH_HOME` (default `~/.porch`).

## Commands (once scaffolded)

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Canonical, agent-read verify commands (with `--workspace`, which these omit) live in
**[docs/agents/project.md](docs/agents/project.md)**. `default-members = ["crates/porch"]`,
so a bare `cargo test` skips `porch-gate`, `porch-git`, and `porch-quality`.

Do not run real LLM / review-CLI network / `gh` network in unit tests. Use PATH fakes and fixtures under `tests/fixtures/`.

## Dogfood

Consumers: **mailgate** (first) and **klynt** (second). Porch must help those monorepos without replacing their CI.

| Tree | Role |
|---|---|
| mailgate | Production CI porch must not replace; first dogfood |
| klynt | Messy monolith CI; PR base `dev` vs `origin/HEAD` `main` |

Local clone locations live in the gitignored `.research/`, not in committed files.

## Language

Source, comments, commit messages, and docs in this repo are **English**.

## Agent skills

This repo is configured for a spec-driven skill set.

- Feature flow: `frame-change` → `specify-behavior` → `design-solution` →
  `plan-tasks` → `build-in-waves`
- Vague ask you want turned into a prompt for a fresh session: `/forge-prompt` (user-run)
- Bug on-ramp: `root-cause` (clear unexpected behavior first, then a guarded fix);
  deployed env: `debug-remote` then `root-cause`; telemetry readiness:
  `assess-observability`
- Capture a conversation/spec/idea into tracker issues: `/publish-issues` (user-run)
- Incoming issues and PRs: `/triage` (user-run)
- Traceability check: the docs-only `audit-trace` skill — run by `prove-claim` and `cut-release`;
  keep it clean
- Project docs (layer enabled): `/define-project` maintains `docs/product/vision.md`
  and the `docs/architecture/` invariant spine; engineering rules live in this file's
  **Non-negotiables**; the feature skills consult them

Repo config the skills read:

- verify commands, release steps, Remote environments: `docs/agents/project.md`
- Team composition (roster, ownership notes, workflow band): `docs/agents/project.md` (`## Team`)
- Issue tracker operations: `docs/agents/issue-tracker.md`
- Triage label mapping: `docs/agents/triage-labels.md`
