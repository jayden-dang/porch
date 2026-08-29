# Source: mailgate (dogfood target)

Clone: [../../../work/CommandOss/mailgate](../../../work/CommandOss/mailgate). Production-intent Web3 mail: gated inbound, Sui contracts, TEE enclave, SMTP/IMAP, Stripe, etc. This is the repo porch must help **without replacing**. File list: [references.md](references.md).

## Shape

Monorepo: Bun workspaces (`apps/web`, `apps/docs`, `apps/e2e`, `apps/enclave-registry`, `packages/*`) + root Cargo workspace (`crates/gateways`, `enclave`, `auth_service`, `persistence`, `imap_protocol`, `smtp`, `mcp`, …) + `contract/` (Move) + `infra/` (Pulumi, SoT for every env/secret).

Agent culture is spec-driven (`brainstorm` → `write-plan` → `execute-plan`) with many `AGENTS.md` files. That is a **jailbreak risk** for a gate agent running with cwd = worktree unless instructions are neutralized.

## CI is two rings

```
feature PR ──► PR Checks (pr.yaml)          ◄── porch deliver MAY babysit
                 path filter:
                 lint, docs drift, types/bindings,
                 mcp-conformance (cargo test -p mcp + enclave inventory),
                 test-js, test-infra, test-contract,
                 plus sibling workflows: canon-schema, cargo-audit
                    │ merge
                    ▼
             push dev|staging|main ──► CI/CD (ci.yaml)
                 Workers / EIF / Coolify / on-chain Move
                 concurrency: cancel-in-progress: false
                    │ success on `dev`
                    ▼
             E2E (e2e.yaml)  Playwright on deployed env
                 spends dedicated testnet SUI + Circle USDC
                 120 min; secrets must not skip journeys
```

Release: `dev → staging → main` with `contract_mode` skip|safe-upgrade-only (`force-publish` is explicitly not a release).

Canon schema: Postgres **17.10-alpine** service, `pg_dump` byte-identical to `schema.golden.sql`. Local `db-reset` comments in `justfile` exist because a naive `CREATE SCHEMA public` diverges and fails this guard.

Cargo audit: weekly cron + lockfile paths — advisory DB moves without commits.

Lefthook: pre-commit biome/rustfmt/file-size/gitleaks; pre-push clippy/format/audit/machete; **not** unit tests (`just test-push` is manual). Types drift is CI-only because it shares cargo’s `target/` lock with clippy (~22s locally, still).

## Commands that look like “the test step” and must not be porch certify defaults

From `docs/agents/project.md`:

| Check | Command | Porch? |
|---|---|---|
| Biome | `bun run check` / `check:fix` | Certify adapter, yes |
| Clippy | `just clippy` workspace `-D warnings` | Path-aware subset, maybe; full workspace cold worktree is expensive |
| rustfmt | `just fmt-check` | Yes |
| TS types | `bun run check:types` | Yes |
| Drift | `bun run types:check` `api:check` `docs:check` | **Yes — this is the class porch exists to catch before CI** |
| Unit TS | `bun run test` | Targeted by intent, not default full |
| Unit Rust | `just test-push` (no Postgres) | Targeted crate, not workspace |
| Integration | `just test` needs Postgres | **No** (not default) |
| Move | `just verify-contract` | If contract files changed |
| E2E | Playwright; self-start or post-deploy | **No** |
| Full Rust | `just gate` | **No** |
| `just verify` | Explicitly **does not** run Rust workspace | Do not confuse names |

Cold enclave compile: 6–15 min, disk reclaim composite action, rust-cache `shared-key: workspace`, `CARGO_INCREMENTAL=0`. A porch worktree does not have that cache. Reviewer/fixer with free shell **will** spawn `cargo test --workspace` unless prompts and process limits forbid it.

## What porch should do for mailgate

1. Independent review of enclave/auth/persistence/contract diffs **before** the PR is public (`ask-user` for schema/on-chain/env).
2. Catch generated-binding drift (`types:check` / `api:check` / `docs:check`) and Biome/fmt **before** burning GitHub runners.
3. After PR open: babysit **PR Checks** allowlist (`lint`, `types-check`, `mcp-conformance`, `test-js`, `test-contract`, `canon-schema`, `cargo-audit`, …). Auto-fix mechanical fails; rereview; re-push lease.
4. Rebase onto `dev` when another PR merged (conflict) — CI/CD on `dev` cannot cancel in-flight deploys, so landing a conflicted PR is expensive.

## What porch must never do on mailgate

- Replace `pr.yaml` / `ci.yaml` / `e2e.yaml` / release.
- `commands.test = just gate` or `just test` or Playwright.
- Rerun E2E or Coolify or `force-publish` via `ci.rerun_transient`.
- Let a contributor set `no_ci: true`.
- Auto-fix review on enclave/contract by default.
- Run as an agent that **obeys** repo `AGENTS.md` as if it were the fleet captain (neutralize project instructions).
- Publish secrets, wallet addresses, or home paths in PR bodies (redact home-directory prefixes).
- `test.evidence.store_in_repo` if artifacts can contain keys/addresses.

## Canonical config

**Canonical file:** [`examples/mailgate.porch.yaml`](examples/mailgate.porch.yaml).

Trusted default-branch only for executing fields. `pr.base_branch: dev` matches how they open PRs (once a PR exists, the live forge base wins).

**Skip-as-Ready:** most PR Checks jobs are path-filtered. Allowlisted checks that report skip / skipped / skipping / neutral (state or bucket) are **Ready for that name**; a missing name is still Waiting. Without skip-as-Ready, babysit stalls on narrow PRs.

Never put `just gate` / `just test` / e2e / Coolify in certify or `watch_checks`. Keep `rerun_transient: 0`.
