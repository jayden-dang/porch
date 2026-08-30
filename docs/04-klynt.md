# Source: klynt (second dogfood)

Clone: [../../../klynt/klynt](../../../klynt/klynt). Absolute fallback: `/Users/jayden/Developer/klynt/klynt`. Messy CI porch must sit **in front of**, never absorb. Canonical config: [`examples/klynt.porch.yaml`](examples/klynt.porch.yaml).

## Shape

Monorepo orchestrated by **moon** + root **justfile**: Rust backend (`SQLX_OFFLINE` clippy), Bun/Biome frontend, `packages/api-contract` OpenAPI drift, Coolify deploy workflows. Agent culture is skill-chain / ARCH-cite heavy — jailbreak risk for a gate fixer unless project ORDERS are neutralized.

## Default branch vs PR base

| Fact | Value |
|---|---|
| `origin/HEAD` | `main` |
| Team / moon default PR base | `dev` |
| CI triggers | push/PR to `dev`, `staging`, `main` |

Porch `repos.default_branch` follows `origin/HEAD` (`main`). Trusted `pr.base_branch: dev` drives rebase onto / `gh pr create --base`. Do not conflate the two.

## CI is a monolith

```
feature PR ──► CI workflow
                 job "moon ci"     ◄── includes Playwright e2e + coverage (Postgres)
                 job "Security checks"
                 job "CI status"   ◄── aggregate
                    │ merge / push
                    ▼
             Deploy to {dev,staging,production}  ◄── Coolify; NEVER babysit / rerun
```

Lefthook **pre-push** is the expensive local gate (fmt/lint/typecheck/**test-coverage**/build + **Platform E2E**). Porch certify must not mirror it.

**`git push --no-verify porch` is expected.** Porch is not lefthook: the `porch` remote's hooks are porch admit/notify, and lefthook's pre-push suite must not block consent to the inner gate. Use `--no-verify` (or an equivalent skip) when pushing to `porch`; keep lefthook for pushes that actually target `origin`.

There is **no** cheap GitHub check that is only fmt/lint/drift. Honest `watch_checks: []` — push+PR, no babysit.

## Cheap local commands (certify)

| Role | Command | Notes |
|---|---|---|
| Format | `just fmt` | cargo fmt + Biome write |
| Lint + drift | `moon run :fmt-check :lint :typecheck api-contract:drift-check` | SQLX_OFFLINE clippy; OpenAPI may cold-compile |

Cold clippy / `emit_openapi` compiles are an operator concern (sccache helps). Still do **not** run `moon ci` or Playwright as certify.

## What porch should do for klynt

1. Independent review with path instructions for auth, migrations, persistence, api-contract, deploy workflows (`ask-user` when scope expands).
2. Cheap certify: format + lint/typecheck/drift **before** burning `moon ci` runners.
3. Open PRs against **`dev`**. Empty allowlist — do not pretend to babysit the monolith.

## What porch must never do on klynt

- Absorb or re-run `moon ci` / `CI status` / `Deploy to *` / `AI code review`.
- `commands.test = just test` / `just e2e` / Playwright / Postgres suites.
- Mirror lefthook pre-push as certify.
- Obey klynt skill-chain / ARCH cite / audit-trace ORDERS inside the fixer.
- Publish secrets or home paths in PR bodies.

## Canonical config

See [`examples/klynt.porch.yaml`](examples/klynt.porch.yaml). Research notes (gitignored): `.skills/research/2026-08-29-m7-klynt.md`.
