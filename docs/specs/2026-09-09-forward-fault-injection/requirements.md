# Requirements — fault injection across the forward boundary

**Feature code:** FAULT
**Roadmap item:** ROAD-9 (MILE-3)
**Goals:** GOAL-1
**Status:** Draft

## Context

ROAD-7 made the forward boundary leave a durable **Forward record**. ROAD-8 made a
restart read it and state a **Forward verdict**. Both were proved by seeding rows into
`forward_records` and calling `classify` directly
(`crates/porch-gate/tests/m18_rounds.rs`). No test has ever killed a gate at the forward
boundary, so the step from *the gate really died here* to *the record says this* is
asserted in a design table and nowhere in the code.

GOAL-1 states its own checkability in this item's vocabulary: *"Checkable by
fault-injection tests asserting no unauthorized forward, no duplicate or unsafe retry,
no approval outliving its evidence, and discovery of a push that completed before the
crash"* (`docs/product/vision.md:36`-`:38`). ROAD-8's `Evidence` enumeration was named
"so that ROAD-9's fault injection has an enumeration to assert against: a real kill must
produce one of these and no other" (`crates/porch-gate/src/rounds/reconcile.rs:70`-`:72`).
This feature discharges that promise for three of GOAL-1's four clauses. The fourth —
*no approval outliving its evidence* — is the open MILE-3 blocker and is out of scope;
`§6` says what this build does about it instead.

Framing this feature found a defect in ROAD-8 that no kill-point suite would have found,
because it is a defect in what reconciliation *writes* rather than in what it concludes.
`§5` owns it. Fixing it is a production change outside this item's declared surface
(`crates/porch/tests/`), recorded here rather than absorbed silently.

## 1. A real kill produces the states the classifier was tested against

**Story:** As a maintainer, I want the durable state after a genuine mid-forward death to
be the state ROAD-8's classifier was tested on, so that the verdict an operator reads
rests on a mapping someone checked rather than on a design table.

- **FAULT-1.1** THE SYSTEM SHALL be tested by killing a running gate at the forward
  boundary and asserting the `forward_records` rows and the persisted
  `forward_reconciliations` row that the restart produces.
- **FAULT-1.2** Each such test SHALL assert the exact `(verdict, evidence)` pair, not
  merely that some conclusion was reached.
- **FAULT-1.3** THE SYSTEM SHALL be killed by a signal to a process that installs no
  handler for it, so that the death under test is unclean — no unwinding, no
  destructors, no `atexit` — as an operator's `kill` or a lost host would be.
- **FAULT-1.4** THE SYSTEM SHALL NOT be made to kill itself from its own production code
  at a chosen instruction. `§4` records why.
- **FAULT-1.5** THE SYSTEM SHALL reach each tested state deterministically, by waiting
  for a marker the fixture writes *after* the decisive side effect and killing on that
  marker, and SHALL NOT reach it by racing a timer against a subprocess.
- **FAULT-1.6** No test SHALL kill the gate while a real `git push` is in flight,
  because whether `origin` moved would then be genuinely nondeterministic and the test
  would assert a coin flip.

## 2. The verdict is sound about `origin`, checked against `origin` itself

**Story:** As an operator, I want porch's claim about the shared remote checked against
the shared remote, so that a confidently worded wrong answer cannot pass as a right one.

The fixtures' `origin` is a local bare repository, so a test can read ground truth with
no network (`RECON-7.3`).

- **FAULT-2.1** WHERE a test produces a reached-origin verdict THE TEST SHALL assert
  that the fixture `origin` really carries the authorized SHA on that ref.
- **FAULT-2.2** WHERE a test produces any other verdict THE TEST SHALL assert what
  `origin` actually holds, and SHALL assert that the operator-visible message does not
  claim `origin` is unchanged unless the test has established that it is.
- **FAULT-2.3** THE SYSTEM SHALL be shown to write the gate repository's
  remote-tracking ref as part of a forward push through the production path, because
  `RECON-2.2`'s discriminator rests on that and nothing has ever checked it.
- **FAULT-2.4** WHEN a gate is killed after the push landed and before the outcome
  record commits THE SYSTEM SHALL reconcile to reached-origin on the tracking ref, and
  the test SHALL assert `origin` carries the SHA. This is the window ROAD-7 left open
  and ROAD-8's discriminator exists to close.
