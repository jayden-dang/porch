# Design — reviewed normalization and binding by equality

**Feature code:** EQUAL
**Roadmap item:** ROAD-23 (MILE-3)
**Requirements:** `requirements.md`
**Respects:** ARCH-4, ARCH-6, ARCH-13

## What this build is

Three production changes, in dependency order:

1. **Certify never commits.** Both `commands.format` and `commands.lint` still
   run after approval; a dirty tree is a named certify failure. The helper that
   `git add -A`s and commits is deleted from this phase.
2. **Mutating format moves to the end of rebase**, after git has finished and
   before emptiness is computed, so its output is inside `base..head` and inside
   the **Review round**. The parked-rebase fixer retry uses the same helper;
   deliver-repair rebase does not.
3. **`authorized_forward_sha` binds by equality** and returns the approved SHA.
   Descendant tolerance is deleted. The ROAD-9 tripwire becomes two guards.

No DDL. `phase_events.kind` already admits `evidence`. `phase_attempts.operation_kind`
and `phase` CHECKs stay untouched — they sit inside `CREATE TABLE IF NOT EXISTS`.

## Why not the advisor's check-only option

Framing and the advisor agreed on the GOAL-1 close (no post-approval HEAD
movement, forward by equality, lint must not remain a writer). They disagreed
on whether `commands.format` may still mutate *before* review.

Check-only — delete the writer, fail closed on dirt, teach `fmt --check` — is
the smaller production diff: no new commit site, no rebase early-return
surgery, no ARCH-6-scope argument, glossary **Certify** stays a check without
caveat. It is also the option ADR-0003 considered and rejected: "The mutating
step moves earlier instead of being deleted." The accepted cost is a
self-extinguishing first-run reformat inside the reviewed range, versus a
recurring refusal on every push until the operator converts the command.

This wave does not reopen that ruling. It implements it, with the coding
details the ADR left incomplete (lint, early-returns, which SHA is returned,
gpgsign on the new commit). Teaching the checking form remains a *supported
choice* (EQUAL-5.1), not the only one.

## 1. Certify is verify-only

`run_certify_phase` keeps both adapters and loses both `maybe_correction_commit`
calls. After each adapter, `git status --porcelain`: non-empty is
`CertifyError::Msg` naming the command and the paths. `refresh_head_sha` has
no remaining caller in this file and is deleted with the helper.

The certify re-run of format is the termination rule. A non-idempotent
formatter that rewrites on every invocation fails closed at certify rather than
looping. Do not loop. The `porch-fake-format-dirty` fixture currently appends
on every invocation; it becomes a stable overwrite so the happy-path tests still
reach compose-park rather than a second dirt.

## 2. Format at the end of rebase

`run_rebase` today:

```text
resolve_rebase_onto → keep path_instructions, drop cfg.commands
if HEAD == onto: return empty
if ancestor: reset --hard
else rebase (conflict → park)
compute empty
```

It becomes:

```text
resolve_rebase_onto → keep cfg.commands
git rebase / reset / already-on-onto (conflict still parks; format does not run)
apply_format_mutating(cfg.commands.format)
re-parse HEAD, set_run_shas
if committed: Evidence(format_applied) on the open rebase attempt
maybe_persist_path_instructions
empty = diff_is_empty(onto..HEAD)   # post-format
```

`start_phase` already returns the rebase `AttemptId`; pass it in. Do not call
`evidence_open` — that helper `open_attempt`s a *new* attempt then writes
Evidence, which is how fixer resume works and is the wrong shape here.

Format failure is `Err` out of `run_rebase`. The existing `fail_run_with_phase`
walk finds the open rebase attempt and terminalizes it `failed`. That is a run
failure, not a park. Park stays the conflict outcome.

The parked-rebase retry (`lib.rs` after `porch_git::rebase` succeeds, before
the emptiness skip) calls the same helper, loading `Commands` via the existing
trusted-SHA loader so the pin is not refreshed. Deliver-repair rebase is not
on that helper's call graph.

The new commit argv is the certify helper's argv plus `-c commit.gpgsign=false`.
ADR-0004's five-site sweep is out of scope; this site is new and would otherwise
invoke the operator's signing program *before* review.

`git add -A` stays unfiltered. The same dirt that today entered a post-approval
commit now enters the reviewed range. A path filter is a different product
question.

## 3. Equality at the boundary

```rust
let head = porch_git::rev_parse_c(wt, "HEAD")?;
if head != approved {
    return Err(... naming both ...);
}
Ok(approved)
```

Not `Ok(head)`. Observationally identical at the decision instant; the source
of truth is the authorized SHA, which is what FWDAUTH-1.4 asked for and what
ROAD-7 recorded as unmet when it returned the re-read.

`assert_head_continuity` stays a `map(|_| ())` over that function. Empty-diff
skip never calls it (EQUAL-3.6 / FWDAUTH-7.1).

## 4. What does not move

`applicable_round_id_tx` and `tip_complete_seed` keep `to_sha == runs.head_sha`.
That lookup is why a post-approval rewrite makes `porch agent status` say
`not_reviewed`. Stopping the rewrite makes the lookup true again for this case.
Widening it would make a round cover a SHA it did not review.

The audit anomaly `inconsistent_review_state` fires only on `parked|running`.
ADR-0003 overclaimed it for completed runs. This wave does not change the
anomaly. Status `not_reviewed` on a completed drifted *historical* row is left;
new runs will not produce that drift from certify.

Compose's later `rev-parse HEAD` for PR attestation is not the forward. Once
HEAD no longer drifts, that re-read matches the approved SHA. Do not fold it
into 1.4.
