# Roadmap: Porch

Status: Approved
Date: 2026-09-10

| ID | Milestone | Outcome | Depends-on | Commitment |
|---|---|---|---|---|
| MILE-1 | Inner gate | A developer pushes to `porch` and gets an independently reviewed, certified branch forwarded to `origin` with a PR opened. | none | Committed |
| MILE-2 | Auditable assurance record | An operator can trace any assurance outcome to the evidence behind it. | MILE-1 | Closed |
| MILE-3 | Crash-safe forwarding | A gate that dies mid-forward never leaves an unauthorized or duplicated push. | MILE-2 | Committed |
| MILE-4 | Escape without the daemon | An operator gets their checkout back when porch cannot reconcile itself. | MILE-3 | Committed |
| MILE-5 | One porch binary | One installed command runs assurance with the deterministic floor always on. | MILE-2 | Committed |
| MILE-7 | Dogfood baseline | A reader can look up porch's measured effectiveness on mailgate and klynt. | MILE-2, MILE-5 | Committed |
| MILE-6 | External producers | A team's existing review system can count toward a porch approval, or be told why it cannot. | MILE-2, MILE-5 | Committed |
| MILE-8 | Assurance from CI | The same assurance protocol can be entered from a CI run, not only a local push. | MILE-5 | Committed |

Every owner blocker recorded on this roadmap was ruled on 2026-09-10 and carries an
**ADR** under [`../adr/`](../adr/). No milestone is now waiting on a decision; each
waits only on work. The rulings changed no invariant — all eight land inside
**ARCH-1 … ARCH-13** as they stand.

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
- **ROAD-23** reviewed normalization and binding by equality — move the mutating `commands.format` into the rebase phase so its output is inside the reviewed range, make certify verify-only after approval, and delete the descendant tolerance at the forward boundary — Surfaces: `crates/porch-run/src/certify.rs`, `crates/porch-run/src/lib.rs`, `crates/porch/tests/m23_forward_fault.rs`, `docs/usage.md`, `CONTEXT.md`. Added 2026-09-10 when the blocker below was ruled on; the blocker itself had recorded that it deserved promotion to a member once picked up.
**Depends-on:** MILE-2
**Commitment:** Committed 2026-09-08 — ROAD-7/8/9 shipped; the milestone now waits on ROAD-23, which is work rather than a decision
**Closed:** ROAD-7 (2026-09-08), ROAD-8 (2026-09-09), ROAD-9 (2026-09-09). **The milestone does not close with its members.** GOAL-1 names four checkable properties and ROAD-9's suite discharges three — no unauthorized forward, no duplicate or unsafe retry, and discovery of a push that completed before the crash. The fourth, *no approval outliving its evidence*, is the open blocker below: `crates/porch/tests/m23_forward_fault.rs` pins today's behaviour as a tripwire, and deliberately does not assert it is correct. Two surfaces moved from the ones planned here. ROAD-8's classification is pure over the durable record in `porch-gate`, so `porch-run/src/deliver.rs` and `daemon.rs` were not touched. ROAD-9 reached past `crates/porch/tests/` for two production changes, both recorded in `docs/specs/2026-09-09-forward-fault-injection/requirements.md` rather than absorbed silently: a `PORCH_GIT_BIN` override so a fixture can interpose on `git` without a crash switch in the forward path, and a correction to ROAD-8 — the verdict *write* was still filtered by `runs.status`, so `RECON-3.6` held for selection and was defeated for persistence, and its guard test asserted only selection.
**Deferred:** None
**Blockers:**
- ~~Restart reconciliation behaviour when the branch was pushed but PR creation or local completion persistence did not finish~~ — resolved 2026-09-08 in ROAD-7 discovery: the forward boundary persists an intent record before the push and an outcome record after it, so a restart reads durable local state instead of inferring from `pr_url` alone and can distinguish an authorized, completed push from one never attempted. The residual window between push completion and the outcome write stays owned by ROAD-8, which owns discovery; ROAD-7 does not probe `origin`.
- ~~Whether an approval may remain valid after HEAD advances past the reviewed SHA~~ — **resolved 2026-09-10 in [ADR-0003](../adr/0003-reviewed-normalization-and-equality-binding.md)**, and promoted to **ROAD-23**. The sharpened question was *may porch's own certify correction commit be forwarded without re-review?*, and the answer is that it is neither forwarded unreviewed nor forbidden: the mutating step moves before review, so the correction is inside the reviewed range, and certify becomes verify-only after approval so nothing moves HEAD afterwards. Binding is then exact, as ROAD-7 originally built it. Re-review, exemption, and bounded-provenance binding were each costed and rejected — the reasons are in the ADR. Two things ruling this surfaced that whoever takes ROAD-23 must carry. Head drift already breaks MILE-2's `Closed` read path today: the applicable-round lookup keys on `to_sha == runs.head_sha`, so a reviewed, approved run reports `assurance_record: not_reviewed` and raises the `inconsistent_review_state` anomaly. And the refusal reason is discarded on the way out, so an operator reads "not reviewed" where porch means "the applicable round does not cover the live HEAD"; surfacing that reason is worth doing regardless of ROAD-23. The dirty-tree rate on mailgate and klynt must be measured before the wave lands — the loop needs only git and a shell, and on porch's own last 80 commits it was zero.

