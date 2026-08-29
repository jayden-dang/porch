# Porch briefing index

This directory is the product briefing. Treat these files as the current spec until a decision in `decisions.md` is superseded in writing. Implementation is through M7 (dogfood yaml + allowlist skip-as-Ready + trusted `pr.base_branch` + path_instructions persist) plus 0.1.0 operator UX (`porch doctor`). First-run auto-setup (OCR wrapper, `$PORCH_HOME/config.yaml`) is **M9**, after M8 TUI.

## Read order

1. **[decisions.md](decisions.md)** — what is already locked.
2. **[01-briefing.md](01-briefing.md)** — narrative: problem, product bet.
3. **[06-architecture.md](06-architecture.md)** — how porch is supposed to work.
4. **[07-rust.md](07-rust.md)** — stack and process traps.
5. **[08-security.md](08-security.md)** — trust, custody, containment.
6. **[09-roadmap.md](09-roadmap.md)** — what to build first.

## Supporting notes

| File | Takeaway |
|---|---|
| [05-review-loop.md](05-review-loop.md) | Adversarial review, session split, intent, park vs auto-fix |
| [04-mailgate.md](04-mailgate.md) | First dogfood; production CI porch must sit *in front of*, never absorb |
| [04-klynt.md](04-klynt.md) | Second dogfood; monolith `moon ci`; PR base `dev` vs `origin/HEAD` `main` |
| [examples/mailgate.porch.yaml](examples/mailgate.porch.yaml) | Canonical mailgate trusted yaml |
| [examples/klynt.porch.yaml](examples/klynt.porch.yaml) | Canonical klynt trusted yaml |

## Dogfood

Clickable path: **[references.md](references.md)**.

- [mailgate](../../../work/CommandOss/mailgate)
- [klynt](../../../klynt/klynt)

## One-sentence product

Porch is a Rust local git gate whose review is coverage-enforcing and line-anchored, whose certify step is cheap and targeted, and whose deliver step talks only to GitHub PR checks — so “passed the porch” means independently reviewed and locally certified, **not** “production CI/CD is green.”
