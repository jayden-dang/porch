# porch

Local git gate. Independent review. Push only what survived.

```
git push porch
```

Agents produce diffs faster than a human can validate them. Pre-commit hooks have to stay light or they freeze the working tree. CI runs **after** the branch is already public. Branch protection can reject a bad outcome; it cannot get a change ready.

Porch is the missing **inner gate**: opt-in, local, isolated. You push to a remote named `porch` instead of `origin`. That push is consent. A disposable worktree rebases, reviews, and runs cheap local checks. Only then is the branch forwarded and a PR opened. `origin` is never hijacked.

“Passed the porch” means independently reviewed and cheaply certified. It does **not** mean production CI, deploy, or E2E went green.

## How it works

```
your clone  --git push porch-->  local bare gate (~/.porch/repos/<id>.git)
                                       │
                              pre-receive: admit
                              post-receive: notify daemon
                                       │
                                disposable worktree
                                       │
                    intent → rebase → review → certify → deliver
                                       │
                         GitHub (branch + PR + allowlisted checks)
```

- **Consent is a git remote.** Installing a hook on `origin` is out of scope. You keep working while the gate runs in another tree.
- **Reviewer ≠ fixer.** Review is a cold, session-free process with no shell. A fixer may resume a session and has a shell, but rereview treats pipeline-authored commits as new unreviewed code.
- **Certify is cheap.** Format, lint, generated-artifact drift. Not Postgres, not Playwright, not a full workspace `just gate`.
- **Deliver is narrow.** Force-push is `--force-with-lease` on the exact reviewed SHA. PR checks are babysat by **allowlist only**. Deploy, on-chain publish, and spend-money E2E are never rerun.
- **Config that executes is untrusted from the branch you just pushed.** `commands.*`, agent selection, and review rules that change what runs are read from the default-branch SHA. Fetch failure fails closed.

Findings that would extend scope (schema, durable state, on-chain, a new subsystem) park for a human. Review auto-fix is off unless you raise the limit.

## Why these mechanics

| Constraint | Why |
|---|---|
| Shell out to `git`, never libgit2 | Worktrees, hooks, credentials, `safe.bareRepository=explicit` |
| Absolute `--git-dir` / `-C` | Agent harnesses and CI break cwd discovery |
| OS flock, then Unix socket | PID file is identity, not liveness |
| SQLite, one writer | Daemon state; no async connection pool |
| External review CLI as a subprocess | Grouping, language rules, line anchors, coverage — not a generic “review this branch” agent |
| GitHub only, ACP + one native agent | Adapter surface is a maintenance swamp |
| Rust, virtual workspace | One published binary (`porch`); slice crates are use cases (`porch-git`, `porch-gate`), not layers |

Year-1 review quality comes from that review engine, not from rustc.

## What this is not

Not CI. Not a deploy tool. Not a merge bot. Not team governance. Not a six-forge client. Production PR checks, Coolify, Nitro EIF, on-chain Move, and post-deploy E2E that spend testnet funds stay where they are. Porch sits in front of those rings; it does not swallow them.

## Build

Needs `git` and Rust 1.85+ (stable).

```sh
cargo build -p porch
export PATH="$PWD/target/debug:$PATH"

cd /path/to/your/clone
porch init                 # ~/.porch bare repo, remote `porch`, hooks, daemon
git push porch HEAD:refs/heads/your-branch
```

`$PORCH_HOME` overrides `~/.porch`. Today the gate is **dead**: the push updates the bare repo and records a pending run. The five-phase pipeline is not wired yet.

```sh
cargo test --workspace
cargo clippy --all-targets -- -D warnings
```

## License

Intended Apache-2.0. No `LICENSE` file yet.
