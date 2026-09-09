# Porch

Porch is a **local git gate**. You push to a remote named `porch` instead of
`origin`; a disposable worktree rebases the branch, runs independent review and
cheap local certification, and only then forwards the branch to `origin` and
opens a PR. It is an *inner* gate — opt-in, local, isolated. It is not CI, not a
deploy system, and it never hijacks `origin`.

## Language

**Gate**:
The whole inner loop between `git push porch` and the branch reaching `origin` —
admit, rebase, review, certify, deliver. "The gate passed" means every stage
succeeded and the branch was forwarded.
_Avoid_: "pipeline", "CI" — porch does not replace a consumer's CI.

**Admit**:
The decision to accept a pushed ref into the gate at all, taken by the receiving
hook before any worktree exists.
_Avoid_: "accept", "queue"

**Run**:
One execution of the gate over one pushed SHA, identified by a ULID and recorded
in the state database under `$PORCH_HOME`.
_Avoid_: "job", "build"

**Worktree**:
The disposable git worktree a run is executed in. Created per run, removed after.
Never the operator's checkout.
_Avoid_: "sandbox", "container" — no isolation boundary beyond the filesystem is implied.

**Review**:
The layered assurance stage of the **Assurance protocol**. It always includes the
mandatory **Deterministic floor** over the diff, and may also include a
session-free **Judgment layer** that emits findings as JSON — optional, but never
implicit: which layers a round required is recorded as its **Assurance shape**.
The judgment layer is porch-native by default and may be supplied by an external
**Producer** that meets the declared bar; the floor never is.
_Avoid_: "lint" — lint is a certification check, not review.

**Review round**:
One execution of **Review** for a specific reviewed input within a **Run**,
identified by a `review_round_id`. A post-fix review creates a new round;
round records are never merged or overwritten.
_Avoid_: "review pass"; "rereview" — a rereview is a round like any other.

**Finding**:
One reviewed issue. It identifies its criterion, evidence, consequence, action,
producer provenance, and a **Fingerprint**; confidence is optional and typed
by producer epistemology — a deterministic producer never manufactures
model-style confidence. A finding is **blocking** when its severity is error or
warning, or its action is ask-user; blocking findings park the run, info findings
do not.
_Avoid_: "comment" — a comment is the raw producer output a finding is mapped from.

**Finding instance**:
One immutable occurrence of a **Finding** within one **Review round**,
identified by a `finding_instance_id`. This is the durable audit key. The
positional handles (`f0`, `f1`, …) are per-round ordinals for display and
selection only, and are never audit keys.
_Avoid_: "finding id" — ambiguous between the handle and the instance.

**Fingerprint**:
The logical reconciliation key for recognizing the same issue across
**Review rounds**. It is never the database identity of a **Finding instance**
and may recur on multiple instances over time.
_Avoid_: "hash", "finding id"

**Disposition history**:
The porch-owned, append-only event log of authority actions about a **Finding
instance**, keyed by `finding_instance_id`. The log is the source of truth; any
current-state view is a derived projection and must not become a second
authority. Finalized instance rows are never updated to carry operator
disposition, and producer `action` is never overwritten. After a later
**Review round**, prior occurrences remain readable through their own events.
Distinct from a producer **Finding** `action` and from coverage-file `authority`
on `round_coverage`. Never keyed by display `fN`.
_Avoid_: "comment", "note", "action" when the operator or porch decision is meant.

**Phase-event history**:
The porch-owned append-only `phase_events` log — source of truth for a
**Run**'s phase lifecycle. Canonical top-level phases are exactly `intent`,
`rebase`, `review`, `certify`, and `deliver`. Re-entry into a canonical phase
mints a new top-level **Phase attempt** with an incremented ordinal.
`started` persists before work; a separate terminal is appended; the start
row is never updated. Crash recovery appends interrupted/cancelled evidence.
The log reconstructs the timeline without the live stream; subscribe events
are notifications (a gap refreshes durable state). `step_results` and
`steps[]` are compatibility projections, rebuildable from `phase_events`,
never an independent authority. The timeline is exposed through the **Audit
read path**.
_Avoid_: "activity", "log line", "steps[]" as the audit record.

