# Design: PR Compose

Feature code: PRCMP
Status: In-progress
Date: 2026-08-30
Requirements: ./requirements.md

## Context

Today deliver lease-pushes the certified tip, then builds a fixed markdown body
(`Intent` / path-list `What Changed` / `_not assessed_` Risk / Review / Certify /
Pipeline) plus a hidden `porch-attestation` HTML comment, and creates or
body-edits a GitHub PR (`porch-run` `deliver.rs` → `porch-deliver`
`build_pr_body` / `create_pr` / `edit_pr_body`). Titles are `porch: {branch}` on
create only. No `.github` PR template is read. Park exists only for `rebase` and
`review`; Agent `respond` verbs are approve/skip/abort/fix with review-centric
skip semantics (skip review also skips certify+deliver).

The binding constraint is the approved close package: porch is an **inner**
self-review gate, so the **public** PR must carry What/Why/How-tested (or the
consumer template), not gate theater; porch must not author that prose — it
hands a compose packet to the **Agent** after a scaffold PR exists. The
rejected alternative is a porch-spawned composer subprocess inside deliver
(would blur ARCH-3 roles and contradict “Agent writes from facts”).

Spine reliance: ARCH-1 (consent remains `git push porch`), ARCH-3 (Agent ≠
reviewer ≠ fixer), ARCH-4 (template + deliver config from trusted SHA), ARCH-5
(lease-push unchanged), ARCH-7 (allowlist watch after compose resolves), ARCH-8
(GitHub only), ARCH-11 (no producer verdict as visible “approval”), ARCH-13
(forward already authorized before PR prose; prose is not authorization).

Neighbors (fresh retrieval for PRCMP): **DELIVER** (body/PR adapters), **RUN**
(deliver orchestration + agent respond), **OPERATOR** (CLI/TUI), **GATE**
(park/status RPC), **AGENT** (fixer only — leave). ROUND shares `porch-run`
paths but is orthogonal.

## Decisions

1. **Compose park inside Deliver** — `PHASES` stays
   `intent → rebase → review → certify → deliver` (compose is **not** a pipeline
   phase). After scaffold, record a **step_results** row `step=compose`,
   `status=parked` so existing `parked_phase` returns `"compose"` for status/
   respond branching. The deliver PHASES iteration pauses until compose
   resolves, then finishes watch under step `deliver` completed.
2. **Order:** lease-push → scaffold create/edit → park compose → Agent
   respond/skip/abort → allowlisted watch (if any) → complete.
3. **Scaffold body:** consumer template from trusted SHA when present; else
   default Summary / Why / How tested / Links. Never visible Review/Certify/
   Pipeline/findings. Keep hidden `<!-- porch-attestation … -->`.
4. **Agent authors prose** via compose respond carrying title+body; porch merges
   porch-managed regions + refreshes attestation.
5. **Skip ≠ review-skip:** compose `skip` accepts scaffold and **continues**
   deliver (watch + complete). Review `skip` behavior unchanged.
6. **Abort:** fail/cancel run; leave GitHub PR open.
7. **No auto-timeout** on compose park.
8. **Open questions locked here:**
   - **Packet path:** `$PORCH_HOME/runs/<run_id>/compose-packet.json` (via
     existing `run_artifact_dir`; absolute path also in status).
   - **Packet schema (v1):** JSON object with
     `schema_version` (1), `run_id`, `repo_id`, `branch`, `base_sha`, `head_sha`,
     `pr_url`, `pr_number`, `intent`, `title_scaffold`, `body_scaffold`,
     `template_source` (`repo_template` | `porch_default`), `template_path`
     (trusted-tree path or null), `change_summary` (short prose bullets from
     commit subjects + optional intent — not a raw path dump),
     `theater_reject_rules` (structured rules — see body builders), 
     `porch_managed_markers` (`begin`/`end` strings).
   - **Region markers:** wrap the entire visible scaffold in
     `<!-- porch-managed:begin -->` … `<!-- porch-managed:end -->`; attestation
     remains a separate trailing `<!-- porch-attestation … -->` outside the
     managed pair (still porch-owned on every write).
   - **Porch-managed title:** true if title equals the last title porch wrote
     for this PR (stored on the run/DB as `pr_title_written`), OR matches
     `^porch: `, OR equals the current scaffold deterministic title algorithm
     output. Otherwise human-owned — do not edit.
   - **Multi-template pick (trusted tree):** first existing among
     `.github/pull_request_template.md`, `pull_request_template.md`,
     `docs/pull_request_template.md`, then if
     `.github/PULL_REQUEST_TEMPLATE/` is a directory, the lexicographically
     first `*.md` file. No query-parameter picker.

9. **Glossary:** extend **Park** to include `compose` (define-domain sync when
   landing). No ADR — decisions fit existing ARCH spine; no invariant change.

