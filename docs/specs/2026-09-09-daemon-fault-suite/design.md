# Design — bounded daemon RPC and the daemon-fault suite

**Feature code:** DFAULT
**Roadmap item:** ROAD-11 (MILE-4)
**Respects:** ARCH-2 (git via the CLI only), ARCH-10 (use-case slices), ARCH-13
(durable authorization before any external forward — untouched here)

## Why the suite came second

ROAD-11's declared deliverable is a test file. Framing it produced the opposite
conclusion twice, from two independent passes, and measurement settled it.

Against a daemon stopped with `SIGSTOP` on a repo created by `porch init`:

| Command | Before | After |
|---|---|---|
| `porch daemon status` | no output, still running at 12s | `condition=not-answering` in 2.0s |
| `porch doctor` | no output, still running at 12s | reports the condition |
| `porch status` | no output, still running at 12s | returns with the condition |
| `porch runs` | no output, still running at 12s | returns with the condition |
| `porch eject` | detaches in 8ms | unchanged |

The diagnostic commands were the ones that hung, in exactly the state `GOAL-4`
exists for. A suite written against that could assert only "this did not finish in
N seconds" — the timer race `FAULT-1.5` forbids — and would freeze the hang as
correct, which is the trap `ESCAPE`'s tasks file named for this item. So the
vocabulary was built first and the suite asserts it, the same relationship ROAD-8's
`Evidence` enumeration has to ROAD-9's fault injection.

## 1. The deadline (`DFAULT-1`)

`rpc_call` now sets a read and a write deadline before sending. There was no
`set_read_timeout`, `set_write_timeout`, or `connect_timeout` anywhere in the
workspace before this.

Why an unbounded read was reachable at all: a daemon stopped after
`UnixListener::bind` leaves a bound socket in the kernel with its listen backlog
intact. The client's `connect` succeeds, its `write` succeeds into the socket
buffer, and its `read_line` waits for a reply from a process that will never be
scheduled. Nothing times out because nothing failed.

Deadlines are per method rather than global, because the methods differ in kind:

- `health` — 2s. Answered from a literal (`{"ok": true, "pid": …}`) without
  touching the database, so a `health` that does not answer at once is a wedged
  daemon rather than a busy one. A short deadline here is what makes
  `not-answering` a fast, cheap observation.
- `start_run` — 120s. It joins a superseded run's executor thread before replying,
  so it is the one RPC that can legitimately take a long time while the daemon is
  perfectly healthy. Giving it the same deadline as `health` would report a working
  gate as wedged, which is the failure mode a blanket timeout invites.
- everything else — 15s. Database reads on the daemon's connection.

`PORCH_RPC_TIMEOUT_MS` overrides all of them. It is a timeout configuration, not a
test-only switch that changes behaviour, so it does not reopen what `FAULT-4.2`
settled: an operator on a loaded machine can widen it, and the suite narrows it to
700ms so that every wedged-daemon assertion costs under a second.

A deadline expiry becomes `Error::RpcTimeout { method, ms }`, not `Error::Io`.
`SO_RCVTIMEO` surfaces as `WouldBlock` on Linux and `TimedOut` elsewhere; both are
classified as a timeout. The distinction is what lets the condition separate a
daemon that never answered from one that could not be reached.

### The event stream needs no carve-out

Both framings expected the hard part to be `subscribe`: it is a long-lived stream,
idle between events, kept alive by a server-side newline every 30s, so a deadline
on its reads would break the TUI.

It turned out not to arise. `subscribe_events` builds its own `UnixStream` and does
its own framing rather than going through `rpc_call`, so the two paths were already
separate. Only the subscribe *acknowledgement* is bounded — a wedged daemon should
not hang a TUI startup either — and the deadline is cleared with
`set_read_timeout(None)` before the event loop begins.

### `wait_for_health` can now keep its promise

`wait_for_health(home, timeout)` reaches its elapsed check only when `health_check`
returns, so with an unbounded read it could not honour its own argument at all.
Nothing about the loop changed; bounding the probe is what makes the bound real. It
is now `timeout` plus at most one health deadline, since a probe already in flight
when the budget expires still has to come back.

## 2. A refusal that survives a retry (`DFAULT-2`)

`run_daemon` refuses before `UnixListener::bind` when either startup barrier fails —
`reconcile_stale` or `recover_stale`. So a refusal leaves no socket and no pid file:
byte-identical to a daemon that was never started. The cause went to
`$PORCH_HOME/logs/daemon.log`, which `spawn_detached_with_env` opens with
`File::create`. Every retry truncated it. An operator who tried three times
destroyed the diagnosis three times.

