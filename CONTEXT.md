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
A session-free reviewer turn that emits findings as JSON. The default engine is a
coding-agent turn; `porch-quality` is the first-party engine; OCR is legacy
(`--engine ocr`).
_Avoid_: "lint" — lint is a certification check, not review.

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
Setting aside hunks of a change so the rest can proceed through the gate;
`eject` is the escape hatch that returns the operator to a plain checkout.
_Avoid_: "stash" — parking is porch state, not git stash.

**Trusted SHA**:
The default-branch commit that code-executing config is loaded from. Never the
pushed SHA. Fetch failure fails closed.
_Avoid_: "HEAD", "current config"

**Slice**:
A crate in the workspace that owns a use case (`porch-gate`, `porch-run`,
`porch-deliver`), not a technical layer.
_Avoid_: "module", "layer"

**Consumer**:
A repository that uses porch as its inner gate. First dogfood consumers are
mailgate, then klynt.
_Avoid_: "client", "user" — a user is a person.

## Relationships

- A **Run** executes in exactly one **Worktree** and is scoped to one pushed SHA
- A **Run** passes through **Admit** → rebase → **Review** → **Certify** → **Deliver**
- A **Review** produces findings; a **Fixer** consumes them
- A **Run** holds **Custody** of a ref; custody is what makes a force-push refusable
- A **Consumer** repo has one porch remote and keeps its own CI

## Flagged ambiguities

- **review** vs **certify** — both were called "checks" early on. Review is
  judgment (findings, possibly agent-authored); certify is deterministic local
  commands. They are separate stages and separate crates.
- **lease** vs **custody** — `lease` is reserved for the `--force-with-lease`
  git mechanism; **custody** is porch's own claim on the ref.
