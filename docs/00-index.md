# Porch briefing index

This directory is the product briefing. Treat these files as the current spec until a decision in `decisions.md` is superseded in writing. Release **0.2.1** (crates.io). Install: [install.md](install.md). Operator loop: [10-operator-checklist.md](10-operator-checklist.md). OCR wrapper is optional/legacy. See [09-roadmap.md](09-roadmap.md), [11-review-quality-brief.md](11-review-quality-brief.md), and D9/D13/D15.

## Read order

1. **[decisions.md](decisions.md)** — what is already locked.
2. **[01-briefing.md](01-briefing.md)** — narrative: problem, product bet.
3. **[06-architecture.md](06-architecture.md)** — how porch is supposed to work.
4. **[07-rust.md](07-rust.md)** — stack and process traps.
5. **[08-security.md](08-security.md)** — trust, custody, containment.
6. **[09-roadmap.md](09-roadmap.md)** — what to build first.
7. **[10-operator-checklist.md](10-operator-checklist.md)** — setup → push porch → agent review → park → certify → PR (M15 freeze).
8. **[11-review-quality-brief.md](11-review-quality-brief.md)** — M16 quality engine: coverage, relocate, rule packs, precision bias.

## Supporting notes

| File | Takeaway |
|---|---|
| [11-review-quality-brief.md](11-review-quality-brief.md) | Agent-review gaps; porch-owned engine bar; fixture corpus |
| [10-operator-checklist.md](10-operator-checklist.md) | Operator loop without OCR; klynt / mailgate dogfood notes |
| [05-review-loop.md](05-review-loop.md) | Adversarial review, session split, intent, park vs auto-fix |
| [04-klynt.md](04-klynt.md) | **M15 freeze first dogfood**; monolith `moon ci`; PR base `dev` vs `origin/HEAD` `main` |
| [04-mailgate.md](04-mailgate.md) | D10 product bar / M15 second — blocked until trusted `.porch.yaml` on default branch; CI porch must sit *in front of*, never absorb |
| [examples/klynt.porch.yaml](examples/klynt.porch.yaml) | Canonical klynt trusted yaml |
| [examples/mailgate.porch.yaml](examples/mailgate.porch.yaml) | Canonical mailgate trusted yaml |

## Dogfood

Clickable path: **[references.md](references.md)**.

**M15 freeze order:** [klynt](../../../klynt/klynt) first (completed). [mailgate](../../../work/CommandOss/mailgate) second — skipped until trusted yaml lands on its default branch. D10 still holds: if porch cannot live on mailgate without swallowing its CI, the product is wrong.

## One-sentence product

Porch is a Rust local git gate whose review is independent of the authoring session, whose certify step is cheap and targeted, and whose deliver step talks only to GitHub PR checks — so “passed the porch” means independently reviewed and locally certified, **not** “production CI/CD is green.” Coverage-enforcing, line-anchored review is the **M16** porch-owned quality engine (`porch-quality`); the M10 session-free coding agent remains an available reviewer.