10. **Requirements sync:** PRCMP-5.1 already states deterministic create title;
    Agent title on compose respond under PRCMP-5.2 — no further amendment.

## Architecture

### Body + template builders (`porch-deliver`)

Satisfies: PRCMP-1.2, PRCMP-1.3, PRCMP-1.4, PRCMP-1.5, PRCMP-2.1, PRCMP-2.2,
PRCMP-2.3, PRCMP-2.4, PRCMP-5.1, PRCMP-5.2, PRCMP-5.3, PRCMP-6.1
Reuse: rung 2 — extend `build_pr_body`, `pr_title`, `redact_home_paths`,
`create_pr`, `edit_pr_body` in `crates/porch-deliver/src/lib.rs`; replace
theater-oriented `build_pr_body` section layout with scaffold builders
Respects: ARCH-4, ARCH-8, ARCH-11
Surface:
- `build_pr_body` — **replace** (signature becomes scaffold-oriented; update
  unit tests + `assemble` call sites)
- `pr_title` — **replace** (improved deterministic subject; tests
  `pr_title_deterministic` updated)
- `m14_agent_run` body assertions — **replace** (expect new sections / markers,
  not Intent/Review/Certify theater)
- HTML attestation marker `porch-attestation` — **frozen** (external/tools may
  scrape; keep marker name and `head_sha` binding)
Interface: `load_pr_template(bare, trusted_sha) -> TemplateBytes`;
`build_scaffold_body(template_or_default, facts, attestation) -> String`;
`merge_porch_managed(existing_body, new_visible, attestation) -> String`;
`is_porch_managed_title(current, last_written, scaffold_title) -> bool`;
`deterministic_pr_title(branch, intent, commit_subject) -> String`;
existing `create_pr` / `edit_pr_body`; **new** `edit_pr_title` (`gh pr edit --title`)
Depth: n/a — extends porch-deliver
Locality: edit lands in `porch-deliver`; neighbors `porch-run` deliver **extend**;
fixer crate **leave**

Scaffold default markdown (visible):

```markdown
<!-- porch-managed:begin -->
## Summary

…

## Why

…

## How tested

…

## Links

…
<!-- porch-managed:end -->

<!-- porch-attestation {…} -->
```

When a repo template is used, the template bytes become the interior of the
managed region (placeholders left for Agent fill); attestation still appended
outside.

Theater rejection on respond (not a blanket heading ban): reject if the body
reintroduces **porch theater signatures** — e.g. a `## Pipeline` board of
`intent → … → deliver`, a Certify step transcript block, Review **findings dump**
/ “approved at `<sha>`” lines, or an HTML-free visible restatement of
attestation. Consumer template headings such as `## Review` (human checklist)
are allowed when they come from the trusted template and do not carry those
signatures. Rules ship in packet `theater_reject_rules`.

### Deliver orchestration (`porch-run` deliver)

Satisfies: PRCMP-1.1, PRCMP-3.1, PRCMP-3.2, PRCMP-3.3, PRCMP-3.5, PRCMP-4.1,
PRCMP-4.2, PRCMP-4.3, PRCMP-4.4, PRCMP-5.4, PRCMP-6.2, PRCMP-7.1, PRCMP-7.2,
PRCMP-7.3, PRCMP-7.3a, PRCMP-7.4
Reuse: rung 2 — extend `run_deliver_phase` / `assemble_body` in
`crates/porch-run/src/deliver.rs`; reuse park/status helpers in `porch-run` +
`porch-gate` Db step recording
Respects: ARCH-1, ARCH-5, ARCH-7, ARCH-13
Surface:
- `run_deliver_phase` control flow — **replace** (insert scaffold → park →
  resume)
- `assemble_body` — **replace** (become scaffold assembly + packet write)
- step_results may include `compose`+`parked` — **compat** until TUI/status
  consumers taught (`porch` TUI, `porch agent status`, skill docs) then remove
  assumption that only rebase/review park; follow-up: same PRCMP landing
- attestation step list shape — **frozen** (keep `head_sha` + step snapshots;
  may omit theater section text)
Interface: deliver returns `DeliverOutcome::{ParkedCompose, Completed}`; on
park, writes packet file + sets run `parked` with compose phase metadata;
`resume_deliver_after_compose(home, run_id, resolution)` applies respond/skip
and finishes watch
Depth: n/a — extends deliver
Locality: `porch-run/src/deliver.rs` **extend**; `porch-run/src/lib.rs` pipeline
loop **extend** (handle deliver parked like review parked); ROUND surfaces
**leave**

Flow:

1. `ensure_gh` → lease-push (unchanged).
2. Build scaffold body + deterministic title; create or merge-edit PR.
3. Persist `pr_url`, `pr_title_written`, packet JSON; `record_step(compose,
   parked)` (this row drives `parked_phase` → `"compose"` — never
   `deliver`+`parked` as the parked status source); `set_status(parked)`;
   return without removing worktree.
