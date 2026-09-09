# Roadmap: Porch

Status: Approved
Date: 2026-09-08

| ID | Milestone | Outcome | Depends-on | Commitment |
|---|---|---|---|---|
| MILE-1 | Inner gate | A developer pushes to `porch` and gets an independently reviewed, certified branch forwarded to `origin` with a PR opened. | none | Committed |
| MILE-2 | Auditable assurance record | An operator can trace any assurance outcome to the evidence behind it. | MILE-1 | Closed |
| MILE-3 | Crash-safe forwarding | A gate that dies mid-forward never leaves an unauthorized or duplicated push. | MILE-2 | Committed |
| MILE-4 | Escape without the daemon | An operator gets their checkout back when porch cannot reconcile itself. | MILE-3 | Planned |
| MILE-5 | One porch binary | One installed command runs assurance with the deterministic floor always on. | MILE-2 | Planned |
| MILE-7 | Dogfood baseline | A reader can look up porch's measured effectiveness on mailgate and klynt. | MILE-2, MILE-5 | Planned |
| MILE-6 | External producers | A team's existing review system can count toward a porch approval, or be told why it cannot. | MILE-2, MILE-5 | Planned |
| MILE-8 | Assurance from CI | The same assurance protocol can be entered from a CI run, not only a local push. | MILE-5 | Planned |

## MILE-1 — Inner gate

**Outcome:** A developer pushes to `porch` and gets an independently reviewed, certified
branch forwarded to `origin` with a PR opened, without `origin` being hijacked or their
CI replaced.
**Goals:** None — this milestone predates the approved goals; it is the base the rest builds on.
**Members:**
- **ROAD-1** gate loop: admit, worktree, rebase, certify, deliver — Surfaces: `crates/porch-gate/src/`, `crates/porch-run/src/`, `crates/porch-deliver/src/`
- **ROAD-2** review adapter and first-party quality engine — Surfaces: `crates/porch-review/src/`, `crates/porch-quality/src/`
- **ROAD-3** operator surface: CLI, doctor, setup, park TUI, headless agent contract — Surfaces: `crates/porch/src/`, `crates/porch-gate/porch-agent.md`
**Depends-on:** none
**Commitment:** Committed 2026-08-30
**Closed:** None
**Deferred:** None
**Blockers:** None

## MILE-2 — Auditable assurance record

