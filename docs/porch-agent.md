# Porch agent skill (headless)

Use when a `git push porch` run is **parked** on review findings, or when you need gate status without a TUI. The optional park TUI (`porch` / `porch attach`) is additive; this JSON contract is unchanged.

JSON on **stdout**. Human logs may appear on stderr. Exit codes (D11): `0` ok/in-progress gate, `1` failed/cancelled, `2` usage.

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

`--findings` and `--yes` are only valid with `fix`. `--findings` defaults to all blocking ids. `--yes` means one fix round then approve remaining (standing consent; never the default).

Stdout after respond is the same shape as `status` for the updated run.

## Operator tips

- Run `porch doctor` if review (`PORCH_REVIEW_BIN`), `gh` (`PORCH_GH_BIN`), or fixer (`PORCH_FIXER_BIN`, **required** for `fix` — no default) are missing.
- Review auto-fix default is **off**; do not assume unattended `fix`.
- Config that executes (`commands.*`, watch allowlists) comes from the trusted default-branch SHA in `.porch.yaml`, not from the parked tip.