## MILE-4 — Escape without the daemon

**Outcome:** When automatic reconciliation cannot complete, an operator can inspect state,
recover every reachable porch-authored commit, and detach porch from the checkout — with
no healthy daemon and no hand-editing of hooks, git config, refs, or the database.
**Goals:** GOAL-4
**Members:**
- **ROAD-10** daemon-independent inspect → recover or abandon → detach, with distinct operator-facing states — Surfaces: `crates/porch-gate/src/custody.rs`, `crates/porch-gate/src/eject.rs`, `crates/porch-gate/src/db.rs`, `crates/porch-gate/src/daemon.rs`, `crates/porch-run/src/sync.rs`, `crates/porch/src/doctor.rs`. First wave shipped as `ESCAPE` (`docs/specs/2026-09-09-daemon-free-escape/`): custody of porch-authored commits, and detach that does not depend on the state it escapes. The inspect half is open — see the third blocker.
- ~~**ROAD-11** wedged, dead, and refusing-startup daemon suite~~ — **Closed 2026-09-09** as `DFAULT` (`docs/specs/2026-09-09-daemon-fault-suite/`). Declared surface was `crates/porch/tests/`; it also changed `crates/porch-gate/src/{rpc,daemon,condition,home,proc,service}.rs` and `crates/porch/src/{doctor,main}.rs`. **Drift recorded, not absorbed**, for the reason framing found and measurement confirmed: the suite was not writable first. `rpc_call` had no read deadline, so against a daemon stopped after `bind` — which leaves a bound socket with a listen backlog and no acceptor — `porch status`, `porch doctor`, `porch daemon status`, and `porch runs` each produced no output and had not returned at twelve seconds. The only assertion available was "this did not finish in N seconds", the timer race `FAULT-1.5` forbids, and it would have frozen the hang as correct. So the wave bounded every RPC, named four daemon conditions to assert against, made a refused startup durable across retries, and then wrote the suite.
- **ROAD-24** purge that refuses over unforwarded custody tips — the refusal predicate, `--abandon`, the manifest, the active-run interlock, and the `Purged`-on-failure repair — Surfaces: `crates/porch-gate/src/eject.rs`, `crates/porch-gate/src/custody.rs`, `crates/porch-gate/src/rounds/reconcile.rs`, `crates/porch/src/main.rs`, `docs/usage.md`. Added 2026-09-10 from [ADR-0005](../adr/0005-purge-refuses-over-unforwarded-custody-tips.md).
**Depends-on:** MILE-3
**Commitment:** Committed 2026-09-10 — both owner blockers were ruled on that day; what remains is work. `ESCAPE` and `DFAULT` were each scoped to what needed no policy decision, and that scoping held.
**Closed:** ROAD-11
**Deferred:** None
**Blockers:**
- ~~The refusal and explicit-abandon policy for `eject --purge`~~ — **resolved 2026-09-10 in [ADR-0005](../adr/0005-purge-refuses-over-unforwarded-custody-tips.md)**, and scheduled as **ROAD-24**. `ESCAPE`'s carried predicate is ratified with four corrections measurement forced. It is expressed over *tips* rather than commits, because the deliver-repair rebase relabels the committer to the operator and a committer filter would therefore under-count porch's own commits exactly after a repair. The reached-`origin` leg falls back to the bare's own tracking ref, because ROAD-8's classifier returns `NotAttempted` for a run with no intent record *before* its tracking-ref upgrade can run — so reading `forward_records` alone would refuse on the longest-serving state roots, which is the behaviour the runner-up was rejected for. The manifest is the refusal path's output rather than a `--dry-run`, because `Db::open` takes an IMMEDIATE transaction and can terminalize active runs. And purge refuses on active runs the way `stop_daemon` does, because `delete_repo` ignores status and the live run thread then falls through to an unpinned worktree removal.
- ~~Whether porch's machine-authored commits should carry the operator's signature~~ — **resolved 2026-09-10 in [ADR-0004](../adr/0004-porch-never-invokes-the-operators-signing-key.md)**: porch never invokes the operator's signing key, at any of the five sites. The premise recorded here was inverted — GitHub confirms a signature with `verified_signature?` rather than accepting its presence, and porch pins the committer to `porch@example.com`, an RFC 2606 reserved domain that can never be verified, so porch's commits are already rejected by a signing ruleset exactly as unsigned ones are. Two things ruling this found. The enumeration above was incomplete: `git rebase` inherits `commit.gpgsign` too, at three sites *earlier* in the phase order than certify, so the wedge fires before the commits this blocker was written about. And the mitigation credited to `DFAULT` does not reach the ordinary case — runs execute on a per-run thread while `health` answers from a literal, so a wedged run leaves the daemon reporting `ready`, not `not-answering`. The unbounded git child is repaired as a defect, unconditionally. Carried forward, owned by nobody yet: `porch@example.com` ships in consumers' permanent history and replacing it is a durable-history decision.
- ~~The daemon-free inspect surface, and where ROAD-8's `indeterminate` verdict reaches an operator~~ — **not an owner blocker; moved to ROAD-10's second wave 2026-09-10.** It always said so in its own text, and leaving it under this heading now reads as though the milestone were still waiting on a decision. `ESCAPE` found the verdict reaches an operator only as prose appended to `runs.error`, and that neither `sync.rs` nor `doctor.rs` knows forward verdicts exist. `DFAULT` cleared its prerequisite and added the vocabulary such a surface should report. What remains: the verdict projection, a reader for `refs/porch/recover/*` (which `ESCAPE` now pins far more often, and which `agent sync` surfaces only for the run it resolves), and read-only database opens for inspection paths, since `Db::open` runs an IMMEDIATE transaction and can terminalize active runs on a protocol bump. That read-only open is now also a prerequisite for ROAD-24's manifest, and the projection is what the held `SIGKILL` sliver waits behind.
- ~~Whether `porch daemon stop --force` should stop unlinking `daemon.lock`~~ — **reclassified 2026-09-10 as defects, not a policy question.** The framing was wrong in two ways, both reproduced with the shipped binary. The unlink is not force-specific: it lives in `stop_process`, which plain `stop` and `uninstall_service` also call. And two daemons do not need a `SIGTERM` to go missing — `kill_group` calls only `killpg`, so a daemon that is not its own process-group leader is never signalled at all, `stop --force` prints `daemon stopped` over it after the full wait, deletes the lock, and the next start binds the same `$PORCH_HOME` and answers `ready`. Repair, with no owner input needed: signal the process as well as the group but validate the pid first, since a bare `kill` on a recycled pid is more dangerous than today's missed `killpg`; confirm exit; delete the socket and pid only once the process is confirmed gone; never delete `daemon.lock`, which nothing reads and whose deletion is the only route past the fixed guard — a surviving lock *file* can never block a start, because `flock` is released when the descriptor closes, including on `SIGKILL` and host crash; and stop printing success unconditionally. One sliver stays a decision and is deliberately held: whether `--force` may escalate to `SIGKILL`. It widens a window that `SIGTERM`, reboots and OOM kills already open, but it manufactures the `indeterminate` forward verdict that the next bullet says has no operator-facing surface yet — so it waits behind that surface rather than shipping ahead of it.

