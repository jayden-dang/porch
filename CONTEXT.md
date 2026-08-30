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
The layered judgment stage of the **Assurance protocol**: a mandatory
**Deterministic floor** over the diff, plus a session-free **Judgment layer**
that emits findings as JSON. The judgment layer is porch-native by default and
may be supplied by an external **Producer** that meets the declared bar; the
floor never is.
_Avoid_: "lint" — lint is a certification check, not review.

**Finding**:
One reviewed issue. It identifies its criterion, evidence, consequence, action,
producer provenance, and a stable fingerprint; confidence is optional and typed
by producer epistemology — a deterministic producer never manufactures
model-style confidence. A finding is **blocking** when its severity is error or
warning, or its action is ask-user; blocking findings park the run, info findings
do not.
_Avoid_: "comment" — a comment is the raw producer output a finding is mapped from.

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
conflict (`fix` | `abort` only), or at `review` when the round carries blocking
findings (`approve` | `skip` | `abort` | `fix`). `parked` is the run status while
it waits; the response resumes or ends the run.
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
- A **Park** interrupts a run at `rebase` or `review`; only an **Operator** or an
  **Agent** response clears it
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