**Phase attempt**:
One numbered execution of a canonical top-level phase on a **Run**. `parked`
is a nonterminal suspension; a later operator response either resumes the
attempt or produces its terminal outcome. Nested operations use
`parent_attempt_id` (execution containment) only. Causal links between
top-level attempts use `caused_by_attempt_id` and never replace chronological
`phase_events` order. A **Nested operation** must not start after its parent
attempt has a terminal event. Rereview, recertification, and redelivery are
new top-level attempts, not children of `deliver_repair` or a prior `deliver`
attempt. Compatibility may still show `phase=compose`; the canonical audit
path is `deliver/<attempt>/compose/<operation-attempt>`.
_Avoid_: treating compose, fixer, or `deliver_repair` as a sixth top-level phase.

**Nested operation**:
Typed work contained by an active (nonterminal) **Phase attempt**: `compose`
and the mechanical work of `deliver_repair` belong to the owning `deliver`
attempt; fixer execution belongs to the suspended `review` attempt.
`AllowlistFailed` and `MergeConflicting` are nonterminal repairable-failure
evidence on the active `deliver` attempt, not its terminal. Nested
`deliver_repair` ordinals are allocated only while budget remains; `started`
persists before work; the budget counts nested operations **started**.
Succession is taken only from explicit post-success HEAD before/after on the
nested terminal — never from repair kind or crash inference. HEAD-changing
success: atomic handoff that revokes old-HEAD approval, binds the new HEAD,
terminals the old `deliver` attempt, and starts the next `review` attempt
(`caused_by` that deliver). Unchanged-HEAD success: atomic handoff to the
next `deliver` attempt only — no rereview, no recertify, never reuse the
same deliver ordinal. Repair fail/interrupt: terminal nested op, owning
deliver, and run — no successor. Budget already exhausted: no nested op;
terminal current deliver and run with an explicit budget-exhausted cause.
_Avoid_: "phase" for these nodes; "step string" as their identity; counting
budget by deliver-attempt ordinals.

**Audit read path**:
The operator-facing reconstruction that joins **Disposition history** and
**Phase-event history** for one assurance outcome. Completing only one domain
does not satisfy **GOAL-2**. Cross-round related findings are **not** stored
as instance-lineage edges. The read path groups already-finalized
fingerprints with the exact key `(run_id, fingerprint_version, fingerprint)`
into an equivalence set (`related_occurrences` or equivalent) — never a
predecessor/successor chain, never a rerun of reconciliation, never a
candidate-key or similarity join. Occurrences stay independent (round,
provenance, finding data, disposition events, bulk memberships); nothing is
folded or transferred. Grouping does not span runs or fingerprint versions,
does not participate in current-round authorization or forward eligibility,
and is omitted when conservative reconciliation minted a new fingerprint.
`caused_by_attempt_id` remains phase-attempt causality only and never implies
a finding-level causal edge. Deterministic order: review-round ordinal, then
durable instance id.
_Avoid_: "status", "get_run" as the audit join; "successor finding"; treating
a related-occurrence group as one merged finding.

**Audit document**:
The one dedicated typed read model produced by a shared server-side **Audit
read path** over durable round, finding-instance, disposition-event, and
`phase_events` sources. It is derived, not a new source of truth. Each
document is built from one consistent SQLite read snapshot (or equivalent)
and carries a revision/watermark. One builder serves every surface: daemon
RPC + `porch agent` JSON, human CLI pretty-print, additive TUI audit/history
view. Consumers must not duplicate join or interpretation rules.
`get_run`, `porch agent status`, `findings[]`, and compatibility `steps[]`
stay compact live state — they may advertise audit availability, never the
full audit contract. The TUI loads the document lazily when that view opens,
not on every live `State` / `StreamGap`. Subscribe events remain
notifications only. Ordering in the document is deterministic. The same
schema is available for every existing run (`running`, `parked`, terminal).
Each response uses a durable database-backed watermark/revision — not
in-memory `EventHub.state_rev` — and includes every durable fact committed
at that snapshot (rounds, findings, disposition/authority events, active /
suspended / terminal attempts, nested ops, causal links). Nonterminal
documents are labeled partial/as-of that watermark and observed run status;
they must not expose a final assurance outcome before one exists. Active and
parked nodes appear when their events are durable. A lifecycle/phase-tree
inconsistency is an explicit anomaly or unknown, not a silently complete
document. Partial audit is a successful RPC/agent response. Later reads may
show later watermarks; consumers must not cache a partial document as final.
If pagination exists, every page/cursor of one assembled document shares that
watermark.
_Avoid_: stuffing the log into `status`; fetching audit on every live event;
using `state_rev` as the audit watermark.