## MILE-5 — One porch binary

**Outcome:** `cargo install porch` gives an operator a single command surface on which the
deterministic floor runs every time, and an existing `porch-quality` setup keeps working
through a stated deprecation window.
**Goals:** None — enabling work for MILE-6, MILE-7, and MILE-8; no approved goal depends on it alone.
**Members:**
- **ROAD-12** consolidate the quality engine into the porch binary as the always-on floor — Surfaces: `crates/porch/src/bin/porch_quality.rs`, `crates/porch-quality/src/`, `crates/porch-review/src/engine.rs`. First wave shipped as `ONEBIN` (`docs/specs/2026-09-09-one-binary-install-coherence/`) and **touched none of those three surfaces**. **Drift recorded, not absorbed.** The consolidation the entry describes already happened: `20e3060` copied `crates/porch-quality/src/main.rs` into `crates/porch/src/bin/porch_quality.rs` on 2026-08-30, four and a half hours before this entry naming it as a surface was written, and `cargo install porch` has shipped the floor sibling since. It was a copy, not a consolidation — the original stayed published and installable, so two packages emit one file name and the rest is held by the first blocker below. What was reachable without that decision is that **the two installed files move independently while every surface assumed they move together**: the v0.2.1-documented upgrade now installs nothing, a daemon outliving an install adopts the new floor under old code, `setup --engine quality` refused on a correct installation, `doctor` blessed any executable of the right name, and the release smoke gate asserted a flag that never existed. `ONEBIN` closed those in `crates/porch-review/src/{floor,setup,lib}.rs`, `crates/porch/src/doctor.rs`, and the docs.
- **ROAD-13** retire the standalone floor executable and sequence the registry — drop the `porch-quality` package's `[[bin]]`, keep its library published, relocate the CLI smoke tests onto the surviving binary, then publish library-only *before* yanking the standalone — Surfaces: `crates/porch-quality/`, `crates/porch/tests/m26_one_binary.rs`, `docs/install.md`, `docs/agents/project.md`. Retitled 2026-09-10: [ADR-0006](../adr/0006-one-package-owns-the-floor-binary-name.md) rules out a shim, because a shim must still occupy the name `porch-quality` and so removes neither the collision nor the second artifact.
- **ROAD-14** the judgment layer is never dropped implicitly — reorder `default_engine`, warn whenever floor-only is selected without being asked — Surfaces: `crates/porch-review/src/setup.rs`, `crates/porch/tests/m26_one_binary.rs`. Retitled 2026-09-10 from "native fallback policy", which [ADR-0007](../adr/0007-the-judgment-layer-is-never-dropped-implicitly.md) settled; `home_config.rs` is not a surface, since the fix is in selection order rather than configuration.
**Depends-on:** MILE-2
**Commitment:** Committed 2026-09-10 — all three blockers were ruled on that day. `ONEBIN` was scoped to what needed no policy decision, on the `ESCAPE` / `DFAULT` terms, and that scoping held.
**Closed:** None — ROAD-12 shipped its first wave and is still open; the consolidation it names is now a costed choice rather than a blocked one, and [ADR-0006](../adr/0006-one-package-owns-the-floor-binary-name.md) records a re-exec of `porch` itself as the live candidate.
**Deferred:** None
**Blockers:**
- ~~The exact one-binary command surface and its compatibility period~~ — **resolved 2026-09-10 in [ADR-0006](../adr/0006-one-package-owns-the-floor-binary-name.md)**, and scheduled as **ROAD-13**. `ONEBIN`'s carried recommendation is ratified: the `porch-quality` package retires its `[[bin]]` and keeps its library published. The unverified lever was checked, against a local sparse registry — `cargo install --locked` survives a yanked lockfile entry with a warning, while bare `cargo install` fails at resolution with no fallback to an older root. So the sequence is load-bearing and runs the other way round from the obvious one: **publish library-only first, then yank.** In-process is rejected for now on containment alone — the floor is the one gate child with a deadline and a process-group kill, and MILE-4's untimed-child class is still open. The two costs recorded here against in-process are **struck as disproven**: the equivalence digest hashes neither the spawned path nor `selection_source`, so relocating the hashed bytes is a value change of a kind every release already makes; and no ADR-0002-style fence is needed, because that fence exists so a released binary cannot forward with *no* floor, which in-process never reaches. The audit-contract claim below inherited the same error and is corrected there.
- ~~Whether native fallback runs automatically or only when explicitly configured~~ — **resolved 2026-09-10 in [ADR-0007](../adr/0007-the-judgment-layer-is-never-dropped-implicitly.md)**, and scheduled as **ROAD-14**: `default_engine` prefers a judgment engine — Agent, then Quality, then Generic, then Ocr — and floor-only selected without being asked always warns. Quality stays ahead of Generic on purpose, since `Generic` detects any `PATH` binary named `review` and ranking that above the first-party engine would invert ARCH-9. The recorded trap does not bind the fix: the flip lives in the *ordering* inside `default_engine`, which only the default path reaches, so detection is untouched and `ONEBIN`'s reason for fixing the explicit path alone is preserved. Refusing to guess was rejected — `porch setup --yes` is both the documented headless path and the remedy `doctor` prints, so a `--yes` that then demands a flag is self-contradictory.
- ~~Whether porch should hold an expected identity for the floor and refuse a sibling that does not match~~ — **resolved 2026-09-10 in [ADR-0008](../adr/0008-no-expected-floor-identity-but-doctor-reports-incoherence.md)**. `ONEBIN-7.1`'s reading of the invariants is upheld and the refusal is declined: an owner who can overwrite the sibling can overwrite `porch` beside it, so the check would defend against one implausible attacker while firing on every legitimate `--force` upgrade. What is accepted instead is the accident case, which retiring the bin target does not drain — `install.sh` installs the floor only when the release artifact exists, and the dogfood loop copies one file — so `doctor` escalates from reporting the identity to warning when the pair's versions disagree, which after ROAD-13 is a string comparison because both files come from one build. The cost recorded here is corrected: a new `reported_version` shape does **not** blank historic rows, which read cleanly; it blanks *new* rows under a widening that leaves `unavailable` required, a same-version footgun rather than a downgrade. And the block on a floor `--version` flag was misattributed — `ONEBIN-7.2`'s hazard only fires if the version enters the descriptor, so a flag `doctor` reads and the descriptor ignores is inert.

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
**Commitment:** Committed 2026-09-10 — the blocker below turned out to restate settled policy plus ordinary discovery, so nothing was waiting on a decision.
**Closed:** None
**Deferred:** None
**Blockers:**
- ~~Numeric effectiveness targets, and the versioned definitions, denominators, observation windows, exclusions, and adjudication rules the baseline uses~~ — **closed 2026-09-10 by citation; no ADR was needed.** Both halves were already dispositioned, one of them inside this blocker's own sentence. `docs/product/vision.md` states that numeric targets "are deliberately absent: they receive **new** `GOAL-N` IDs once the **GOAL-3** baseline exists, and are never inserted into a goal above" — Approved text, so deciding targets now is not merely premature, it is forbidden. And GOAL-3 already fixes the contract's required contents; what they say is ROAD-18's discovery, which the blocker itself routed there. A residual was proposed and struck: whether the baseline may read consumer CI and PR-review history is not open either, because ROAD-19 is defined as "escaped-defect adjudication correlating porch results with downstream CI and human review". The non-goal is *replacing* a consumer's CI, and ARCH-7 governs deliver rerunning checks; neither constrains a measurement artifact from reading history. This is the second blocker on this roadmap to have outlived its content, so ROAD-18 should re-read it before spending time on it.

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
**Commitment:** Committed 2026-09-10 — all three blockers were downstream of one question none of them asked, and [ADR-0009](../adr/0009-only-the-operator-may-excuse-a-changed-file-from-review.md) answers it.
**Closed:** None
**Deferred:** None
**Blockers:**
- ~~The machine-checkable minimum bar an external judgment layer must meet~~ — **resolved 2026-09-10 in [ADR-0009](../adr/0009-only-the-operator-may-excuse-a-changed-file-from-review.md).** The bar is the one that already ships, plus a required `schema_version` and a required echoed `range` that must equal what porch passed — the one addition that lets porch detect a producer that reviewed a range it was not handed. "Waivers" reads as coverage waivers; finding-level producer waivers are foreclosed by ARCH-11. `ocr` is **not** retired: it is `generic` with a two-word argv prefix supplied by a porch-authored wrapper, so ROAD-17's promised migration stands.
- ~~The transport choice — constrained SARIF versus another structured-command profile~~ — **resolved 2026-09-10 in the same ADR**, after the question was re-stated. The transport is not open: the argv contract already shipped in M3 and is already vendor-neutral, so ROAD-16's title points at the wrong artifact and most of it is done. The open half was the payload, and it becomes a porch-owned versioned envelope. Constrained SARIF was weighed and rejected — not because it lacks homes for a revision or a completion assertion, which it has, but because its *populated* coverage vocabulary stops at `analysisTarget`, defined as having been instructed to scan, which is exactly the inference `derive_states` already refuses.
- ~~Whether SARIF import belongs to this milestone or a later integration one~~ — **resolved 2026-09-10 in the same ADR: it belongs here, paired.** SARIF import and the operator coverage source ship together or neither does, because a converter alone yields a producer that is always `incomplete` and an operator coverage source alone has nothing to convert. Independently and now, porch sniffs a SARIF log at the output path, because today a valid SARIF 2.1.0 log parses as an *empty review* and dies later as an indistinguishable `coverage_shortfall`.

