# Design — fault injection across the forward boundary

**Feature code:** FAULT
**Roadmap item:** ROAD-9 (MILE-3)
**Requirements:** `requirements.md`
**Respects:** ARCH-1, ARCH-2, ARCH-5, ARCH-8, ARCH-12, ARCH-13

## What this build is

Three things, in dependency order:

1. **A binary override** — `PORCH_GIT_BIN`, so a fixture can interpose on the `git` that
   the gate invokes without shadowing the `git` that the operator and the gate's own
   receive hook invoke.
2. **A correction to ROAD-8** — reconciliation currently resolves a verdict for every
   attempt awaiting one and then throws away every verdict whose run is not `running`.
   `§4` fixes what is selected and what is written.
3. **The suite** — `crates/porch/tests/m23_forward_fault.rs`, one `#[test]` per seam,
   each killing a real gate and asserting the durable state against both porch's records
   and the fixture `origin`.

`ARCH-13` is the invariant under test: durable authorization before any external
forward. This build adds no authorization logic; it establishes that the records
`ARCH-13` rests on survive an unclean death and that a restart reads them soundly.
`ARCH-8` is respected by the override's shape — a binary path, not a new adapter
capability. `ARCH-12` (porch-only assurance outcomes) is untouched: the shim influences
only whether a git subprocess completes, never a verdict.

## 1. The instrument

### Why not the two obvious instruments

**Soft `Err` hooks cannot reach the states.** The four existing `PORCH_TEST_*` hooks
return `Err`. An `Err` out of the phase loop reaches `fail_run_with_phase`
(`crates/porch-run/src/lib.rs:566`-`:569`), which writes a `terminal` phase event for the
open `deliver` attempt. After `§4`, a terminalized attempt is definitionally not
interrupted and receives no verdict — so a soft hook can produce every record shape and
still never exercise one line of the code under test. This is not a preference; the
mechanism is incapable.

**A hard exit in production code is refused.** Upgrading the idiom to
`std::process::exit` at the seams of `record_and_forward` would work, and it is the only
way to split a pure-Rust window. It also puts a crash-on-demand primitive into the
fail-closed forward path of a published binary, reachable by anyone who can set an
environment variable on the daemon — and the daemon re-injects `PORCH_*` from its own
environment (`crates/porch-gate/src/proc.rs:29`-`:33`). Gating it on
`#[cfg(debug_assertions)]` or a cargo feature converts that risk into a worse one: a
`cargo test --release` or a feature left off makes every test in this suite pass having
asserted nothing (`FAULT-4.3`). Refused, and the refusal is what makes the override
necessary.

### `PORCH_GIT_BIN`

`porch-git` gains

```rust
pub const GIT_BIN_ENV: &str = "PORCH_GIT_BIN";

pub fn git_bin() -> String {
    std::env::var(GIT_BIN_ENV).unwrap_or_else(|_| "git".to_string())
}
```

and its six `Command::new("git")` sites (`crates/porch-git/src/lib.rs:54`, `:81`, `:108`,
`:246`, `:346`, `:389`) become `Command::new(git_bin())`. That is the whole production
diff for the instrument: no control flow, identical in debug and release
(`FAULT-4.2`). It is the shape `PORCH_GH_BIN`, `PORCH_REVIEW_BIN`, and `PORCH_FIXER_BIN`
already have, so it introduces a pattern the codebase does not already carry no new
concept. `porch doctor` reports the resolved path (`FAULT-4.6`) so that an override in an
operator's environment cannot be invisible.

Every git call on the forward path goes through `porch-git`, so the override reaches all
of them. It deliberately does **not** reach the operator's `git push porch`, which the
fixture invokes directly, nor the gate's `pre-receive` hook, which runs under the
operator's environment. A `PATH` shim would have caught both, which is why the override
is preferred over the cheaper `PATH` trick.

### The shim

One `/bin/sh` script, installed per scenario, selected by `PORCH_SHIM_MODE`. Its default
and its fallthrough are `exec "$PORCH_REAL_GIT" "$@"` (`FAULT-4.4`).