**Bulk operator response**:
A review-level `approve` or `skip` recorded as one event, not as synthesized
per-finding dispositions. It freezes every `finding_instance_id` from the
applicable finalized **Review round** at response time. Membership means
included in that decision context, not individually disposed. `approve`
authorizes continuation, binds that `review_round_id` and the approved HEAD,
and keeps the certify → deliver path. `skip` records termination without
approval, does not record an approved HEAD, and does not authorize
continuation. A later **Finding instance** with the same **Fingerprint** is
not covered. The write fails closed if the applicable round or reviewed HEAD
changed before persistence.
_Avoid_: "accepted findings", "skipped findings" — those names imply
per-finding disposition.

**Rebase abort**:
Operator `respond abort` while the canonical `rebase` **Phase attempt** is
parked. One atomic transaction: terminal that rebase attempt, then
`parked → cancelled` with an explicit operator rebase-abort cause. Transaction
failure rolls back completely and leaves the run parked with its worktree.
After commit: publish live notifications, run the existing recovery-pin
check, then force-remove the disposable worktree. Do not invoke
`git rebase --abort` on this path — the initial and retry parks already abort
Git successfully before parking. Post-commit pin or worktree failure does not
rewrite the cancelled/terminal outcome; residue is left for recovery. No
finding-instance or **Disposition history** events (`rebase0` is fixer input,
not durable identity). No successor **Phase attempt**. The event cause is
distinct from the internal `git rebase --abort` command.
_Avoid_: confusing operator abort with Git `rebase --abort`; synthesizing
findings from `rebase0`.

**Compose abort**:
Operator `abort` while nested `compose` is parked. One atomic transaction, in
order: terminal the nested compose operation; terminal the owning `deliver`
attempt, causally linked to that compose abort; transition the **Run**
`parked → cancelled` with an explicit compose-abort cause. Any write failure
rolls back the whole transaction, leaves the run parked, and keeps the
worktree. After commit: publish live notifications, then clean the worktree.
Post-commit cleanup or process death does not rewrite the terminal audit
outcome; startup recovery removes local residue. The GitHub PR stays open —
no `gh` close/draft/edit/mutate on abort. Persist the known PR reference and
record intended external disposition `left_open`; if no reference exists,
record that honestly. No **Disposition history** or finding events. Distinct
from compose `skip`, which accepts the scaffold and lets `deliver` complete.
Compatibility projections must show both compose cancellation and owning
deliver termination; the durable audit tree remains after live `phase=compose`
ends.
_Avoid_: treating compose abort as review abort; closing the PR; synthesizing
a PR reference.

**Review aborted**:
A review-level `abort` on a modern parked review, recorded as one bulk
`review_aborted` operator-response event binding the applicable
`review_round_id`, the reviewed HEAD, and the frozen set of every
`finding_instance_id` in that round. Membership is decision context only — not
per-finding `rejected` or `aborted`. Grants no approval and no continuation; the
**Run** becomes `cancelled`. Distinct from review `skip` and from system
cancellation such as `superseded_by_new_push`. The event and the
`parked → cancelled` transition persist atomically; worktree cleanup runs only
after that commit. Legacy parks without round identity may abort: record at
run/review level with audit identity explicitly unavailable; never treat `fN`
as instance ids or synthesize membership.
_Avoid_: "skip", "superseded" — those are different causes and terminals.

**Fix requested**:
A review-level `fix` recorded as one append-only `fix_requested` event.
Target relations are durable `finding_instance_id`s resolved at response time
from explicit `--findings` or the default all-blocking selection — never
display `fN`. A target means requested for this fixer attempt, not fixed,
resolved, accepted, or otherwise terminally disposed. An empty target set is
rejected. `fix_requested` and the nested fixer `started` event persist before
the fixer is spawned. The parked **Phase attempt** stays nonterminal for the
whole nested fixer operation. Fixer terminal and parent review terminal are
separate events. A fixer outcome that permits rereview uses an atomic **Phase
handoff** into a new review attempt; at most one successor review attempt is
created from a given review attempt. Fixer failure or interruption terminals
the nested op and the parent review, fails or interrupts the run, and creates
no successor. Fixer `Ok` with `HEAD_after == HEAD_before` is a successful nested
outcome, not failure or approval; the fixer terminal carries durable
no-change evidence (bound HEAD and `head_changed=false` or equivalent).
The same atomic **Phase handoff** still runs; a fresh **Review round**
executes required producers on that unchanged HEAD — no short-circuit, no
reuse of prior producer results because the SHA matches. New identities for
round, invocations, attempt, and finding occurrences; fingerprint
reconciliation only, no transfer of disposition, target membership, or
authority. Exactly one rereview for that `fix` response — no automatic
no-op loop. If the fresh round parks and `--yes` is exercised, the bulk
`review_approved` binds the new round, unchanged HEAD, and new instance set,
and the audit path shows that the fixer did not change HEAD. Changed HEAD
after a crash is not evidence of fixer success.
Rereview mints a new **Review round** only after the handoff commits; if the
process dies after handoff but before the round opens, recovery terminals the
already-started new review attempt as interrupted and does not synthesize a
round. `--yes` remains bounded standing consent as already defined.
_Avoid_: "fixed findings" — the target set is a request, not an outcome.

