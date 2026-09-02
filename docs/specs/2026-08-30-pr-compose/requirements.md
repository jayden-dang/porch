# Requirements: PR Compose

Feature code: PRCMP
Status: Implemented
Date: 2026-08-30

Roadmap item: — (not a pre-planned ROAD slot; DELIVER-adjacent).
Respects: ARCH-1, ARCH-3, ARCH-4, ARCH-5, ARCH-7, ARCH-8, ARCH-11, ARCH-13.

Approach: B — first-class `compose` park after forward. Deliver CUJ becomes
lease-push → scaffold PR → park `phase=compose` → Agent respond|skip|abort →
then allowlisted check watch when configured. Outer pipeline label may still
read as ending in deliver; compose is a distinct parked phase.

## 1. Scaffold PR after forward without self-review theater

**Story:** As an Operator, I want a GitHub PR to exist as soon as the certified tip
is lease-pushed, with a public body that explains the change — not porch's inner
gate theater — so that outer reviewers are not confused by self-review dump.

- **PRCMP-1.1** WHEN deliver has successfully lease-pushed the certified tip THE
  SYSTEM SHALL create a GitHub PR if none is open for that head branch, or update
  the open PR's porch-managed body regions if one exists, before entering compose
  park.
- **PRCMP-1.2** WHEN the scaffold (or updated) PR body is written THE SYSTEM SHALL
  make the visible markdown free of porch inner-gate theater: it SHALL NOT include
  visible sections named or equivalent to Review findings dump, Certify step
  transcript, Pipeline step board, or “approved at SHA” self-review status.
- **PRCMP-1.3** WHEN the scaffold (or updated) PR body is written THE SYSTEM SHALL
  NOT use a raw changed-path list as the sole “what changed” narrative.
- **PRCMP-1.4** WHEN the scaffold (or updated) PR body is written THE SYSTEM SHALL
  append a hidden HTML comment attestation that binds the delivered `head_sha`
  (and SHALL CONTINUE TO redact home-path prefixes from the composed body).
- **PRCMP-1.5** WHEN no consumer PR template is present at the template source of
  truth THE SYSTEM SHALL use porch's default visible skeleton with sections for
  Summary (what), Why, How tested, and Links (optional Notes for reviewers / AI
  disclosure may be present).

## 2. Respect the consumer PR template

**Story:** As an Operator of a consumer repo, I want porch to fill my repo's PR
template when I have one, so that team checklist slots are not replaced by porch
defaults.

- **PRCMP-2.1** WHEN a pull-request template file exists at the trusted
  default-branch SHA (`trusted_config_sha`) under GitHub's documented locations
  THE SYSTEM SHALL use that template bytes as the scaffold body structure for
  both the immediate scaffold and the compose packet.
- **PRCMP-2.2** IF no such template file is readable at that trusted SHA THEN THE
  SYSTEM SHALL use the porch default skeleton from PRCMP-1.5.
- **PRCMP-2.3** THE SYSTEM SHALL load template bytes from the trusted
  default-branch SHA only — never from the pushed feature tip alone — and SHALL
  use the same template bytes for Agent compose and for scaffold fallback.
- **PRCMP-2.4** WHERE multiple templates exist under
  `.github/PULL_REQUEST_TEMPLATE/` THE SYSTEM SHALL select a single documented
  default (design locks which file); it SHALL NOT invent a template query-parameter
  UX in this feature.

## 3. Park compose and hand a packet to the Agent

**Story:** As an Agent driving a run headlessly, I want deliver to park after the
scaffold PR exists and give me a compose packet, so that I write the PR title/body
from facts rather than porch inventing prose.

- **PRCMP-3.1** WHEN the scaffold PR create-or-update has succeeded THE SYSTEM
  SHALL park the run with `status=parked` and `phase=compose` and SHALL NOT
  treat deliver as completed until compose is resolved.
- **PRCMP-3.2** WHEN entering compose park THE SYSTEM SHALL persist a compose
  packet the Agent can read (path and/or inline fields exposed on status) that
  includes at least: intent (if any), base/head SHAs, branch names, PR URL and
  number, template-or-default skeleton in force, and a non-path-dump change
  summary sufficient to author What/Why (exact schema is an Open Question).
- **PRCMP-3.3** THE SYSTEM SHALL NOT author the human-readable PR prose itself
  as the primary path; prose comes from Agent `respond` or remains the
  deterministic scaffold placeholders when compose is skipped.
- **PRCMP-3.4** WHILE the run is parked in `phase=compose` THE SYSTEM SHALL expose
  via status/RPC the `pr_url`, compose-packet locator, and allowed actions
  `respond`, `skip`, and `abort`.
- **PRCMP-3.5** THE SYSTEM SHALL NOT auto-timeout compose park; it waits until
  `respond`, `skip`, or `abort`.

## 4. Resolve compose: respond, skip, or abort

**Story:** As an Agent, I want to submit polished title/body, accept the scaffold,
or abort the run, so that unattended and attended paths both finish cleanly.

- **PRCMP-4.1** WHEN the Agent or Operator responds to compose park with a title
  and body THE SYSTEM SHALL merge the response into porch-managed body regions,
  preserve non-porch-managed regions when parseable, refresh the hidden
  attestation for the delivered `head_sha`, apply title rules in §5, and then
  complete compose so deliver may finish (including allowlisted check watch when
  configured).
