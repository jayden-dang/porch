# Using porch (A–Z)

Porch is a **local git gate**. You push to a remote named `porch` instead of `origin`. A disposable worktree rebases, reviews, and runs cheap checks. Only then does porch lease-push your branch and open or update a GitHub PR. **`origin` is never hijacked.**

“Passed the porch” means independently reviewed and cheaply certified. It does **not** mean production CI, deploy, or E2E went green.

Install: [install.md](install.md). This page is the operator loop after the binary is on `PATH`.

## A. What you need

| Need | Why |
|---|---|
| `porch` 0.2.2+ on `PATH` | Gate CLI (`cargo install porch --locked` also installs the floor sibling) |
| `porch-quality` next to `porch` | Mandatory **deterministic floor**. Not PATH-selected, not optional. `porch doctor` checks the sibling of `current_exe` |
| `git` | Gate operations shell out to git |
| `gh` logged in | Deliver (PR). `porch doctor` checks it |
| A judgment engine (optional) | `claude` / `codex` for `engine: agent`. `engine: quality` is floor-only (no judgment) and still requires the sibling |
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

`porch setup` writes `$PORCH_HOME/config.yaml` and, for `quality` / `ocr` / `generic`, a porch-owned `$PORCH_HOME/bin/review` wrapper. That wrapper is the **judgment** selection. The floor is always the `porch-quality` sibling of the running `porch` binary — setup cannot turn it off.

| Engine | When |
|---|---|
| `quality` | Floor-only (no judgment). Selected by default when `porch`'s own bindir is on `PATH`, which `cargo install porch` makes true — so on a machine that also has a coding agent, the default is floor-only and the agent is passed over. Requesting it explicitly resolves the sibling, not a `PATH` lookup |
| `agent` | Coding agent (`claude` / `codex`) for the judgment layer — session-free review turn. Still requires the floor sibling |
| `generic` | a binary already named `review` that speaks `--from --to --format json --output` (judgment). Floor sibling still required |
| `ocr` | legacy only: `porch setup --engine ocr` (judgment). Floor sibling still required |

Do **not** set `PORCH_REVIEW_BIN=ocr` (missing the `review` subcommand). Env still overrides config: `PORCH_REVIEW_BIN`, `PORCH_REVIEW_AGENT_BIN`, `PORCH_FIXER_BIN`, `PORCH_GH_BIN`, `PORCH_HOME`. None of those env vars relocates the floor.

Re-apply after editing `config.yaml`: `porch setup --apply`. Re-check: `porch setup --verify`. Optional login service: `porch setup --yes --install-daemon`.

`porch doctor` must show **floor** ok (`porch-quality` next to `porch`), judgment **review** ok when you selected `agent` / `generic` / `ocr`, and `git` **ok**. Exit 1 only if a hard check fails (`git` missing).

The floor line also carries `identity=<sha256>` — the artifact identity porch observed
for that sibling, and the one the assurance record stores. Two installations reporting
the same identity are running the same floor. Porch does not judge the value: the floor
is whatever sits next to the running `porch`, and porch holds no expected identity to
check it against.

## D. Trusted repo config

Put `.porch.yaml` on the **default branch** (`origin/HEAD`, often `main`). Executing fields are read from that **SHA**, never from the branch you push.

