# Quality engine eval fixtures (M16)

Start of an eval corpus for the porch-owned quality engine. Unit tests under
`crates/porch-quality` (and setup wiring in `crates/porch/tests/m16_quality.rs`)
assert the same behaviors on synthetic git ranges — no live LLM. These dirs are
the named corpus for the porch-owned quality engine.

| Dir | Expectation |
|---|---|
| `coverage-miss/` | Omitting a changed path from coverage/files must fail closed |
| `relocate-drift/` | Unique `existing_code` moves the line; ambiguous / empty → drop |
| `rule-rust-unwrap/` | `.unwrap()` in lib added lines → comment; `*_test.rs` excluded |
| `skip-lockfile/` | `Cargo.lock` / `package-lock.json` → skip reason `lockfile`, still covered |
