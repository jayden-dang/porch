# Product Vision: Porch

Status: Approved
Date: 2026-08-30

## Problem

Code production is no longer the bottleneck in a development lifecycle; independent
review is. An agent can produce more changed lines in an hour than anyone reviews,
and the same agent that wrote the change is the one asked whether it is sound. What
is missing is a final self-serve assurance step that runs after a change is finished
and *before* it reaches a shared remote or becomes a PR — one that judges whether the
change is correct, clean, maintainable, and appropriate for this specific project.
Adding such a gate today usually means taking over `origin`, replacing the team's CI,
or adopting a platform.

## Users

Developers and the agentic harnesses they work through, who want an independent
review and a cheap certification pass to happen before anything reaches `origin` —
locally, opt-in, without changing what the rest of the team does. Porch-native review
runs through the harness the operator already uses and never asks for a second
model-provider account; an external producer may run on its own runtime instead.
First dogfood consumers are the mailgate and klynt repositories; both keep their
existing CI.

## Goals

Each goal carries a bold **GOAL-N** ID, flat and repo-wide, so a roadmap milestone can
cite the goals it serves. Same immutability rules as **ARCH-N**: unique forever, never
renumbered, never reused; retire one by striking it through with a reason.

- **GOAL-1** Porch never initiates a forward unless a complete assurance record and
  reviewed-input binding were durably persisted beforehand; after crash, kill, or
  restart it reconciles ambiguous external effects before another forward.
  *Checkable by fault-injection tests asserting no unauthorized forward, no duplicate
  or unsafe retry, no approval outliving its evidence, and discovery of a push that
  completed before the crash.*
- **GOAL-2** Every assurance outcome is auditable: an operator can trace it to the
  reviewed range, producer and version, coverage state per changed file, findings,
  disposition and authority events, and phase events.
- **GOAL-3** A named, versioned effectiveness baseline artifact exists for mailgate and
  klynt and validates against a versioned contract carrying metric definitions,
  denominators, observation windows, exclusions, adjudication rules, results for both
  consumers, and explicit `unavailable(reason)` values. File existence alone does not
  satisfy this goal.
- **GOAL-4** When automatic reconciliation cannot complete, the operator can inspect
  state, recover every reachable porch-authored commit, and detach porch from the
  checkout — without a healthy daemon, and without hand-editing hooks, git config,
  refs, or the database.

Numeric effectiveness targets are deliberately absent: they receive **new** `GOAL-N`
IDs once the **GOAL-3** baseline exists, and are never inserted into a goal above.

## Non-goals

- Replacing a consumer repository's CI.
- Owning or rewriting `origin` (see **ARCH-1**).
- Merging, deploying, releasing, or becoming a hosted service.
- Requiring a second model-provider account, API key, or separately billed token path.
  Review consumes the harness's own inference budget.
- Choosing or requiring a different model from the writer: model routing and vendor
  diversity are not porch's differentiation — the assurance protocol and review
  quality are.
- A per-product adapter for every review tool (see **ARCH-8**).
- Guarantees over `$PORCH_HOME` corruption, disk loss, host loss, uncommitted edits,
  or process memory.

## Scope boundaries

- Porch owns the assurance protocol end to end — inventory, required coverage,
  normalization, reconciliation, authority, SHA binding, and the fail-closed outcome.
  A producer's success verdict is evidence, never a porch approval (**ARCH-11**).
- Porch-native review is the default path and stays usable with no additional review
  service installed. External producers may supply the judgment layer only at a
  declared, producer-independent bar; shortfall makes the run `incomplete` and fails
  closed. The deterministic floor always runs (**ARCH-12**).
- External producers own their own runtime, network access, credentials, provider
  configuration, and cost. Only porch-native review runs through the operator's
  harness.
- Day-1 forge: GitHub. Day-1 agents: ACP plus one native CLI.
- Everything runs locally; state lives under `$PORCH_HOME` (default `~/.porch`).
- Rust 1.85+, edition 2024, and a working `git` are the only hard requirements.
- Distribution is `cargo install porch --locked` plus the tagged `install.sh` one-liner.
