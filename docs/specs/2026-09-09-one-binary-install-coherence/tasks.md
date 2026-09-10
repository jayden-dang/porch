# Tasks — the installed pair, and porch telling the truth about it

**Feature code:** ONEBIN
**Roadmap item:** ROAD-12 (MILE-5), first wave
**Status:** Implemented

## Wave 1 — establish what is actually true before proposing anything

- [x] Find that `cargo install porch` already ships the floor sibling, and that
      `crates/porch/src/bin/porch_quality.rs` is a two-comment-lines copy of
      `crates/porch-quality/src/main.rs`, left beside a still-published original.
- [x] Date it: the copy landed in `20e3060` at 08:27, the roadmap entry naming it as
      a surface to change in `d60d5b3` at 13:00 the same day. ROAD-12's one-line
      entry describes work already done; its surface list describes work never done.
- [x] Confirm cargo itself diagnoses the collision, and that it appears under
      `cargo test --workspace` — the canonical unit-test step — while `cargo check`
      and `cargo clippy` never link and never show it. Framing's explanation that
      `check` hid it was wrong; it has been printing all along, just not fatally.
- [x] Reproduce the upgrade break end to end: install the standalone package, then
      run the currently documented `cargo install porch --locked`, and confirm it
      exits non-zero with the destination holding only `porch-quality`.
- [x] Establish that this is not hypothetical — `git show v0.2.1:docs/install.md`
      instructed both installs, and v0.2.1's `porch` declared one `[[bin]]`, so
      installing the standalone package was **required** to obtain a mandatory floor.
- [x] Probe `current_exe()` across a live in-place replacement: the path gains a
      deleted marker and `exists()` is false, the parent is intact, and the sibling
      found there is the new floor.
- [x] Confirm `porch setup --engine quality` refuses on a correct installation, and
      that `porch doctor` prints `[ok] floor:` for a three-line shell script.

## Wave 2 — reject the framing recommendation, on evidence

Framing proposed a floor identity handshake as ARCH-9/ARCH-12 enforcement. Adversarial
review argued it was neither enforcement nor free. Both claims were checked rather
than adjudicated by preference.

- [x] Read ARCH-9 and ARCH-12 against `docs/specs/2026-09-02-mandatory-floor/requirements.md:283`.
      FLOOR-9.2's guarantee is scoped to configuration and environment and explicitly
      excludes an owner replacing the installed binaries. The handshake extends a
      bounded guarantee, which is an ADR.
- [x] Verify the audit break by execution, not inspection: `AuditReportedVersion` is a
      struct with a required `unavailable` field and no serde default, its neighbour
      `AuditObservedIdentity` is untagged, and `project_descriptor` falls back on
      whole-descriptor failure. A new `reported_version` shape blanks every producer
      descriptor in every recorded round — into MILE-2's `Closed` read path.
- [x] Apply the `ESCAPE` / `DFAULT` test: does the mechanism serve the blocker
      *whichever way it resolves*? A purge that can complete did. A bounded RPC did.
      The handshake does not — it is dead under two of the three outcomes.
- [x] Verify the no-fence claim anyway, since it decided one of the options:
      `descriptor_equivalence_digest`'s preimage excludes `reported_version`, and
      every check against a recorded round is stored-against-stored. It holds. The
      inference drawn from it did not, because it looked at one durable artefact and
      missed `descriptor_json`.

## Wave 3 — the refusal

- [x] `floor::resolve` refuses when the launch path no longer names a file, before the
      sibling is considered.
- [x] `Error::FloorLaunchReplaced`, distinct from `FloorUnresolved`, carrying the
      remedy in its rendering.
- [x] Thread a distinguishable code to the round record, so an interrupted upgrade is
      `floor_launch_replaced` rather than a generic `floor_unresolved`.
- [x] Test existence, not the platform's deleted-file marker, so a platform that
      reports this differently degrades to the previous behaviour.

## Wave 4 — one description of the floor

- [x] `FloorState` with `Ready` / `LaunchReplaced` / `Unresolved`, a `remedy()` in the
      shape `DaemonCondition` established, derived from `resolve()` so it cannot
      disagree with what a run will do.
- [x] `doctor` reports the observed artifact identity and the replaced condition,
      instead of rebuilding sibling lookup and grading `is_executable`.
- [x] Explicit `--engine quality` falls back to the sibling; `detect_engines` left
      alone so the default is not decided by side effect.

## Wave 5 — the spawn, and the docs

