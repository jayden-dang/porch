# Requirements — bounded daemon RPC and the daemon-fault suite

**Feature code:** DFAULT
**Roadmap item:** ROAD-11 (MILE-4)
**Goals:** GOAL-4
**Status:** Implemented

## Context

ROAD-11 is named "wedged, dead, and refusing-startup daemon suite" and its declared
surface is `crates/porch/tests/`. Framing it found that the suite as scoped could not be
written, for a reason that measurement confirmed rather than merely suggested.

An operator whose daemon is wedged cannot use porch to find that out. Against a daemon
stopped with `SIGSTOP`, `porch status`, `porch doctor`, `porch daemon status`, and
`porch runs` each **produce no output and never return** — measured at 12 seconds and
still running, on a repo created by `porch init`. The diagnostic commands are the ones
that hang. Only `porch eject` and the two database-only `porch agent` commands survive,
`eject` returning in 8ms.

The mechanism is that `rpc_call` sets no read deadline
(`crates/porch-gate/src/rpc.rs`) — there is no `set_read_timeout`, `set_write_timeout`,
or `connect_timeout` anywhere in the workspace. A daemon that is stopped after
`UnixListener::bind` leaves a bound socket with a listen backlog and no acceptor, so a
client's `connect` and `write` both succeed and its `read_line` blocks forever.
`wait_for_health(home, timeout)` therefore cannot honour its own `timeout` argument: it
reaches its elapsed check only if `health_check` returns.

Two consequences shape this wave.

- A fault suite written against this behaviour could only assert "this did not finish in
  N seconds", which is the timer race `FAULT-1.5` forbids, and it would freeze a hang as
  correct — the trap `ESCAPE`'s tasks file warned about for this very item.
- *Dead* and *refusing startup* are externally identical. A refusal returns from
  `run_daemon` before the socket is bound and before the pid file is written, so it
  leaves the same absence a dead daemon leaves. Its cause reaches only
  `$PORCH_HOME/logs/daemon.log`, which the next spawn **truncates** (`File::create`,
  `crates/porch-gate/src/proc.rs`). An operator who retries three times destroys the only
  record of why, three times.

So the artefact ROAD-11 was missing is not a test file. It is a bounded RPC and a named
set of daemon conditions to assert against — the same thing ROAD-8's `Evidence`
enumeration was to ROAD-9, which `crates/porch-gate/src/rounds/reconcile.rs` says in as
many words was created "so that ROAD-9's fault injection has an enumeration to assert
against". This wave builds that, then builds the suite on top of it.

**Surface drift.** ROAD-11's declared surface is `crates/porch/tests/`. This wave reaches
into `crates/porch-gate/src/{rpc,daemon,home,service}.rs` and `crates/porch/src/`, for
the reason above: the suite is unwritable without it. Recorded on the roadmap in the
manner ROAD-9's drift was recorded, rather than absorbed silently.

## 1. No operator command waits forever on the daemon

**Story:** As an operator whose gate has stopped responding, I want every porch command
to either answer or tell me it cannot, so that I can find out what is wrong instead of
watching a cursor.

- **DFAULT-1.1** THE SYSTEM SHALL apply a read deadline to every request/response RPC
  issued to the daemon, so that no such call can block indefinitely on a peer that
  accepts a connection and never replies.
- **DFAULT-1.2** THE SYSTEM SHALL apply a write deadline to those RPCs as well, so that
  a peer whose receive buffer is full cannot block the client indefinitely.
- **DFAULT-1.3** THE SYSTEM SHALL set the `health` deadline short. `health` is answered
  from a literal without touching the database, so a `health` that does not answer
  quickly is a wedged daemon, not a slow one.
- **DFAULT-1.4** THE SYSTEM SHALL give `start_run` a deadline long enough to cover its
  legitimate blocking. `start_run` joins the executor thread of a superseded run before
  it replies, so it is the one RPC that can be slow while the daemon is healthy.
