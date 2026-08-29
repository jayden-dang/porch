# Briefing: why porch exists

Product briefing, 2026-08-29. Dogfood target: **[mailgate](../../../work/CommandOss/mailgate)**. Clone file index: [references.md](references.md). This file is the narrative.

## The bottleneck

AI agents produce diffs faster than humans can validate them. Pre-commit hooks must stay light and they block the working tree. CI runs **after** the branch is public. Branch protection rejects bad outcomes; it does not get a branch ready.

The missing object is an **inner gate**: opt-in, local, isolated, before anyone else sees the change — and honest about what it does *not* certify.

## What porch is for

A Go-style always-on git proxy with a nine-step pipeline, a nine-agent matrix, and six forges is the wrong shape. A generic coding agent asked to “review the branch” cuts corners on large changesets, drifts line numbers, and fluctuates with prompt wording. Production CI on a repo like mailgate includes deploy and spend-money E2E — babysitting *all* PR checks is dangerous.

Porch keeps the inner-gate job and drops the rest:

- Named remote as consent (`origin` untouched).
- Disposable worktree so the author keeps working.
- Human owns judgment; mechanics can auto-fix. Unclassified findings fail closed to the human.
- Reviewer and fixer are separate sessions; pipeline-authored code is re-reviewed as new code.
- Trusted config: shell/agent selection from default-branch SHA, not the pushed branch.
- Force-push never blind; custody recovery after crash.
- Agent-first CLI (`porch agent`) plus a later TUI.
- “Pass” is opinionated: you cannot delete core phases via standing config.

Review is a **constrained tool loop** with file grouping, language rules, line anchors, and a coverage manifest — composed year-1 as an external CLI subprocess. The reviewer has no shell and makes no edits. Porch still owns the gate, the fixer, intent, certify, and deliver.

Values: one gate one meaning; never lose work; judgment stays human; independent adversarial validation; evidence over confidence; humans and agents are both first-class; not a CI system.

## mailgate (CommandOSS)

Production monorepo: Rust crates (gateway, enclave/TEE, SMTP/IMAP, persistence), Bun/TS apps, Move contracts, Pulumi infra, Cloudflare Workers, Nitro EIF.

CI is **two rings**, on purpose:

1. **PR Checks** (`pr.yaml`) — path-filtered: lint, types/drift, MCP conformance, JS tests, infra, Move tests, canon-schema, cargo audit. This is what a pre-PR gate may babysit.
2. **CI/CD** (`ci.yaml`) on push to `dev`/`staging`/`main` — deploys. Concurrency **does not cancel** (mid-deploy would desync Coolify/on-chain). Then **E2E** (`workflow_run`) against the deployed `dev` environment, spending dedicated testnet SUI + USDC. GitHub-hosted runners cannot boot `just dev`.

Lefthook: pre-commit format/secrets; pre-push clippy/audit/machete; **explicitly not unit tests**. Generated TS bindings from Rust: local tests can pass, CI `types:check` fails. Cold `cargo` on the enclave tree is 6–15 minutes and fills disk.

Lesson: porch’s inner gate is valuable **before PR Checks**. It must not pretend to be CI/CD or E2E. Allowlist check names. Never `commands.test = just gate`. Never raise transient reruns that re-spend testnet.

## The bet

A local git gate in **Rust**, with coverage-enforcing, line-anchored review (grouping, language rules, coverage manifest) via an external CLI, and mailgate-shaped humility about outer CI.

Implemented in Rust because the dogfood target and the team are Cargo-native — not because Rust magically reviews better. The review quality upgrade is the review engine, not rustc.

## Name

`porch` — 5 letters, works as `git push porch`, everyday English, not a generic `gate`/`ci`/`review`. You stand on the porch before stepping onto the street (`origin` / production CI). crates.io crate id `porch` was free (2026-08-29). Rejected: crowded working titles, `knocker` (port-knocking CLIs), obscure ceramic jargon, `airlock` (long), `proof` (math), `ward`, `hatch`, Vietnamese CLI names.

## Product shape

| Dimension | porch |
|---|---|
| Review | External CLI: groups, rules, line anchors, coverage manifest |
| Pipeline | 5 phases; certify is adapters |
| Local tests | Forbidden as default; targeted only |
| CI step | Allowlist of PR **quality** checks; no deploy/E2E |
| Agents | ACP + 1 native |
| Forges | GitHub |
| Language | Rust |
| Review auto-fix | Default 0 |
| Reviewer ≠ fixer | Required |

Advertise: **the gate whose review is actually good, and that stops before production CI.**
