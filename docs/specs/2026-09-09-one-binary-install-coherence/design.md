# Design — the installed pair, and porch telling the truth about it

**Feature code:** ONEBIN
**Roadmap item:** ROAD-12 (MILE-5), first wave
**Respects:** ARCH-9, ARCH-12

## The shape of the decision

Framing and adversarial review disagreed, and the disagreement is the design.

Framing's recommendation was a floor **identity handshake**: teach the floor binary to
report an engine identity, have `porch` hold the expected value through the
`porch-quality` library it already links, and refuse a sibling that does not match.
It presented this as enforcing ARCH-9 and ARCH-12 — defect repair, therefore
shippable without the owner.

Review broke that, and I verified the break rather than taking either side:

1. **The invariants do not compel it.** ARCH-12 forbids substituting *an external
   judgment producer* for the floor. ARCH-9 forbids *vendoring or wrapping a
   third-party review CLI* as the engine. An operator who replaces
   `~/.cargo/bin/porch-quality` has done neither. And the mandatory-floor spec closed
   this door deliberately: `docs/specs/2026-09-02-mandatory-floor/requirements.md:283`
   scopes FLOOR-9.2's no-redirect guarantee to "configuration and environment only;
   it does not extend to an owner who replaces the installed `porch` or
   `porch-quality` binaries." So the handshake is not enforcement of a live
   invariant — it is an **extension of a guarantee a shipped spec bounded on
   purpose**, which is an ADR, not a coding session.

2. **Its cheapest-looking part is not cheap.** `porch_gate::audit::AuditReportedVersion`
   is a struct with a required `unavailable` field and no serde default, while its
   neighbour `AuditObservedIdentity` is an untagged enum that tolerates both shapes.
   `project_descriptor` falls back on *whole-descriptor* deserialization failure. So
   plumbing a real version into `reported_version` turns `adapter_kind`,
   `declared_engine_kind`, and `observed_version_identity` into `unavailable` for
   every producer in every recorded round, and raises `unreadable_producer_descriptor`
   — a regression into MILE-2's `Closed` audit read path and GOAL-2.

3. **It is dead under two of three outcomes.** If the owner retires the standalone
   package, there is nothing to defend against. If the floor moves in-process, the
   sibling the handshake hardens is deleted. It survives only the "keep both" ruling.

`ESCAPE` and `DFAULT` each shipped mechanism the blocker needed **whichever way it
resolved** — a purge that can complete, a bounded RPC. That is the test, and the
handshake fails it. So this wave is built on a different thesis.

## The thesis

**The two installed files move independently, and every surface porch has assumes
they move together.** Fix what porch says and what porch refuses. Do not touch what
porch installs.

That reframing also relocates the harm. The reachable damage is not a hostile floor;
it is an operator whose upgrade silently did nothing, or a daemon quietly pairing old
code with a new floor. Those hurt someone today, under any ruling.

## What changed, and why each is free of the blocker

### The upgrade that installs nothing (§1)

`v0.2.1`'s `docs/install.md` instructed `cargo install porch --locked` **and**
`cargo install porch-quality --locked`, and `v0.2.1`'s `porch` declared one `[[bin]]`.
Installing the standalone package was therefore *required* to obtain a mandatory
floor. `v0.2.2`'s `porch` declares two, so cargo now refuses to overwrite a file it
records as another package's. Reproduced end to end: the command exits non-zero and
the destination holds only `porch-quality`.

Documentation only. It describes what the shipped code does; it decides nothing.

### Refusing a porch replaced underneath itself (§2)

An install replaces its destination by rename. A process that outlives one keeps its
inode, and on Linux `current_exe()` returns the old path with `" (deleted)"`
appended. Verified with a probe: the path's `exists()` is **false** while its
`parent()` is intact, and the sibling found there is the **new** floor.

Inside a run this is already caught — the pinned `required_set_digest` covers the
floor's artifact hash, so a new round fails `assurance_shape_mismatch`. A run
*started* after the upgrade compares nothing, pins the new floor, and proceeds under
old gate code.