- **FAULT-2.5** WHEN a gate is killed after the intent record commits and before the
  push moves anything THE SYSTEM SHALL reconcile to undetermined, and the test SHALL
  assert `origin` does not carry the branch and that the message states the remedy
  without asserting that the push failed.
- **FAULT-2.6** WHEN a gate is killed before the intent record commits THE SYSTEM SHALL
  leave no forward record and no conclusion, SHALL reconcile the run to `failed`, and
  the test SHALL assert `origin` is untouched.
- **FAULT-2.7** WHEN `origin` refuses the push THE SYSTEM SHALL record a `push_failed`
  outcome, and the test SHALL assert `origin` is unchanged and that porch's own error
  does not report the remote's state.

## 3. The retry is not a duplicate

**Story:** As an operator re-pushing after an interrupted forward, I want the retry to
adopt what already exists, so that recovering from a crash does not cost me a second
pull request on a shared remote.

- **FAULT-3.1** WHEN a branch reconciled as reached-origin is pushed again THE SYSTEM
  SHALL open no second pull request, asserted on an append-only log of every `gh`
  invocation across both runs.
- **FAULT-3.2** That retry SHALL record its own forward record under a new `deliver`
  attempt, never a second intent under the interrupted one (`FWDAUTH-2.7`).
- **FAULT-3.3** WHERE `origin` already carries the authorized SHA the retry's outcome
  SHALL be `already_current`, and the test SHALL assert `origin` did not move.
- **FAULT-3.4** Two consecutive restarts over one state root SHALL leave exactly one
  conclusion per `deliver` attempt (`RECON-3.7`).

## 4. The instrument is a binary override, not a crash switch

**Story:** As a maintainer, I want the ability to interrupt a forward to live in the test
fixture, so that shipping crash-safety does not mean shipping a way to crash a gate
mid-push.

The existing soft hooks (`PORCH_TEST_FAIL_ROUND_OPEN` and its three siblings, read at
`crates/porch-run/src/lib.rs:76`, `:936`, `:1017`, `:1167`) return `Err`. That is
structurally incapable of exercising this feature: an `Err` out of the phase loop reaches
`fail_run_with_phase` (`crates/porch-run/src/lib.rs:566`-`:569`), which terminalizes the
`deliver` attempt, and a terminalized attempt is by definition not interrupted (`§5`).
Upgrading that idiom to `std::process::exit` would put a crash-on-demand primitive into
the fail-closed forward path of a published binary, on the one seam MILE-3 exists to
protect. Both are refused.

- **FAULT-4.1** THE SYSTEM SHALL resolve the `git` binary it invokes through an
  environment override, defaulting to `git` on `PATH`, in the same shape as the
  `PORCH_GH_BIN`, `PORCH_REVIEW_BIN`, and `PORCH_FIXER_BIN` overrides porch already
  ships.
- **FAULT-4.2** That override SHALL be a binary path indirection only: no control flow,
  no conditional compilation, and no behaviour that differs between a debug and a
  release build.
- **FAULT-4.3** THE SYSTEM SHALL NOT gate the fault instrument on
  `#[cfg(debug_assertions)]` or on a cargo feature, because `cargo test --release` or a
  feature left off would make the whole suite pass having asserted nothing.
- **FAULT-4.4** The fixture shim SHALL pass every invocation through to the real `git`
  unless the invocation matches the forward push it was told to interrupt, so that the
  gate under test behaves normally everywhere else.
- **FAULT-4.5** The shim SHALL be installed only on the gate that is to be killed, and
  the restarted gate SHALL run the real `git`, so that no test asserts a verdict derived
  through the instrument.
- **FAULT-4.6** `porch doctor` SHALL report the resolved `git` binary, so that an
  override in an operator's environment is visible rather than silent.

## 5. Correction: a verdict is written for every interrupted forward

**Story:** As the next porch process, I want the conclusion actually stored, so that a
restart does not re-derive the same verdict forever and an operator whose run was
terminalized by something other than reconciliation still gets an answer.

