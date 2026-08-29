# porch

Local git gate. Independent review. Push only what survived.

```
git push porch
```

Porch sits **in front of** the real remote. A named remote is the consent boundary: pushing to `porch` authorizes an isolated run to rebase, review, certify cheap local checks, and only then forward the branch and open a PR. `origin` is never hijacked.

**Status:** M1 dead gate. `porch init` plus `git push porch` updates a local bare repo and records a pending run. Review, certify, and deliver are not built yet.

## What this is

An inner gate between a local branch and the configured push target.

- Named remote as consent; disposable worktree; reviewer ≠ fixer.
- Trusted config from the default-branch SHA; fail-closed force-push.
- “Pass” means independently reviewed and cheaply certified — not that production CI/CD is green.
- Review is a constrained external CLI (file grouping, language rules, line anchors, coverage), not one generic coding-agent pass.
- Five-phase pipeline (`intent → rebase → review → certify → deliver`). GitHub only. Production CI stays the outer gate.

**Stack:** Rust (edition 2024), virtual workspace. Year-1 review is an external CLI subprocess, not a rewrite of that engine.

## What this is not

Not a CI system. Not a deploy tool. Not a merge bot. Not a team-governance platform. Not a multi-forge client. Path-filtered PR checks, deploys, on-chain publish, and spend-money E2E stay where they are.

## Try the dead gate

Requires `git` and a recent stable Rust (1.85+).

```sh
cargo build -p porch
export PATH="$PWD/target/debug:$PATH"

cd /path/to/your/clone
porch init          # bare repo under ~/.porch, remote `porch`, hooks
git push porch HEAD:refs/heads/your-branch
```

`$PORCH_HOME` overrides the default `~/.porch`.

```sh
cargo test --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt
```

## Layout

| Crate | Role |
|---|---|
| `crates/porch` | CLI binary |
| `crates/porch-git` | `git` CLI wrapper, absolute `--git-dir` |
| `crates/porch-gate` | init, hooks, daemon, run rows |

Later slices (not M1): run / review / deliver.

## Docs

| Doc | Contents |
|---|---|
| [docs/00-index.md](docs/00-index.md) | Map of this briefing |
| [docs/decisions.md](docs/decisions.md) | Locked product and engineering decisions |
| [docs/01-briefing.md](docs/01-briefing.md) | Problem, product bet, name |
| [docs/04-mailgate.md](docs/04-mailgate.md) | Dogfood target: what CI porch must not swallow |
| [docs/05-review-loop.md](docs/05-review-loop.md) | What “good review” means here |
| [docs/06-architecture.md](docs/06-architecture.md) | Phases, components, data |
| [docs/07-rust.md](docs/07-rust.md) | Workspace, process model, traps |
| [docs/08-security.md](docs/08-security.md) | Trust boundary, custody, containment |
| [docs/09-roadmap.md](docs/09-roadmap.md) | Implementation order |

## License

Intended **Apache-2.0**. Not applied until a `LICENSE` file is added.
