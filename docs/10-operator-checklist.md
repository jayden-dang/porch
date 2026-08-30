# Operator checklist (M15 workflow freeze)

End-to-end loop **without OCR**: setup → init → push porch → review → park (TUI or JSON) → cheap certify → PR. Tagged **0.2.1** (crates.io). Install: [install.md](install.md). **M16** `porch setup --engine quality` (`porch-quality`); see [11-review-quality-brief.md](11-review-quality-brief.md).

Dogfood proven on a toy repo and **klynt** (M15). Mailgate is second (see blockers below).

## Prerequisites

- `git`, Rust toolchain (for build/install), `gh` authenticated to the forge
- Review engine on `PATH`: prefer `porch-quality` (M16) when installed; otherwise a coding agent `claude` / `codex` (M10). Fallback: a generic `review` binary / `PORCH_REVIEW_BIN` PATH fake that emits coverage for every changed file. **Do not** use the OCR engine for this loop.
- Install porch onto a PATH you control for the session:

```sh
cargo install --path crates/porch --locked
cargo install --path crates/porch-quality --locked   # M16 engine binary
# or use the tree binaries:
export PATH="/path/to/porch/target/debug:$PATH"
```

Optional: `./install.sh`. Confirm with `porch doctor` (`engine=quality` or `engine=agent`, not OCR).

`$PORCH_HOME` defaults to `~/.porch`. Use a temp home for isolated dogfood.

## 1. Setup (quality or agent; not OCR)

```sh
porch setup --yes                  # prefers porch-quality when on PATH, else agent
# force: porch setup --yes --engine quality
# force: porch setup --yes --engine agent
porch doctor
```

Expect JSON `ok: true`, `engine: "quality"` or `"agent"`. Quality writes a `$PORCH_HOME/bin/review` wrapper to `porch-quality`. Agent writes `review.agent_bin` without that wrapper. Legacy OCR remains available only via `--engine ocr`. If a prior operator `~/.porch` still has `review.engine: ocr`, re-run `porch setup --yes`.

If no agent is installed, use a coverage-aware PATH fake and be honest in the run notes (“fake reviewer”):

```sh
# generic CLI fake that lists every changed path under files[]
export PORCH_REVIEW_BIN=/path/to/fake-review
# or: porch setup --yes --engine generic   # requires `review` on PATH
```

## 2. Init in the working tree

```sh
cd /path/to/clone
porch init --yes                   # or: porch init --skip-setup after setup
```

Creates the local bare gate, `porch` remote, and hooks. Copies the `/porch` skill for detected agents. Consent remains `git push porch` — `origin` is never rewritten.

Trusted executing config (`.porch.yaml`: `commands.*`, `pr.base_branch`, `deliver.github.watch_checks`, path instructions) is loaded from the **default-branch SHA** (`origin/HEAD`), never from the pushed tip. Put `.porch.yaml` on that branch before expecting certify/deliver policy to apply. Missing file → empty commands (review still runs).

## 3. Push (consent)

```sh
git push porch HEAD:refs/heads/<branch>
# or drive from an agent:
porch agent run --intent "short why" --wait
```

`porch agent run` ensures the daemon, pushes if needed, and with `--wait` streams JSONL until **parked** / **completed** / **failed** / **cancelled**. Prefer `--intent` over remembering `PORCH_INTENT`.

## 4. Park: TUI or JSON

If review parks (blocking findings):

| Surface | Commands |
|---|---|
| TUI | `porch` / `porch attach` — keys in footer; `y` = one `fix --yes` round |
| JSON | `porch agent status` → `porch agent respond approve\|fix\|skip\|abort` |

```sh
porch agent respond approve        # continue certify → deliver; writes review_approved_head_sha
porch agent respond fix --yes      # one unattended fix round only — not whole-gate yolo
porch agent run --wait             # resume until next park or terminal
```

Reviewer turns stay session-free. Fixer may resume. A rereview must not certify its own prescription. Rebase parks (`phase: rebase`) accept **`fix` \| `abort` only**.

## 5. Certify (cheap only)

