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
already forbids a producer to issue. The `"producer"` default is deleted and
`round_coverage.authority` is constrained to the closed set `authority_events.actor_kind` already
uses. A producer's stated reason is retained as evidence. This is a **live defect against a
shipped invariant and is repaired immediately**, not held for the milestone; the module's own
`waived_without_authority_is_rejected` test already asserts the manifest path fails closed while
the status-row path beside it never can.

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