**Phase handoff**:
The atomic succession of one top-level **Phase attempt** by the next: append
the old attempt's terminal, allocate the next ordinal, append `started` for
the new attempt, and set `caused_by_attempt_id` to the old attempt. Old
terminal and new start occupy consecutive deterministic sequence positions.
Never two attempts of the same canonical phase nonterminal at once. The new
attempt's `started` event is durable before that attempt's work begins.
_Avoid_: overlapping attempts; inferring succession from HEAD movement.

**Assurance protocol**:
Porch's own end-to-end contract over a review: inventory, required coverage,
normalization, reconciliation, authority, SHA binding, and the fail-closed
outcome. Porch owns it whoever produced the findings.
_Avoid_: "pipeline", "review flow"

**Producer**:
Anything that emits findings for the assurance protocol to consume — the
porch-native review, or an external review system. A producer's success verdict
is evidence, never an approval (**ARCH-11**).
_Avoid_: "engine" when the external case is meant; "reviewer" — that is the turn,
not the party.

**Deterministic floor**:
The computation-only layer of an assurance run — rule packs and the coverage
manifest over the diff, no shell, no network, no model. Always runs; never
substitutable (**ARCH-12**).
_Avoid_: "static analysis", "lint"

**Judgment layer**:
The layer above the floor that exercises judgment on the change. Supplied by the
porch-native review by default, or by an external **Producer** that meets the
declared bar.
_Avoid_: "the reviewer" — that names the turn, not the layer.

**Required producer set**:
The producers a **Review round** was required to have, recorded when the round is
opened and never reconstructed from the producers that actually ran. Porch owns
it; the floor is always in it. Authorization compares against this record.
_Avoid_: "expected producers", "producer list" — those name what ran.

**Assurance shape**:
Which layers a round's **Required producer set** demanded — floor-only, or floor
plus judgment. Pinned per **Run** at its first round and identical for every later
round of that run. Its human-readable name is presentation; the authorization
identity is the canonical digest over the required set.
_Avoid_: "review mode", "engine" — the engine is a selection, not the shape.

**Independence**:
Context and process isolation of review from the writing of the change, so
review is not anchored by it. Porch may inherit the harness's engine,
credentials, and runtime; it never inherits the writing session, conversation,
memory, or session id.
_Avoid_: "impartial", "third-party" — independence is not vendor difference.

**Incomplete**:
The outcome when a **Producer** misses the declared bar or the protocol cannot
establish its required facts. Fails closed; never a clean approval.
_Avoid_: "failed" — a failed run is a different terminal status; "partial"

**Fixer**:
The agent turn that acts on review findings. May resume a session; the reviewer
may not. A rereview must never certify its own prescription.
_Avoid_: "reviewer" — the two roles are deliberately separate.

**Certify**:
The cheap local check phase (format, typecheck, lint, tests) that runs inside the
worktree after review. Certification is not review.
_Avoid_: "verify", "validate" — those name different skill-side concepts.

**Deliver**:
Forwarding the certified branch to `origin` and opening the PR, then babysitting
PR checks **by allowlist only**.
_Avoid_: "deploy", "publish"

**Forward authorization**:
The durable fact that porch may forward a commit for a **Run**, resolved at one
decision point from the recorded approved SHA. Today a live HEAD that descends
from that SHA is authorized, because certify's own correction commit advances
HEAD after approval; whether that commit may be forwarded without re-review is
an open MILE-3 blocker, and binding by equality waits on it. Persisted before
any external mutation (**ARCH-13**), never inferred from remote state.
_Avoid_: "approval" for the forward act; "continuity" as the durable record;
stating the equality rule as though it were in force.

