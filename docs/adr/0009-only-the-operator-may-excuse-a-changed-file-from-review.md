# 9. Only the operator may excuse a changed file from review

MILE-6's three blockers — the producer bar, the transport, and SARIF's placement — all turned out
to be downstream of one question none of them asked: **who may say that a changed file did not
need the judgment layer?** Today the answer is the producer, by accident. A judgment producer
that returns `status: "skip"` for every changed path gets every path recorded as `waived` with
the authority string `"producer"` *fabricated by porch* when the producer supplies none
(`crates/porch-review/src/coverage_state.rs:210-217`); `meets_required` then returns true and the
round finalizes `complete`. The column is unconstrained `TEXT`, so a producer that does supply one
can write `"operator"` into porch's durable audit record. ARCH-12 still holds — the floor runs in
its own required slot — so this is not an unreviewed forward; it is an audit-integrity hole. The
pinned assurance shape says floor-plus-judgment while the judgment layer contributed nothing, and
porch attributes the excuse to an authority that never spoke.

**A producer states a reason; it never supplies an authority.** Waiving review of a changed file
is a decision that the file may reach `origin` unreviewed, which is an approval, which ARCH-11
already forbids a producer to issue. A producer's stated reason is retained as evidence; the
authority slot is `operator` or `porch`, or the path is not waived. The module's own
`waived_without_authority_is_rejected` test already asserts the manifest path fails closed while
the status-row path beside it never can.

**The repair is not to delete the default, and finding out why changed this ADR.** Going to
implement the deletion showed that `"producer"` is doing two jobs, and only one of them is the
hole. The deterministic floor emits `skip` with a reason for every changed path it does not
review — a lock file, for instance — and `porch-quality`'s own `CoverageEntry` carries only
`path`, `status` and `reason`, with **no authority field at all**
(`crates/porch-quality/src/coverage.rs:7-15`, `:105-115`). Its output is parsed back through the
same `from_status_rows` path, so the fabricated string is what currently lets the floor's own
skips satisfy `meets_required`. Deleting it would make the mandatory floor fail closed on the
first run that touches a skipped path, which is an ARCH-12 break far worse than the hole being
closed. Both adversarial reviews probed this code with a judgment producer and neither noticed
that the floor depends on the same line.

So the repair is **role-aware stamping**, and it is a wave rather than a one-line change:
derivation must be told which authority porch is willing to stamp for the slot it is deriving.
The floor's skips are porch's own determination and are stamped `porch` — which is what they
always were, honestly labelled. A judgment producer's skips are stamped by nobody, so they are
not waivers and the path falls to a shortfall. The two halves must land together or ARCH-12
breaks between them. It is scheduled inside ROAD-15 with priority rather than shipped as a
drive-by, and it carries a value migration, because rows already recorded say `"producer"`.

One residual is named rather than closed: until the authority is supplied out of band, a producer
that writes `"operator"` into its own payload is still believed. A closed-set check in derivation
does not catch that, and a `CHECK` constraint cannot be added to `round_coverage` retroactively,
because the table is created with `IF NOT EXISTS` and existing databases would never gain it —
the same trap already recorded for `authority_events.kind`. Moving the authority out of the
producer's payload is therefore the first thing ROAD-15 does, not the last.

**Operator configuration loaded from the trusted default-branch SHA may supply a coverage waiver
under `operator` authority,** recorded with the config SHA as its evidence. This is what makes
MILE-6's outcome reachable, and it keeps ARCH-11 intact rather than bending it: the authority is
the operator, evidenced by a commit on the default branch that passed the repository's own
review, loaded by the same ARCH-4 path that already governs code-executing config.

Everything the three blockers actually asked now falls out. **The bar** is the de facto bar that
already ships — exit status, a JSON payload at the chosen output path, a coverage claim for every
changed path with presence never implying completion, mandatory reason and completion evidence,
non-null finding fields, and artifact stability — plus a required `schema_version` and a required
echoed `range` that must equal what porch passed, which is the one addition that lets porch detect
a producer that reviewed a different range than it was handed. Identity stays observation-only.
"Waivers" in ROAD-15 reads as coverage waivers; finding-level producer waivers are foreclosed by
ARCH-11 and are not in scope.

**`ocr` is not retired.** It is `generic` with a two-word argv prefix supplied by a porch-authored
wrapper, so porch has shipped a first-party adapter for a third-party calling convention since M9.
The line a converter must not cross is supplying a value the producer did not state, not
restating what it did: a converter may assert `schema_version`, because it knows the dialect it
wrote; it must report an absent range as absent rather than echoing porch's own input back; and
it can never supply a waiver authority. ROAD-17 already commits to migrating `generic` and `ocr`,
and that commitment stands.

**The transport stays as shipped and the payload becomes a porch-owned versioned envelope.** The
argv contract — `--from --to --format json --output`, cwd at the worktree, exit 0, JSON read from
the file — is already the vendor-neutral transport ROAD-16 names, so that entry is largely done
and its title points at the wrong artifact. Constrained SARIF was weighed as the wire format and
rejected, but not for the reason first offered: SARIF does have homes for a revision and, in
`result.kind: "pass"`, for a completion assertion. It fails because its *populated* coverage
vocabulary stops at `analysisTarget`, which the specification defines as having been **instructed
to scan** — the exact inference `derive_states` already refuses — so every off-the-shelf emitter
lands on all-`selected` and an `incomplete` round. Since such a producer needs porch-specific help
either way, the wire format should be the one that carries all the bar's facts natively.

**SARIF import and the operator coverage source ship together in MILE-6, or neither does.** A
converter alone yields a producer that is always `incomplete`; an operator coverage source alone
has nothing to convert. One first-party converter covers the ecosystem's findings half, which is
the answer to the vision's non-goal of a per-product adapter for every review tool. Independently
and now, porch sniffs a SARIF log at the output path and says so, because today a valid SARIF
2.1.0 log parses as an *empty review* — every `ReviewJson` field carries `#[serde(default)]` and
nothing rejects unknown keys — and fails later as a `coverage_shortfall` indistinguishable from a
producer that reported nothing.