Certify runs `commands.format` / `commands.lint` from trusted yaml. Dirty format/lint may produce `--no-verify` correction commits. Fail closed on non-zero exits. Default per-command timeout is **600s** (`PORCH_CERTIFY_TIMEOUT_SECS`); restart the daemon after changing it. Pin `tools.biome` (and friends) in `$PORCH_HOME/config.yaml` to the **repo** version — a newer global binary on PATH will fail closed in the disposable worktree.

Never wire full-suite CI, Playwright, Postgres-heavy gates, deploy, or on-chain publish into certify.

## 6. Deliver (PR; allowlist only)

Deliver lease-pushes the tip (`--force-with-lease=<ref>:<observed-sha>`), opens/updates a GitHub PR (`gh`), and babysits **only** `deliver.github.watch_checks`. Empty allowlist → push+PR, no babysit.

**Never merge** from porch. Never babysit deploy / Coolify / spend-money E2E.

Custody: if the author branch lags the pipeline tip, `porch agent sync` (optional `--recover`). Never rewrite `origin`.

## Klynt notes (first dogfood)

Clone: see [04-klynt.md](04-klynt.md). Canonical yaml: [examples/klynt.porch.yaml](examples/klynt.porch.yaml) (already on klynt default branch).

| Fact | Operator action |
|---|---|
| Lefthook pre-push is expensive | **`git push --no-verify porch`** is expected. Porch is not lefthook. |
| `origin/HEAD` is `main`; team PR base is `dev` | Trusted `pr.base_branch: dev` — rebase/PR against `dev`. Do not conflate with `repos.default_branch`. |
| No cheap GitHub check | `watch_checks: []` — push+PR only; do not add `moon ci` / `CI status`. |
| Certify | `just fmt` + `moon run :fmt-check :lint :typecheck api-contract:drift-check` — OK. **Never** `moon ci` / Playwright / `just e2e`. |
| Merge | **Do not merge** the dogfood PR. |
| Cold `biome` | `$PORCH_HOME/config.yaml` `tools.biome` must match the repo pin (klynt frontend: `@biomejs/biome@1.9.2`). A newer Homebrew biome on PATH / in `tools.*` fails `just fmt` in the disposable worktree. |
| Lefthook | Author `git push --no-verify porch` is expected. Deliver pushes origin with `--no-verify` from the bare gate (certify already ran) so a worktree `lefthook install` that plants `pre-push` into the shared bare hooks cannot block forward. |

Throwaway branch tip: prefer a docs/comment-only change (avoid auth, migrations, persistence, deploy workflows — those park as `ask-user`). Prefer `porch agent run --intent "…" --wait` (push+wait) over a manual push then attach so intent is recorded.

## Mailgate notes (second)

Canonical yaml: [examples/mailgate.porch.yaml](examples/mailgate.porch.yaml). Allowlist babysits PR Checks only (`lint`, `types-check`, `docs-check`, …) with skip-as-Ready; `rerun_transient: 0`. Never `just gate` / Playwright / Coolify / `force-publish`.

**Blocker (M15):** mailgate has no `.porch.yaml` on `origin/main` or `origin/dev`. Trusted certify/deliver policy therefore cannot load without committing that file to the default branch (or another trusted-config strategy). Prefer not rewriting the mailgate product tree from a porch dogfood session — land the example yaml via a normal mailgate change when ready, then repeat this checklist. Until then, treat mailgate dogfood as **skipped**.

## Suggested one-liner loop

```sh
porch setup --yes && porch doctor
cd /path/to/clone && porch init --yes
porch agent run --intent "…" --wait
# if parked: porch agent respond approve   # or fix / skip / abort
porch agent run --wait
porch agent sync                           # if author branch lags
# stop — do not merge
```

## Related

- Skill: [porch-agent.md](porch-agent.md)
- Roadmap M15: [09-roadmap.md](09-roadmap.md)
- Klynt / mailgate briefing: [04-klynt.md](04-klynt.md), [04-mailgate.md](04-mailgate.md)
- Decisions: [decisions.md](decisions.md)