```yaml
pr:
  base_branch: dev          # empty → repos.default_branch from origin/HEAD

commands:
  format: just fmt          # runs at rebase (may rewrite) and again as a check in certify
  lint: just lint           # certify-only; a command that writes files fails certify, it does not commit

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
| rebase | Onto `pr.base_branch` or `origin/HEAD`. Then `commands.format` (may rewrite; a check-only command refuses dirt). Conflict → **park** (`fix` or `abort`) |
| review | Session-free engine. Blocking findings → **park** |
| certify | Re-runs `commands.format` as a check, then `lint`. A dirty tree fails; certify never commits |
| deliver | Lease-push (`--force-with-lease`), scaffold PR (no self-review theater on the visible body), **park** at `compose`, then after respond/skip babysit **allowlisted** checks only |

A **Park** can halt at `rebase`, `review`, or `compose`. Reviewer turns never resume the fixer session. Review auto-fix stays **off**.

## H. Watch a run

From the clone (TTY):

```sh
porch                 # attach TUI if this branch has pending/running/parked
porch attach          # same; --run-id <ULID> to pick a run
porch status          # **Daemon condition**, latest run, recover refs, **Forward verdict**s — does not start a daemon
porch runs            # JSON list (same join; does not start a daemon)
```

Non-TTY `porch` / `attach` prints a snapshot (never raw mode).

When `phase=compose`, `porch agent status` also shows `pr_url`, `compose_packet_path` (`$PORCH_HOME/runs/<run_id>/compose-packet.json`), and `allowed_actions` `respond` | `skip` | `abort`.

`porch agent status` states the run's **assurance shape**
(`floor-only` or `floor+judgment`) on `assurance_record` when a round backs the
run. Legacy and unreviewed records omit the shape. A delivered PR's hidden
`porch-attestation` comment includes the same shape next to `head_sha`.
`porch status` is inspect, not the compact live snapshot: it reports
**Daemon condition**, recover refs, leftover worktree HEADs, and **Forward
verdict**s without calling the daemon.

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
| `h` | Toggle history / audit panel (lazy `get_audit`; not on live subscribe) |
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
porch agent audit                 # pretty audit-document JSON
porch audit [--run-id]            # producers, coverage, and phase tree
porch audit --json [--run-id]     # same JSON as agent audit
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
| `approve` | review | Continue; writes `review_approved_head_sha`. Modern rounds also append a bulk `review_approved` disposition/authority event |
| `skip` | review | Skip remaining review; **no** approved SHA. Modern rounds append `review_skipped` |
| `skip` | compose | Accept scaffold PR body; complete deliver (does **not** skip certify/deliver) |
| `abort` | rebase / review / compose | Cancel the run. Modern review abort appends `review_aborted`. Compose abort leaves the GitHub PR open |
| `fix` | review / rebase | Native fixer, then **session-free** rereview (or rebase retry). Modern review fix appends `fix_requested` **before** the fixer spawns |
| `--body-file` [+ `--title`] | compose | Merge Agent prose into porch-managed PR regions; complete deliver |

Status / `get_run` stay a **compact** live snapshot (findings, optional `audit_available`). Reconstruct disposition/authority from `porch agent audit` or `porch audit --json` (same typed audit document as daemon `get_audit` / TUI `h`). Default `porch audit` prints producers, then per-round coverage counts with non-completed paths, then the phase tree as plain text. Audit `schema_version` 3 is independent of `PROTOCOL_SCHEMA_VERSION` (also 3).

Read the packet at `compose_packet_path` before writing `--body-file`. Empty or theater-shaped bodies (gate Review/Certify/Pipeline boards) are rejected; the run stays parked. Do not combine `--body-file` with approve/skip/abort/fix.

`--yes` is **one** fix round then approve remaining — never the default, never the whole gate. On a modern round it requires a durable `fix_requested` cite and fails closed if that cite is missing.

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

`porch daemon status` and `porch doctor` both report which of four conditions the
daemon is in. `porch status` and `porch runs` report the same **Daemon condition**
and also join custody tips and **Forward verdict**s. None of those four starts a
daemon, so asking does not change the answer, and none of them can hang: every RPC
has a deadline, and inspect does not wait on one.

| `condition` | Means | What to do |
|---|---|---|
| `ready` | answering | nothing |
| `unreachable` | no socket — dead, or never started | `porch daemon start` |
| `refusing` | it starts, hits a startup barrier, and exits before serving | the reported cause names the barrier; fix it, then `porch daemon start` |
| `not-answering` | the socket accepts and nothing comes back — wedged | `porch daemon stop --force`, then `porch daemon start` |

A refusal is recorded in `$PORCH_HOME/daemon.refusal.json` and survives your retries.
It is cleared as soon as a daemon serves. Before it existed the cause went only to
`logs/daemon.log`, which every start truncates, so retrying three times destroyed the
diagnosis three times.

`PORCH_RPC_TIMEOUT_MS` widens or narrows every RPC deadline. Raise it on a loaded
machine if a healthy gate is reported as `not-answering`.

Whatever the condition, `porch eject` still detaches. `porch status` and
`porch runs` still answer: they open the state database read-only (they cannot
create it, migrate it, or terminalize a run) and they list `refs/porch/recover/*`
from the gate repository even when that open fails. `porch agent status` /
`sync` also use that read-only open. `porch agent respond` still writes and still
needs a writable open.

## N. Leave a clone

```sh
porch eject                 # drop remote `porch` + neutralize hooks
porch eject --purge         # also delete this repo’s bare, worktrees, run rows
porch eject --purge --abandon  # chosen loss: proceed despite unforwarded tips or active runs
```

`--purge` does not delete other repos under `$PORCH_HOME` or global `config.yaml`.

**`--purge` is destructive and is outside porch's no-loss guarantee.** It deletes the
bare repository, which holds every porch-authored commit — certify's correction commits
and fixer commits — that you have not pulled into your checkout. Run `porch agent sync`
first if you want them. Plain `porch eject` keeps all of it.

`--purge` refuses, and does **not** detach, while this repo still has unforwarded
custody tips (`refs/porch/recover/*` on the gate repository, or leftover worktree
HEADs) or active runs (`pending` / `running` / `parked`). The refusal prints those
tips and run ids. Inspect with `porch status`, recover with `porch agent sync --recover`,
then either leave the gate state in place or pass `--abandon` to record the chosen
loss under `$PORCH_HOME/abandoned/` and proceed. A tree with nothing unforwarded
and no active runs still purges without `--abandon`.

Detaching does not need a healthy daemon or a readable database. It reports one of three
outcomes after a detach:

| Output | Meaning |
|---|---|
| `PORCH_HOME left intact` | detached; this repo's gate state preserved |
| `purged this repo's bare, worktrees, run artifacts, and DB rows` | detached and removed |
| `detached, but this repo's gate state was left behind: <reason>` | detached; `--purge` could not finish, exit code 1 |

A refused `--purge` is a fourth operator-facing case that is **not** a detach:
the command fails, the `porch` remote remains, and `porch status` still lists the
tips. Re-run `--purge` once the tips are recovered or pass `--abandon`.

The left-behind case is retryable: re-run `porch eject --purge` once the reason is
cleared. So is an eject interrupted part-way — a second run picks up from whatever
step it reached.

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
| a porch command does not come back | It should not any more — every daemon RPC has a deadline. Report it. `porch doctor` names the condition; `porch eject` detaches regardless |
| `condition=not-answering` | Wedged daemon: `porch daemon stop --force && porch daemon start`. If a *healthy* gate reports this under load, raise `PORCH_RPC_TIMEOUT_MS` |
| `condition=refusing` | Read the cause in the same output, or `$PORCH_HOME/daemon.refusal.json`. It survives retries |
| `daemon already running` on start | One daemon per `$PORCH_HOME`, so the previous one has not exited. `porch daemon status` shows which pid holds it |
| doctor warns floor | Install so `porch-quality` sits next to `porch` (`cargo install porch --locked --force`); restart the daemon |
| doctor: `porch was replaced while running` | An install landed under a live daemon, so the sibling beside it is the new floor. `porch daemon restart`. Stop the daemon before upgrading to avoid it |
| `cargo install porch` says `binary porch-quality already exists` | You installed the floor separately on v0.2.1 or earlier. Nothing was installed — add `--force`, or `cargo uninstall porch-quality` first |
| doctor warns review | `porch setup --yes` (judgment: `agent` / `claude`/`codex`; `quality` is floor-only and still needs the sibling) |
| certify `biome: not found` | Put biome on `PATH`; `porch daemon stop --force && porch daemon start` so the daemon inherits it |
| lefthook/e2e on every push | `git push --no-verify porch …` |
| rebase conflict | TUI/`respond` **fix** or **abort** |
| parked at `compose` | `porch agent status` → packet at `compose_packet_path` → `--body-file` / `skip` / `abort` |
| compose respond rejected | Drop gate theater headings from the body; do not paste Review/Certify/Pipeline boards |
| deliver no PR | `gh auth status`; doctor `gh` ok |
| old `~/.porch` still OCR | `porch setup --yes` again |
| run failed: floor unresolved / missing `porch-quality` | Reinstall so the sibling is next to `porch`, restart the porch daemon, then `porch rerun --run-id <ULID>` (copy from status). Do not approve or skip — there is no park |
| run failed: `floor_launch_replaced` | Porch was upgraded under a running daemon. `porch daemon restart`, then `porch rerun --run-id <ULID>` |
| run failed: assurance shape mismatch | Status names pinned vs attempted shape. Fix review config, then `porch rerun --run-id <ULID>` |

## Q. What porch is not

Not CI. Not deploy. Not a merge bot. Not team governance. It sits **in front of** production rings; it does not swallow them.

## R. Upgrading porch (review round identity and the mandatory floor)

**Back up `$PORCH_HOME` before upgrading.** Finish parked legacy runs first, or lose them:
the first open of an upgraded `$PORCH_HOME` installs a **database-resident compatibility
fence** (protocol 2). That transaction **terminalizes** every still-active (`pending` /
`running` / `parked`) run that has no protocol-2 round and **clears undelivered**
`review_approved_head_sha`. Those runs become `failed`. They are **not** still approvable —
recover with `porch rerun --run-id <ULID>` (new run, new worktree, nothing carries forward).

A parked run that still has **no round rows** (a stripped-round leftover, not a post-upgrade
park) can still answer approve / fix / skip / abort, notes, and hunk lookup through its
legacy snapshot. That serve path is not the upgrade path: after the fence, a real pre-floor
park is already `failed`.

New runs after upgrade always require the deterministic floor. An upgrade **cannot** give a
legacy run round identity retroactively.

**Rollback** to a pre-floor (`0.2.x`) binary against an upgraded state root is **unsupported**.
The fence refuses new runs and approvals from a writer that does not understand protocol 2.
Restore a backup of `$PORCH_HOME` taken before the upgrade if you must run an older binary.

**Recovery** after an unsatisfied floor or a shape mismatch: fix the cause (install
`porch-quality` next to `porch`, or restore the judgment engine), **restart the porch daemon**
when the recorded cause is resolution / missing executable, then:

```sh
porch rerun --run-id <ULID>
```

That allocates a new run from the prior tip and intent. It does not reuse the failed run's
authorization.

**Downgrade** after new-format round rows exist is **unsupported**.

## S. Upgrading porch (phase-event history)

**Back up `$PORCH_HOME` before upgrading.** Finish parked and running work first, or lose it:
the first open of a phase-events binary against an older `$PORCH_HOME` raises the
**database-resident compatibility fence** to **protocol 3**. That transaction
**terminalizes** every still-active (`pending` / `running` / `parked`) run with an explicit
phase-events upgrade cause in `runs.error` and **clears undelivered**
`review_approved_head_sha`. Those runs become `failed`. They are **not** still approvable —
recover with `porch rerun --run-id <ULID>` (new run, new worktree, nothing carries forward).

Pre-existing runs keep whatever history they already had. The upgrade **does not** invent
phase attempts, ordinals, timestamps, or nesting for them; `porch audit` reports their phase
slice as unavailable until a fresh run writes real phase events.

The protocol-2 insert and approval triggers stay in force. Protocol 3 adds a status-update
trigger beside them so an under-protocol writer cannot change `runs.status` either.

**Rollback** to a pre-phase-events binary against a protocol-3 state root is **unsupported**.
The fence refuses new runs, approvals, and status writes from a writer that does not
understand protocol 3. Restore a backup of `$PORCH_HOME` taken before the upgrade if you must
run an older binary.

## T. Upgrading porch (durable forward record)

The same fence, one notch higher. Opening a **protocol 4** binary against an older
`$PORCH_HOME` adds the `forward_records` table and raises the minimum to 4, and that
transaction **terminalizes** every still-active run exactly as the protocol-3 upgrade did.
The cause in `runs.error` now names the writer protocol rather than one feature's regime, so
it stays accurate at the next bump; recover the same way, with
`porch rerun --run-id <ULID>`.

**Back up `$PORCH_HOME` first**, and finish parked and running work before upgrading.

What protocol 4 adds is a durable record of each forward attempt: porch commits an *intent*
row naming the ref, the authorized SHA, and the remote tip the lease was resolved against
**before** it pushes, and an *outcome* row naming what landed **after** the push returns and
before it opens or updates the pull request. A gate killed between the push and the pull
request therefore leaves local evidence that the push was authorized and completed, instead
of a run that looks like it never tried. Each `deliver` attempt records at most one
forward, because a deliver repair that leaves HEAD unmoved now hands the phase off to its
next attempt instead of forwarding twice under the same one. Section **U** describes what a
restart makes of that record.

## U. What you see when a gate died mid-forward

`porch status` joins three sources and does not start a daemon to do it.

The durable source of a **Forward verdict** is the `forward_reconciliations`
table, not `runs.error`. A restart still appends the conclusion and, for a run
that was still `running`, still prefixes `runs.error` — but a protocol bump
terminalizes active runs *before* that restart path runs, so those rows never
get the prose. Inspect reads the table. If the table has no row yet (the
daemon died before the next start classified the **Forward record**), `porch
status` derives the same `classify` result as a read and labels it awaiting
reconciliation. It never writes that derivation back.

You will see one of:

- **the branch reached `origin`** — porch's own record says the push succeeded, so your
  commit is on the shared remote. Porch will say that **pull request state is
  unrecorded**, and it means it: a gate can die after the pull request was created but
  before porch stored its URL, and that looks identical on disk to dying before the
  pull request call. Porch will not tell you a pull request does not exist, because it
  cannot know.
- **undetermined** — porch invoked the push and never recorded the result. It narrows
  this case using the gate repository's own `refs/remotes/origin/<branch>`, which git
  writes only once the remote acknowledges a push, so a match resolves it without any
  network call. An absent or different ref resolves nothing and the run stays
  undetermined.
- **not attempted** — there is no intent record. `operator_note` is empty; the typed
  verdict still appears.

Recover every reachable porch-authored commit with `porch status` (lists
`refs/porch/recover/*` and leftover worktree HEADs) then `porch agent sync --recover`
for the one run you want. `--recover` never rewrites `origin`.

**In both cases the remedy is the same: run the branch through the gate again.** The push
is safe to repeat, and porch adopts an existing pull request for that branch rather than
opening a second one. That is why porch tells you what to do rather than leaving you to
inspect `origin` yourself.

Which command depends on whether you have anything left to push. If you have made a commit
since, `git push porch HEAD:refs/heads/<branch>` as usual. If you have not — the common
case, because your branch already matches the gate ref — that push reports *everything up
to date* and starts nothing, so use `porch rerun --run-id <ULID>` instead. It takes a new
run and a fresh worktree over the same commit.

Both are safe against a duplicate: the retry observes `origin` under its own lease, records
`already_current` when the branch is already there, and adopts the existing pull request.

The three earlier writer triggers are unchanged; no new trigger is added.

**Rollback** to a protocol-3 binary against a protocol-4 state root is **unsupported** for
the same reason as above: the fence refuses writes from a writer that does not understand 4.
Reverting the upgrade means restoring a backup of `$PORCH_HOME`. The `forward_records` rows
themselves are append-only and are never rewritten, so a restored backup simply lacks them.

