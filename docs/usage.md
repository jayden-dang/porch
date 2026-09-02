# Using porch (A–Z)

Porch is a **local git gate**. You push to a remote named `porch` instead of `origin`. A disposable worktree rebases, reviews, and runs cheap checks. Only then does porch lease-push your branch and open or update a GitHub PR. **`origin` is never hijacked.**

“Passed the porch” means independently reviewed and cheaply certified. It does **not** mean production CI, deploy, or E2E went green.

Install: [install.md](install.md). This page is the operator loop after the binary is on `PATH`.

## A. What you need

| Need | Why |
|---|---|
| `porch` 0.2.2+ on `PATH` | Gate CLI (`cargo install porch --locked` also installs `porch-quality`) |
| `git` | Gate operations shell out to git |
| `gh` logged in | Deliver (PR). `porch doctor` checks it |
| A review engine | `porch-quality` (preferred), or `claude` / `codex` |
| Optional fixer | `PORCH_FIXER_BIN` (for `respond fix`) |
| Repo tools on `PATH` | Certify runs in a **cold** worktree (no `node_modules`). `biome`, `just`, `moon`, etc. must be on `PATH` |

State lives under `$PORCH_HOME` (default `~/.porch`).

## B. Install and PATH

```sh
cargo install porch --locked
export PATH="$HOME/.cargo/bin:$PATH"    # persist in ~/.zshrc if doctor warns
porch --version                         # porch 0.2.2
```

Full install options: [install.md](install.md).

## C. First-run setup

```sh
porch setup                 # TTY: one screen, Enter applies the recommended engine
porch setup --yes           # headless JSON
porch doctor
```

`porch setup` writes `$PORCH_HOME/config.yaml` and, for `quality` / `ocr` / `generic`, a porch-owned `$PORCH_HOME/bin/review` wrapper.

| Engine | When |
|---|---|
| `quality` | `porch-quality` is on `PATH` (default after `cargo install porch`) |
| `agent` | coding agent (`claude` / `codex`) — session-free review turn |
| `generic` | a binary already named `review` that speaks `--from --to --format json --output` |
| `ocr` | legacy only: `porch setup --engine ocr` |

Do **not** set `PORCH_REVIEW_BIN=ocr` (missing the `review` subcommand). Env still overrides config: `PORCH_REVIEW_BIN`, `PORCH_REVIEW_AGENT_BIN`, `PORCH_FIXER_BIN`, `PORCH_GH_BIN`, `PORCH_HOME`.

Re-apply after editing `config.yaml`: `porch setup --apply`. Re-check: `porch setup --verify`. Optional login service: `porch setup --yes --install-daemon`.

`porch doctor` must show review **ok** (engine `quality` or `agent`) and `git` **ok**. Exit 1 only if a hard check fails (`git` missing).

## D. Trusted repo config

Put `.porch.yaml` on the **default branch** (`origin/HEAD`, often `main`). Executing fields are read from that **SHA**, never from the branch you push.

```yaml
pr:
  base_branch: dev          # empty → repos.default_branch from origin/HEAD

commands:
  format: just fmt          # cheap; not full CI
  lint: just lint           # format/lint/drift — not Playwright/Postgres

deliver:
  github:
    watch_checks: []        # empty = push+PR, no babysit
    rerun_transient: 0

auto_fix:
  review: 0                 # leave off

review:
  path_instructions: []     # optional repo policy for the reviewer
```

Missing file is valid (empty commands). Unreadable trusted commit **fails the run**.

## E. Attach porch to a clone (once)

```sh
cd /path/to/your/clone
git status                  # start from a clean tree
porch init                  # or: porch init --yes / --skip-setup
git remote -v               # must show remote `porch`
```

`init` creates a bare repo under `$PORCH_HOME/repos/<id>.git`, installs hooks, adds remote `porch`, starts the daemon, and copies the `/porch` skill for detected agents.

Do not run `init` as a way to change `origin`.

## F. Push (consent)

