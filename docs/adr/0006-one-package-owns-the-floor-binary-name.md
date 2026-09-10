# 6. One package owns the floor's binary name; the floor stays a separate process for now

Two cargo packages declare a `[[bin]]` that produces the file name `porch-quality`, and cargo
warns twice that the output-filename collision "may become a hard error". The two sources are the
same program apart from two comment lines, and `target/debug/porch-quality` is genuinely
last-writer-wins between two byte-different artifacts — which reaches the *verification* surface,
since `crates/porch/tests/m26_one_binary.rs` installs its fixture from
`cargo_bin("porch-quality")` and cannot say which package it got. No coding session can remove
this: two bin targets cannot produce one file name.

**The `porch-quality` package retires its `[[bin]]` and keeps its library published.** `porch`
already depends on that library, it has exactly one consumer in the workspace, and no installed
machine moves, because the published standalone version stays installable. The registry step is
sequenced and the order is load-bearing: **publish the library-only version first, then yank the
standalone.** Experiment against a local sparse registry settled what the specs correctly flagged
as unverified — `cargo install --locked` survives a yanked lockfile entry with a warning, while
bare `cargo install` fails at resolution with no fallback to an older root version. Yanking first
would break the bare command every Rust user types; republishing first repairs it retroactively
for the already-published roots, because the existing caret requirement admits the new version.

**Moving the floor in-process is rejected for now, and only on containment.** The floor is today
the one child process in the gate with a deadline and a process-group kill; a same-thread library
call would give that up in the one producer that runs on every round, while MILE-4's untimed-child
wedge class is still being closed. The rule-pack argument does not support the rejection — the
packs are compiled-in constants over the linear-time `regex` crate — and it should not be cited.

Two objections the specs recorded against in-process are **struck as disproven**, so they cannot
be quoted back at the consolidation later. The floor's identity basis is not destroyed:
`producer_equivalence_digest` hashes `adapter_kind`, `argv_prefix`, `observed_version` and
`consumed_context` (`crates/porch-run/src/lib.rs:1255-1275`) and contains neither the spawned path
nor `selection_source`, so relocating the hashed bytes to the running executable is a value change
of a kind that already occurs on every release, not a format change. And no ADR-0002-style
durable-state fence is required: that fence exists so an already-released binary cannot forward
with no floor at all, which in-process does not reach. The record must also be accurate about the
audit contract. The claim that a new `reported_version` shape blanks "every producer in every
recorded round" is wrong for historic rows, which read cleanly; it is right for *new* rows under
a widening that leaves `unavailable` required, which is a same-version footgun rather than a
downgrade.

Consequently **ROAD-12 does not close here**, and a short-lived re-exec of `porch` itself under a
hidden subcommand is recorded as the live candidate for the consolidation: one installed file, an
identity that is honestly the running build's own bytes, and containment byte-identical to today.
Whoever takes it must make the spawner apply `argv_prefix`, which today is descriptive of a
wrapper's embedded prefix rather than an instruction the spawn obeys.