It discriminates on argv. The forward push is
`--git-dir=<bare> push --no-verify [--force-with-lease=…] origin <sha>:refs/heads/<branch>`
(`crates/porch-git/src/lib.rs:568`-`:578`), and the lease observation is
`--git-dir=<bare> ls-remote origin refs/heads/<branch>`
(`crates/porch-git/src/lib.rs:489`). The shim matches on the subcommand plus the target
branch, and fires at most once per scenario, guarded by a state file.

| Mode | Fires on | Behaviour |
|---|---|---|
| `hang_before_intent` | `ls-remote … refs/heads/<branch>` | marker, bounded sleep; never observes |
| `hang_before_push` | the forward push | marker, bounded sleep; **never pushes** |
| `push_then_hang` | the forward push | runs the real push to completion, **then** marker, bounded sleep |

`push_then_hang` is the load-bearing one. The real push runs as a child of the shim,
completes, and — verified against git 2.43 with the refspec `porch init` installs
(`+refs/heads/*:refs/remotes/origin/*`, from a plain `git remote add` at
`crates/porch-gate/src/init.rs:120`-`:121`) — writes `refs/remotes/origin/<branch>` in
the gate bare repository. Only then does the marker appear. The daemon is blocked in
`Command::output()` waiting on the shim, so `append_outcome` provably has not run.

### Determinism, and the one kill that is refused

Every kill waits on a marker the shim writes *after* the decisive side effect, then
signals the daemon's process group (`FAULT-1.5`). No test sleeps a guessed interval, and
no test kills while a real push is in flight (`FAULT-1.6`) — the shim's push has already
returned before the marker exists, so "did `origin` move" is never a race. Sleeps are
bounded so a leaked shim cannot wedge the suite (`FAULT-6.5`), and the existing
`pkill -9 -f <home>` reap in `kill_daemon` collects it because the daemon's git argv
always carries `--git-dir=<home>/…`.

The death is `killpg` with `SIGTERM`, and no crate in the workspace installs a handler
for it, so nothing unwinds and no destructor or `atexit` runs (`FAULT-1.3`). SQLite has
already committed the intent — the write returned before the shim was spawned — so the
record under test is durable by construction rather than by timing.

## 2. The seams, and what each must durably produce

`origin` is a local bare repository in every scenario, so each row's last column is read
directly by the test rather than inferred (`FAULT-2.1`, `FAULT-2.2`).

| Seam | Instrument | `forward_records` | Verdict / evidence | `origin` |
|---|---|---|---|---|
| Before the lease observes | `hang_before_intent` | none | none persisted | untouched |
| After intent, before the push | `hang_before_push` | `intent` | `indeterminate` / `intent_only` | branch absent |
| After the push landed, before the outcome | `push_then_hang` | `intent` | `reached_origin` / `tracking_ref_matched` | carries the SHA |
| `origin` refuses the push | origin `pre-receive` exits 1 | `intent` + `push_failed` | none — the run concludes on its own path | unchanged |

The fourth row is the one that changed shape during design. `RECON-1.5` promises an
undetermined conclusion for an `intent` + `push_failed` attempt, and it is easy to read
that as "a refused push gets a verdict". It does not: `RECON-1.1` scopes conclusions to
*interrupted* runs, and a refused push is not interrupted — the run fails closed with its
own error and terminalizes its own attempt. `RECON-1.5` governs the narrow window where a
gate is killed *after* the `push_failed` record commits. The suite asserts the record
shape production writes for a real refusal, and leaves the verdict for that shape to the
store-level test that already covers it.

Two windows are deliberately unreached. The intent-to-outcome window on the
already-current path is pure Rust — `PushDecision::UpToDate` returns without spawning git
(`crates/porch-git/src/lib.rs:561`-`:563`) — and no external instrument splits it; it is
also the least interesting window here, because no external effect occurred. A kill
between `gh pr create` and `set_pr_url` produces evidence byte-identical to a kill before
the call, which is `RECON-1.7`'s whole point; the suite asserts the resulting message
claims nothing about pull-request presence rather than pretending to distinguish the two.

## 3. What the suite asserts

One `#[test]` per scenario, one state root each (`FAULT-6.1`).

**`forward_landed_then_killed_reconciles_to_reached_origin`** — the highest-value test,
and the only end-to-end proof that ROAD-8's discriminator fires on a ref written by a
real push through the production path. Asserts the persisted `(reached_origin,
tracking_ref_matched)` row, that the fixture `origin` really carries the SHA, that
`runs.error` keeps the interruption prefix and carries exactly **one** conclusion, and
that the message does not assert a pull request is absent.

