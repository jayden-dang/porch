# Tasks — bounded daemon RPC and the daemon-fault suite

**Feature code:** DFAULT
**Roadmap item:** ROAD-11 (MILE-4)
**Status:** Implemented

## Wave 1 — measure before building

- [x] Reproduce the wedged daemon end to end rather than by reading. `porch init` on a
      throwaway repo, `SIGSTOP` the daemon, time every operator command. Result:
      `porch status`, `porch doctor`, `porch daemon status`, and `porch runs` each
      produced no output and had not returned at 12s; `porch eject` returned in 8ms.
      This is what turned "ROAD-11 may be unbuildable" from an argument into a fact.
- [x] Check the one inference the whole ordering rested on — whether `wait_for_health`
      honours its `timeout` against a bound socket with no acceptor. It does not.
- [x] Correct `F.3` against the code: `porch status` does not print
      `daemon_healthy=true` while wedged. Inside a work tree both its reads precede
      any output, so it printed nothing.

## Wave 2 — the deadline

- [x] Per-method read/write deadlines in `rpc_call`; `health` short, `start_run`
      generous, the rest moderate.
- [x] `Error::RpcTimeout`, distinct from `Error::Io`, classifying both `WouldBlock`
      and `TimedOut`.
- [x] `PORCH_RPC_TIMEOUT_MS` override.
- [x] Bound the `subscribe` acknowledgement and clear the deadline before the stream.
      No carve-out was needed: `subscribe_events` never went through `rpc_call`.
- [x] Confirm `wait_for_health` now returns, and document the bound.

## Wave 3 — vocabulary

- [x] `condition.rs`: `DaemonCondition` with four variants, `label`, `summary`,
      `remedy`, `is_ready`.
- [x] Durable refusal record at `$PORCH_HOME/daemon.refusal.json`, written at both
      barriers, cleared after a successful `bind`.
- [x] `daemon_condition` resolves without starting a daemon and without opening the
      database.
- [x] Decline a fifth live-but-not-ready variant, having failed to find any path that
      holds the database mutex across an unbounded operation.

## Wave 4 — surfaces

- [x] `porch doctor` reports and grades the condition; its daemon advice stops
      claiming a push notify starts a daemon (`F.5`).
- [x] `porch daemon status` reports the condition in both text and JSON, keeping
      `running` and `socket_healthy`.
- [x] `ensure_daemon_for_cwd` drops its blanket remedy hint.

## Wave 5 — the defect the measurement exposed

- [x] Re-measure after the deadline landed, and find that `porch runs` against a
      wedged daemon now spawns a second daemon that binds and serves.
- [x] Trace it to `fs4`'s `try_lock_exclusive` returning `Result<bool>` with a
      discarded `Ok(false)`; confirm it is the only flock call site in the workspace.
- [x] Refuse on contention in `run_daemon`; refuse to spawn over a `NotAnswering`
      daemon in `ensure_daemon`.
- [x] Handle the blast radius in `kill_group` rather than in 18 fixtures: wait,
      bounded, for the group leader to exit, counting a zombie as exited.
- [x] Drop `stop_process`'s blind 200ms sleep, which is the operator-facing half of
      the same race.

## Wave 6 — proof

- [x] `m25_daemon_fault.rs`, ten tests, one instrument per condition, `Drop`-guarded
      wedge, file markers rather than timers.
- [x] `porch eject` and `porch agent sync` asserted end-to-end with no daemon
      answering — the `GOAL-4` guard `ESCAPE` shipped without.
- [x] `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace`.

## Verification notes

- `m25_daemon_fault.rs`: 10 passed, ~4.9s, stable across three consecutive runs at
  default parallelism and once single-threaded.
- Full sweep: 44 targets green.
- Before/after on the wedged daemon, same fixture: every diagnostic command went from
  "no output at 12s" to answering, `porch daemon status` in 2.0s with
  `condition=not-answering`.

## Two pre-existing flakes met along the way

Both were measured rather than labelled, because while a flake stands, every "the
suite is green apart from a known flake" claim is unfalsifiable — the point
`ESCAPE`'s tasks file makes about the one it fixed.

- **`m6_repair`** failed the first full sweep with `gh CLI timed out after 10s`, and
  reproduced 1 in 4 under 2x CPU oversubscription. It sets a 10s budget for its
  shell-script `gh` fake where `m23` sets 20s, and its `gh` fake blocks on
  cross-thread coordination, so the budget has to exceed coordination latency under
  load. After the `kill_group` wait it passed 6 in 6 under the same load. Whether the
  remaining margin is enough on a slower machine is untested; raising `m6_repair`'s
  fake-tool budgets to `m23`'s 20s is the obvious follow-up (**F.9**).
- **`porch-agent`'s `tests::success_parses_summary`** failed the second full sweep
  with `ExecutableFileBusy`. It writes `fake-fixer` and immediately execs it, and a
  concurrent test thread in the same binary can fork while that file is still open
  for writing, so the exec gets `ETXTBSY`. Rate on this branch: 1 failure in 60 runs
  under 3x oversubscription; on `main`, 0 in 20. The crate depends only on `serde`,
  `serde_json`, `thiserror`, and `nix` — nothing this wave touches — so its test
  binary is identical on both branches and the difference is sampling noise. Not
  fixed here because the fix belongs at the spawn site in another slice's product
  code, not in a test helper (**F.10**).

An hour was also lost to a self-inflicted version of `F.8`: `GIT_CONFIG_GLOBAL=/dev/null`
exported into the working shell while measuring the cost of ambient commit signing,
which then propagated through `cargo test` into the gate's own `git rebase` and
produced `Committer identity unknown` on both branches. Worth recording because it
surfaced something real — `certify.rs` passes `-c user.email` / `-c user.name` for
its correction commits, but the deliver-repair rebase does not, so that path depends
on ambient git identity (**F.11**).

## Follow-ups, not in this wave

- **F.9** Raise `m6_repair`'s fake-tool timeouts to match `m23`'s.
- **F.10** `ETXTBSY` on execing a just-written fake binary in `porch-agent`'s tests.
- **F.11** The deliver-repair rebase relies on ambient git identity while certify
  supplies its own. Inconsistent, and a real fragility for a tool whose whole job
  happens in a disposable worktree.
- **F.12** `porch daemon stop --force` still unlinks `daemon.lock`. With the guard
  fixed this is the remaining route to two daemons: unlinking lets the next start
  create a fresh inode and lock it while a survivor of the `SIGTERM` holds the old
  one. Reachable only when the `SIGTERM` does not land, and fixing it means deciding
  what `--force` promises.
- **F.13** `m18_round_identity.rs`'s refusal test removes `home/daemon.sock`, a path
  that exists nowhere in this codebase — the socket is `$PORCH_HOME/socket`. Harmless
  today, and the string appears exactly once in the repo, but it means that test's
  setup is not doing what it reads as doing.
- **F.4** (from `ESCAPE`, still open) Six commands — bare `porch`, `runs`, `status`,
  `attach`, `rerun`, `agent run` — spawn a daemon before answering. `DFAULT-6.2`
  removed the harm this wave exposed, but they still change what they report on.
- **F.8** (from `ESCAPE`, still open) The git-config isolation sweep. Now with a
  caveat worth carrying: it is not the mechanical addition it looks like. Fixtures
  that run a full gate pass depend on ambient global git identity for the gate's own
  commits, so isolating config without supplying identity breaks them — see F.11.