**Live defect, first work in ROAD-15.** A judgment producer that returns `skip` for every changed path has every path recorded as `waived` with the authority string `"producer"` fabricated by porch, `meets_required` returns true, and the round finalizes `complete` — the judgment layer contributed nothing and the audit says the required set was satisfied. ARCH-12 holds, so the floor still ran and this is an audit-integrity hole rather than an unreviewed forward, but ARCH-11 already decides it. **It cannot be repaired by deleting the default**, which is what ruling on it first proposed: the deterministic floor also emits `skip`, and `porch-quality`'s coverage row has no authority field at all, so that same fabricated string is what lets the floor's own skips satisfy the requirement. Deleting it breaks the mandatory floor on the first run that touches a skipped path. The repair is role-aware stamping — `porch` for the floor slot, nobody for the judgment slot — and both halves must land together. See [ADR-0009](../adr/0009-only-the-operator-may-excuse-a-changed-file-from-review.md).

**Corrections to this milestone's own entries**, found while ruling: ROAD-17's `incomplete` outcome already ships as a `CHECK`-constrained value written from a closed reason vocabulary, so what it still owes is the migration plus one new reason code — and because `completion_reason` is unconstrained `TEXT`, that code needs no schema migration. ROAD-15's declared surfaces understate the change by four files and two crates: the coverage bar lives in `coverage_state.rs`, the finding contract in `identity.rs`, the producer descriptor in `plan.rs`, composition in `crates/porch-run/src/lib.rs`, and the enforcing `CHECK`s in `crates/porch-gate/src/rounds/schema.rs`.

