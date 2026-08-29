# porch

Local git gate. Independent review. Push only what survived.

```
git push porch
```

Porch sits **in front of** the real remote. A named remote is the consent boundary: pushing to `porch` authorizes an isolated run to rebase, review, certify cheap local checks, and only then forward the branch and open a PR. `origin` is never hijacked.

**Status:** research handoff. No implementation yet. Read [`docs/00-index.md`](docs/00-index.md) before writing code.

## What this is

An inner gate between a local branch and the configured push target.

- Named remote as consent; disposable worktree; reviewer ≠ fixer.
- Trusted config from the default-branch SHA; fail-closed force-push.
- “Pass” has one meaning: independently reviewed and cheaply certified — not that production CI/CD is green.
- Review is a constrained external CLI (file grouping, language rules, line anchors, coverage), not one generic coding-agent pass.
- Five-phase pipeline. GitHub-only. Production CI stays the outer gate.

**Stack:** Rust. Year-1 review is an external review binary composed as a subprocess, not a rewrite of that engine.

## What this is not

Not a CI system. Not a deploy tool. Not a merge bot. Not a team-governance platform. Not a multi-forge client. Mailgate-style path-filtered Actions, Coolify, Nitro EIF, on-chain Move, and post-deploy E2E that spend testnet funds stay where they are.

## Docs

| Doc | Contents |
|---|---|
| [docs/00-index.md](docs/00-index.md) | Map of this briefing |
| [docs/decisions.md](docs/decisions.md) | Locked product and engineering decisions |
| [docs/01-briefing.md](docs/01-briefing.md) | Problem, product bet, name |
| [docs/04-mailgate.md](docs/04-mailgate.md) | Production consumer: what CI porch must not swallow |
| [docs/05-review-loop.md](docs/05-review-loop.md) | What “good review” means here |
| [docs/06-architecture.md](docs/06-architecture.md) | Porch shape: phases, components, data |
| [docs/07-rust.md](docs/07-rust.md) | Crate list, process model, traps |
| [docs/08-security.md](docs/08-security.md) | Trust boundary, custody, containment |
| [docs/09-roadmap.md](docs/09-roadmap.md) | Implementation order |
| [docs/references.md](docs/references.md) | Dogfood clone (mailgate) |

## License

Intended **Apache-2.0**. Not applied until a `LICENSE` file is added.
