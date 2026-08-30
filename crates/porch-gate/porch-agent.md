# Porch agent skill (headless)

Use when driving a porch gate run without a TUI: push/wait, park decisions, status, custody sync. The optional park TUI (`porch` / `porch attach`) is additive; this JSON contract is first-class (D12). First-run review wiring is `porch setup` / `$PORCH_HOME/config.yaml` (not this skill).

JSON on **stdout** (pretty for one-shot; **JSONL** when `agent run --wait` streams). Human logs may appear on stderr. Exit codes (D11): `0` ok/in-progress/parked/completed gate, `1` failed/cancelled (or wait timeout), `2` usage.

**Never merge.** **Never babysit deploy** (or spend-money E2E / on-chain publish). Stop at park or completed/failed/cancelled; hand off allowlisted PR checks to humans/CI.

## Run (drive the gate)

```sh
porch agent run
porch agent run --wait
porch agent run --wait --timeout 600
porch agent run --intent "ship the API drift fix" --wait
porch agent run --run-id <ULID> --wait
```

- Ensures the daemon is up.
- Without `--run-id`: attach to an active run on the current branch, or `git push porch` then attach.
- `--intent` is authoritative for a fresh push (E17: empty skips, does not fail). Prefer this over remembering `PORCH_INTENT`; env still works on a manual `git push porch`. Do not combine `--intent` with `--run-id`.
- Without `--wait`: one pretty JSON snapshot, then exit.
- With `--wait`: JSONL snapshots until **parked**, **completed**, **failed**, or **cancelled**. Then stop — do not merge, do not watch deploy.

## Status

```sh
porch agent status
porch agent status --run-id <ULID>
```

Default: latest **parked** run for the cwd repo (`porch.repo-id` / worktree match). Pretty-printed JSON, for example:

```json
{
  "run_id": "01H…",
  "repo_id": "…",
  "branch": "feat/x",
  "status": "parked",
  "phase": "review",
  "head_sha": "abc…",
  "base_sha": "def…",
  "review_approved_head_sha": null,
  "findings": [
    {
      "id": "f0",
      "path": "src/lib.rs",
      "message": "…",
      "severity": "warning",
      "action": "ask-user",
      "category": "bug",
      "start_line": 10,
      "end_line": 12
    }
  ]
}
```

On error: `{"error":"…"}` or `{"error":"…","code":"usage"}`.

## Respond

```sh
porch agent respond approve
porch agent respond skip
porch agent respond abort
porch agent respond fix
porch agent respond fix --findings f0,f1
porch agent respond fix --yes
porch agent respond fix --findings f0 --yes --run-id <ULID>
```

| Verb | Effect |
|---|---|
| `approve` | Accept current findings; continue certify → deliver. Writes `review_approved_head_sha`. |
| `skip` | Skip remaining review gate for this run; does **not** write `review_approved_head_sha`. |
| `abort` | Fail/cancel the run. |
| `fix` | Spawn native fixer (`PORCH_FIXER_BIN`), then **session-free** rereview. |

`--findings` and `--yes` are only valid with `fix`. `--findings` defaults to all blocking ids. `--yes` means **one** fix round then approve remaining (standing consent; never the default). There is **no** default yolo on the whole gate — review auto-fix stays off (D6). Unattended agents may use `respond fix --yes` for a single round only.

Stdout after respond is the same shape as `status` for the updated run.

## Sync / custody

```sh
porch agent sync
porch agent sync --run-id <ULID>
porch agent sync --recover
```

JSON describes whether the author’s branch is behind / ahead / equal relative to the pipeline tip (and any `refs/porch/recover/<run>`). `fetch_hint` prefers `porch agent sync --recover` / the recovery ref when recoverable (`porch/{branch}` may still be the submit SHA); otherwise `git fetch porch && git merge --ff-only porch/<branch>`. `--recover` fast-forwards the local branch from a recorded recovery tip when the local HEAD is an ancestor — **never rewrites `origin`**. Divergent local history: refuse and keep the recovery ref.

Rebase parks (`phase: "rebase"`) accept **`fix` \| `abort` only** (not approve/skip).

## Suggested loop

1. `porch agent run --intent "…" --wait` (or `git push porch` then `porch agent run --wait`)
2. If `status=parked`: inspect findings → `respond approve|fix|skip|abort` (optional `fix --yes` for one unattended round)
3. Re-`run --wait` / `status` until completed or failed
4. `porch agent sync` if the author branch lags the pipeline tip
5. **Stop.** Do not merge the PR. Do not babysit deploy workflows.

## Operator tips

- Run `porch doctor` if review (`PORCH_REVIEW_BIN`), `gh` (`PORCH_GH_BIN`), or fixer (`PORCH_FIXER_BIN`, **required** for `fix` — no default) are missing.
- Review auto-fix default is **off**; do not assume unattended `fix` without `--yes`.
- Config that executes (`commands.*`, watch allowlists) comes from the trusted default-branch SHA in `.porch.yaml`, not from the parked tip.
- Certify PATH: recorded `$PORCH_HOME/config.yaml` `tools.*` dirs are prepended so cold daemons still see e.g. `biome`.
- Intent: `porch agent run --intent`, `porch daemon notify-push --intent`, or `PORCH_INTENT` on `git push porch`. Empty skips; does not fail.
