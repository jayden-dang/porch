# Project configuration (agent-facing)

Written by `configure-repo`. Skills read this file for repo-specific **machine config** —
commands, globs, paths — plus **posture** and **team** (below). Human-facing engineering
guidelines (coding standards, naming, house rules) live in `docs/product/guidelines.md`;
`plan-tasks` sources them from there.

## Project posture

The project's standing intent and lifecycle phase. Skills read this instead of re-asking:
`frame-change` and `clarify-decisions` right-size how much they weigh data migration, backward
compatibility, and deprecation against it; `interpret-session` reuses it as session context.
Edit these two lines directly whenever the project moves phase — no wizard needed.

- **Delivery intent:** `Production` — published crate (`cargo install porch`), fail-closed
  safety rules, `unsafe_code = "forbid"`, clippy `all = deny`.
- **Lifecycle stage:** `Cut Released` — `v0.2.0`–`v0.2.2` tagged and on crates.io; dogfood
  on mailgate/klynt not yet started.
- **Default PR base:** `main`
- **Library docs:** Context7 MCP (preferred) — `research` and `design-solution`
  resolve third-party library facts through it rather than from training knowledge.

These are distinct from the product **Goals** in `docs/product/vision.md` (what success
looks like): posture is *how carefully to build right now*, not *what to build*.

## Team

Who works on this repo and how skills should package collaboration.
Skills that plan, review, or hand off read this section when present and
right-size **packaging** only (Solo / Small / Multi) — Iron Law gates never
change. Edit freely; re-run `/configure-repo` to re-draft from git/CODEOWNERS.
If this section is absent, skills do not invent a team.

**SSOT:** **band** derivation and the **packaging** matrix live only here.
Consumers **read** this section; they do not re-copy these rules into skill bodies.

### Roster

- Tech Lead — Jayden Đặng

Suggested roles (freeform allowed): Tech Lead, Backend Engineer, Frontend Engineer, Full-stack Engineer, Designer, Product Manager, QA, DevOps/SRE, Docs.

### Ownership notes (optional)

None — this repo has no `CODEOWNERS` file.

### Workflow band

- **Override (optional):** `` — blank; band is derived.
- **Derive (when override blank):**
  1. Headcount from **Roster only**: each `Role — Name` = 1; each `N × Role` / `N Role(s)` adds N.
     Ignore Ownership notes and placeholders (`<…>`).
  2. Buckets: empty roster → **no band** (same packaging as Team absent); 1 → **Solo**;
     2–4 → **Small**; ≥5 → **Multi**.
  3. Specialty upgrade only: if **Small** and ≥3 distinct role titles (case-insensitive, trimmed),
     upgrade to **Multi**. Never downgrade Multi→Small.

Currently derives to **Solo** (roster headcount 1).

### Packaging matrix

| Band | Packaging |
|---|---|
| **Solo** | Lean multi-person ritual language; no invented peer reviewers/assignees; agent-as-pair; full gates |
| **Small** | Design-review checkpoints; ownership boundaries via optional freeform notes; name people when roster has names |
| **Multi** | CODEOWNERS-aware review language when ownership notes exist; explicit review responsibilities as prose; write-handoff/docs emphasis |
| **(no band)** | Team absent, or empty roster with blank override — pre-feature default; do not invent a team; do not hard-fail |

## Decision boundaries

Optional. When present, `record-verdict` reads this table. Pins may raise a
floor or bind an action to a boundary type. An entry that would lower a core
floor is ignored with a one-line notice. Absent section → core table only.

| Action | Boundary-Type | Floor |
|---|---|---|
| <e.g. land-branch:discard> | <disposal> | <Accountable> |

## verify commands

Run in this order; all must pass before any completion claim.

| Check | Command |
|---|---|
| Format | `cargo fmt --all --check` |
| Typecheck | `cargo check --workspace --all-targets` |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` |
| Unit tests | `cargo test --workspace` |
| E2E / smoke | none — the `mN_*` integration tests run under Unit tests |

Single test file: `cargo test -p <crate> --test <file-stem>` — e.g. `cargo test -p porch --test m5_certify`

**`--workspace` is not optional.** `default-members = ["crates/porch"]`, so a bare
`cargo test` silently skips the `porch-gate`, `porch-git`, and `porch-quality` tests.

The traceability check is not a command here — the `audit-trace` skill runs it as
`grep`/`git` over `docs/specs/` (and `docs/architecture/`). It is **docs-only**
and does not grep application tests for requirement IDs.

Do **not** add `/// REQ:` or `@CODE-N.M` annotations to Rust source or test names.

Specs directory override: (blank — `docs/specs/`)

## Run locally (dev)

How to start the app for user-facing acceptance checks (read by `validate-api`
and `validate-ui`).

| Surface | Start command | Ready signal |
|---|---|---|
| CLI | `cargo run -p porch -- <subcommand>` | `cargo run -p porch -- doctor` exits 0 |
| Daemon | `cargo run -p porch -- daemon run` | daemon state under `$PORCH_HOME` (default `~/.porch`) |
| Frontend | — | no web surface |

Browser E2E (Playwright, Chromium): none — porch has no browser surface.

## Remote environments

Read by `debug-remote` and `assess-observability`.

**None — not deployed.** Porch runs locally as a CLI + daemon on the operator's
machine; there is no staging or production environment to query.

## release steps

Consumed by `cut-release`. Ordered; do not reorder step 5.

1. Bump version: `[workspace.package] version`, `crates/porch/Cargo.toml` version, and
   every `porch-* = { version = "X.Y.Z", path = … }` workspace dependency.
2. Add the release section to `CHANGELOG.md`.
3. Update the `PORCH_GIT_REF` default and the curl one-liner ref in `install.sh` to `vX.Y.Z`.
4. `cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace`
5. `cargo publish --locked -p <crate>` in dependency order:
   `porch-git`, `porch-agent`, `porch-deliver`, `porch-quality`, `porch-review`,
   `porch-gate`, `porch-run`, `porch`
6. `git tag vX.Y.Z && git push origin vX.Y.Z`
7. Smoke: `cargo install porch --locked` → `porch --version` && `porch-quality --version`

## Paths

- Specs: `docs/specs/`
- ADRs: `docs/adr/`
- Glossary: `CONTEXT.md`
- Out-of-scope KB: `.out-of-scope/`
- Engineering guidelines: `docs/product/guidelines.md`
- Product vision / architecture spine: `docs/product/vision.md`, `docs/architecture/`
