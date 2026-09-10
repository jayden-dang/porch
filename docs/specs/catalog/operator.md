# Operator catalog

CLI entrypoint, doctor/setup, attach TUI, and the native fixer adapter.

| Code | Feature | Capability | Match terms | Surface roots | Spec | Status | Roadmap item | Observation |
|---|---|---|---|---|---|---|---|---|
| OPERATOR | Operator surface | clap entrypoint, doctor, setup, and the attach TUI | cli, doctor, setup, tui, attach | `crates/porch/src/`, `crates/porch/tests/` | — | Recognized | — | OBS-0861ba, OBS-dfd0c3, OBS-57ea08 |
| LOOK | Daemon-free inspect | `porch status` / `porch runs` join **Daemon condition**, git custody tips, and **Forward record**/verdict without spawning or writing the state root | inspect, status, recover ref, forward verdict, open_read, daemon-free | `crates/porch-gate/src/look.rs`, `crates/porch-gate/src/db.rs`, `crates/porch/src/main.rs`, `crates/porch/src/doctor.rs` | `../2026-09-10-daemon-free-inspect/` | Implemented | ROAD-10 | — |
| AGENT | Fixer adapter | Native fixer CLI adapter | agent, fixer, cli-adapter | `crates/porch-agent/src/` | — | Recognized | — | OBS-26ed45 |