`RECON-3.6` requires selecting attempts needing a conclusion from the record's shape
rather than from `runs.status`. `attempts_awaiting_verdict` does exactly that
(`crates/porch-gate/src/rounds/reconcile.rs:190`-`:218`). The *write* does not:
`reconcile_interrupted_with_error` resolves a verdict for every pending attempt and then
filters the results through a `SELECT id, pr_url FROM runs WHERE status = 'running'`
list before appending (`crates/porch-gate/src/rounds/phase.rs:965`-`:1008`), and
`append_verdict_tx` is reached only from `reconcile_one_running`. So the requirement holds
for selection and is defeated for persistence. Three consequences, all verified: the
protocol-upgrade case `RECON-3.6` names is still disarmed; `RECON-1.5`'s promised
conclusion for a `push_failed` record never lands; and because nothing is ever written,
every such attempt is re-selected on every daemon start forever, each costing a
`records_for_attempt` query and — for any attempt with no reached-origin outcome — a
`git rev-parse`, all before the socket binds. `RECON-7.1`'s budget is met per attempt and
blown in aggregate.

Making the write unconditional is not the fix. A run that fails closed on its own path
after a successful push — post-push verification failure, which `RECON`'s out-of-scope
section relies on not being classified — has an intent and a `pushed` record while porch
has *proven* `origin` does not carry the SHA. An unconditional write would give it a
reached-origin verdict and tell the operator the branch is on `origin`.

The discriminator is whether the attempt was interrupted at all, which is durable and
already recorded: a gate that died wrote no `terminal` phase event, whereas every path
that concludes on its own — including `fail_run_with_phase` — writes one
(`crates/porch-run/src/lib.rs:343`-`:358`). The writer-protocol upgrade updates only
`runs` and writes no phase event (`crates/porch-gate/src/db.rs:1056`-`:1066`), so a run
interrupted mid-forward and then terminalized by an upgrade still presents as
interrupted, which is what `RECON-3.6` needs.

- **FAULT-5.1** THE SYSTEM SHALL select a `deliver` attempt for a conclusion only if it
  has a forward intent record, has no conclusion, and has no `terminal` phase event.
- **FAULT-5.2** THE SYSTEM SHALL persist a conclusion for every attempt it so selects,
  regardless of the owning run's `status`.
- **FAULT-5.3** THE SYSTEM SHALL NOT persist a conclusion for a `deliver` attempt that
  reached a terminal phase event, because such an attempt concluded on its own path and
  reported its own error.
- **FAULT-5.4** WHERE the owning run already has a terminal outcome THE SYSTEM SHALL
  append the conclusion record only, and SHALL NOT modify `runs.status`, `runs.error`, or
  any existing phase event (`RECON-4.1`, `RECON-4.2`).
- **FAULT-5.5** WHEN reconciliation completes THE SYSTEM SHALL leave no attempt awaiting
  a conclusion, so that the startup path's work is bounded by the current restart rather
  than by the state root's whole forward history.
- **FAULT-5.6** THE SYSTEM SHALL keep `RECON-3.6`'s guarantee under a writer-protocol
  upgrade, asserted by a test that observes a persisted conclusion for a run whose status
  is not `running` — not merely that the attempt was selected.

## Non-functional

- **FAULT-6.1** Each scenario SHALL use its own state root, and each seam SHALL be its
  own `#[test]`, so that a failure names the seam that produced it.
- **FAULT-6.2** No test SHALL invoke `gh` or any network. `origin` stays a local bare
  path and `gh` stays a fixture fake (`RECON-7.3`, `AGENTS.md` **Commands**).
- **FAULT-6.3** After a restart, assertions SHALL rest on the startup barrier —
  reconciliation completes before `UnixListener::bind`
  (`crates/porch-gate/src/daemon.rs:47`-`:62`) — rather than on a sleep or a poll.
- **FAULT-6.4** A restart SHALL be confirmed by observing that the daemon pid changed,
  so that a health response from a daemon that never died cannot be mistaken for a
  successful restart.
- **FAULT-6.5** Every fixture shim's blocking SHALL be bounded, so that a leaked shim
  cannot wedge the suite.
- **FAULT-6.6** Fixtures SHALL state their own git identity, signing, and default
  branch, and SHALL NOT inherit them from ambient configuration.

## Guards — what must keep working

- **FAULT-7.1** `forward_records` — its DDL, its `CHECK` constraints, and its partial
  unique index — SHALL be unchanged (`RECON-8.4`).
