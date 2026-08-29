# Porch briefing index

This directory is the product briefing. Treat these files as the current spec until a decision in `decisions.md` is superseded in writing. Implementation is through M5 (cheap certify adapters from trusted `.porch.yaml`); deliver remains a stub until M6.

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
| [04-mailgate.md](04-mailgate.md) | Production CI porch must sit *in front of*, never absorb |

## Dogfood

Clickable path: **[references.md](references.md)**.

- [mailgate](../../../work/CommandOss/mailgate)

## One-sentence product

Porch is a Rust local git gate whose review is coverage-enforcing and line-anchored, whose certify step is cheap and targeted, and whose deliver step talks only to GitHub PR checks — so “passed the porch” means independently reviewed and locally certified, **not** “production CI/CD is green.”
