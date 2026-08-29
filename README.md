# porch

Local git gate. Independent review. Push only what survived.

```
git push porch
```

Agents produce diffs faster than a human can validate them. Pre-commit hooks have to stay light or they freeze the working tree. CI runs **after** the branch is already public. Branch protection can reject a bad outcome; it cannot get a change ready.

Porch is the missing **inner gate**: opt-in, local, isolated. You push to a remote named `porch` instead of `origin`. That push is consent. A disposable worktree rebases, reviews, and runs cheap local checks. Only then is the branch forwarded and a PR opened. `origin` is never hijacked.

“Passed the porch” means independently reviewed and cheaply certified. It does **not** mean production CI, deploy, or E2E went green.

## Install

Slice crates stay unpublished (`publish = false`), so **0.1.0 is git/path install only** — not crates.io yet.

```sh
cargo install --git https://github.com/jayden-dang/porch --locked
# or from a clone:
cargo install --path crates/porch --locked
```

crates.io (`cargo install porch`) is a **future** option if/when the slice graph is published. Needs `git` and Rust 1.85+ (stable). Check the machine with `porch doctor`.

## Loop

```sh
porch doctor
cd /path/to/your/clone
porch init
porch daemon start                   # optional: install KeepAlive via `porch daemon install`
git push porch HEAD:refs/heads/your-branch
porch runs                           # or bare `porch` / `porch attach`
```

`$PORCH_HOME` overrides `~/.porch`. Put a trusted `.porch.yaml` on the **default branch** (`commands.format` / `commands.lint`, `deliver.github.watch_checks`, …). Executing config is read from that SHA, never from the pushed tip.

Review is an external CLI (`PORCH_REVIEW_BIN`, default `review`). Deliver uses `gh` (`PORCH_GH_BIN`). When review parks, attach the TUI (`porch` / `porch attach` on a TTY) or respond headlessly:

```sh
porch agent status
porch agent respond approve          # or skip | abort | fix
porch agent respond fix --findings f0,f1 --yes
```

JSON on stdout; exit `0` ok/gate, `1` failed/cancelled, `2` usage. The TUI is optional; `porch agent` stays first-class.

**Cold worktree:** certify runs without your clone’s `node_modules`. If format/lint shells out to `biome` / `bun`, those binaries must be on `PATH` (see `porch doctor`).

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

- **Consent is a git remote.** Installing a hook on `origin` is out of scope.
- **Reviewer ≠ fixer.** Review is session-free. A fixer may resume; rereview treats pipeline-authored commits as new code.
- **Certify is cheap.** Format, lint, generated-artifact drift — not Postgres or Playwright.
- **Deliver is narrow.** `--force-with-lease=<ref>:<observed-sha>`; PR checks by **allowlist only**.

## What this is not

Not CI. Not a deploy tool. Not a merge bot. Not team governance. Porch sits in front of production rings; it does not swallow them.

## Develop

```sh
cargo test --workspace
cargo clippy --all-targets -- -D warnings
```

Agent skill notes: [`docs/porch-agent.md`](docs/porch-agent.md). Product briefing: [`docs/00-index.md`](docs/00-index.md).

## License

Apache-2.0. Copyright 2026 The Porch Authors.