The conclusion *count* matters and is not covered anywhere today. Each deliver repair
handoff mints a new attempt with its own forward record
(`crates/porch-run/src/lib.rs:1637`-`:1648`), and the error concatenates one note per
resolved verdict, so `err.contains("Re-push")` passes just as happily on four
concatenated conclusions as on one.

**`killed_before_the_push_reconciles_to_indeterminate`** — asserts `(indeterminate,
intent_only)`, that `origin` does not carry the branch, and that the message states the
remedy without claiming the push failed.

**`killed_before_intent_leaves_no_conclusion`** — asserts no forward record, no
conclusion row, run `failed`, `origin` untouched. This is `RECON-8.1` reached by a real
kill instead of by an absent fixture.

**`refused_push_records_failure_and_leaves_origin_unchanged`** — an origin-side
`pre-receive` that exits 1. Asserts the `push_failed` record, that `origin` is unchanged,
and that porch's error reports its own command's failure rather than the remote's state.

**`repush_after_reached_origin_adopts_the_pull_request`** — continues the first scenario:
re-push the branch, then assert exactly one `pr create` across both runs on the
append-only `gh-argv.log`, that the retry's forward record is `already_current` under a
**new** `deliver` attempt, and that `origin` did not move (`FAULT-3.1`-`3.3`).

**`two_restarts_leave_one_conclusion`** — restart twice over one state root; exactly one
row per attempt (`FAULT-3.4`).

Assertions after a restart rest on the startup barrier, not on polling: reconciliation
completes inside `Db::open` → `reconcile_stale` → `recover_stale` before
`UnixListener::bind` (`crates/porch-gate/src/daemon.rs:47`-`:62`), and either recovery
error refuses to serve. So a successful `wait_for_health` proves every verdict row, run
status, and `runs.error` is already durable (`FAULT-6.3`). To make that argument valid the
harness first asserts the pid in `daemon.pid` changed, because health alone only proves
*some* daemon answers on that socket (`FAULT-6.4`).

`§4`'s correction gets two store-level tests, in `crates/porch-gate/tests/m18_rounds.rs`
where the rest of the reconciliation store tests live: one that a conclusion is
*persisted* for an interrupted attempt whose run status is not `running`
(`FAULT-5.6` — the existing guard asserts only that the attempt is *selected*, which is
why it passes today), and one that a terminalized attempt receives none (`FAULT-5.3`).

## 4. The correction: what is selected, and what is written

### Today

`attempts_awaiting_verdict` selects from the record's shape, independent of `runs.status`,
and documents why (`crates/porch-gate/src/rounds/reconcile.rs:176`-`:218`).
`reconcile_interrupted_with_error` then resolves a verdict for each and filters the
results through a `status = 'running'` list before appending
(`crates/porch-gate/src/rounds/phase.rs:965`-`:1008`); `append_verdict_tx` is reached only
from `reconcile_one_running`. Selection honours `RECON-3.6`; persistence defeats it.

### Why "write unconditionally" is the wrong fix

A run that fails closed *after* a successful push — post-push verification failure, which
runs after the outcome commits (`crates/porch-run/src/deliver.rs:993`-`:998`) — holds an
`intent` and a `pushed` record while porch has proven `origin` does not carry the SHA. An
unconditional write hands it `reached_origin` and tells the operator the branch is on
`origin`. `RECON`'s out-of-scope section relies on that run never being classified.

### The discriminator

Interruption is already durable: a gate that died wrote no `terminal` phase event, while
every path that concludes on its own writes one — including `fail_run_with_phase`
(`crates/porch-run/src/lib.rs:343`-`:358`). And the case `RECON-3.6` exists to defend
survives it: the writer-protocol upgrade updates only `runs` and writes no phase event
(`crates/porch-gate/src/db.rs:1056`-`:1066`), so a run interrupted mid-forward and then
terminalized by an upgrade still presents an open `deliver` attempt.

So `attempts_awaiting_verdict` gains one clause — the attempt has no `terminal` phase
event — and the write stops being filtered by run status:

