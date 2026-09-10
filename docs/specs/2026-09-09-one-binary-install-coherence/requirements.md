# Requirements — the installed pair, and porch telling the truth about it

**Feature code:** ONEBIN
**Roadmap item:** ROAD-12 (MILE-5), first wave
**Status:** Implemented
**Goals:** None — MILE-5 is enabling work for MILE-6, MILE-7, MILE-8
**Respects:** ARCH-9, ARCH-12

## 0. What this wave is about, and why it is not what ROAD-12 says

ROAD-12 reads "consolidate the quality engine into the porch binary as the always-on
floor". Read as a sentence that describes something that already happened: commit
`20e3060` copied `crates/porch-quality/src/main.rs` to
`crates/porch/src/bin/porch_quality.rs` on 2026-08-30, four and a half hours before
the roadmap entry naming it as a surface was written. `cargo install porch` has
shipped the floor sibling ever since.

It was a copy, not a consolidation. The original was left in place, published, and
installable. So the repository now has two packages that emit one file name, and
`porch` cannot tell which of them an operator is running.

The reachable harm from that is **not** the one it looks like. It is not that a
hostile floor can be substituted — `FLOOR-9.2` already declined to guarantee against
an owner replacing installed binaries, and neither ARCH-9 nor ARCH-12 says otherwise
(§5.1). It is that **the two files move independently**, and every operator-facing
surface porch has assumes they move together:

- the documented upgrade command fails outright on the installed base (§1),
- a daemon that outlives an upgrade silently adopts the new floor under old code (§2),
- `porch setup --engine quality` refuses on a correct installation (§4),
- `porch doctor` blesses any executable of the right name (§5),
- the release smoke gate asserts a flag that does not exist (§3).

This wave fixes what porch says and what porch refuses. It does not decide what
porch installs. That is MILE-5's first blocker, owner Jayden, and §7 records what was
deliberately left alone.

## 1. The documented upgrade works, or says what to do instead

*Story: as an operator who installed porch by following the instructions for the
release I have, I want the instructions for the next release to work on my machine,
so that upgrading does not silently leave me on the old version.*

`v0.2.1`'s `docs/install.md` instructed both `cargo install porch --locked` and
`cargo install porch-quality --locked`, and `v0.2.1`'s `porch` package declared one
`[[bin]]`. Installing the standalone package was therefore **required** to obtain the
mandatory floor. `v0.2.2`'s `porch` declares two, so on every machine that followed
those instructions the currently documented `cargo install porch --locked` fails with
`binary porch-quality already exists in destination` and installs *nothing* — not
even `porch`. The operator stays on the old release, having run the command that was
supposed to move them off it.

- **ONEBIN-1.1** Operator documentation SHALL state that upgrading from a release
  whose floor was installed separately requires `--force`, and SHALL give the exact
  command.
- **ONEBIN-1.2** Operator documentation SHALL state that the failure installs nothing,
  so that an operator who sees it does not assume a partial upgrade.
- **ONEBIN-1.3** The documented `cargo install` route and the `install.sh` route SHALL
  NOT differ in whether they overwrite an existing floor without the difference being
  stated.
- **ONEBIN-1.4** Operator documentation SHALL state that removing the floor sibling
  disarms the gate, and SHALL name reinstallation as the remedy rather than a daemon
  restart.

## 2. A porch that was replaced underneath itself refuses, rather than adopting a floor it never shipped

*Story: as an operator upgrading porch while its daemon is running, I want the gate to
stop rather than grade my change with a mismatched pair, so that an upgrade is never
silently half-applied.*

`cargo install` replaces its destination by rename. A process already running from
that path keeps its inode; on Linux `current_exe()` then returns the path with
`" (deleted)"` appended, and that path no longer exists — but its **parent directory
does**. `floor::resolve` canonicalizes best-effort, takes the parent, and joins
`porch-quality`. A daemon still executing the old `porch` therefore resolves and pins
the **newly installed** floor.

For a round opened inside an in-flight run this already fails closed as
`assurance_shape_mismatch`, because the pinned `required_set_digest` covers the
floor's artifact hash. For a **new** run started after the upgrade nothing compares
anything: the run pins whatever it finds, and old gate code grades against a new
floor with no warning.

- **ONEBIN-2.1** WHEN the running executable's own path no longer names an existing
  file THE SYSTEM SHALL refuse to resolve the deterministic floor, and SHALL NOT
  spawn the sibling found next to it.
- **ONEBIN-2.2** That refusal SHALL be distinguishable from an absent sibling and from
  a sibling that is not executable.
- **ONEBIN-2.3** That refusal SHALL carry an operator remedy naming a command, on the
  same terms as a **Daemon condition** remedy.
- **ONEBIN-2.4** THE SYSTEM SHALL NOT provide configuration, environment, or flag
  input that permits a run to proceed in this state. It is fail-closed, per ARCH-12.
- **ONEBIN-2.5** The check SHALL be an existence test on the resolved launch path, and
  SHALL NOT parse the platform's deleted-file marker, so that a platform which
  reports a replaced binary differently degrades to today's behaviour rather than
  misfiring.