- **DFAULT-1.5** WHEN an RPC exceeds its deadline THE SYSTEM SHALL report a timeout
  distinguishable from a connection failure, so that a wedged daemon is not reported as
  an absent one.
- **DFAULT-1.6** `wait_for_health(home, timeout)` SHALL return within a bounded multiple
  of `timeout` regardless of daemon state. It currently cannot: its elapsed check is
  reached only when `health_check` returns.
- **DFAULT-1.7** THE SYSTEM SHALL NOT apply a deadline to the event stream that
  `subscribe` establishes after its acknowledgement. The stream is long-lived and is kept
  alive by a server-side newline every 30 seconds; a deadline on the streaming read would
  break the TUI. The acknowledgement itself SHALL be bounded.
- **DFAULT-1.8** THE SYSTEM SHALL allow the deadlines to be overridden by
  `PORCH_RPC_TIMEOUT_MS`, so that an operator on a loaded machine can widen them and a
  test can narrow them without a test-only switch compiled into the product.

## 2. A refused startup says why, and keeps saying why

**Story:** As an operator whose daemon will not start, I want to know what it refused on,
after I have already retried, so that retrying does not cost me the diagnosis.

`run_daemon` refuses before binding when `reconcile_stale` or `recover_stale` fails.

- **DFAULT-2.1** WHEN the daemon refuses to start THE SYSTEM SHALL record the cause
  durably in `$PORCH_HOME`, outside `logs/daemon.log`.
- **DFAULT-2.2** THE SYSTEM SHALL NOT destroy that record when a subsequent start is
  attempted. This is the specific failure being fixed: `spawn_detached_with_env` opens
  `daemon.log` with `File::create`, so each retry truncates the previous cause.
- **DFAULT-2.3** WHEN the daemon binds its socket successfully THE SYSTEM SHALL clear the
  record, so that a stale refusal cannot be reported as a current one.
- **DFAULT-2.4** The record SHALL be readable without the daemon and without opening the
  state database, since neither is available in the state it describes.
- **DFAULT-2.5** THE SYSTEM SHALL record which barrier refused — stale-round
  reconciliation or stale-run recovery — and when.

## 3. Porch names the condition its daemon is in

**Story:** As an operator, I want one word for what is wrong with my daemon, so that the
remedy I am given matches the fault I have.

- **DFAULT-3.1** THE SYSTEM SHALL provide a single daemon condition value with four
  distinguishable variants: ready, unreachable, refusing, and not-answering.
- **DFAULT-3.2** *Ready* SHALL mean `health` answered within its deadline.
- **DFAULT-3.3** *Unreachable* SHALL mean the socket could not be connected — a dead or
  never-started daemon — and SHALL carry the reason.
- **DFAULT-3.4** *Refusing* SHALL mean a durable refusal record from section 2 exists and
  the socket is not answering, and SHALL carry the recorded cause.
- **DFAULT-3.5** *Not-answering* SHALL mean the socket accepted a connection and `health`
  exceeded its deadline — the wedged daemon — and SHALL carry the pid when a pid file is
  readable, since that is what the operator needs to act on it.
- **DFAULT-3.6** THE SYSTEM SHALL resolve the condition without starting a daemon. A
  command that reports a condition SHALL NOT change it.
- **DFAULT-3.7** THE SYSTEM SHALL resolve the condition without opening the state
  database, so that it is answerable in the states it describes.
- **DFAULT-3.8** Each variant SHALL carry an operator remedy naming a command to run.

## 4. The condition reaches the operator

- **DFAULT-4.1** `porch doctor` SHALL report the condition, and SHALL NOT hang in any of
  the four states.
- **DFAULT-4.2** `porch daemon status` SHALL report the condition, and its JSON form
  SHALL include it.