Work on a **feature branch**. Then:

```sh
git push porch HEAD:refs/heads/$(git branch --show-current)
# or, with intent:
porch agent run --intent "short why this change" --wait
```

If the repo has a heavy **pre-push** hook (e2e, full CI), skip it for the gate remote only:

```sh
git push --no-verify porch HEAD:refs/heads/$(git branch --show-current)
```

Porch is not that hook. Consent is still `git push porch`, not `git push origin`.

Same-branch push **cancels** the previous run.

## G. Pipeline

Fixed order: **intent → rebase → review → certify → deliver**.

| Phase | What happens |
|---|---|
| intent | Stored from `--intent` / `PORCH_INTENT`. Empty → skip, do not fail |
| rebase | Onto `pr.base_branch` or `origin/HEAD`. Conflict → **park** (`fix` or `abort`) |
| review | Session-free engine. Blocking findings → **park** |
| certify | Trusted `commands.format` / `lint` in the disposable worktree |
| deliver | Lease-push (`--force-with-lease`), scaffold PR (no self-review theater on the visible body), **park** at `compose`, then after respond/skip babysit **allowlisted** checks only |

A **Park** can halt at `rebase`, `review`, or `compose`. Reviewer turns never resume the fixer session. Review auto-fix stays **off**.

## H. Watch a run

From the clone (TTY):

```sh
porch                 # attach TUI if this branch has pending/running/parked
porch attach          # same; --run-id <ULID> to pick a run
porch status          # daemon + latest
porch runs            # JSON list
```

Non-TTY `porch` / `attach` prints a snapshot (never raw mode).

When `phase=compose`, `porch agent status` also shows `pr_url`, `compose_packet_path` (`$PORCH_HOME/runs/<run_id>/compose-packet.json`), and `allowed_actions` `respond` | `skip` | `abort`.

## I. Parked review (TUI)

When `status=parked` and `phase=review`:

| Key | Action |
|---|---|
| `j` / `k` or arrows | Move |
| `space` | Toggle finding for `fix` |
| `d` | On-demand hunk / diff |
| `n` | Edit a note on the finding |
| `a` | Approve (continue certify → deliver) |
| `f` | Fix selected (or all blocking); needs fixer |
| `y` | One `fix --yes` round (not whole-gate yolo) |
| `s` | Skip review for this run (no approved SHA) |
| `x` then `x` | Abort |
| `q` / `Esc` | Detach (run keeps going) |

Rebase parks accept **`f` / `x` only** (not approve/skip).

## J. Parked compose (TUI)

When `status=parked` and `phase=compose`, deliver has already lease-pushed and opened or updated a **scaffold** PR. The visible body is template/placeholders plus a hidden `porch-attestation` comment — **not** Review/Certify/Pipeline self-review theater. The Agent authors the public prose.

| Key | Action |
|---|---|
| `s` | Skip compose: keep the scaffold body; complete deliver; then watch allowlisted checks |
| `x` then `x` | Abort the run (GitHub PR stays open; porch does not `gh pr close`) |
| `q` / `Esc` | Detach |

Compose does **not** accept approve/fix in the TUI. Write prose with `porch agent respond --body-file` (see below). The status panel shows `pr_url` and the compose packet path.

Compose `skip` ≠ review `skip`: it accepts the scaffold and **continues** deliver (certify already ran).

## K. Parked review / compose (JSON / agents)

```sh
porch agent status
# review park
porch agent respond approve
porch agent respond skip
porch agent respond abort
porch agent respond fix --findings f0,f1
porch agent respond fix --yes
# compose park (phase=compose)
porch agent respond --body-file ./pr-body.md
porch agent respond --body-file ./pr-body.md --title "short PR title"
porch agent respond skip
porch agent respond abort
porch agent run --wait
```

Stdout is JSON (JSONL with `agent run --wait`). Exit `0` ok/parked/completed, `1` failed/cancelled, `2` usage.