**Outcome:** An operator can trace any assurance outcome to its reviewed range, producer
and version, coverage state per changed file, findings, disposition and authority events,
and phase events.
**Goals:** GOAL-2
**Members:**
- **ROAD-22** mandatory deterministic floor composition — compose the existing `porch-quality` engine as a required producer invocation on every assurance run, so the floor is never absent and never substituted — Surfaces: `crates/porch-review/src/engine.rs`, `crates/porch-run/src/lib.rs`
- **ROAD-6** finding contract and audit identity — criterion, evidence, consequence, action, producer provenance, plus `review_round_id`, an immutable `finding_instance_id`, and a cross-round `fingerprint` distinct from that database identity — Surfaces: `crates/porch-review/src/lib.rs`, `crates/porch-run/src/lib.rs`, `crates/porch-gate/src/db.rs`, `crates/porch-quality/src/`
- **ROAD-4** per-finding disposition history that survives a review round — Surfaces: `crates/porch-gate/src/db.rs`, `crates/porch-run/src/lib.rs`
- **ROAD-5** phase start and end events, surfaced rather than only stored — Surfaces: `crates/porch-gate/src/db.rs`, `crates/porch-gate/src/rpc.rs`, `crates/porch/src/main.rs`
**Depends-on:** MILE-1
**Commitment:** Closed 2026-09-08
**Closed:** ROAD-22 (FLOOR), ROAD-6 (ROUND), ROAD-4 (DISPO), ROAD-5 (PHASE). The
GOAL-2 join over producer identity and per-path coverage shipped as feature TRACE,
which deliberately carries no `ROAD-N`.
**Deferred:** Three audit-document slices TRACE placed out of scope stay unscheduled
and hold no `ROAD-N`; they are recorded here so the gap is visible rather than lost:
projecting `round_required_producers` (FLOOR's authorization proof) onto the audit
document, TUI rendering of the producer and coverage slices, and an inventory-digest
or `content_blobs` read path. The milestone outcome — reviewed range, producer and
version, per-path coverage, findings, disposition and authority events, phase events
— is met without them.
**Blockers:** None

## MILE-3 — Crash-safe forwarding

**Outcome:** An operator's branch is never forwarded without durable authorization, and a
gate that died mid-forward discovers what actually happened instead of repeating it.
**Goals:** GOAL-1
**Members:**
- **ROAD-7** persist assurance authorization and reviewed-input binding before any external forward — Surfaces: `crates/porch-run/src/lib.rs`, `crates/porch-run/src/deliver.rs`, `crates/porch-gate/src/db.rs`
- **ROAD-8** restart reconciliation of ambiguous external effects — Surfaces: `crates/porch-gate/src/rounds/reconcile.rs`, `crates/porch-gate/src/rounds/phase.rs`
- **ROAD-9** fault-injection suite across the forward boundary — Surfaces: `crates/porch/tests/m23_forward_fault.rs`, `crates/porch-git/src/lib.rs`, `crates/porch-gate/src/rounds/`
**Depends-on:** MILE-2
**Commitment:** Committed 2026-09-08 — all three members shipped; the milestone stays open because the blocker below gates the reviewed-input binding half of GOAL-1
**Closed:** ROAD-7 (2026-09-08), ROAD-8 (2026-09-09), ROAD-9 (2026-09-09). **The milestone does not close with its members.** GOAL-1 names four checkable properties and ROAD-9's suite discharges three — no unauthorized forward, no duplicate or unsafe retry, and discovery of a push that completed before the crash. The fourth, *no approval outliving its evidence*, is the open blocker below: `crates/porch/tests/m23_forward_fault.rs` pins today's behaviour as a tripwire, and deliberately does not assert it is correct. Two surfaces moved from the ones planned here. ROAD-8's classification is pure over the durable record in `porch-gate`, so `porch-run/src/deliver.rs` and `daemon.rs` were not touched. ROAD-9 reached past `crates/porch/tests/` for two production changes, both recorded in `docs/specs/2026-09-09-forward-fault-injection/requirements.md` rather than absorbed silently: a `PORCH_GIT_BIN` override so a fixture can interpose on `git` without a crash switch in the forward path, and a correction to ROAD-8 — the verdict *write* was still filtered by `runs.status`, so `RECON-3.6` held for selection and was defeated for persistence, and its guard test asserted only selection.
**Deferred:** None
**Blockers:**
- ~~Restart reconciliation behaviour when the branch was pushed but PR creation or local completion persistence did not finish~~ — resolved 2026-09-08 in ROAD-7 discovery: the forward boundary persists an intent record before the push and an outcome record after it, so a restart reads durable local state instead of inferring from `pr_url` alone and can distinguish an authorized, completed push from one never attempted. The residual window between push completion and the outcome write stays owned by ROAD-8, which owns discovery; ROAD-7 does not probe `origin`.
- **Open (reopened 2026-09-08).** Whether an approval may remain valid after HEAD advances past the reviewed SHA — owner Jayden. The intended answer was "it may not; binding is exact", and ROAD-7 built the forward to carry exactly the SHA continuity authorized. Implementing the equality rule then found that certify's own correction commit advances HEAD after approval without revoking it (`crates/porch-run/src/certify.rs:71`, `:81`), so equality fail-closes every run whose formatter rewrites the tree and breaks the dogfood consumers. The real question underneath is narrower than first framed: **may porch's own certify correction commit be forwarded without re-review?** Candidates and their trade-offs are in the Open Questions of `docs/specs/2026-09-08-forward-authorization/requirements.md`. Resolve through feature discovery/design, not here. ROAD-9 made the cost executable: `tripwire_a_correction_commit_forwards_an_unreviewed_sha` shows `origin` carrying porch's own `apply format` commit, which descends from the reviewed SHA and which no reviewer saw. Since this is now the only work MILE-3 is waiting on, and a bullet under **Blockers** reads as done to anyone scanning the members table, it is worth promoting to its own member when it is picked up.

## MILE-4 — Escape without the daemon

**Outcome:** When automatic reconciliation cannot complete, an operator can inspect state,
recover every reachable porch-authored commit, and detach porch from the checkout — with
no healthy daemon and no hand-editing of hooks, git config, refs, or the database.
**Goals:** GOAL-4
**Members:**
- **ROAD-10** daemon-independent inspect → recover or abandon → detach, with distinct operator-facing states — Surfaces: `crates/porch-gate/src/custody.rs`, `crates/porch-gate/src/eject.rs`, `crates/porch-gate/src/db.rs`, `crates/porch-gate/src/daemon.rs`, `crates/porch-run/src/sync.rs`, `crates/porch/src/doctor.rs`. First wave shipped as `ESCAPE` (`docs/specs/2026-09-09-daemon-free-escape/`): custody of porch-authored commits, and detach that does not depend on the state it escapes. The inspect half is open — see the third blocker.
- ~~**ROAD-11** wedged, dead, and refusing-startup daemon suite~~ — **Closed 2026-09-09** as `DFAULT` (`docs/specs/2026-09-09-daemon-fault-suite/`). Declared surface was `crates/porch/tests/`; it also changed `crates/porch-gate/src/{rpc,daemon,condition,home,proc,service}.rs` and `crates/porch/src/{doctor,main}.rs`. **Drift recorded, not absorbed**, for the reason framing found and measurement confirmed: the suite was not writable first. `rpc_call` had no read deadline, so against a daemon stopped after `bind` — which leaves a bound socket with a listen backlog and no acceptor — `porch status`, `porch doctor`, `porch daemon status`, and `porch runs` each produced no output and had not returned at twelve seconds. The only assertion available was "this did not finish in N seconds", the timer race `FAULT-1.5` forbids, and it would have frozen the hang as correct. So the wave bounded every RPC, named four daemon conditions to assert against, made a refused startup durable across retries, and then wrote the suite.
**Depends-on:** MILE-3
**Commitment:** Planned — the first blocker was due before approval and is still open, so the milestone stays `Planned` even though both members have shipped work. `ESCAPE` and `DFAULT` were each scoped to what needs no policy decision.
**Closed:** ROAD-11
**Deferred:** None
**Blockers:**
- The refusal and explicit-abandon policy for `eject --purge` — owner Jayden; due before this milestone is approved for implementation; resolved through feature discovery/design, not here. Discovery ran during `ESCAPE` and carried a recommendation forward for ratification (`docs/specs/2026-09-09-daemon-free-escape/requirements.md` §5): refuse when a porch-authored commit is neither reachable from the operator's checkout nor recorded as having reached `origin`; `--abandon` as the single override; a dry-run manifest by default; abandoned SHAs written outside the deleted tree. The runner-up — refuse on any unreachable commit — was rejected because it fires on the benign post-success case and would therefore refuse nearly always, training the override. `ESCAPE` deliberately did not decide this.
- Whether porch's machine-authored commits should carry the operator's signature — owner Jayden; discovered during `ESCAPE`. Certify's correction commits and the deliver-repair commit already neutralize hooks and pin porch's own identity, but inherit `commit.gpgsign`, so on a signing host the daemon invokes the operator's signing program mid-run with no timeout. Unsigning them would break a remote that requires signed commits; leaving them is a wedge path. `DFAULT` reduced the blast radius rather than resolving it: such a wedge is now reported as `not-answering` within the health deadline instead of hanging every diagnostic command, but the run is still stuck. Still owned here.
- The daemon-free inspect surface, and where ROAD-8's `indeterminate` verdict reaches an operator — `ESCAPE` found it reaches one only as prose appended to `runs.error`, and that neither `sync.rs` nor `doctor.rs` knows forward verdicts exist. This is the MILE-3 → MILE-4 handoff; it needs no policy decision and is the next wave rather than a blocker on the owner. `DFAULT` cleared its prerequisite — an inspect command built on the old unbounded client would have hung in the one situation it exists for — and added the vocabulary such a surface should report. What remains: the verdict projection, a reader for `refs/porch/recover/*` (which `ESCAPE` now pins far more often, and which `agent sync` surfaces only for the run it resolves), and read-only database opens for inspection paths, since `Db::open` runs an IMMEDIATE transaction and can terminalize active runs on a protocol bump.
- Whether `porch daemon stop --force` should stop unlinking `daemon.lock` — discovered during `DFAULT`, which fixed the single-instance guard that had never worked (`fs4` reports a contended lock as `Ok(false)`, and `run_daemon` discarded it, so two daemons could serve one `$PORCH_HOME`; this was observed, not inferred). With the guard fixed, unlinking the lock is the remaining route to two daemons: it lets the next start create a fresh inode and lock it while a survivor of the `SIGTERM` still holds the old one. Reachable only when the `SIGTERM` does not land, and fixing it means deciding what `--force` promises. Small, but it is a correctness question rather than a coding-session choice.

## MILE-5 — One porch binary

**Outcome:** `cargo install porch` gives an operator a single command surface on which the
deterministic floor runs every time, and an existing `porch-quality` setup keeps working
through a stated deprecation window.
**Goals:** None — enabling work for MILE-6, MILE-7, and MILE-8; no approved goal depends on it alone.
**Members:**
- **ROAD-12** consolidate the quality engine into the porch binary as the always-on floor — Surfaces: `crates/porch/src/bin/porch_quality.rs`, `crates/porch-quality/src/`, `crates/porch-review/src/engine.rs`
- **ROAD-13** compatibility shim and deprecation path for the separate executable — Surfaces: `crates/porch/src/`, `docs/install.md`
- **ROAD-14** native fallback policy — Surfaces: `crates/porch-review/src/home_config.rs`, `crates/porch-review/src/setup.rs`
**Depends-on:** MILE-2
**Commitment:** Planned
**Closed:** None
**Deferred:** None
**Blockers:**
- The exact one-binary command surface and its compatibility period — owner Jayden; due before this milestone is approved for implementation; resolved through feature discovery/design, not here.
- Whether native fallback runs automatically or only when explicitly configured — owner Jayden; same due point and route.

## MILE-7 — Dogfood baseline

**Outcome:** A reader can look up how porch actually performed on mailgate and klynt,
against a versioned contract that states what each metric means and which ones were
unavailable.
**Goals:** GOAL-3
**Members:**
- **ROAD-18** versioned baseline contract: metric definitions, denominators, observation windows, exclusions, adjudication rules, `unavailable(reason)` — Surfaces: None — the artifact's home is decided when this is specified
- **ROAD-19** escaped-defect adjudication correlating porch results with downstream CI and human review — Surfaces: None — depends on consumer trees outside this repository
- **ROAD-20** baseline run and published results for mailgate and klynt — Surfaces: None — produced against consumer trees, not this one
**Depends-on:** MILE-2, MILE-5
**Commitment:** Planned
**Closed:** None
**Deferred:** None
**Blockers:**
- Numeric effectiveness targets, and the versioned definitions, denominators, observation windows, exclusions, and adjudication rules the baseline uses — owner Jayden; targets remain unavailable until the baseline exists; resolved through feature discovery/design, not here.

## MILE-6 — External producers

**Outcome:** A team already running a review system it trusts can have that system's
findings count toward a porch approval when they meet the bar, and get a fail-closed
`incomplete` with a stated shortfall when they do not.
**Goals:** None — serves the vision's producer scope; the approved goals are producer-independent.
**Members:**
- **ROAD-15** machine-checkable producer bar: identity, schema version, input range, coverage states, evidence, waivers — Surfaces: `crates/porch-review/src/lib.rs`, `crates/porch-review/src/engine.rs`
- **ROAD-16** vendor-neutral producer transport — Surfaces: `crates/porch-review/src/`
- **ROAD-17** `incomplete` outcome and migration for existing `generic` / `ocr` configurations — Surfaces: `crates/porch-review/src/`, `crates/porch-run/src/lib.rs`
**Depends-on:** MILE-2, MILE-5
**Commitment:** Planned
**Closed:** None
**Deferred:** None
**Blockers:**
- The machine-checkable minimum bar an external judgment layer must meet — owner Jayden; due before this milestone is approved for implementation; resolved through feature discovery/design, not here.
- The transport choice — constrained SARIF versus another structured-command profile — owner Jayden; same due point and route.
- Whether SARIF import belongs to this milestone or a later integration one — owner Jayden; same due point and route.

## MILE-8 — Assurance from CI

**Outcome:** A consumer can enter the same assurance protocol from a CI run rather than a
local push, without porch orchestrating or replacing that CI.
**Goals:** None — an execution mode for the protocol the approved goals already cover.
**Members:**
- **ROAD-21** CI entry point into the assurance protocol — Surfaces: None — the entry-point shape is not yet decided
**Depends-on:** MILE-5
**Commitment:** Planned
**Closed:** None
**Deferred:** None
**Blockers:**
- Whether CI mode is a supported peer entry point or a fallback only — owner Jayden; due before this milestone is approved for implementation; resolved through feature discovery/design, not here.

## Goal dispositions

Every live `GOAL-N` in `docs/product/vision.md` that no milestone cites belongs here, so
that a goal is never silently dropped (S6).

None — `GOAL-1` (MILE-3), `GOAL-2` (MILE-2), `GOAL-3` (MILE-7), and `GOAL-4` (MILE-4) are
each cited by a milestone.

| Goal | Disposition | Date | Reason |
|---|---|---|---|