- **DFAULT-4.3** `porch doctor` SHALL NOT claim the daemon is started by a push notify.
  The post-receive hook execs `porch daemon notify-push`, which makes a best-effort
  `start_run` RPC and logs a warning; it does not spawn a daemon. `porch init` does spawn
  one. Recorded as follow-up `F.5` of `ESCAPE` and fixed here because this wave rewrites
  the line.
- **DFAULT-4.4** A not-answering daemon SHALL be reported as a fault, not as health.
  Today `porch daemon status` prints `socket_healthy=false` for it only after it stops
  hanging, and `running=true` because a pid file exists — which is correct and is worth
  keeping alongside the condition rather than replacing.

## 5. The suite

**Story:** As a maintainer, I want each daemon fault to be a test, so that a regression
that reopens one of these hangs fails the build instead of reaching an operator.

- **DFAULT-5.1** THE SUITE SHALL cover all four conditions of section 3, each induced by
  a distinct mechanism: kill for unreachable, the recovery-failure hook for refusing,
  `SIGSTOP` for not-answering, and a live daemon for ready.
- **DFAULT-5.2** THE SUITE SHALL assert the condition value, not the absence of output
  within a window. Asserting a hang is a timer race (`FAULT-1.5`) and freezes a defect.
- **DFAULT-5.3** THE SUITE SHALL assert that the diagnostic commands terminate against a
  not-answering daemon, with a bound derived from the configured deadline rather than a
  sleep.
- **DFAULT-5.4** THE SUITE SHALL assert the daemon-free surfaces end-to-end through the
  `porch` binary in each condition: `porch eject` detaches and reports `Preserved`, and
  `porch agent sync` answers. This is `GOAL-4`'s inspect-and-recover clause, and it is
  the guard `ESCAPE` shipped without — its properties are proved today only by
  `porch-gate` unit tests and one `porch-gate` integration test, never through the binary
  with no daemon running.
- **DFAULT-5.5** THE SUITE SHALL assert that a refusal cause survives a subsequent start
  attempt (`DFAULT-2.2`).
- **DFAULT-5.6** THE SUITE SHALL be hermetic: its own state root, its own git identity,
  signing disabled, its own default branch, and no dependence on ambient git config. The
  `F.8` stall this repo already measured — a fixture going from a stable 220ms to an
  intermittent 30-second stall because ambient `commit.gpgsign` invoked a signing program
  — lands hardest on a suite that deliberately stops processes.
- **DFAULT-5.7** THE SUITE SHALL confirm process death by pid state rather than by
  sleeping, reusing the `/proc` state check `m23` established, and SHALL leave no stopped
  process behind on any exit path.
- **DFAULT-5.8** THE SUITE SHALL NOT duplicate dead-daemon coverage that already exists.
  `crates/porch/tests/` holds 219 `kill_daemon(` call sites across 19 files and roughly
  14 named tests that kill and then assert. What is uncovered is not "the daemon died" but
  "what the operator's commands do once it has".

## 6. One daemon per state root

**Story:** As an operator, I want at most one daemon writing my gate state, so that
recovering from a wedge does not silently give me two.

Found by measurement while proving section 1, not by framing. Bounding the RPC turned
`ensure_daemon`'s infinite hang against a wedged daemon into a reachable spawn, and the
spawn succeeded: a second daemon bound the socket, wrote the pid file, and answered
`health` while the first was still alive and holding the lock. The guard that was
supposed to prevent this has never worked.

`fs4` 0.13's `try_lock_exclusive` returns `Result<bool>` and reports contention as
`Ok(false)`, not as an `Err`. `run_daemon` applied `map_err` and discarded the `bool`, so
every caller was admitted. Nothing warns: `bool` is not `#[must_use]`, so neither
`clippy::pedantic` nor `unused_must_use` sees it. The pre-existing consequence is two
daemons on one state root, each with its own `Db` handle and each running `kick_pending`.

- **DFAULT-6.1** WHEN the daemon lock is already held THE SYSTEM SHALL refuse to start
  and SHALL say the lock is held. Contention reported as a non-error value SHALL be
  treated as contention.
