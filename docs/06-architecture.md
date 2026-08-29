# Porch architecture

Status: design. No code.

## Mental model

```
working clone  --git push porch-->  bare gate  (~/.porch/repos/<id>.git)
                                      │
                             pre-receive: admit
                             post-receive: notify daemon
                                      │
                                   daemon
                                      │
                              disposable worktree
                                      │
                 intent → rebase → review → certify → deliver
                                      │
                         GitHub (branch + PR + allowlisted checks)
```

`origin` remains the real remote. Fork routing (parent PR, fork push) is **not year-1** unless mailgate needs it.

## Phases

| # | Phase | Job | Auto-fix default |
|---|---|---|---|
| 1 | **intent** | Use `--intent` (authoritative) or skip. Transcript inference is optional later. | n/a |
| 2 | **rebase** | Fetch integration branch + push target; rebase or fast-forward. Conflict → fixer. Empty diff vs base → skip remaining. Do not silently bundle unpushed local default-branch commits. | 3 |
| 3 | **review** | External review CLI on `base..HEAD` in the worktree. Map to findings. Park on error/warning/ask-user. Fixer optional. Rereview cold. Write `review_approved_head_sha`. | 0 |
| 4 | **certify** | Run configured cheap commands (format/lint/drift). Agent only for leftover targeted checks if commands empty — **do not** default to full workspace test. | 3 for command failures that are mechanical |
| 5 | **deliver** | Format leftover? Commit leftover with a porch subject. `ls-remote`, lease-push exact SHA, `gh pr create/update`, watch **allowlisted** checks. Repair restart-at-review after CI fix / conflict rebase is **deferred** (M6 fail-closed on red allowlisted checks and incorporate refuse; see `docs/09-roadmap.md`). | 3 for mechanical check fails once repair lands; never for deploy |

Skip is per-run (`--skip`, push option), never a standing config hole in the core five.

## Components

### Gate (`crates/porch-gate`)

`porch init`:

- Bare repo under `$PORCH_HOME/repos/<id>.git`
- `pre-receive` / `post-receive` calling this binary
- Remote `porch`
- Ensure daemon
- Repo id: first 12 hex of sha256(absolute working path); preserve id on rename

Admission refuses descendants of an active validation step (recursive containment).

### Daemon (in `crates/porch-gate`, not its own crate)

Owns runs, worktrees, executor, IPC subscribers, crash recovery, child reaping.

- Flock `$PORCH_HOME/daemon.lock`
- Socket `$PORCH_HOME/socket`
- SQLite `$PORCH_HOME/state.sqlite`
- Same-branch serialize: new push cancels old run
- Recovery: resume only parked-complete gates; otherwise fail the stale run and pin unpublished commits under a recovery ref

### Git wrapper (`crates/porch-git`)

Every call: absolute git dir, env that won’t pick up the caller’s hooks (`core.hooksPath` empty **only** for porch’s own correction commits — husky in a disposable worktree otherwise sources missing `_/husky.sh`). Redact credentials in logs. Timeout per call.

### Pipeline executor

Sequential phases. Finding loop: auto-fix eligible → fixer → re-run phase; else park; `respond` approve/fix/skip/abort.

Shared in-memory: certify may consume nothing from review except HEAD + findings history.

### Review adapter (`src/review`)

Spawn the configured review CLI with a from/to SHA range and JSON output (exact flags: `.research/`, confirm against current `--help` at implementation time). Require the binary on PATH or a configured path. Parse comments; map to `Finding`; attach coverage: fail the phase if reviewable files are missing from the manifest without a skip reason.

### Agent (`src/agent`)

`Agent` trait. Impl: `Acp` via `acpx`, plus one native (Claude or Codex — pick at first implementation, don’t abstract nine). Used for: rebase conflicts, review fixer, optional certify leftovers. **Not** used for the primary review pass year-1.

### SCM (`src/scm/github`)

Spawn `gh`. Find PR by branch (not filtered by base — update existing rather than open a duplicate). Create/update with porch-generated body: Intent, What Changed, Risk, Review, Certify, Pipeline attestation (JSON in HTML comment, bind `head_sha`). Home-path redaction. Check watch: `gh pr checks` / checks API; allowlist.

### IPC / CLI

JSON-RPC on the socket. CLI:

- `porch` — attach TUI or wizard (later)
- `porch init | eject | daemon | status | runs`
- `porch agent run|respond|status|logs|abort|sync`

### Data (SQLite, first cut)

Tables: `repos`, `runs` (branch, shas, worktree, status, pr_url, review_approved_head_sha, awaiting_agent_since, parked_ms, porch_version), `step_results`, `step_rounds` (findings JSON, selection, fix_summary), maybe `agent_invocations` (privacy-safe: no prompts/diffs).

Run statuses: pending, running, completed, failed, cancelled, plus a CI-monitor-interrupted analogue if deliver was watching checks.

## Worktrees

Default `$PORCH_HOME/worktrees/<repo>/<run>/`. Placement recorded on the run at creation (config edits do not move live runs). APFS clone / reflink when possible. Do **not** share `target/` with the author’s tree. `sccache` is an operator concern, not a year-1 feature.

## Config

Global `$PORCH_HOME/config.yaml`. Repo `.porch.yaml`. Merge: trusted SHA for executing fields; pushed branch may set ignore patterns and non-executing metadata.

## Eventing

Bounded mailboxes. Slow TUI must not stall the run. State events must not be dropped silently (coalesce a gap signal; clients re-read).