- For a `running` run, nothing changes: the verdict is appended in the same transaction
  that terminalizes the run's open attempts (`RECON-3.2`).
- For a run already terminal, the verdict is appended in its own transaction and nothing
  else is touched — not `runs.status`, not `runs.error`, not any phase event
  (`FAULT-5.4`). That is `RECON-4.2`'s append: evidence about a past forward, carried on
  the conclusion record where a machine can query it, not a rewrite of a settled run.

This bounds the startup path as a side effect (`FAULT-5.5`). Because every selected
attempt now gets a row, and a completed or cleanly-failed attempt is no longer selected at
all, the pre-socket work is proportional to the current restart rather than to the state
root's entire forward history — which is what `RECON-7.1` intended and, per-restart,
did not deliver.

`PROTOCOL_SCHEMA_VERSION` stays at 4 (`FAULT-7.3`): the change adds no column and no
table. `forward_records` is untouched (`FAULT-7.1`).

## 5. The blocker tripwire

`FAULT-8.1` adds one test pinning that after certify's correction commit the SHA carried
to the forward boundary is not the reviewed SHA. It asserts today's behaviour and says in
its own doc comment that it changes when the blocker is decided (`FAULT-8.2`). It is
deliberately not the inverse test — an assertion that an approval *survives* a HEAD
advance would freeze an undecided product question as a regression guard
(`FAULT-8.3`).

## Risks

**The shim runs for every git call in the daemon.** A `/bin/sh` wrapper on a hot path
costs a fork per git invocation. Acceptable inside a fixture; it is why the override is
set only on the gate that is to be killed and never on the restarted one
(`FAULT-4.5`), which also guarantees no verdict in this suite is derived through the
instrument.

**Restart lock contention.** A new daemon takes an exclusive flock and exits with
`daemon already running` if the killed one has not released it
(`crates/porch-gate/src/daemon.rs:43`-`:46`); `wait_for_health` would then burn its budget
and fail with a message that does not name the cause. The harness waits for the old pid to
be gone rather than sleeping a fixed interval, and asserts the pid changed after restart.

**`gh` fake state is per-home, not per-run.** `create` overwrites `gh-pr-state`, so a
two-run scenario must assert on the append-only argv log rather than on the state file, or
"no second pull request" becomes an ordering accident. `FAULT-3.1` says argv log for that
reason.

**Ambient git configuration.** Thirty-seven integration failures during ROAD-7 came from
fixtures inheriting `init.defaultBranch`. This VM also sets `commit.gpgsign=true`. Every
fixture here states its own default branch, identity, and signing (`FAULT-6.6`).

## Alternatives considered

**A `PATH` `git` shim, with no production change at all.** Cheapest, and it was the
leading option. Rejected because `PATH` is inherited by the operator's `git push porch`
and by the gate's `pre-receive` hook, so the shim would sit under paths the scenario is
not testing — a wide blast radius inside the fixture for no gain over a named override.

**A test-only `RunExecutor`.** Sounds like the clean seam, and is not: the daemon injects
the executor at run granularity (`crates/porch-gate/src/daemon.rs:40`) while these tests
spawn the real `porch daemon run` binary. Substituting the executor would mean asserting
against a binary that is not the one shipped, which is the opposite of what a
fault-injection suite is for.

**A randomized crash-consistency harness** — many forwards, kills at seeded pseudo-random
times, asserting invariants rather than a seam table. It finds states nobody enumerated,
which is exactly where this design's two unreached windows live. Rejected as the primary
artifact because a failure would say "some kill somewhere produced a bad verdict" with no
seam attached, and because it needs these fixtures anyway to widen the windows enough to
hit. The soundness assertions it would make are instead attached to each named scenario
(`FAULT-2.1`, `FAULT-2.2`).

**Making a landed push self-evidencing** — promote the remote-tracking ref to primary
success evidence and demote the outcome record to a cache, so the post-push window stops
existing rather than being classified. A better product answer to the ambiguity, and out
of bounds here: it contradicts `FWDAUTH-3.5` (an outcome comes only from porch's own
command result) and softens `ARCH-13`, so it is an ADR under `docs/adr/`, not a coding
session. It would also shrink this suite rather than replace it — the window where
`origin` moved and the client died before the ref was written is untouched by it.