**Forward record**:
The append-only porch-owned evidence of one forward attempt, keyed to the owning
`deliver` **Phase attempt** and its position in that attempt's sequence of
forwards: an intent entry committed before the pushing command (run, ref,
authorized SHA, observed remote tip) and an outcome entry committed after it and
before the pull request adapter. At most one forward is in flight per attempt. It
is a reconciliation input — it makes an attempt whose push completed
distinguishable from one never invoked — and never an assurance outcome
(**ARCH-11**). Porch derives the outcome from its own command result, never from
probing `origin`.
_Avoid_: "push log", "audit event"; `pr_url` as the evidence a push landed; one
forward record per `deliver` attempt.

**Custody**:
Porch's claim over a ref while a run holds it — the basis for refusing a
force-push that would drop live remote commits.
_Avoid_: "lock", "lease" — `lease` means the `--force-with-lease` mechanism specifically.

**Intent**:
The operator-supplied statement of what a run is meant to accomplish, passed to
the agent turn (`porch agent run --intent`).
_Avoid_: "prompt", "goal"

**Park**:
A run halting mid-pipeline to wait for an **Operator** decision: at `rebase` on a
conflict (`fix` | `abort` only), at `review` when the round carries blocking
findings (`approve` | `skip` | `abort` | `fix`), or at `compose` after the
scaffold PR exists (`respond` | `skip` | `abort`). `parked` is the run status
while it waits; the response resumes or ends the run.
_Avoid_: "stash", "pause". Never "setting aside hunks" — porch has no hunk-level
splitting; the TUI's hunk view only shows a finding's diff snippet.

**Eject**:
The escape hatch back to a plain checkout: removes the `porch` remote and
neutralizes the bare hooks. Safe eject preserves the database, bare repository,
recovery refs, and custody evidence. `--purge` is destructive — it deletes this
repo's bare, worktrees, run artifacts, and DB row (other repos under
`$PORCH_HOME` untouched) — and is outside the no-loss guarantee of **GOAL-4**.
_Avoid_: "uninstall" — that is the daemon-service verb (`porch daemon uninstall`).

**Trusted SHA**:
The default-branch commit that code-executing config is loaded from. Never the
pushed SHA. Fetch failure fails closed.
_Avoid_: "HEAD", "current config"

**Slice**:
A crate that owns a use case, not a technical layer: `porch-gate`, `porch-run`,
`porch-review`, `porch-quality`, `porch-deliver`, `porch-agent`, and the `porch`
operator binary. `porch-git` is the one deliberate exception — shared plumbing,
the only place the gate shells out to `git`, and not a slice.
_Avoid_: "module", "layer"

**Operator**:
The person at the working checkout who pushes to `porch` and answers parks,
through the TUI or the CLI. Holds the decision; never the Consumer repo itself.
_Avoid_: "user", "developer"

**Agent**:
A coding agent driving a run headlessly through `porch agent`
(run / status / respond / sync) — same authority as the Operator, no TUI.
Not the reviewer or **Fixer** turns porch invokes inside a run.
_Avoid_: "bot", "automation"

**Consumer**:
A repository that uses porch as its inner gate. First dogfood consumers are
mailgate, then klynt.
_Avoid_: "client", "user" — a user is a person.

## Relationships

- A **Run** executes in exactly one **Worktree** and is scoped to one pushed SHA
- **Admit** gates entry; the run's phases are intent → rebase → **Review** →
  **Certify** → **Deliver**
- A **Park** interrupts a run at `rebase`, `review`, or `compose`; only an
  **Operator** or an **Agent** response clears it
- A **Review** produces findings; a **Fixer** consumes them
- A **Run** holds **Custody** of a ref; custody is what makes a force-push refusable
- A **Consumer** repo has one porch remote and keeps its own CI

## Flagged ambiguities

- **review** vs **certify** — both were called "checks" early on. Review is
  judgment (findings, possibly agent-authored); certify is deterministic local
  commands. They are separate stages and separate crates.
- **agent** is overloaded three ways in the code: the `porch agent` CLI (an
  **Agent** driving a run), the `porch-agent` crate (the **Fixer** adapter), and
  `--engine agent` (the reviewer turn). The glossary term means only the first.
- **lease** vs **custody** — `lease` is reserved for the `--force-with-lease`
  git mechanism; **custody** is porch's own claim on the ref.