- **PRCMP-4.2** WHEN the Agent or Operator skips compose park THE SYSTEM SHALL
  leave the scaffold PR body in place (still without self-review theater), mark
  compose as resolved with scaffold provenance, and complete deliver without
  failing the run for lack of Agent prose.
- **PRCMP-4.3** WHEN the Agent or Operator aborts compose park THE SYSTEM SHALL
  fail the run and SHALL leave the existing GitHub PR open (no automatic
  `gh pr close`).
- **PRCMP-4.4** IF Agent-supplied body fails validation (empty required scaffold
  structure, or attempts to reintroduce forbidden self-review theater sections)
  THEN THE SYSTEM SHALL reject the respond and remain parked in `phase=compose`
  with an error the Agent can read.

## 5. Title and redeliver semantics

**Story:** As an Operator, I want porch to set a meaningful title on create and not
clobber a title or checklist I edited on GitHub, so that redeliver is safe.

- **PRCMP-5.1** WHEN creating the scaffold PR (before compose park resolves) THE
  SYSTEM SHALL set the title to an improved deterministic subject derived from
  intent or commits — not solely `porch: {branch}` as the permanent public title.
- **PRCMP-5.2** WHEN compose `respond` supplies a title, or when updating an
  existing open PR on redeliver, THE SYSTEM SHALL change the title only if the
  current title is still classified as porch-managed; otherwise THE SYSTEM SHALL
  leave the human title unchanged.
- **PRCMP-5.3** WHEN updating an existing open PR body THE SYSTEM SHALL refresh
  only porch-managed regions (including hidden attestation) and SHALL preserve
  operator/human regions when they can be parsed as such.
- **PRCMP-5.4** (guard) WHEN an open PR already exists for the head branch THE
  SYSTEM SHALL CONTINUE TO prefer `gh pr edit` for body updates rather than
  opening a duplicate PR.

## 6. Quality attributes

**Section-kind:** nfr

**Story:** As a stakeholder, I want measurable quality targets for this feature, so that how-well is not left implicit.

- **Performance:** None — compose park is human/Agent paced; no standing latency SLO for PR prose authorship (`docs/product/metrics.md` / `docs/ops/reliability.md` absent).
- **Security:** **PRCMP-6.1** WHEN composing or scaffolding a PR body THE SYSTEM SHALL CONTINUE TO redact home-directory path prefixes from body text before sending it to `gh`, verified by unit tests of the redaction helper. (No standing TB/THR IDs — `docs/security/threat-model.md` absent.)
- **Reliability:** **PRCMP-6.2** WHEN lease-push has succeeded and scaffold PR create-or-update succeeds THE SYSTEM SHALL leave a recoverable open PR even if compose stays parked or is skipped, verified by integration coverage of scaffold-then-skip and scaffold-then-abort (PR remains). No auto-timeout (close-package reliability lock).
- **Accessibility:** None — no browser UI; operator surfaces remain CLI/TUI/headless Agent.

## 7. Guards for existing deliver behavior

Touched surfaces: `crates/porch-deliver/src/lib.rs`, `crates/porch-run/src/deliver.rs`,
`crates/porch/src/` (agent status/respond, TUI), agent skill contract under
`crates/porch-gate/porch-agent.md` / installed skill docs.

- **PRCMP-7.1** (guard) WHEN deliver runs THE SYSTEM SHALL CONTINUE TO lease-push
  with `--force-with-lease=<ref>:<observed-sha>` and refuse when live remote
  commits are not incorporated.
- **PRCMP-7.2** (guard) WHEN deliver config is loaded THE SYSTEM SHALL CONTINUE TO
  read executing deliver/`pr` fields from the trusted default-branch SHA, not the
  pushed tip.
- **PRCMP-7.3** (guard) WHEN `deliver.github.watch_checks` is non-empty THE SYSTEM
  SHALL CONTINUE TO babysit only those allowlisted check names (never an
  unbounded check set).
- **PRCMP-7.3a** WHEN compose has resolved (`respond` or `skip`) and
  `deliver.github.watch_checks` is non-empty THE SYSTEM SHALL run that
  allowlisted babysit only after compose resolution — not while still parked in
  `phase=compose`.
- **PRCMP-7.4** (guard) WHEN `deliver.github.rerun_transient` is greater than zero
  THE SYSTEM SHALL CONTINUE TO perform no `gh run rerun` (no new rerun surface
  in this feature; ARCH-7).
- **PRCMP-7.5** (guard) WHEN a run is parked at rebase or review THE SYSTEM SHALL
  CONTINUE TO accept the existing park response verbs for those phases without
  requiring compose actions.
- **PRCMP-7.6** (guard) THE SYSTEM SHALL CONTINUE TO keep reviewer turns
  session-free and SHALL NOT collapse reviewer, fixer, and Agent PR-author roles.

## Out of Scope

- Spawning a porch-owned PR-composer subprocess as the primary author of PR prose.
- Replacing or orchestrating consumer CI; merging; deploying.
- Auto-closing or auto-drafting the GitHub PR on compose abort.
- Pasting porch Review / Certify / Pipeline / findings into the visible PR body.
- Multi-template picker UX / `template=` query-parameter flow beyond a single
  documented default when several templates exist.
- Forges other than GitHub (ARCH-8).
- Changing ROAD-6 / ROUND identity work.

## Open Questions

None — owned unknowns resolved in `./design.md` Decisions §8 (packet path/schema,
managed markers, title heuristic, multi-template pick order).
