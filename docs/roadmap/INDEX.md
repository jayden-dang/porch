# Roadmap: Porch

Status: Approved
Date: 2026-08-30

| ID | Milestone | Outcome | Depends-on | Commitment |
|---|---|---|---|---|
| MILE-1 | Inner gate | A developer pushes to `porch` and gets an independently reviewed, certified branch forwarded to `origin` with a PR opened. | none | Committed |
| MILE-2 | Auditable assurance record | An operator can trace any assurance outcome to the evidence behind it. | MILE-1 | Planned |
| MILE-3 | Crash-safe forwarding | A gate that dies mid-forward never leaves an unauthorized or duplicated push. | MILE-2 | Planned |
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
- **ROAD-4** per-finding disposition history that survives a review round — Surfaces: `crates/porch-gate/src/db.rs`, `crates/porch-run/src/lib.rs`
- **ROAD-5** phase start and end events, surfaced rather than only stored — Surfaces: `crates/porch-gate/src/db.rs`, `crates/porch-gate/src/rpc.rs`, `crates/porch/src/main.rs`
- **ROAD-6** finding contract — criterion, evidence, consequence, action, producer provenance, stable fingerprint — Surfaces: `crates/porch-review/src/lib.rs`, `crates/porch-quality/src/`
**Depends-on:** MILE-1
**Commitment:** Planned
**Closed:** None
**Deferred:** None
**Blockers:** None

## MILE-3 — Crash-safe forwarding

**Outcome:** An operator's branch is never forwarded without durable authorization, and a
gate that died mid-forward discovers what actually happened instead of repeating it.
**Goals:** GOAL-1
**Members:**
- **ROAD-7** persist assurance authorization and reviewed-input binding before any external forward — Surfaces: `crates/porch-run/src/lib.rs`, `crates/porch-run/src/deliver.rs`, `crates/porch-gate/src/db.rs`
- **ROAD-8** restart reconciliation of ambiguous external effects — Surfaces: `crates/porch-run/src/deliver.rs`, `crates/porch-gate/src/daemon.rs`
- **ROAD-9** fault-injection suite across the forward boundary — Surfaces: `crates/porch/tests/`
**Depends-on:** MILE-2
**Commitment:** Planned
**Closed:** None
**Deferred:** None
**Blockers:**
- Restart reconciliation behaviour when the branch was pushed but PR creation or local completion persistence did not finish — owner Jayden; due before this milestone is approved for implementation; resolved through feature discovery/design, not here.
- Whether an approval may remain valid after HEAD advances past the reviewed SHA, and under which copy conditions — owner Jayden; same due point and route.

## MILE-4 — Escape without the daemon

**Outcome:** When automatic reconciliation cannot complete, an operator can inspect state,
recover every reachable porch-authored commit, and detach porch from the checkout — with
no healthy daemon and no hand-editing of hooks, git config, refs, or the database.
**Goals:** GOAL-4
**Members:**
- **ROAD-10** daemon-independent inspect → recover or abandon → detach, with distinct operator-facing states — Surfaces: `crates/porch-gate/src/eject.rs`, `crates/porch-run/src/sync.rs`, `crates/porch/src/doctor.rs`
- **ROAD-11** wedged, dead, and refusing-startup daemon suite — Surfaces: `crates/porch/tests/`
**Depends-on:** MILE-3
**Commitment:** Planned
**Closed:** None
**Deferred:** None
**Blockers:**
- The refusal and explicit-abandon policy for `eject --purge` — owner Jayden; due before this milestone is approved for implementation; resolved through feature discovery/design, not here.

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
