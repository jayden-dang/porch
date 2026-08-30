# porch

Local git gate. Independent review. Push only what survived.

```
git push porch
```

Agents produce diffs faster than a human can validate them. Pre-commit hooks have to stay light or they freeze the working tree. CI runs **after** the branch is already public. Branch protection can reject a bad outcome; it cannot get a change ready.

Porch is the missing **inner gate**: opt-in, local, isolated. You push to a remote named `porch` instead of `origin`. That push is consent. A disposable worktree rebases, reviews, and runs cheap local checks. Only then is the branch forwarded and a PR opened. `origin` is never hijacked.

“Passed the porch” means independently reviewed and cheaply certified. It does **not** mean production CI, deploy, or E2E went green.

## Install

**0.2.1** is on crates.io. Full guide: **[docs/install.md](docs/install.md)**.

```sh
cargo install porch --locked
cargo install porch-quality --locked
export PATH="$HOME/.cargo/bin:$PATH"
porch setup
porch doctor
```

Needs Rust 1.85+ and git. `~/.cargo/bin` must be on `PATH`.

## Loop

```sh
porch setup                          # or: porch setup --yes  (detects coding agent; writes config.yaml)
# optional login service (default remains detached):
#   porch setup --yes --install-daemon
porch doctor
cd /path/to/your/clone
porch init                           # copies /porch skill for detected claude/codex; --yes also runs setup
porch daemon start                   # optional: install KeepAlive via `porch daemon install`
git push porch HEAD:refs/heads/your-branch
porch runs                           # or bare `porch` / `porch attach`
```

`$PORCH_HOME` overrides `~/.porch`. Operator config is `$PORCH_HOME/config.yaml` (from setup). Put a trusted `.porch.yaml` on the **default branch** (`commands.format` / `commands.lint`, `deliver.github.watch_checks`, …). Executing config is read from that SHA, never from the pushed tip. Full operator loop (M15 / 0.2-class): [`docs/10-operator-checklist.md`](docs/10-operator-checklist.md).

**Review default is a session-free coding-agent turn** (`porch setup` detects `claude` / `codex`). Legacy OCR remains available via `porch setup --engine ocr` (wrapper at `$PORCH_HOME/bin/review`). `PORCH_REVIEW_BIN` still overrides for PATH fakes / generic CLIs; `PORCH_REVIEW_AGENT_BIN` overrides the agent binary. To switch engines later: edit `review.engine` in `$PORCH_HOME/config.yaml`, then `porch setup --apply`. Deliver uses `gh` (`PORCH_GH_BIN`). When review parks, attach the TUI (`porch` / `porch attach` on a TTY) or respond headlessly:

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