## MILE-8 — Assurance from CI

**Outcome:** A consumer can enter the same assurance protocol from a CI run rather than a
local push, without porch orchestrating or replacing that CI.
**Goals:** None — an execution mode for the protocol the approved goals already cover.
**Members:**
- **ROAD-21** CI entry point into the assurance protocol — Surfaces: None — the entry-point shape is not yet decided
**Depends-on:** MILE-5
**Commitment:** Committed 2026-09-10
**Closed:** None
**Deferred:** None
**Blockers:**
- ~~Whether CI mode is a supported peer entry point or a fallback only~~ — **resolved 2026-09-10 in [ADR-0010](../adr/0010-ci-is-a-peer-entry-point-that-owes-the-same-audit.md): a peer entry point, with no amendment to ARCH-1.** The case for "fallback" rested on a peer entry point requiring one, and it does not: `porch rerun` already enqueues a gate run with no push at all, and a CI job that runs `git push porch` on its own checkout satisfies ARCH-1 verbatim. The ADR also names the question this blocker did not ask, which is the one that actually decides the milestone — the assurance record lives in SQLite under `$PORCH_HOME`, which dies with the runner, so a CI-entered run owes GOAL-2 in full and porch owes a durable, self-describing audit export to discharge it. An archived `porch audit --json` does not suffice, because `CONTEXT.md` states a nonterminal audit document is partial. GOAL-1's reconciliation of a crashed forward is stated as a bounded exemption for a truly ephemeral runner.
- **Prerequisite defect, not CI-specific.** `repo_id` is already an indirection — `init` prefers the `porch.repo-id` git-config value and writes it back — but only `init` and `eject` honour it; every other call site recomputes the hash of the canonicalized work-tree path. So presetting the key or moving an initialized checkout makes `porch rerun` compute an identity `init` never stored and report no prior run. Routing every call site through one resolver fixes that and hands a CI runner a stable identity with no further change.

## Goal dispositions

Every live `GOAL-N` in `docs/product/vision.md` that no milestone cites belongs here, so
that a goal is never silently dropped (S6).

None — `GOAL-1` (MILE-3), `GOAL-2` (MILE-2), `GOAL-3` (MILE-7), and `GOAL-4` (MILE-4) are
each cited by a milestone.

| Goal | Disposition | Date | Reason |
|---|---|---|---|