## 3. The release smoke gate asserts something true

`docs/agents/project.md` release step 7 is `porch --version && porch-quality
--version`. The floor binary's clap command sets no `version`, so `porch-quality
--version` exits 2. No test and no installer path exercises it, so the gate is
discovered by hand at release time, if at all. Three tags have shipped past it.

- **ONEBIN-3.1** The release smoke step SHALL consist only of commands that pass
  against a correct installation.
- **ONEBIN-3.2** The verify suite SHALL assert that the release smoke step's floor
  command succeeds, so that it cannot regress unobserved between releases.
- **ONEBIN-3.3** THE SYSTEM SHALL NOT add a `--version` flag to the floor executable
  in this wave. See §7.2.

## 4. Setup does not require a lookup the floor refuses to perform

*Story: as an operator whose porch and its floor sibling are correctly installed, I
want `porch setup --engine quality` to succeed, so that setup is not gated on a
resolution rule the gate itself rejects.*

`floor::resolve` never consults `PATH` — deliberately, and `docs/usage.md` says so to
operators. `porch setup --engine quality` requires a `PATH` hit and fails with
``engine `quality` requested but `porch-quality` not found on PATH`` on a machine
where the floor is present and would run.

- **ONEBIN-4.1** `porch setup --engine quality` SHALL succeed WHEN the canonical floor
  sibling is present and executable, whether or not any `porch-quality` is on `PATH`.
- **ONEBIN-4.2** `porch setup --engine quality` SHALL continue to fail WHEN the
  canonical floor sibling cannot be resolved, and SHALL say which sibling it looked
  for.
- **ONEBIN-4.3** THE SYSTEM SHALL resolve the floor for this purpose through the same
  rule `floor::resolve` uses, and SHALL NOT reimplement sibling resolution.
- **ONEBIN-4.4** THE SYSTEM SHALL NOT change which engine `porch setup` selects when
  the operator names none. See §7.4.

## 5. Doctor reports what it observed, and does not claim more

`porch doctor` grades the floor with `is_executable` and nothing else. It prints
`[ok] floor:` for a three-line shell script named `porch-quality`. It reports no
identity, version, or provenance, so an operator cannot tell a correct installation
from a mismatched one by asking porch.

Reporting the observation is free. *Judging* it is not — see §7.1.

- **ONEBIN-5.1** `porch doctor` SHALL report the resolved floor's observed artifact
  identity, not only that a file of the right name is executable.
- **ONEBIN-5.2** The reported identity SHALL be the same observation the assurance
  record already stores for that producer, so that an operator and the audit document
  are reading one fact.
- **ONEBIN-5.3** `porch doctor` SHALL report the floor's condition when the running
  executable was replaced underneath it (§2), rather than reporting the sibling it
  would otherwise have found.
- **ONEBIN-5.4** THE SYSTEM SHALL NOT treat any observed identity as correct or
  incorrect. Doctor reports; it does not adjudicate.

## 6. The floor spawn survives its own upgrade

`b31f0a8` gave `porch-agent` and `porch-deliver` a bounded retry on
`ExecutableFileBusy`, naming "a binary mid-install" as the motivating case. The review
producer spawn — which is how the floor runs — did not get it. The floor is the one
binary that porch's own documented upgrade command replaces, which makes it the most
exposed spawn site in the workspace, not the least.

- **ONEBIN-6.1** The review producer spawn SHALL retry, bounded, WHEN the operating
  system reports the target as busy for execution.
- **ONEBIN-6.2** The retry SHALL be bounded in time and SHALL surface the original
  error when the budget expires.

## 7. What this wave did not decide

Recorded so each is decided once, in the open, by its owner — in the form `ESCAPE-5`
and `DFAULT-8` used.

- **ONEBIN-7.1** THE SYSTEM SHALL NOT establish an expected identity for the floor, or
  refuse a sibling for being the wrong build. Framing proposed exactly that, as
  enforcement of ARCH-9 and ARCH-12. It is not: ARCH-12 forbids substituting *an
  external judgment producer* for the floor, ARCH-9 forbids *vendoring or wrapping a
  third-party CLI as the engine*, and `docs/specs/2026-09-02-mandatory-floor/requirements.md:283`
  states that FLOOR-9.2's no-redirect guarantee "covers configuration and environment
  only; it does not extend to an owner who replaces the installed `porch` or
  `porch-quality` binaries." Binding that case is a **new invariant or an amendment to
  ARCH-12, and needs an ADR** under `docs/adr/` — not a wave that arrives at it
  sideways as defect repair. It would also add a refusal an operator's currently
  passing gate can hit, and a durable-format change (§7.2), which are the two things
  least worth building while the milestone's blocker is open.
- **ONEBIN-7.2** THE SYSTEM SHALL NOT change the shape of `reported_version` in the
  producer descriptor. `porch_gate::audit::AuditReportedVersion` is a struct with a
  required `unavailable` field and no `serde` default, while its neighbour
  `AuditObservedIdentity` is untagged; `project_descriptor` falls back on
  *whole-descriptor* deserialization failure. So one new `reported_version` shape
  turns `adapter_kind`, `declared_engine_kind`, and `observed_version_identity` into
  `unavailable` for every producer in every recorded round, and raises the
  `unreadable_producer_descriptor` anomaly — a regression into MILE-2's `Closed` read
  path and GOAL-2. Widening the audit contract is the prerequisite for any real
  floor-version reporting, and belongs with whoever does §7.1.
- **ONEBIN-7.3** THE SYSTEM SHALL NOT remove, rename, or gate the `[[bin]]` target of
  the `porch-quality` package, and SHALL NOT move the floor in-process. Both decide
  MILE-5's first blocker — "the exact one-binary command surface and its compatibility
  period" — verbatim. The cargo output-filename collision therefore remains; two bin
  targets cannot produce one file name, so no wave can remove it without that
  decision. `ONEBIN-8.1` pins it.
- **ONEBIN-7.4** THE SYSTEM SHALL NOT change which engine `porch setup` selects when
  the operator names none. Installing `porch` puts `porch-quality` on `PATH`, which
  flips the default from `agent` to `quality` on a machine that has a coding agent —
  so porch's own artifact is read as evidence of an operator's choice. Note the
  coupling that makes this a trap: `detect_engines` feeds *both* the default and the
  explicit lookup §4 fixes, so repairing §4 through detection would make floor-only
  the universal default. §4 is therefore fixed on the explicit path only.
  `ONEBIN-8.2` pins the flip.
- **ONEBIN-7.5** This wave SHALL NOT move MILE-5 from `Planned`.

**Recommendations carried forward for ratification**, so the owner rules on options
that have been costed rather than on a blank question:

- **On the compatibility period.** Retire the `porch-quality` bin target and keep the
  library published; `porch` links it already and it has exactly one consumer in the
  workspace. Measured installed base at framing: 31 downloads of `porch` across two
  versions, 22 of `porch-quality`, one reverse dependency (`porch` itself), dogfood
  not started. The runner-up — keeping both behind a shim — was discriminated against
  because a shim must still occupy the name `porch-quality`, so it does not remove the
  collision and adds a second artifact to keep in sync.
- **A cheaper lever nobody has costed.** The durable hazard is that crates.io keeps
  serving an unyanked standalone floor that pairs with any future `porch`. Yanking
  `porch-quality 0.2.1`, or publishing the next one as library-only, removes the
  hazard class with no code at all. **Unverified and to be checked before acting:**
  whether yanking breaks `cargo install porch --locked` through the packaged lockfile.
- **On the command surface.** In-process is the strongest reading of "one porch
  binary" and `docs/specs/2026-09-02-mandatory-floor/requirements.md:305` already
  pointed here. It is not free: the floor's identity basis today *is* the sibling's
  artifact hash, which flows into each run's pinned `required_set_digest`, so moving
  in-process needs a durable-state fence in the manner of ADR-0002.

## 8. Tripwires — today's behaviour pinned, not endorsed

In the form ROAD-9 used for MILE-3's blocker: make the cost executable so the owner
rules on a measurement instead of an argument. These assert what porch does today and
explicitly do **not** assert that it is correct.

- **ONEBIN-8.1** THE SUITE SHALL pin that two packages in this workspace declare a bin
  target named `porch-quality`, so that the collision cannot be quietly resolved or
  quietly worsened without the pin moving.
- **ONEBIN-8.2** THE SUITE SHALL pin that engine selection depends on whether porch's
  own install directory is on `PATH`, given an otherwise identical machine. This is
  MILE-5's second blocker made executable.
- **ONEBIN-8.3** THE SUITE SHALL pin that a foreign executable of the right name is
  accepted as the floor, citing FLOOR-9.2's exclusion, so that §7.1's open question
  has a failing-if-changed record rather than a paragraph.

## 9. Out of scope

- MILE-4's three open blockers — the `eject --purge` refusal policy, the commit-signing
  wedge, and the daemon-free inspect surface. All owned, all elsewhere.
- ROAD-13's compatibility shim and ROAD-14's native-fallback policy.
- Any change to which producers a **Review round** requires, to the **Assurance
  shape**, or to `round_required_producers`.
- Making `porch doctor` exit non-zero on a floor warning. An operator whose gate
  cannot run gets exit 0 today, which is a real question, but it changes what the
  command promises and belongs with the inspect surface — the neighbour `DFAULT-7.3`
  already reasoned about.

## 10. Surface drift

ROAD-12's declared surfaces are `crates/porch/src/bin/porch_quality.rs`,
`crates/porch-quality/src/`, and `crates/porch-review/src/engine.rs`. This wave
touches **none of the first two** and `engine.rs` only incidentally. It changes
`crates/porch-review/src/{floor,setup,lib}.rs`, `crates/porch/src/doctor.rs`,
`docs/{install,usage}.md`, and `docs/agents/project.md`.

Recorded rather than absorbed, for the same reason `DFAULT` recorded its own: the
declared surfaces describe the *consolidation* — which either already happened or
awaits the owner's blocker — while the reachable harm lives in the surfaces that
assume the two installed files move together. A reader comparing the roadmap entry to
the diff should find the discrepancy explained here rather than be left to infer it.
