# Engineering Guidelines: Porch

Status: Active
Date: 2026-08-30

The human-facing engineering rules features must follow. `plan-tasks` sources the
engineering rules for its Global Constraints from here rather than from
`docs/agents/project.md` (which holds machine config only).

Standing architectural rules are **not** here — they are the `ARCH-N` spine in
[../architecture/INDEX.md](../architecture/INDEX.md).

## Coding standards

- Judgment calls only; anything `cargo fmt` and `cargo clippy` already enforce is
  not repeated here. The workspace sets `unsafe_code = "forbid"`,
  `clippy::all = deny`, and `clippy::pedantic = warn` — treat a pedantic warning
  as something to fix or explicitly `allow` with a reason, not to ignore.
- Slices are use cases, not layers. Before adding a crate, name the use case it
  owns; if you cannot, the code belongs in an existing slice (see **ARCH-10**).
- `porch-git` is the only place that shells out to `git` for gate operations. Do
  not spawn `git` from other slices.

## Naming and i18n

- Source, comments, commit messages, and documentation in this repo are **English**.
- Domain terms follow [`CONTEXT.md`](../../CONTEXT.md). It records the distinctions
  that matter — *review* vs *certify*, *custody* vs *lease* — and the synonyms to
  avoid. Use the glossary term, not a paraphrase.
- Integration test files are named for the milestone that introduced them:
  `crates/<slice>/tests/m<N>_<topic>.rs`.

## House rules

- **No network in unit tests.** Do not run real LLM calls, review-CLI network, or
  `gh` network in tests. Use PATH fakes and fixtures under `tests/fixtures/`.
- **Do not copy third-party source into this tree.** No vendored review engines
  (see **ARCH-9**), no vendored git implementations (see **ARCH-2**).
- **Do not put clone paths or prior-product names into committed files.** Local
  implementation notes live in the gitignored `.research/`.
- Read `.research/` before implementing the gate, the review adapter, or the
  custody / force-push paths.
- Always verify with `--workspace`. `default-members = ["crates/porch"]` means a
  bare `cargo test` skips `porch-gate`, `porch-git`, and `porch-quality`.
- Porch must help its dogfood consumers (mailgate first, klynt second) **without
  replacing their CI**. A change that only works by taking over a consumer's CI is
  out of scope.

## Dogfood consumers

| Tree | Role | Clone |
|---|---|---|
| mailgate | Production CI porch must not replace; first dogfood | [../../../../work/CommandOss/mailgate](../../../../work/CommandOss/mailgate) |
| klynt | Messy monolith CI; PR base `dev` vs `origin/HEAD` `main` | [../../../../klynt/klynt](../../../../klynt/klynt) |
