# Locked decisions

Reopen only with an explicit written change. Coding sessions do not get to “just use libgit2” or “just add GitLab.”

## Product

| ID | Decision |
|---|---|
| D1 | Name is **porch**. CLI `porch`. Git remote `porch`. Skill `/porch`. Home `$PORCH_HOME` (default `~/.porch`). |
| D2 | Metaphor: the porch between the house and the street. Local work stays inside; `origin` is the road. Consent: `git push porch`. `origin` is never hijacked. |
| D3 | Category: **inner gate** between a local branch and the configured push target. Not CI, not deploy, not merge, not team governance. |
| D4 | “Passed the porch” means: fresh rebase attempted, independent review completed (or an explicit per-run skip), cheap certify adapters passed, HEAD is a descendant of the review-approved commit, push used a lease anchored on observed SHA, PR opened or updated. It does **not** mean deploy/E2E/on-chain succeeded. |
| D5 | Pipeline is five phases, **order not configurable**: `intent → rebase → review → certify → deliver`. |
| D6 | Review auto-fix default is **0**. Test/lint-style certify adapters may auto-fix; review does not, unless the operator raises the limit. |
| D7 | Year-1 forge: **GitHub only** (`gh`). |
| D8 | Year-1 agents: **ACP + one native CLI**. No nine-adapter matrix. |
| D9 | Year-1 review engine: compose an **external review CLI** as a subprocess. Do not reimplement its grouping/rules/relocation until a later milestone. Binary name, flags, and comment schema live in `.research/` (gitignored). |
| D10 | First dogfood target: **mailgate**. If porch cannot live on that monorepo without swallowing its CI, the product is wrong. |
| D11 | Agent CLI is JSON (and JSONL for streams), not a custom encoding as the only option. Exit codes: 0 ok/gate, 1 failed/cancelled, 2 usage. |
| D12 | TUI is secondary. Headless `porch agent` is first-class. |
| D13 | Intended license: **Apache-2.0**. Do not paste third-party source into this tree. Ideas may be reimplemented. |
| D14 | crates.io package name is **porch** (id free as of 2026-08-29). Binary `porch`. Fallbacks `git-porch` / `porch-gate` only if the id is taken at publish time. |

## Engineering

| ID | Decision |
|---|---|
| E1 | Implementation language is **Rust** (edition 2024). Virtual Cargo workspace: one published binary (`porch`) plus slice library crates (`porch-git`, `porch-gate`, later phase crates). Slices are use cases, not technical layers. Internal libs `publish = false`. Do not add a crate per layer (`daemon`/`db`/`ipc`). |
| E2 | Git is **always the `git` CLI** with absolute `--git-dir` / `-C`. Never libgit2 for gate operations. |
| E3 | SQLite via **`rusqlite`**, single writer (mutex or actor). No async connection pool. |
| E4 | Async runtime **Tokio** for daemon, IPC, process spawn. Git and rusqlite stay blocking (`spawn_blocking` or dedicated threads). |
| E5 | Child processes: Unix process groups + `killpg`; Windows Job Objects. Step end reaps the tree. Worktree sweep on run end. |
| E6 | Daemon singleton: OS flock on `$PORCH_HOME/daemon.lock` for process lifetime. PID file is identity, not truth. Bind socket only after lock. Prefer socket activation later; detached process is the fallback. |
| E7 | Hooks call **this same binary** (`porch daemon admit-push` / `notify-push`). One artifact. |
| E8 | Force updates: `--force-with-lease=<ref>:<observed-sha>`. Push the exact verified commit SHA, not mutable `HEAD`. Fail closed if the live remote cannot be verified. |
| E9 | Reviewer invocations are session-free. Fixer may resume. Rereview treats pipeline-authored commits as unreviewed new code. |
| E10 | Trusted config: fetch default branch, pin SHA, read executing fields from that SHA. Empty trusted tree is valid; unreadable trusted commit aborts the run. |
| E11 | Certify adapters are **cheap and targeted** (format, lint, generated-artifact drift). They are not `just test` with Postgres, not Playwright, not enclave EIF builds. |
| E12 | Deliver watches **allowlisted PR check names** only. `ci.rerun_transient` default 0. Never rerun spend-money or deploy workflows. |
| E13 | Unit tests never call real LLMs, a real review-CLI network, or real `gh`. PATH fakes + JSON fixtures. |
| E14 | `Cargo.lock` is committed (this is a binary). |
| E15 | Rebase conflict: **fail the run** after `git rebase --abort`. No park until a later milestone owns the park TUI. |
| E16 | Fetch/resolve of `origin/<default>` fails: **fail the run** (fail closed). Default-branch tip is a safety fact for rebase. |
| E17 | Intent for execute: hook/`notify` reads **`PORCH_INTENT`**. Empty → skip intent phase, do not fail. Default branch column: `repos.default_branch` (default `main`). |
| E18 | M3 park + agent JSON: blocking review findings set `status=parked` and keep the worktree; `porch agent status` / `respond` emit JSON on stdout (D11). Respond supports **`approve` \| `skip` \| `abort` only** (no fixer). `review_approved_head_sha` is written only on completed review or **approve**; **skip** does not write it. Review subprocess timeout **fails** the run (does not park). Review adapter lives in `porch-review`; `porch-gate` must not depend on it. |
| E19 | `porch init` sets `repos.default_branch` from the clone's `origin/HEAD` (`symbolic-ref refs/remotes/origin/HEAD` or `rev-parse --abbrev-ref origin/HEAD`, stripping `refs/remotes/origin/` / `origin/`). Fallback **`main`**. |
| E20 | `ensure_daemon` forwards every current-process `PORCH_*` env var into the detached daemon (`spawn_detached_with_env`), and always sets `PORCH_HOME` to the init home last so it wins. |
| E21 | `notify_push` enqueues runs only for `refs/heads/*`. Other refs (e.g. tags) still update the bare via admit/receive so `followTags` can succeed, but do not create runs. |
| E22 | Rebase fetch is `git -c fetch.prune=false fetch <remote> +<refspec>` (add `+` when the caller omitted it). Fetch + tip `rev-parse` in `run_rebase` are serialized with a process-wide mutex. |

## Explicitly rejected (for now)

| ID | Rejected idea | Why |
|---|---|---|
| R1 | Port an existing Go gate and rename | Wrong language; copies a 9-agent / 6-forge maintenance swamp |
| R2 | Rewrite the review engine in Rust year 1 | The external CLI is already better at review than a generic agent |
| R3 | Nine fixed steps including Document/Lint/Test/CI as first-class clones | Encoding is heavy; local Test must not be CI; Document can wait |
| R4 | Always-on heavy daemon as the only mode | Socket activation + on-demand is enough; complexity tax is real |
| R5 | libgit2 | Worktrees, hooks, credentials, `safe.bareRepository=explicit` |
| R6 | TypeScript/Bun core | Wrong for a git-proxy daemon |
| R7 | `no_ci: true` as a contributor-settable field | Mailgate (and any real repo) has CI; self-declaration is a bypass |
| R8 | Auto-fix review by default | Intent-touching; enclave/contract must not be silently rewritten |
| R9 | Supporting glab/tea/az on day 1 | Flag-surface drift is a research career |
