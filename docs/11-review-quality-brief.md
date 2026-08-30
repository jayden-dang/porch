# Review quality brief (M16)

What the M10 session-free **agent reviewer** gets wrong on mailgate / klynt-class diffs, and what the porch-owned quality engine must fix. Ideas only from constrained review tools (coverage denominator, DiffMap spirit, relocate, grouping, precision bias). Do not wrap or vendor that product (D13).

## Failure modes of coverage-lite agent review

| Gap | Symptom on dogfood-class diffs | Engine response |
|---|---|---|
| **Coverage gaps** | Large monorepo PRs: agent emits findings for a subset of paths and invents a pass for the rest, or omits paths. Porch only sees `files[]` / coverage-lite. | Hard coverage manifest: every changed path is `pass` or explicit `skip` + reason. Missing path → fail closed (same spirit as `Error::Coverage`). |
| **Line drift** | Agent cites `start_line` from an earlier read; later hunks or formatter noise move the construct. Park UI / future PR comments land wrong. | Store short `existing_code` anchors; relocate against current file content when context still matches uniquely; **drop** ambiguous / unanchored findings (precision bias). |
| **No language rules** | A single natural-language prompt cannot hold Rust unwrap/expect hygiene, JS `==`, lockfile noise, or path policy at once. Quality oscillates by model mood. | Rule packs as **data** (YAML), not a nine-agent zoo. Deterministic packs run without an LLM. Optional session-free agent helper per group is orchestration only — never required for unit tests. |
| **Weak grouping** | Agent reviews files in arbitrary order; related TSX + route + test land in separate turns; token budget truncates mid-tree. | Porch-owned grouping by language / top-level dir, max files per group, before emitting comments. |
| **False-positive park cost** | Vague “consider refactoring” or wrong-line bugs park humans on every push. | Precision bias: prefer skip / false-negative over a wrong line or ungrounded claim. Style/docs stay non-blocking or omitted (certify/linters own style). |

## DiffMap spirit (porch-owned)

Keep a path → hunk map from `git diff --from..--to` (unified). Rule packs and relocate consult **added/changed lines and surrounding context**, not the whole tree. Filtered / skipped siblings remain addressable for “is this construct in the diff?” checks — without shell, without edits.

## Eval corpus (start)

Fixtures live under [`tests/fixtures/quality/`](../tests/fixtures/quality/). Start small:

- `coverage-miss/` — multi-file range where omitting a path must fail.
- `relocate-drift/` — synthetic file where a finding’s line moved but `existing_code` still matches.
- `rule-rust-unwrap/` — added `.unwrap()` in lib code should emit; test files may skip or soften.
- `skip-lockfile/` — `Cargo.lock` / `package-lock.json` → skip + reason, still in coverage.

Gold findings are advisory for humans; unit tests assert engine behavior on these fixtures, not live LLM output.

## Contract

Stable M3 argv (cwd = worktree):

```text
porch-quality --from <sha> --to <sha> --format json --output <path>
```

JSON must include `comments` and `files` (and may include `coverage` / `groups`) in the shape `porch-review` already parses. Reviewer remains **session-free**, **no shell**, **no edits**. Fixer unchanged.

## Setup

`porch setup --engine quality` detects `porch-quality` on PATH (build dir or `~/.cargo/bin`), writes the porch-owned wrapper, verifies `--help` (+ optional tempfile range smoke). Doctor reports `engine=quality`. When the binary is present, setup may prefer `quality` over `agent`; otherwise keep `agent`. OCR remains legacy.