- **FAULT-7.2** `authorized_forward_sha`, `assert_head_continuity`, and certify's
  correction commit SHALL be unchanged; they belong to the open blocker
  (`RECON-8.5`).
- **FAULT-7.3** `PROTOCOL_SCHEMA_VERSION` SHALL NOT be raised (`RECON-6.1`).
- **FAULT-7.4** `RECON-8.1` and `RECON-8.2` SHALL still hold: a run with no forward
  record reconciles to `failed`, and a run with a stored `pr_url` reconciles to
  `ci_monitor_interrupted`.
- **FAULT-7.5** The four existing soft fault hooks SHALL be unchanged; this feature adds
  no `PORCH_TEST_*` variable.

## 6. What this build does about the open blocker

**Story:** As the owner of the MILE-3 blocker, I want its cost executable rather than
argued, so that deciding it is informed by a test I can read.

The live tolerance forwards a descendant of the approved SHA
(`crates/porch-run/src/lib.rs:1897`-`:1902`), so certify's own correction commit is
forwarded without re-review and an approval covers a tree that was never reviewed.
`FWDAUTH-1.4` is recorded Blocked for exactly this reason.

- **FAULT-8.1** THE SYSTEM SHALL be covered by one test pinning today's behaviour: after
  a correction commit, the SHA carried to the forward boundary is not the reviewed SHA.
- **FAULT-8.2** That test SHALL NOT assert that the behaviour is correct, and SHALL say
  in its own documentation that it changes when the blocker is decided. It exists so the
  gap is visible in the suite rather than only in prose.
- **FAULT-8.3** THE SYSTEM SHALL NOT be given a test asserting that an approval survives
  a HEAD advance, because that would encode an undecided product question as a regression
  guard.

## Out of scope

- Deciding the head-continuity blocker, or touching the head-continuity code
  (`FAULT-7.2`).
- Closing MILE-3. GOAL-1's third checkable clause — *no approval outliving its
  evidence* — is unmet while the blocker is open, so ROAD-9 closes as a member and the
  milestone stays committed. Recorded in `docs/roadmap/INDEX.md` rather than left to a
  reader's inference.
- The intent-to-outcome window on the already-current path. `PushDecision::UpToDate`
  returns without spawning git (`crates/porch-git/src/lib.rs:561`-`:563`), so that window
  is pure Rust and no external instrument reaches it. It is also the least interesting
  window at this boundary, because no external effect occurred. Covered at the store
  level only, and named here so its absence is deliberate.
- A second outcome record for one `deliver` attempt. `append_outcome` does not constrain
  it and `classify` folds outcomes last-write-wins, so a two-outcome set would silently
  reduce to the last one. No live path writes it — `append_intent`'s `IntentExists` gate
  makes attempt re-entry fail closed, and both the repair loop and compose resume take a
  new attempt. Left as a named deferral rather than fixed here, because the fix is a
  partial unique index on `forward_records` and `FAULT-7.1` freezes that table.
- Retention of `forward_records` and of conclusion records (`RECON`'s deferral, still
  open). `FAULT-5.5` bounds the *startup work*, not the table growth.
- Present-tense wording in `operator_note`. It says `origin` "carries" the SHA, which a
  later human force-push makes false. The remedy sentence is safe either way, so this is
  a wording defect and not a safety one; it is recorded rather than fixed, because
  changing operator strings is `RECON-5.3`'s prefix contract and MILE-4's surface.

## Open questions

1. **Should `PORCH_GIT_BIN` be an operator-supported override or a test-only one?**
   `FAULT-4.1` adds it in the shape of three overrides porch already documents for
   operators, and `FAULT-4.6` makes `doctor` report it, which implies operator support.
   Pinning a git version is a plausible operator need on a machine with several. Left as
   an undocumented-in-`usage.md` override in this build — `doctor` reveals it, the
   operator guide does not advertise it — so that supporting it stays a decision rather
   than a side effect. Owner Jayden.
2. **Does the blocker deserve its own roadmap member?** A milestone whose only remaining
   work is a bullet under **Blockers** reads as done to anyone scanning the table.
   Promoting the correction-commit decision to a `ROAD-N` would make MILE-3's remaining
   work visible as a member. Not minted here, because minting roadmap items is
   `/define-project`'s surface and the blocker's owner is the human. Owner Jayden.