- **DFAULT-6.2** `ensure_daemon` SHALL NOT spawn a replacement daemon when the existing
  one is not answering. It SHALL fail with the condition and its remedy. A wedged daemon
  holds the lock, so a spawn cannot succeed once `DFAULT-6.1` holds; refusing early
  reports the real fault instead of a lock-contention error.
- **DFAULT-6.3** THE SUITE SHALL assert that a second daemon cannot bind while a first
  holds the lock, and that an inspection command does not create one.

## 7. Out of scope

- **DFAULT-7.1** Making the daemon not wedge. `ESCAPE-5` already put this out of scope,
  and the commit-signing wedge (`F.7`) is a named MILE-4 blocker with an owner.
- **DFAULT-7.2** A readiness probe distinct from liveness. Framing looked for a path that
  holds `Db`'s `Mutex<Connection>` across an unbounded operation and did not find one:
  no `conn()` guard in `crates/porch-gate/src` is held across a subprocess call, and
  `busy_timeout` is 5s, so contention yields `SQLITE_BUSY` rather than a hang. A
  `SELECT 1` probe would not have detected either reachable wedge. Recorded as
  `F.3`'s stated mechanism being unconfirmed, not as work deferred.
- **DFAULT-7.3** Stopping the six commands that spawn a daemon before answering
  (`ESCAPE` follow-up `F.4`). `porch doctor` and `porch daemon status` — the two this
  wave makes authoritative — already do not spawn. Changing bare `porch`, `runs`,
  `status`, `attach`, `rerun`, and `agent run` alters what an operator gets from six
  commands and belongs with the inspect surface. `DFAULT-6.2` removes the harm this
  wave exposed without changing what the six commands report.
- **DFAULT-7.4** The inspect surface itself: projecting ROAD-8's `indeterminate` verdict
  somewhere an operator sees it, a reader for `refs/porch/recover/*`, and read-only
  database opens for inspection paths. ROAD-10's second wave.
- **DFAULT-7.5** The `eject --purge` refusal and explicit-abandon policy. MILE-4's first
  blocker, owned by Jayden, still awaiting ratification.
- **DFAULT-7.6** The `F.8` git-config isolation sweep across the remaining test files.
  This wave is hermetic per `DFAULT-5.6`; it does not fix its neighbours.
- **DFAULT-7.7** Whether `porch daemon stop --force` should stop removing the lock file.
  It does (`service.rs`), which with `DFAULT-6.1` fixed is now the remaining way to get
  two daemons: unlinking the lock lets the next start create a fresh inode and lock it
  while a survivor of the `SIGTERM` still holds the old one. Reachable only when the
  `SIGTERM` does not land, and fixing it means deciding what `--force` promises. Recorded
  as a follow-up rather than changed here.

## 8. What this wave did not decide

- **DFAULT-8.1** MILE-4 stays `Planned`. Its first blocker is unratified and its inspect
  half is unbuilt. This wave closes ROAD-11 and removes the third blocker's RPC
  prerequisite; it does not close the milestone.
- **DFAULT-8.2** Whether porch should report a fifth condition, live-but-not-ready, is
  left open. Nothing reachable today produces it (`DFAULT-7.2`), and inventing a variant
  no fault can induce would give the suite an assertion it cannot write.
- **DFAULT-8.3** The default deadline values are a judgement, not a measurement. They are
  overridable (`DFAULT-1.8`) precisely because a loaded machine may need different ones.
- **DFAULT-8.4** `ESCAPE` follow-up `F.3` is retired as recorded rather than fixed as
  recorded: its stated symptom — `porch status` printing `daemon_healthy=true` while
  wedged — does not occur inside a git work tree, which is where the operator is. Inside
  one, `porch status` reads health, then calls `list_runs`, and both reads precede any
  output, so it prints nothing. The recorded symptom describes only the out-of-work-tree
  path. The underlying defect was worse than the note.