The check is `!launch.exists()`, not a scan for the deleted-file marker. That is
deliberate: a platform that reports a replaced binary differently then falls through
to today's behaviour instead of misfiring on a path porch simply could not stat.
Detection is therefore best-effort, and the docs say so — "stop the daemon before
upgrading" is the reliable step, and this refusal is the net beneath it.

Why this is not the handshake in disguise: it makes no claim about *which* floor is
correct. It refuses only when porch cannot vouch for the pairing at all, because the
executable that would have vouched is gone. It adds no durable field and no
comparison against an expected value.

### `FloorState`, and why a second type

`resolve()` answers "can I run the floor"; doctor and setup ask "what is the floor's
situation". Doctor previously answered that by rebuilding sibling lookup itself, which
is how it drifted into blessing anything executable of the right name while setup
drifted into requiring a `PATH` hit the resolver refuses. `FloorState` gives both one
description, derived from `resolve()` so it cannot disagree with what a run will do.
It carries a `remedy()` in the shape `DaemonCondition` established.

### Explicit `--engine quality` (§4)

Only the explicit branch falls back to the sibling. `detect_engines()` is untouched
**on purpose**, and this is the trap in the area: it also feeds `default_engine()`, so
teaching detection about the sibling would make floor-only the default on every
installation. That is MILE-5's second blocker. Fixing the refusal through detection
would have answered a blocker by side effect and reported it as a bug fix — the exact
move this wave rejected in §1 of this document.

### Doctor reports identity (§5)

The value is `observed_version_identity`, the same sha256 the assurance record stores,
so an operator and the audit document read one fact. Reporting it is what makes the
open question in §7.1 *observable* without answering it: two installations can now be
compared by a human, while porch continues to hold no opinion.

### The floor spawn's `ETXTBSY` retry (§6)

`b31f0a8` gave `porch-agent` and `porch-deliver` a bounded retry, naming a binary
mid-install as the motivating case, and skipped the review producer spawn. The floor
is the binary porch's own documented upgrade replaces, so of the three sites this was
the most exposed. Mechanical and precedented.

### The release smoke gate (§3)

Corrected the step rather than teaching the binary a flag. Adding `--version` is
either a compatibility promise on an executable the first blocker may delete, or —
if it were plumbed anywhere real — the audit-contract break above. The honest free
fix is the one that makes the procedure describe the product. A test now asserts the
step, because nothing did: three tags shipped past a gate that could not pass.

## Tripwires instead of arguments

Three pins record today's behaviour without endorsing it, in the form ROAD-9 used to
make MILE-3's blocker executable: the two-package bin collision, the engine default
flipping on `PATH`, and a foreign executable passing as the floor. Each fails if the
answer moves, so the owner rules on a measurement.

The collision pin scans `porch-git` and `porch-gate` as well, so a predicate that
always answered "yes" could not satisfy it — checked by removing `porch`'s second bin
and confirming the test fails. That guard exists because ROAD-8 shipped a test that
passed for the wrong reason, and the correction cost a wave.

## Testing

`ONEBIN-2` is proven in `porch-review`'s `floor` unit tests, not the integration
suite. Reaching the replaced state through a real binary means unlinking an
executable after its process starts and before it resolves the floor, which is the
timer race `FAULT-1.5` forbids. The existing `LAUNCH_OVERRIDE` hook sets the end state
directly, which is both deterministic and the same instrument the pre-existing floor
tests use.

`m26_one_binary.rs` installs its own pair into a directory it owns rather than
resolving `porch-quality` by name. That matters here more than usual: `cargo_bin`
resolves the one colliding path, so any test that used it could not say which package
it got — which is why `m16_quality.rs`'s "cargo install ships the bin" assertion has
never distinguished the two.

## Surface drift

ROAD-12's declared surfaces are `crates/porch/src/bin/porch_quality.rs`,
`crates/porch-quality/src/`, and `crates/porch-review/src/engine.rs`. This wave
touches none of the first two and `engine.rs` not at all.

Recorded rather than absorbed, as `DFAULT` recorded its own. The declared surfaces
describe the consolidation — which either already happened in `20e3060`, four and a
half hours before the roadmap entry naming them was written, or waits on the owner's
blocker. The reachable harm lives in the surfaces that assume the two installed files
move together.