The record is `$PORCH_HOME/daemon.refusal.json` — plain JSON with the barrier name,
the barrier's own error text, and a unix-seconds timestamp. Written at each refusal
site on the way out; cleared immediately after a successful `bind`, so a stale
refusal is never reported as a current one.

Deliberately not under `logs/`, and deliberately not a database row: the states it
describes are ones in which the daemon is absent and the database may be exactly
what is broken (`DFAULT-2.4`).

Writing it is best-effort. A daemon that cannot write the record still reports its
error by exiting; refusing to refuse would be worse than an unrecorded refusal.

## 3. Four conditions, and why not five (`DFAULT-3`)

`DaemonCondition` in `porch-gate/src/condition.rs`:

| Variant | Means | Carries |
|---|---|---|
| `Ready` | `health` answered in time | pid |
| `Unreachable` | the socket would not connect | reason, naming the socket |
| `Refusing` | a refusal record exists and the socket is quiet | barrier, cause, timestamp |
| `NotAnswering` | connected, `health` exceeded its deadline | pid, waited_ms |

Resolution order is health probe first, then the refusal record. A refusal record
outlives the process that wrote it, so it is only current while the socket is also
not answering — which is precisely the branch that consults it.

`daemon_condition` starts nothing and opens no database, so it is answerable in
every state it describes. Each variant carries a remedy naming a command, because a
condition an operator cannot act on is a diagnosis and not a fix.

**There is no fifth live-but-not-ready variant**, and this was a deliberate refusal
rather than an oversight. `ESCAPE` follow-up `F.3` describes the wedge as the daemon
holding the database mutex, which would make a readiness probe the natural fix. That
mechanism could not be confirmed: no `conn()` guard in `crates/porch-gate/src` is
held across a subprocess call, `reconcile.rs` documents its tracking-SHA read as
local-only for exactly this reason, and `busy_timeout` is 5s so contention yields
`SQLITE_BUSY` rather than a hang. A `SELECT 1` probe would not have detected either
reachable wedge. Inventing a variant no fault can induce would have handed the suite
an assertion it could not write, so the question is recorded open (`DFAULT-8.2`).

`F.3`'s other claim is also corrected rather than implemented: `porch status` does
not print `daemon_healthy=true` while wedged. Inside a git work tree — where the
operator is — it reads health, then calls `list_runs`, and both precede any output,
so it printed nothing. The recorded symptom described only the out-of-work-tree
path, and the real defect was worse than the note (`DFAULT-8.4`).

## 4. Where the condition surfaces (`DFAULT-4`)

`porch doctor` and `porch daemon status` are the commands an escaping operator
reaches for, and neither spawns a daemon, so reporting from them means asking does
not change the answer.

`doctor` grades the condition: `ready` is `ok`; `refusing` and `not-answering` are
`FAIL` with cause and remedy, because a daemon that will not serve is something the
operator must act on; `unreachable` stays `info`, because a checkout whose daemon
has exited is in an ordinary state. Its advice also stops claiming the daemon is
"started on first push notify" — the post-receive hook execs
`porch daemon notify-push`, which makes a best-effort `start_run` RPC and logs a
warning. `porch init` does start one.

`daemon status` gains `condition` and keeps `running` and `socket_healthy`. Both of
those are true statements that together cannot name a wedged daemon: a pid file plus
an unanswering socket reads exactly like a daemon that died between the pid write
and the bind. Keeping them alongside costs nothing and removing them would drop
detail the condition does not carry.

`ensure_daemon_for_cwd` no longer appends `try: porch daemon start` to every
failure, because that hint is wrong for a wedged daemon and each condition now
carries the right one.

## 5. One daemon per state root (`DFAULT-6`)

Not framed. Found by re-running the wedge measurement after the deadline landed,
which is the value of measuring rather than reasoning: bounding the RPC turned
`ensure_daemon`'s infinite hang into a reachable spawn, and the spawn succeeded.

Observed directly. Daemon 1 stopped, `/proc` state `T`, still holding the flock on
`daemon.lock` (inode confirmed identical to the live file). `porch runs` spawned
daemon 2, which bound the socket, wrote the pid file, logged `daemon listening`, and
answered `health`. Two daemons, one state root, each with its own `Db` handle, each
having run `kick_pending`.

The cause is one discarded value. `fs4` 0.13's `try_lock_exclusive` returns
`Result<bool>` and reports contention as `Ok(false)`, not `Err`. `run_daemon`
applied `map_err` and dropped the `bool`:

```rust
lock_file
    .try_lock_exclusive()
    .map_err(|e| crate::Error::Other(format!("daemon already running: {e}")))?;
```

