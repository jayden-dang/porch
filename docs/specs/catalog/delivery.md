# Delivery catalog

Forwarding the certified branch, GitHub PR open, allowlisted checks, and Agent-authored PR compose.

| Code | Feature | Capability | Match terms | Surface roots | Spec | Status | Roadmap item | Observation |
|---|---|---|---|---|---|---|---|---|
| DELIVER | Delivery | Forwards the certified branch, opens the GitHub PR, babysits allowlisted checks | pr, forward, checks, allowlist, github | `crates/porch-deliver/src/` | — | Recognized | — | OBS-75c657 |
| PRCMP | PR compose | Scaffold then park-compose for Agent-authored PR title/body: repo template or default narrative, no self-review theater, hidden attestation | compose, pr body, pr template, scaffold, park compose, attestation | `crates/porch-deliver/src/`, `crates/porch-run/src/deliver.rs`, `crates/porch-gate/src/`, `crates/porch/src/` | `../2026-08-30-pr-compose/` | Implemented | — | — |