| Verb / form | Phase | Effect |
|---|---|---|
| `approve` | review | Continue; writes `review_approved_head_sha` |
| `skip` | review | Skip remaining review; **no** approved SHA |
| `skip` | compose | Accept scaffold PR body; complete deliver (does **not** skip certify/deliver) |
| `abort` | rebase / review / compose | Cancel the run. Compose abort leaves the GitHub PR open |
| `fix` | review / rebase | Native fixer, then **session-free** rereview (or rebase retry) |
| `--body-file` [+ `--title`] | compose | Merge Agent prose into porch-managed PR regions; complete deliver |

Read the packet at `compose_packet_path` before writing `--body-file`. Empty or theater-shaped bodies (gate Review/Certify/Pipeline boards) are rejected; the run stays parked. Do not combine `--body-file` with approve/skip/abort/fix.

`--yes` is **one** fix round then approve remaining — never the default, never the whole gate.

**Never merge the PR from the skill.** **Never babysit deploy / spend-money E2E.**

## L. After compose / deliver

Deliver parks at **compose** until respond or skip. On **completed**, porch has lease-pushed, resolved compose (Agent prose or scaffold), and babysat allowlisted checks if configured. Allowlist empty → no CI babysit.

If pipeline commits moved HEAD:

```sh
porch agent sync
porch agent sync --recover
```

`--recover` fast-forwards **your local branch** from a recorded recovery tip when it is an ancestor. It **never** rewrites `origin`.

Failed or cancelled:

```sh
porch rerun                 # new run id, fresh worktree; default = latest on this branch
porch rerun --run-id <ULID>
```

## M. Daemon

`init` starts a detached daemon. Optional login service:

```sh
porch daemon install
porch daemon start
porch daemon status
porch daemon stop           # refuses if runs are active unless --force
porch daemon uninstall
```

## N. Leave a clone

```sh
porch eject                 # drop remote `porch` + neutralize hooks
porch eject --purge         # also delete this repo’s bare, worktrees, run rows
```

`--purge` does not delete other repos under `$PORCH_HOME` or global `config.yaml`.

## O. Typical day

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cd /path/to/clone
git switch -c feat/x origin/dev     # or origin/main — match your PR base
# … commit …
porch agent run --intent "…" --wait
# if phase=review parked: porch   or   porch agent respond approve
# if phase=compose parked: read compose_packet_path →
#   porch agent respond --body-file ./pr-body.md   (or skip / abort)
porch agent sync                    # if local branch lags pipeline
# stop. You merge the PR, not porch.
```

## P. Troubleshooting

| Symptom | What to try |
|---|---|
| `porch: command not found` | `export PATH="$HOME/.cargo/bin:$PATH"` then `porch doctor` |
| doctor warns review | `porch setup --yes` (need `porch-quality` or `claude`/`codex`) |
| certify `biome: not found` | Put biome on `PATH`; `porch daemon stop --force && porch daemon start` so the daemon inherits it |
| lefthook/e2e on every push | `git push --no-verify porch …` |
| rebase conflict | TUI/`respond` **fix** or **abort** |
| parked at `compose` | `porch agent status` → packet at `compose_packet_path` → `--body-file` / `skip` / `abort` |
| compose respond rejected | Drop gate theater headings from the body; do not paste Review/Certify/Pipeline boards |
| deliver no PR | `gh auth status`; doctor `gh` ok |
| old `~/.porch` still OCR | `porch setup --yes` again |

## Q. What porch is not

Not CI. Not deploy. Not a merge bot. Not team governance. It sits **in front of** production rings; it does not swallow them.

## R. Upgrading porch (review round identity)

Finish parked runs before upgrading when you can. An upgrade **preserves** a parked legacy run
(`runs.findings_json`) so approve / fix / skip / abort, notes, and hunk lookup still work, but it
**cannot** give that run round identity retroactively. A **fresh run after upgrade** is how a
change gets a durable review round.

Back up `$PORCH_HOME` before upgrading. **Downgrade** after new-format round rows exist is
**unsupported**.