The guard has therefore never excluded anything, and nothing warns: `bool` is not
`#[must_use]`, so neither `clippy::pedantic` nor `unused_must_use` sees it. It is
the one flock call site in the workspace, verified by grep.

Two changes follow, and they belong with the deadline rather than after it, because
the deadline is what made the defect reachable from a wedge:

1. `run_daemon` treats `Ok(false)` as contention and refuses, naming the lock path.
2. `ensure_daemon` refuses over a `NotAnswering` daemon instead of spawning. A
   wedged daemon still holds the lock, so a spawn cannot succeed once (1) holds —
   and reporting `daemon already running` would name the symptom of the recovery
   attempt rather than the fault the operator has.

### The blast radius, and why it went in `kill_group`

Fixing a guard that never worked breaks whatever relied on it not working.

`kill_group` returned as soon as `SIGTERM` was delivered, and `stop_process` then
slept a flat 200ms. The daemon holds the lock until it actually exits, so a start
issued straight after a stop could lose that race — invisibly, because the
replacement used to serve anyway. With the guard fixed it becomes an honest
`daemon already running`.

That reaches the operator through the command `usage.md` prescribes for a wedge
(`porch daemon stop --force` then `porch daemon start`), and it reaches 18 of the 20
test fixtures, whose `kill_daemon` sleeps rather than waits. Only
`m23_forward_fault.rs` waited, and its comment says why in as many words.

So the wait went into the shared primitive rather than into 18 copies: `kill_group`
waits, bounded at 10s, for the group leader to be gone. A zombie counts as gone —
it has released the lock, and a caller that spawned the daemon itself and never
reaps it would otherwise wait out the entire bound. Liveness is read from
`/proc/<pid>/stat` where procfs exists and from a `kill(pid, 0)` probe otherwise, so
macOS is not left with a 10s stall per kill.

Measured on `m6_repair` under 2x CPU oversubscription: 1 failure in 4 before, 0 in 6
after.

## 6. The suite (`DFAULT-5`)

`crates/porch/tests/m25_daemon_fault.rs`. Every assertion is on a named condition
value; none is on the absence of output within a window.

Instruments, one per condition, none of them a switch compiled into the product:

- **ready** — a live daemon.
- **unreachable** — `kill_group`, then the pid file removed.
- **refusing** — the `PORCH_TEST_FAIL_RECOVER_STALE` hook that already existed for
  `m18`. Nothing new was added to the product to reach this state.
- **not-answering** — `SIGSTOP`. The honest wedge: the process stays alive, so the
  socket and its backlog stay in the kernel and the client's `connect` and `write`
  still succeed. A `SIGKILL` would produce `unreachable` instead.

The wedge is held by a `WedgedDaemon` guard whose `Drop` continues the process
before reaping it, so a failed assertion cannot leave a stopped daemon holding the
state root — and `Drop` runs on the unwind, which a teardown line at the end of the
test body would not.

Determinism comes from files rather than timers. The refusal record is the marker
for "the daemon has refused"; `/proc` state `T` is the marker for "the wedge is in
place"; the loser's `daemon already running` line in `daemon.log` is the marker for
the lock refusal. `waited_ms` is asserted to equal the configured deadline rather
than a measured elapsed time, which ties the assertion to the contract instead of
the clock.

Two things the suite covers that are not about conditions at all. `porch eject`
detaching through the binary with no daemon answering, in all three fault states,
and `porch agent sync` answering while the daemon is stopped: `ESCAPE` shipped both
properties and proved them only with `porch-gate` tests, never end-to-end. That is
`GOAL-4`'s inspect-and-recover clause, and it was the one assertion in this area
that could not rot.

Dead-daemon coverage is deliberately thin, and that is a scoping decision rather
than an omission. `crates/porch/tests/` already holds 219 `kill_daemon(` call sites
across 19 files and roughly 14 named tests that kill and then assert. What none of
them assert is what an operator's commands do once the daemon has died.

## Surface drift

ROAD-11's declared surface is `crates/porch/tests/`. This wave also changed
`crates/porch-gate/src/{rpc,daemon,condition,home,proc,service}.rs` and
`crates/porch/src/{doctor,main}.rs`.

That is recorded on the roadmap rather than absorbed, in the manner ROAD-9's drift
was recorded. The reason is the one this document opens with: the suite was not
writable without it, and the alternative was a test file that asserted a hang.

## What MILE-4 still needs

Unchanged by this wave: the `eject --purge` refusal and explicit-abandon policy
(owner blocker, unratified), and the inspect surface — ROAD-8's `indeterminate`
verdict projected somewhere an operator sees it, a reader for
`refs/porch/recover/*`, and read-only database opens for inspection paths. The
milestone stays `Planned`.