- [x] `ETXTBSY` retry on the review producer spawn, matching `b31f0a8`.
- [x] Install docs: the v0.2.1 upgrade, that it installs nothing, the recovery, and
      the `--force` disagreement between the two documented routes.
- [x] Upgrading: stop the daemon first, why, and that detection is best-effort.
- [x] Release step 7 corrected to commands that pass, with a test asserting them.
- [x] `CONTEXT.md` gains **Floor condition**; the **Deterministic floor** entry now
      says what ARCH-12 binds and what it does not.
- [x] `usage.md`: the identity line, the PATH-dependent default stated plainly, and
      three troubleshooting rows.

## Wave 6 — proof

- [x] Four `floor` unit tests: the refusal, its distinguishability from an absent
      sibling, identity agreeing with what the record stores, and every state
      offering a command.
- [x] `m26_one_binary.rs`, ten tests, installing its own pair into a directory it
      owns rather than resolving the colliding artifact by name.
- [x] Three tripwires pinning what stays undecided, each failing if the answer moves.
- [x] Check the collision tripwire is not vacuous — scan `porch-git` and `porch-gate`
      too, then remove `porch`'s second bin and confirm the test fails. Done because
      ROAD-8 shipped a guard that passed for the wrong reason and the correction cost
      a wave.
- [x] `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace`.

## Verification notes

- `m26_one_binary.rs`: 10 passed. `porch-review` lib: 63 passed, including the four
  new floor tests.
- Full sweep: 46 targets, all green, no failures — the two flakes `DFAULT`'s Wave 7
  closed stayed closed.
- Before/after on the same fixture: `porch setup --yes --engine quality` went from
  `{"ok": false, "error": "… not found on PATH"}` to `{"ok": true}` with the sibling
  off `PATH`; `porch doctor`'s floor line went from a bare path to one carrying
  `identity=<sha256>`, and that identity changes when the sibling does.

## Why `ONEBIN-2` has no integration test

Reaching the replaced state through a real binary means unlinking an executable
after its process starts and before it resolves the floor. That is the timer race
`FAULT-1.5` forbids, and a flaky proof of a fail-closed path is worse than none. The
unit tests set the end state directly through the `LAUNCH_OVERRIDE` hook the
pre-existing floor tests already use, which is deterministic and tests the same
branch a real replacement reaches.

## Follow-ups, not in this wave

- **G.1** `crates/porch/tests/m16_quality.rs:209`
  `cargo_install_porch_package_includes_quality_bin` resolves `Command::cargo_bin`,
  which is the one colliding path, so it cannot distinguish the two packages and
  passes either way. `crates/porch-quality/tests/m16_cli_smoke.rs` and
  `crates/porch/tests/m8_doctor.rs` resolve the same path, so the standalone
  package's own smoke tests may be exercising the other package's artifact. Left
  alone here because the honest fix is to stop having two artifacts, which is the
  owner's first blocker; `ONEBIN-8.1` pins the collision meanwhile.
- **G.2** Under `engine: quality`, setup writes `review.bin` and a
  `$PORCH_HOME/bin/review` wrapper that `compose_prepared_invocations` never
  executes, and doctor grades the operator on that dead file. Reporting it
  accurately is free; changing what setup *writes* touches an existing config
  contract, so it wants its own wave.
- **G.3** `porch doctor` exits 0 when the floor is missing, so an operator whose gate
  cannot run gets a green-looking exit. Real, but it changes what the command
  promises — the same neighbour `DFAULT-7.3` reasoned about, and it belongs with the
  daemon-free inspect surface.
- **G.4** The macOS behaviour of `current_exe` after in-place replacement is
  **unverified**. If it keeps naming a path that now holds the new file, `ONEBIN-2`
  will not fire there and the docs' "stop the daemon first" is the only protection.
- **G.5** Whether yanking `porch-quality 0.2.1` breaks `cargo install porch --locked`
  through the packaged lockfile — **unverified**, and it gates the cheapest option on
  the owner's list.
- **F.11** (from `DFAULT`, still open) The deliver-repair rebase relies on ambient git
  identity while certify supplies its own.
- **F.12** (from `DFAULT`, still open) `porch daemon stop --force` still unlinks
  `daemon.lock`.
- **F.13** (from `DFAULT`, still open) `m18_round_identity.rs`'s refusal test removes
  a socket path that exists nowhere in this codebase.
- **F.4**, **F.8** (from `ESCAPE`, still open) The daemon-spawning read commands, and
  the git-config isolation sweep.
