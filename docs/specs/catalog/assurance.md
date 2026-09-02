# Assurance catalog

Review producers, first-party quality engine, durable round identity, and the mandatory deterministic floor.

| Code | Feature | Capability | Match terms | Surface roots | Spec | Status | Roadmap item | Observation |
|---|---|---|---|---|---|---|---|---|
| REVIEW | Review adapter | Selects the review engine (coding-agent turn, quality, or CLI) and emits JSON findings | review, engine, findings, adapter, reviewer | `crates/porch-review/src/` | — | Recognized | — | OBS-208976 |
| QUALITY | Quality engine | First-party review quality engine: diff, rules, grouping, relocation, coverage | quality, rules, grouping, relocation, coverage, diff | `crates/porch-quality/src/`, `tests/fixtures/quality/` | — | Recognized | — | OBS-9edece, OBS-dc9cb3 |
| ROUND | Review round identity | Durable review-round records: input and review-context bindings, canonical fingerprints, finding instances, structured coverage, and interrupted-round reconciliation | round, fingerprint, finding instance, coverage state, audit identity | `crates/porch-gate/src/`, `crates/porch-run/src/`, `crates/porch-review/src/` | `../2026-08-30-review-round-identity/` | Implemented | ROAD-6 | — |
| FLOOR | Mandatory deterministic floor | Composes the deterministic floor as a required producer on every assurance run and makes authorization prove it ran: Porch-owned required-set policy, per-round requirement records, run-level assurance pin, protocol 2 boundary | floor, required producer, assurance shape, required set, protocol 2, resolver | `crates/porch-review/src/`, `crates/porch-run/src/`, `crates/porch-gate/src/rounds/` | `../2026-09-02-mandatory-floor/` | Implemented | ROAD-22 | — |