4. On compose respond: validate body; merge regions; edit PR; clear compose
   park step; mark deliver step completed with detail `compose=agent`; refresh
   attestation steps to post-compose state; watch; done.
5. On skip: complete compose step as skipped/`compose=scaffold`; refresh
   attestation so `deliver` is `completed` (not left `parked`); watch; done.
6. On abort: status failed/cancelled; PR left open; worktree cleanup per
   existing abort paths.
7. **Compose skip MUST NOT** take the review `AgentResponse::Skip` arm (that
   arm skips certify+deliver). Branch on `parked_phase == "compose"` first.

### Agent respond + status (`porch-run` + `porch` CLI)

Satisfies: PRCMP-3.4, PRCMP-4.1, PRCMP-4.2, PRCMP-4.3, PRCMP-4.4, PRCMP-7.5,
PRCMP-7.6
Reuse: rung 2 — extend `agent_respond` / `AgentStatus` / clap in `porch` binary;
mirror review park branching by `parked_phase`
Respects: ARCH-3
Surface:
- `AgentResponse` / `porch agent respond` — **compat** then **replace**: add
  compose path `respond --body-file <path> [--title <str>]` when
  `phase=compose`; reject approve/fix on compose with usage error; keep
  review/rebase verbs unchanged
- status JSON — **compat**: add `pr_url`, `compose_packet_path`,
  `phase=compose`, `allowed_actions` when parked compose
- `porch-agent.md` / installed skill — **replace** (document compose park)
- TUI park — **extend** minimal: show compose park with skip/abort/open packet
  hint (full TUI editor out of scope — Agent/CLI primary)
Interface: `parked_phase` returns `compose` when step_results has
`compose`+`parked`; `agent_respond_compose(title, body)` branched before review
skip; status fields as above
Depth: n/a — extends agent CLI surface
Locality: `porch-run` respond **extend**; `porch` clap **extend**; fixer
`porch-agent` crate **leave**

### Template load from trusted SHA (`porch-git` + deliver)

Satisfies: PRCMP-2.1, PRCMP-2.2, PRCMP-2.3, PRCMP-2.4
Reuse: rung 2 — `git show <trusted_sha>:<path>` via existing git CLI wrapper
patterns in `porch-git` (add thin helper if none)
Respects: ARCH-2, ARCH-4
Surface: new helper — omit Surface (new); callers only deliver scaffold
Interface: reuse existing `show_path_at` (or equivalent) — do not invent a
parallel `show_blob` name unless wrapping it
Depth: n/a — extends porch-git
Locality: `porch-git` **extend** / reuse; deliver calls it

## Seams for testing

| Seam | Kind | Covers |
|---|---|---|
| `porch_deliver::build_scaffold_body` / default skeleton / forbidden headings | unit | PRCMP-1.2, PRCMP-1.3, PRCMP-1.5 |
| `porch_deliver::merge_porch_managed` + attestation append | unit | PRCMP-1.4, PRCMP-5.3 |
| `porch_deliver::is_porch_managed_title` / `deterministic_pr_title` | unit | PRCMP-5.1, PRCMP-5.2 |
| `porch_deliver::redact_home_paths` (existing) | unit | PRCMP-6.1 |
| template pick order via `git show` fake bare | unit/integration | PRCMP-2.1–2.4 |
| deliver: push → scaffold → park compose (fake gh) | integration (`mN_pr_compose` or extend m6/m14) | PRCMP-1.1, PRCMP-3.1–3.5 |
| agent respond compose / skip / abort | integration | PRCMP-4.1–4.4, PRCMP-6.2 |
| review/rebase respond unchanged | integration (existing m8/m14) | PRCMP-7.5, PRCMP-7.6 |
| lease-push + allowlist watch after compose skip | integration | PRCMP-7.1–7.4, PRCMP-5.4 |

## Coverage check

| ID | Satisfies section |
|---|---|
| PRCMP-1.1 | Deliver orchestration |
| PRCMP-1.2–1.5 | Body + template builders |
| PRCMP-2.1–2.4 | Body builders + Template load |
| PRCMP-3.1–3.3, 3.5 | Deliver orchestration |
| PRCMP-3.4 | Agent respond + status |
| PRCMP-4.1–4.4 | Deliver orchestration + Agent respond |
| PRCMP-5.1–5.3 | Body builders |
| PRCMP-5.4 | Deliver orchestration |
| PRCMP-6.1 | Body builders |
| PRCMP-6.2 | Deliver orchestration |
| PRCMP-7.1–7.4, PRCMP-7.3a | Deliver orchestration |
| PRCMP-7.5–7.6 | Agent respond + status |

Unmapped: none.

UI design: deleted — no browser-rendered Satisfies.
