# Product Vision: Porch

Status: Draft
Date: 2026-08-30

Seeded by `configure-repo` from the README framing. Run `/define-project` to fill
the `GOAL-N` slots and tighten the scope fence — they are placeholders, not
decisions.

## Problem

Code reaches a shared remote before anyone has independently judged it. Review
happens after the push, on the reviewer's time, in the forge's UI — or it does not
happen at all. CI is the only gate, and CI is expensive, remote, and shaped for the
team rather than for the change in front of you. Adding a real gate usually means
taking over `origin`, replacing the team's CI, or adopting a platform.

## Users

Developers who want an independent review and a cheap certification pass to happen
*before* anything reaches `origin`, on their own machine, opt-in, without changing
what the rest of the team does. First dogfood consumers are the mailgate and klynt
repositories — both keep their existing CI.

## Goals

Each goal carries a bold **GOAL-N** ID, flat and repo-wide, so a roadmap milestone can
cite the goals it serves. Same immutability rules as **ARCH-N**: unique forever, never
renumbered, never reused; retire one by striking it through with a reason.

- **GOAL-1** <a concrete, checkable outcome that defines success — fill via /define-project>
- **GOAL-2** <another>

## Non-goals

- Replacing a consumer repository's CI.
- Owning or rewriting `origin` (see **ARCH-1**).
- Becoming a deploy system, a release manager, or a hosted service.
- Broad forge or agent adapter coverage for its own sake (see **ARCH-8**).

## Scope boundaries

- Day-1 forge: GitHub. Day-1 agents: ACP plus one native CLI.
- Everything runs locally; state lives under `$PORCH_HOME` (default `~/.porch`).
- Rust 1.85+, edition 2024, and a working `git` are the only hard requirements.
- Distribution is `cargo install porch --locked` plus the tagged `install.sh` one-liner.
