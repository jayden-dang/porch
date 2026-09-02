# Design: Review Round Identity

Feature code: ROUND
Status: In-progress
Date: 2026-08-30
Requirements: ./requirements.md

## Context

Porch invokes a producer with a computed `--from`/`--to` range (`porch-review/src/lib.rs:469-472`),
maps the returned comments to positional `f0..fn` handles (`:319-320`), and serializes the whole
set into `runs.findings_json` (`porch-run/src/lib.rs:291-292`). That column is the only record a
review ever leaves. It is overwritten on every rereview, carries no producer identity, no coverage
state, and no notion of when or under what context the judgment happened. The respond path, the
TUI, and `get_finding_hunk_result` all read it, so it is simultaneously the audit record and the
working store — and it is neither reliable as the first nor structured enough for the second.

The binding constraint is `ROUND-1.1` with `ROUND-2.1`/`2.2`: the round must be durably committed
*before* the producer is invoked, and finalized in one transaction or not at all. That rules out
the cheapest shape — a richer JSON document on `runs`. A blob cannot carry instance identity unique
across rounds (`ROUND-3.7`), cannot be scanned by state for startup reconciliation (`ROUND-4.7`),
and cannot answer applicability (`ROUND-4.12`) without parsing every run. It also rules out
computing identity inside `porch-review`, which has no database dependency at all.

Two facts about the existing system shape the rest. SQLite runs in **WAL** with `foreign_keys=ON`
(`db.rs:89-90`), and the database is opened by more than one process — the post-receive hook
(`notify.rs:39`), `eject` (`eject.rs:52`), and the service stop check (`service.rs:352`) alongside
the daemon — so "one writer" is not available as an assumption and optimistic concurrency needs a
real change detector. And `unchecked_transaction()` already has a precedent (`db.rs:895`), so
atomic finalization needs no new concurrency machinery, only discipline about what runs inside it.

The reconciliation model is what drove the requirement amendments. Identity cannot be a stateless
hash: a fix rewrites the evidence a content hash would key on, a rename moves the path, and two
distinct issues can share path, criterion, and anchor. So porch derives a producer-independent
**candidate key** during normalization and assigns the **canonical fingerprint** during
finalization, reusing a prior one only on a unique match. That split moved work across a crate
boundary and retired `ROUND-3.1`/`3.2`.

## Decisions

1. **Candidate key at normalization, canonical fingerprint at finalization.** `porch-review` is
   stateless and has no history; matching is conservative and stateful, and ambiguity yields new
   identity rather than inherited identity (`ROUND-3.16`, `3.17`, `3.19`).
2. **Producer keys are provenance, never identity.** A first-party rule id may derive a canonical
   criterion through a registered mapping, but never becomes the fingerprint (`ROUND-3.4`, `3.21`).
3. **Three-dimensional context recording.** Source state, effective per-recipient digest, and
   snapshot state are independent, so `absent` stays distinguishable from `present-and-empty`
   even when both yield the same prompt (`ROUND-1.8`, `1.9`, `1.13`).
4. **One immutable invocation plan.** Resolution happens once before round open; the spawn uses
   exactly the recorded target and argv, closing the gap where PATH or config could change between
   describing and running (`ROUND-1.17`, `1.18`).
5. **Composite artifact identity, no version probe.** A porch wrapper stays byte-identical when its
   backend is upgraded in place, so identity spans wrapper, known backend, and effective argv. No
   `--version` probe ships: it would either run the producer before the round is durable or produce
   a value that cannot belong to the committed binding (`ROUND-1.19`, `1.20`, `1.22`).
6. **Seven tables, additive.** The physical model follows the audit model's cardinality: a round has
   many producers, and one context element is applied differently to each (`ROUND-1.26`, `1.10`).
7. **Two-phase optimistic finalization** keyed on a monotonic per-run `review_history_revision`.
   `max(ordinal)` is not a change detector — an older pending round can finalize while a higher
   ordinal already exists (`ROUND-1.30`).
8. **Legacy is read, never rewritten.** Additive migration, explicit labeling, no backfill, no
   dual-write (`ROUND-5.2`, `5.3`, `5.4`, `5.5`).

9. **Identity lineages do not cross fingerprint versions.** Reconciliation matches only prior
   instances recorded under the current `fingerprint_version`; a bump starts fresh lineages rather
   than reinterpreting history (`ROUND-3.11`).
10. **Coverage is recorded per producer invocation**, and the round-level state of a path is the
    weakest state across required invocations. A multi-producer round otherwise loses which
    producer covered what, and `ROUND-2.7`'s completion evidence has no owner.
11. **The trusted-config ref is pinned before the round row commits.** If the open then fails, a
    sweepable ref leak is the safe residue; the reverse order can leave a committed round whose
    trusted commit is unpinned.
12. **`path_instructions.json` stays a transient run-level input.** It is derived from
    `.porch.yaml` at the trusted SHA (`porch-run/src/config.rs:55-119`) and materialised per run
    (`m6_deliver.rs:845-848`), so it cannot vary across rounds of one run. The database snapshot is
    the authoritative per-invocation context; only prompt and result artifacts are namespaced per
    invocation.
13. **Legacy records decode through their own DTO.** Enriched fields are never fabricated by
    defaulting a legacy row into the new contract (`ROUND-5.3`).

Spine invariants this design relies on: **ARCH-2** (git via the CLI), **ARCH-3** (session-free
producer turns), **ARCH-4** (repository-controlled executing config bound to `trusted_config_sha`),
**ARCH-6** (forced `ask-user` on scope-extending findings, preserved through enrichment),
**ARCH-10** (use-case slices), **ARCH-11** (porch alone issues assurance outcomes).

**ARCH-4 scope, stated precisely because two things could be confused.** Repository-controlled
executing config — `commands.*`, review rules, `review.path_instructions` — is loaded from the
trusted default-branch SHA and is bound by `trusted_config_sha`. Operator and harness *engine
selection* (`PORCH_REVIEW_BIN`, the `$PORCH_HOME` wrapper) is **not** repository-controlled and is
not covered by that binding; it is captured instead by the producer descriptor and its observed
artifact identity. Conflating them would either let a repository choose porch's executable or let
an operator silently change repository-controlled rules.

**ARCH-12 is honoured through a prerequisite, not weakened.** `ARCH-12` requires the deterministic
floor to run on every assurance run and never be substitutable. The shipped system cannot satisfy
that — `EngineKind` selects exactly one engine (`porch-review/src/engine.rs:11-20`). The invariant is
made true by **ROAD-22**, a MILE-2 slot ordered *before* ROAD-6, which composes `porch-quality` as a
required producer invocation on every assurance run. ROUND depends on that slot and relies on it:
`ROUND-1.31`'s producer-set correspondence is only meaningful because the floor is a required member
of every set — that is what stops a round carrying floor plus judgment from being equivalent to one
carrying judgment alone. One-binary consolidation stays ROAD-12 in MILE-5. The invariant was not
narrowed to fit the roadmap; the roadmap grew a slot to meet the invariant.

## Architecture

### 1. Round store — `porch-gate/src/rounds/`

Satisfies: ROUND-1.1, ROUND-1.2, ROUND-1.4, ROUND-1.5, ROUND-1.8, ROUND-1.11, ROUND-1.13,
ROUND-1.15, ROUND-1.25, ROUND-1.26, ROUND-1.27, ROUND-1.28, ROUND-1.30, ROUND-2.1, ROUND-2.2,
ROUND-2.3, ROUND-2.4, ROUND-2.5, ROUND-2.6, ROUND-2.7, ROUND-3.7, ROUND-3.8, ROUND-3.17,
ROUND-3.22, ROUND-4.10, ROUND-5.5, ROUND-6.1, ROUND-6.2, ROUND-7.2, ROUND-7.4, ROUND-7.5
Reuse: rung 2 — extends `porch-gate::db` (`Mutex<Connection>`, `ensure_column` at `db.rs:939`,
`unchecked_transaction` at `db.rs:895`, ULID minting already in `insert_run`)
Respects: ARCH-10, ARCH-11
Interface: `open_round(plan, bindings) -> RoundId` · `read_history(run_id) -> (HistoryRevision, Vec<PriorInstance>)`
· `finalize_round(round_id, proposal, seen_revision) -> Finalized | Stale` · `round_for_decision(run_id) -> Option<Round>`
Depth: n/a — extends `porch-gate::db`
Locality: new module under `porch-gate/src/`; `db.rs` gains only the migration calls — **extend**.
Neighbours `daemon.rs` and `rpc.rs` are **extend**; `porch-run` is **extend** at the call site.

Seven additive tables. `review_rounds` (id, run_id, ordinal, from_sha, to_sha,
`inventory_digest`, execution, assurance_completion, protocol/fingerprint versions, opened_at,
finalized_at); `round_producers` (descriptor per invocation, `descriptor_json` plus
`descriptor_equivalence_digest` computed over only the equivalence-bearing subset);
`round_context_elements` (element, source_state, source_reason, snapshot_state, snapshot_digest);
`round_context_applications` (element × producer/layer → effective digest, applied | not_applied);
`round_coverage` (path, state, reason, authority, completion_evidence); `finding_instances`
(id, round_id, producer_invocation_id, fingerprint, fingerprint_version, candidate_key, criterion,
evidence, consequence, action, provenance_json, confidence_value, confidence_kind, path, anchor);
`content_blobs` (digest PK, byte_length, bytes) — content-addressed, so an unchanged intent across
five rounds stores once, and `ROUND-1.28` fails closed when stored bytes disagree with a digest.

Excluding `selection_source`, `declared_engine_kind`, and `reported_version` from
`descriptor_equivalence_digest`'s preimage makes `ROUND-1.22` and `ROUND-1.24` structural rather
than dependent on a comparison function remembering to skip fields. The round row is written once
at open and thereafter only its terminal columns are filled, which makes `ROUND-4.10` a property of
the write path rather than a convention. Child rows use `ON DELETE CASCADE`; blob collection and
ref removal stay explicit (§3).

Finalization is two-phase: phase 1 reads history and the revision in one consistent read and closes
it; matching runs outside the write lock; phase 2 opens `BEGIN IMMEDIATE`, re-verifies the revision
and that the round is still `running`/`pending`, then writes coverage, instances, terminal state,
and the incremented revision — one committed transaction, which with the open transaction is the
two-write bound of `ROUND-7.4`.

#### Schema (additive; all `CREATE TABLE IF NOT EXISTS`, migrations via `ensure_column`)

```sql
ALTER runs ADD review_history_revision INTEGER NOT NULL DEFAULT 0;  -- per-run change detector

review_rounds(
  id TEXT PRIMARY KEY,                       -- ULID, minted by porch-gate
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL,
  from_sha TEXT NOT NULL, to_sha TEXT NOT NULL,
  inventory_digest TEXT NOT NULL REFERENCES content_blobs(digest),
  execution TEXT NOT NULL CHECK (execution IN ('running','finished','interrupted')),
  assurance_completion TEXT NOT NULL CHECK (assurance_completion IN ('pending','complete','incomplete')),
  completion_reason TEXT,
  trusted_config_sha TEXT NOT NULL,
  protocol_schema_version INTEGER NOT NULL,
  fingerprint_version INTEGER NOT NULL,
  opened_at TEXT NOT NULL, finalized_at TEXT,
  UNIQUE (run_id, ordinal))
INDEX review_rounds_open ON review_rounds(execution, assurance_completion);

round_producers(
  id TEXT PRIMARY KEY,                       -- ULID = producer_invocation_id
  round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
  slot INTEGER NOT NULL,                     -- position in the required producer set
  descriptor_json TEXT NOT NULL,
  descriptor_equivalence_digest TEXT NOT NULL,
  UNIQUE (round_id, slot),
  UNIQUE (round_id, id))                     -- composite target for same-round FKs
INDEX round_producers_equiv ON round_producers(descriptor_equivalence_digest);

round_context_elements(
  round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
  element_name TEXT NOT NULL,
  source_state TEXT NOT NULL CHECK (source_state IN ('absent','present','unreadable')),
  source_reason TEXT,
  snapshot_state TEXT NOT NULL CHECK (snapshot_state IN ('stored','omitted')),
  snapshot_reason TEXT,
  snapshot_digest TEXT REFERENCES content_blobs(digest),
  PRIMARY KEY (round_id, element_name))

round_context_applications(
  round_id TEXT NOT NULL, element_name TEXT NOT NULL,
  producer_invocation_id TEXT NOT NULL REFERENCES round_producers(id) ON DELETE CASCADE,
  application TEXT NOT NULL CHECK (application IN ('applied','not_applied')),
  effective_digest TEXT,                     -- NULL iff not_applied
  PRIMARY KEY (round_id, element_name, producer_invocation_id),
  FOREIGN KEY (round_id, element_name) REFERENCES round_context_elements(round_id, element_name) ON DELETE CASCADE,
  FOREIGN KEY (round_id, producer_invocation_id) REFERENCES round_producers(round_id, id) ON DELETE CASCADE,
  CHECK ((application = 'applied') = (effective_digest IS NOT NULL)))

round_coverage(
  round_id TEXT NOT NULL,
  producer_invocation_id TEXT NOT NULL,
  FOREIGN KEY (round_id, producer_invocation_id) REFERENCES round_producers(round_id, id) ON DELETE CASCADE,
  path TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('selected','completed','failed','waived')),
  reason TEXT, authority TEXT, completion_evidence TEXT,
  PRIMARY KEY (producer_invocation_id, path),
  CHECK (state <> 'waived' OR authority IS NOT NULL),
  CHECK (state NOT IN ('failed','waived') OR reason IS NOT NULL),
  CHECK (state <> 'completed' OR completion_evidence IS NOT NULL))

finding_instances(
  id TEXT PRIMARY KEY,                       -- ULID = finding_instance_id
  round_id TEXT NOT NULL REFERENCES review_rounds(id) ON DELETE CASCADE,
  producer_invocation_id TEXT NOT NULL,
  FOREIGN KEY (round_id, producer_invocation_id) REFERENCES round_producers(round_id, id) ON DELETE CASCADE,
  fingerprint TEXT NOT NULL, fingerprint_version INTEGER NOT NULL,
  candidate_key TEXT NOT NULL,
  criterion_id TEXT NOT NULL, evidence TEXT NOT NULL, consequence TEXT NOT NULL,
  action TEXT NOT NULL, severity TEXT NOT NULL,
  provenance_json TEXT NOT NULL,             -- producer-local key and metadata (ROUND-3.3)
  confidence_value TEXT, confidence_kind TEXT,
  path TEXT NOT NULL, anchor_kind TEXT NOT NULL, anchor_value TEXT,
  CHECK ((confidence_value IS NULL) = (confidence_kind IS NULL)))
INDEX finding_instances_fp ON finding_instances(fingerprint, fingerprint_version);
INDEX finding_instances_round ON finding_instances(round_id);

content_blobs(
  digest TEXT PRIMARY KEY, byte_length INTEGER NOT NULL, bytes BLOB NOT NULL,
  CHECK (byte_length = length(bytes)))
```

`review_history_revision` lives on `runs` and is incremented, in the same transaction, by exactly
two mutations: finalizing a round, and removing a round (which cascades its children). Standalone
finding-instance deletion is not a supported operation — a finalized round is immutable, so its
children leave only with the round itself.

#### Transactions and retry

Open: `BEGIN IMMEDIATE` → allocate `ordinal = COALESCE(MAX(ordinal),0)+1` for the run → insert round,
producers, context elements and applications → COMMIT → return the id. Ordinal allocation inside the
immediate transaction plus `UNIQUE(run_id, ordinal)` makes concurrent opens fail rather than collide.

Finalize, two-phase: **phase 1** reads `review_history_revision` and prior instances in one deferred
read transaction, then closes it; matching runs with no lock held. **Phase 2** `BEGIN IMMEDIATE`,
re-reads the revision and asserts the round is still `running`/`pending`; on match it writes coverage,
instances, terminal state, and `review_history_revision + 1`, then COMMITs.

`busy_timeout` 5000 ms. Stale-revision retries: **3**. On exhaustion the round is closed in one short
transaction as `interrupted` / `incomplete` with `completion_reason = 'history_contention'` — a
terminal, honest outcome rather than an unbounded retry or a round left dangling for startup.

Storage lives entirely in `$PORCH_HOME/state.sqlite` (`ROUND-7.2`).

#### Write-transaction accounting across every terminal path

Today the review phase issues up to three autocommit writes — `set_run_shas` always
(`porch-run/src/lib.rs:265`), `set_findings_json` whenever the producer returned
(`:292`), `set_review_approved_head_sha` only when nothing blocks (`:300`).

| Terminal path | Today | With ROUND | Net |
|---|---|---|---|
| Approved (no blocking findings) | 3 | shas + open + finalize + approved-sha = 4 | **+1** |
| Parked (blocking findings) | 2 | shas + open + finalize = 3 | **+1** |
| Handled timeout | 1 | shas + open + terminal finalize = 3 | **+2** |
| Unsuccessful producer exit | 1 | 3 | **+2** |
| Malformed / unnormalizable output | 1 | 3 | **+2** |
| Coverage shortfall | 1 | 3 | **+2** |
| History contention (retries exhausted) | 1 | shas + open + up to 3 rolled-back attempts + terminal close = 3..6 | **+2 … +5** |
| Process death mid-producer | ≤1 | shas + open = 2 | **+1**, plus one reconciliation write at next startup |

The retired `ROUND-7.1` could not bound this: it asked for at most one added transaction, which
fails on the four failure paths because today they write nothing after `set_run_shas`, while
`ROUND-4.2`–`4.5` require an honest terminal state. The three bounds that replace it map onto this
table directly — **ROUND-7.4** covers the contention-free rows (one open plus one terminal write),
**ROUND-7.5** bounds the contention row to three further attempts that leave no durable
finalization, and **ROUND-7.6** bounds the process-death row's later startup write to one. Tests
count committed writes and rolled-back attempts separately.
### 2. Applicability — `porch-gate/src/rounds/applicability.rs`

Satisfies: ROUND-1.23, ROUND-1.24, ROUND-1.31, ROUND-4.11, ROUND-4.12, ROUND-4.13, ROUND-4.14
Reuse: rung 2 — extends the round store's stored digests
Respects: ARCH-11, ARCH-12
Interface: `applicable_round(run_id, current_bindings, required_producers) -> Applicable(RoundId) | Requires New Round(reason)`
Depth: n/a — extends the round store
Locality: same module tree — **extend**; no neighbour changes.

Comparison is over the **applicability binding** only, so audit-only fields (`intent_source`,
`reported_version`, `selection_source`, `declared_engine_kind`) are outside it by construction.
Producer sets must correspond one-to-one by `descriptor_equivalence_digest`, so a round carrying
floor plus judgment is never equivalent to one carrying judgment alone. An `unavailable` observed
identity contributes no matching signal and therefore can never establish equivalence.

#### Equivalence digest preimage

Fields joined with `0x1F`, each prefixed by its ASCII decimal byte length, SHA-256 hex:

```
"porch-producer-equivalence/v1" ⟂ adapter_kind ⟂ argv_prefix_joined
  ⟂ observed_version_identity ⟂ consumed_context_sorted
```

`selection_source`, `declared_engine_kind`, and `reported_version` are absent from the preimage, so
`ROUND-1.22` and `ROUND-1.24` hold by construction. When `observed_version_identity` is
`unavailable(reason)`, the field is written as the literal `unavailable` **plus a per-invocation
nonce**, which guarantees the digest can never equal another invocation's — `ROUND-4.14` enforced
arithmetically rather than by a comparison rule that could be forgotten.

#### The complete applicability tuple

A recorded round is applicable to the current change iff **all** hold:

```
execution = 'finished'  AND  assurance_completion = 'complete'
from_sha, to_sha, inventory_digest            equal
trusted_config_sha                            equal
protocol_schema_version, fingerprint_version  equal
for every context element: source_state       equal
for every (element, producer) application:
    application state and effective_digest    equal
multiset of descriptor_equivalence_digest     equal        (ROUND-1.31)
```

Nothing else is compared. `intent_source`, `selection_source`, `declared_engine_kind`, and
`reported_version` are outside the tuple by construction, which is `ROUND-1.15`, `1.22` and `1.24`.
### 3. Retention and the trusted-config ref — `porch-gate/src/rounds/retention.rs`

Satisfies: ROUND-1.12, ROUND-1.16, ROUND-1.29
Reuse: rung 2 — `porch_git` CLI plumbing; namespace pattern copied from `refs/porch/recover/<run_id>`
(`porch-run/src/sync.rs:17-20`)
Respects: ARCH-2
Surface: the bare repository's ref namespace — new `refs/porch/config/<sha>` namespace, **no existing
readers**; `eject --purge` (`eject.rs`) — **replace**: purge gains ref removal in the same path that
deletes rows.
Interface: `pin_trusted_config(bare, sha)` · `sweep_unreferenced(bare) -> removed_count`
Depth: n/a — extends `porch-git`
Locality: new file in the rounds module; `eject.rs` is **extend**.

Database deletion commits before ref removal, so an interruption leaks a ref (recoverable by the
sweep) rather than unpinning a commit a retained round still references.

### 4. Invocation plan and effective context — `porch-review/src/plan.rs`

Satisfies: ROUND-1.9, ROUND-1.10, ROUND-1.14, ROUND-1.17, ROUND-1.19, ROUND-1.20, ROUND-1.21,
ROUND-1.22
Reuse: rung 2 — extends `review_bin()` (`lib.rs:208-227`) and `EngineKind` (`engine.rs:11-69`)
Respects: ARCH-3, ARCH-4
Surface: `review_bin()` — **replace**: it becomes a resolution-phase function with a single caller
and is removed from the spawn path; `run_review` / `run_agent_review` spawn sites
(`lib.rs:453-542`, `agent_review.rs:353`) — **replace**: they take a pre-resolved plan;
`PORCH_REVIEW_BIN` / wrapper config as an operator-facing contract — **frozen**: the design builds
around it, resolution semantics are unchanged, only *when* resolution happens moves.
Interface: `prepare(opts) -> PreparedInvocation { plan, context_elements }` where `plan` holds the
absolute target, argv prefix, descriptor, and consumed-context declaration
Depth: n/a — extends the existing resolution path
Locality: new file in `porch-review/src/`; `lib.rs` and `agent_review.rs` are **extend**.

The effective bytes a producer receives are known only where its input is built, so the digest of
`ROUND-1.9` is computed here rather than from source bytes. Elements a layer never receives are
recorded `not_applied` instead of implying their source bytes were context.

#### Producer descriptor

```
adapter_kind            native_agent | porch_json_cli
declared_engine_kind    agent | quality | generic | ocr | unavailable(reason)
selection_source        env_review_bin | env_agent_bin | home_config_wrapper
                        | home_config_agent | path_detection | default_path_name
invocation              { requested_target, spawned_target_absolute, argv_prefix }
wrapper                 none | { absolute_path, sha256 }
backend                 known { absolute_path, sha256 } | opaque_entrypoint | unavailable(reason)
observed_version_identity  artifact_sha256(composite) | unavailable(reason)
reported_version        string | unavailable(not_reported)
consumed_context        [ element_name, … ]   (sorted)
```

Composite artifact identity, same framing as above:

```
"porch-producer-identity/v1" ⟂ adapter_kind ⟂ wrapper_sha256|"-"
  ⟂ backend_tag ⟂ backend_sha256|"-" ⟂ argv_prefix_joined
```

Never the wrapper digest alone (`ROUND-1.20`): a porch wrapper is byte-identical across a backend
upgrade in place.

#### TOCTOU handling

Hash **both** the wrapper and the known backend, recording `(dev, inode, size, mtime_ns)` for each,
spawn, then re-stat both after spawn and compare. Checking only the spawned target would miss the
case the composite identity exists for: a backend swapped underneath an unchanged wrapper. A mismatch finalizes the round `finished` / `incomplete` with
`completion_reason = 'producer_artifact_changed'`. This narrows rather than closes the race — `fexecve`
is not available through `std` — and the design says so instead of implying atomicity it does not have.

#### Context canonicalization and digests

Effective bytes are the exact bytes handed to a producer or layer after that layer's transformation,
never the source bytes (`ROUND-1.9`). Digest preimage:

```
"porch-review-context/v1" ⟂ element_name ⟂ effective_state_tag ⟂ byte_length ⟂ effective_bytes
```

SHA-256 hex, length-delimited and domain-separated, so no version salt is needed inside the element.
Snapshot ceiling: **256 KiB** per element — two orders of magnitude above observed intents and path
instructions, and it bounds a round's snapshot cost below 1 MiB. Above it, `snapshot_state =
omitted`, `snapshot_reason = 'too_large'`, digest retained, `source_state` unchanged (`ROUND-1.11`).
### 5. Candidate key and finding contract — `porch-review/src/identity.rs`

Satisfies: ROUND-3.3, ROUND-3.4, ROUND-3.12, ROUND-3.13, ROUND-3.14, ROUND-3.15, ROUND-3.16,
ROUND-3.18, ROUND-3.21, ROUND-6.11
Reuse: rung 2 — extends the existing map/normalize pass (`porch-review/src/lib.rs:270-320`)
Respects: ARCH-6, ARCH-11
Surface: the `Finding` struct (`porch-review/src/lib.rs:69-83`) — readers are `porch-run` (park, respond, fixer input
building at `:1195-1198`), `rpc.rs:79-99`, `tui.rs:31`, and `runs.findings_json` rows already on
disk. Disposition: in-repo readers **replace** (they take the enriched type); persisted
`findings_json` rows **frozen** — the design builds around them, reading them through the legacy
path in §10 and never rewriting them.
Interface: `derive(finding, mapping) -> CandidateKey` · enriched `Finding` carrying criterion,
evidence, consequence, action, provenance, confidence
Depth: n/a — extends the normalize pass
Locality: new file in `porch-review/src/`; `lib.rs` **extend**; `porch-quality` **extend** (§11).

`f0..fn` handles keep their existing meaning as display and selection handles, unchanged.

#### Canonicalization

`path_key` — the repository-relative path **exactly as git reports it**, byte for byte, with no
`./` prefix. No Unicode normalization: `unicode-normalization` is not a workspace dependency, and
adopting one for this would be a new-dependency decision for a problem git does not have — git
tracks path bytes, so byte equality is the same notion of identity the rename evidence uses.

`criterion_id` — in order: (1) the registered mapping for the producer's `rule_id`; (2) the
normalized `category`; (3) `unclassified`.

`anchor` — first that resolves, recorded as `(anchor_kind, anchor_value)`:

| kind | value |
|---|---|
| `symbol` | enclosing declaration at `to_sha`, from the declaration-pattern table below |
| `hunk` | the diff hunk header section text |
| `snippet` | normalized first non-blank line of `existing_code` (whitespace-collapsed) |
| `none` | no anchor resolvable |

Declaration patterns, deliberately small: for `.rs`, a line matching
`^\s*(pub\s+)?(async\s+)?(fn|struct|enum|trait|impl|mod|macro_rules!)\b`. **Every other
extension has no symbol pattern** and falls through to `hunk`. Adding a language is adding a row;
guessing at one is how an anchor silently changes meaning across file types.

Line numbers, severity, action, raw producer wording, and producer name are **not** inputs.

#### Candidate key

```
candidate_key = SHA-256( "porch-candidate-key/v1" ⟂ fingerprint_version ⟂ path_key
                         ⟂ criterion_id ⟂ anchor_kind ⟂ anchor_value )
```

Stateless, producer-independent, computed with no history (`ROUND-3.16`, `3.18`). It is **not** the
fingerprint.

Enrichment preserves `ARCH-6`: the forced `ask-user` action on scope-extending findings
(`porch-review/src/lib.rs:270-300`) is carried into `action` unchanged; nothing in this module can
downgrade it.
### 6. Reconciliation matcher — `porch-review/src/reconcile.rs`

Satisfies: ROUND-3.5, ROUND-3.6, ROUND-3.9, ROUND-3.10, ROUND-3.11, ROUND-3.19, ROUND-3.20,
ROUND-3.23
Reuse: rung 7 — none; no existing code matches finding sets across rounds. `relocate_finding`
(`porch-quality`) re-anchors one finding within one diff and does not generalise to set matching.
Respects: ARCH-11
Interface: `reconcile(current: &[CandidateKey], history: &History) -> Proposal` — a pure function,
no IO, no clock, no database
Depth: if this module vanished, callers would still need to know only that a proposal maps each
current finding to *reuse this prior fingerprint* or *mint a new one*, and that ambiguity means
mint. The matching rules, scoring, and tie-breaking stay behind that.
Locality: new file in `porch-review/src/`; callers are `porch-run` (§8) — **extend**. No neighbour
module changes.

Purity is what lets the expensive step run outside the write transaction: `porch-gate` supplies
history, this decides, `porch-gate` re-validates and persists.

#### Matching

Inputs: current candidate keys; history = prior instances of the same run **whose
`fingerprint_version` equals the current one**; optional rename evidence (`git diff -M` name pairs).

```
1. within-round: group current findings by identical candidate_key. A singleton group is
   unambiguous. A group of size > 1 collapses to ONE fingerprint only when ALL hold:
     (a) every member comes from a different producer invocation;
     (b) at most one member per producer invocation;
     (c) every member carries a source range;
     (d) all member ranges share one non-empty COMMON intersection.
   A duplicated producer, a missing range, pairwise-but-not-common overlap, more than one
   possible pairing, or any other ambiguity => every member is distinct.       (ROUND-3.20)
   The signal is deliberately independent of the candidate key: line ranges are excluded
   from the key by design, so this is evidence the key does not already assert. Each
   occurrence remains its own finding instance regardless of the outcome.
2. cross-round: for each unambiguous current group, collect prior instances with an equal
   candidate_key. Where the path differs, first rewrite the prior path_key through rename
   evidence and recompute the comparison.                                       (ROUND-3.23)
3. claim: reuse a prior fingerprint only when exactly one prior instance matches AND that
   prior instance is claimed by exactly one current group — a strict one-to-one.  (ROUND-3.17)
4. otherwise mint.                                                               (ROUND-3.19)
```

There is **no score and no tie-break**: ties are ambiguity, and ambiguity mints. That makes the
function deterministic without a ranking rule to get wrong, and it is why disappearing findings need
no special case — a prior instance nothing claims is simply not carried forward.

#### Minting, and why collisions are safe

```
fingerprint = SHA-256( "porch-fingerprint/v1" ⟂ fingerprint_version ⟂ candidate_key
                       ⟂ minting_instance_id )
```

The minting instance's ULID disambiguates, so two genuinely distinct issues that share a candidate
key (same file, same criterion, same anchor) receive **different** fingerprints — `ROUND-3.6` holds
even at a semantic-key collision. A cryptographic SHA-256 collision is outside the threat model. A
fingerprint is never a database identity (`ROUND-3.10`): instances carry their own ULIDs and many
may share one fingerprint.

#### Version transition

`fingerprint_version` is a compile-time constant. It bumps when any of these change: candidate-key
inputs, anchor derivation, criterion mapping, preimage framing, or the matching rules above. On a
bump, recorded fingerprints are never recomputed (`ROUND-3.11`) and matching does not cross versions
— a bump starts fresh lineages rather than silently reinterpreting old identity.

#### Crate boundary, stated to avoid a cycle

```
porch-gate  StoredPriorInstance  →  porch-run converts  →  porch-review History
porch-review Proposal            →  porch-run submits   →  porch-gate finalization
```

`porch-review::reconcile` takes and returns its own plain types and **does not depend on
porch-gate**. `porch-run` already depends on both and owns the conversion.
### 7. Coverage derivation — `porch-review/src/coverage_state.rs`

Satisfies: ROUND-2.8, ROUND-2.9, ROUND-6.10
Reuse: rung 2 — extends `assert_coverage` (`lib.rs:371`, applied `:542`) and the derivation at
`:310-311`, `:348-356`
Interface: `derive_states(changed, producer_output) -> Vec<(path, CoverageState, evidence)>`
Depth: n/a — extends `assert_coverage`
Locality: new file beside the existing coverage logic — **extend**; fail-closed behaviour unchanged.

#### Cardinality

Coverage is recorded **per producer invocation** (`round_coverage` keyed by
`(producer_invocation_id, path)`), because a state is a producer's claim and `ROUND-2.7`'s completion
evidence belongs to whoever produced it. The round-level state of a path is **derived** as the
weakest state across required invocations, ordered `failed < selected < waived < completed`. A
multi-producer round that collapsed coverage into one row could not say which producer covered what.
### 8. Orchestration — `porch-run/src/lib.rs`

Satisfies: ROUND-1.3, ROUND-1.18, ROUND-1.32, ROUND-4.1, ROUND-4.2, ROUND-4.3, ROUND-4.4,
ROUND-4.5, ROUND-4.6, ROUND-4.9, ROUND-6.7, ROUND-6.8, ROUND-6.9, ROUND-6.14
Reuse: rung 2 — extends `run_review_phase` (`:250-303`) and `resolve_review_from` (`:305-331`)
Respects: ARCH-3, ARCH-4
Surface: `$PORCH_HOME/runs/<run_id>/review/` artifact path (`agent_review.rs:20`, joined
`lib.rs:419-422`) and `runs/<run_id>/path_instructions.json` (`:423-427`) — **replace**: writers and
readers move to `runs/<run_id>/rounds/<round_id>/producers/<invocation_id>/`; pre-existing flat
artifacts are left inert and are not treated as round evidence.
Interface: unchanged from the caller's view — `run_review_phase` keeps its signature and outcome
Depth: n/a — extends `run_review_phase`
Locality: `porch-run/src/lib.rs` — **extend**; `porch-review` **extend**; `porch-gate` **extend**.

Sequence: resolve plan → open round (durable, id returned post-commit) → spawn exactly the plan →
normalize and derive candidate keys → read history → reconcile → submit → finalize, retrying phase 1
on a stale revision under a bounded policy. A failure to open aborts before any spawn (`ROUND-1.3`).

#### Executable sequence

```
1. select effective context in memory (intent, path instructions from trusted config)
2. resolve every required invocation plan                                   (ROUND-1.17)
3. pin the trusted-config ref                                               (decision 11)
4. open the round + producer rows in one transaction; receive ids           (ROUND-1.1, 1.2)
5. write per-invocation artifacts under runs/<run>/rounds/<round>/producers/<inv>/
6. spawn exactly the recorded target and argv                               (ROUND-1.18)
7. normalize, derive candidate keys
8. read history (phase 1) → reconcile → submit → finalize (phase 2)
```

Step 3 precedes step 4 so a failure after pinning leaves a sweepable ref leak rather than a committed
round whose trusted commit is unpinned. Step 1 precedes everything because the effective context must be selected before any plan is built.
`path_instructions.json` **keeps its current writer and location** — it is a transient input, not
evidence. What is bound is not that file but the **exact effective bytes each invocation received**,
digested per application in `round_context_applications` and snapshotted in `content_blobs`. The
design makes no claim that the file cannot vary; it binds what was actually supplied. A failure at step 4 aborts
before any spawn (`ROUND-1.3`).

Terminal state mapping: valid output with required coverage → `finished`/`complete`; handled timeout,
unsuccessful exit, malformed or unnormalizable output, or coverage shortfall → `finished`/`incomplete`
with a distinct `completion_reason`; process death → the row stays `running`/`pending` for startup
(`ROUND-4.1`–`4.6`). Blocking findings do not affect completion (`ROUND-4.9`).
### 9. Startup reconciliation — `porch-gate/src/daemon.rs`, `executor.rs`

Satisfies: ROUND-4.7, ROUND-4.8, ROUND-6.3, ROUND-6.6, ROUND-7.3, ROUND-7.6
Reuse: rung 2 — extends `recover_stale` (`executor.rs:17`, invoked `daemon.rs:48-49`)
Respects: ARCH-10
Surface: the `RunExecutor` trait (`executor.rs:17`) — **replace**: stale-round reconciliation joins
stale-run recovery behind the same contract; the daemon's refuse-on-failure behaviour is unchanged.
Interface: `recover_stale(home)` — same signature, wider responsibility
Depth: n/a — extends `recover_stale`
Locality: `daemon.rs` and `executor.rs` — **extend**. No new module.

### 10. Read path and legacy labeling — `porch-gate/src/rpc.rs`, `porch/src/tui.rs`

Satisfies: ROUND-5.1, ROUND-5.2, ROUND-5.3, ROUND-5.4, ROUND-6.4, ROUND-6.5, ROUND-6.13
Reuse: rung 2 — extends the snapshot builder (`rpc.rs:79-99`) and `get_finding_hunk_result`
(`rpc.rs:319-332`)
Respects: ARCH-11
Surface: the `porch agent status` JSON contract — **compat**: an additive `assurance_record` object
is introduced and the existing `findings` array keeps its shape and meaning; follow-up that removes
the duplication is deferred to ROAD-4, which owns the audit read path. `runs.findings_json` as a
**write** target — **replace**: finalized rounds stop writing it. The TUI findings panel
(`tui.rs:31`, `:124`) — **replace**: it renders from the round when one backs the decision.
Interface: `assurance_record { kind: round | legacy_snapshot | none, review_round_id, audit_identity }`
Depth: n/a — extends the snapshot builder
Locality: `rpc.rs` and `tui.rs` — **extend**; no new module.

The three-state `kind` is what keeps `ROUND-5.2` honest: a null round id alone cannot distinguish
legacy evidence from a run that was never reviewed.

#### `assurance_record` and legacy decoding

```
assurance_record = { kind: "round",           review_round_id: "<ulid>", audit_identity: "available" }
                 | { kind: "legacy_snapshot", review_round_id: null,
                     audit_identity: { unavailable: { reason: "predates_round_identity" } } }
                 | { kind: "none",            review_round_id: null,
                     audit_identity: { unavailable: { reason: "not_reviewed" } } }
```

Two explicit adapters, so neither contract leaks into the other:

- `LegacyFindingDto` deserializes `runs.findings_json` **only**. It never deserializes into the
  enriched finding contract, and no enriched field is defaulted into existence for a legacy row
  (`ROUND-5.3`).
- `StatusFindingDto` projects an enriched persisted instance down to the *existing*
  `agent status.findings` shape. New fields are exposed only under `assurance_record`, never by
  accidentally widening the legacy array.

`round_for_decision(run_id)` returns the finalized, applicable round backing the current parked
decision — not merely the latest round — so `ROUND-5.1` cannot be satisfied by an inapplicable round.
### 11. Quality enrichment — `porch-quality/src/`

Satisfies: ROUND-6.12
Reuse: rung 2 — extends `CommentOut` (`porch-quality/src/lib.rs:47-59`) and the rule pack pass (`rules.rs:143-149`)
Surface: the `porch-quality` JSON output contract — **compat**: `rule_id` is added as an optional
field; a caller that does not read it is unaffected, and the argv contract is untouched.
Interface: `CommentOut` gains `rule_id: Option<String>`
Depth: n/a — extends `CommentOut`
Locality: `porch-quality/src/lib.rs` and `rules.rs` — **extend**.

Today `RawComment.rule_id` is computed as `pack/rule` (`porch-quality/src/rules.rs:149`) and then dropped, surviving
only as prose inside `content` (`porch-quality/src/rules.rs:143`). Exposing it is what lets §5 derive a canonical criterion
from a registered mapping instead of parsing a message.

### 12. Repository id helper — `porch-gate/src/id.rs`

Satisfies: ROUND-6.15
Reuse: rung 2 — the module already exists
Interface: `repo_id_for(work_tree)` — unchanged
Depth: n/a — extends the existing module
Locality: **leave** — any new id minting lives in the round store, not here.

### 13. Reconciliation fixture corpus — `tests/fixtures/reconcile/<fingerprint_version>/`

Infrastructure — no `Satisfies:` line. Normative test material the criteria in §6 are graded
against.
Reuse: rung 2 — follows the existing fixture-directory pattern (`tests/fixtures/quality/*`,
`tests/fixtures/review/`)
Interface: `case.json` + `MANIFEST.json` — what a test knows is the case name and its expected
mapping; the corpus layout is otherwise opaque to callers
Depth: n/a — test material, not a module
Locality: new directory under `tests/fixtures/` — **leave** (no module impact).

`case.json` per case carries inputs and expected reconciliation semantics as a **mapping** — prior
correspondence, within-round equivalence groups, newly assigned fingerprints, and disappeared
findings — not pairwise assertions, which cannot express disappearance or several producers
collapsing onto one fingerprint. `MANIFEST.json` inventories cases and their fingerprint version
without duplicating expectations. Seven required families: moved code, rewritten message, path
change, collision, disappearing finding, multi-producer duplicate, ambiguous correspondence.
Changing an existing expectation requires a new fingerprint version and preserves the old corpus;
adding a case that exercises already-defined semantics does not.

#### Normative case schema

```json
{
  "case": "moved-code",
  "fingerprint_version": 1,
  "prior_rounds": [ { "ordinal": 1, "findings": [
      { "ref": "p1", "path": "src/a.rs", "criterion_id": "rust/unwrap-in-lib",
        "anchor": {"kind":"symbol","value":"fn load"}, "lines": [10,12], "producer": "quality" },
      { "ref": "p2", "path": "src/a.rs", "criterion_id": "rust/unwrap-in-lib",
        "anchor": {"kind":"symbol","value":"fn save"}, "lines": [40,41], "producer": "quality" } ] } ],
  "current_round": { "producers": ["quality","agent"], "findings": [
      { "ref": "c1", "path": "src/a.rs", "criterion_id": "rust/unwrap-in-lib",
        "anchor": {"kind":"symbol","value":"fn load"}, "lines": [18,20], "producer": "quality" },
      { "ref": "c2", "path": "src/a.rs", "criterion_id": "rust/unwrap-in-lib",
        "anchor": {"kind":"symbol","value":"fn load"}, "lines": [18,20], "producer": "agent" },
      { "ref": "c3", "path": "src/b.rs", "criterion_id": "rust/unwrap-in-lib",
        "anchor": {"kind":"symbol","value":"fn other"}, "lines": [5,6], "producer": "quality" } ] },
  "rename_evidence": [ { "from": "old/path.rs", "to": "new/path.rs" } ],
  "expect": {
    "reuse":            [ { "current": "c1", "prior": "p1" } ],
    "equivalence_groups": [ ["c1","c2"] ],
    "minted":           ["c3"],
    "disappeared":      ["p2"]
  }
}
```

The expectation is a **mapping**, not pairwise assertions: `reuse` carries prior correspondence,
`equivalence_groups` within-round collapse, `minted` newly assigned identity, `disappeared` prior
instances nothing claims. `MANIFEST.json` lists `{ case, fingerprint_version, families[] }` only, and
does not duplicate expectations.
### 14. Operator documentation — `docs/usage.md`, `docs/install.md`

Infrastructure — no `Satisfies:` line.
Reuse: rung 2 — existing operator docs
Interface: prose; `docs/install.md` links to the upgrade section in `docs/usage.md`
Depth: n/a — documentation, not a module
Locality: **extend** both files.

States that parked runs should be finished before upgrading, that upgrading preserves a parked
legacy run but cannot give it round identity retroactively, that a fresh run after upgrade is how
that change gets round identity, that `$PORCH_HOME` should be backed up first, and that downgrade
after new-format rounds exist is unsupported.

## Seams for testing

| Seam | Kind | Covers |
|---|---|---|
| `porch_gate::rounds` open/finalize/read_history | unit | ROUND-1.1, ROUND-1.2, ROUND-1.4, ROUND-1.5, ROUND-1.8, ROUND-1.11, ROUND-1.13, ROUND-1.15, ROUND-1.25, ROUND-1.26, ROUND-1.27, ROUND-1.28, ROUND-1.30, ROUND-2.1, ROUND-2.2, ROUND-2.3, ROUND-2.4, ROUND-2.5, ROUND-2.6, ROUND-2.7, ROUND-3.7, ROUND-3.8, ROUND-3.17, ROUND-3.22, ROUND-4.10, ROUND-7.4, ROUND-7.5 |
| `porch_gate::rounds` migration on an existing database | integration | ROUND-5.5, ROUND-6.1, ROUND-6.2 |
| `porch_gate::rounds::applicability` | unit | ROUND-1.23, ROUND-1.24, ROUND-1.31, ROUND-4.11, ROUND-4.12, ROUND-4.13, ROUND-4.14 |
| `porch_gate::rounds::retention` + bare refs | integration | ROUND-1.12, ROUND-1.16, ROUND-1.29 |
| `porch_review::plan::prepare` | unit | ROUND-1.9, ROUND-1.10, ROUND-1.14, ROUND-1.17, ROUND-1.19, ROUND-1.20, ROUND-1.21, ROUND-1.22 |
| `porch_review::identity::derive` | unit | ROUND-3.3, ROUND-3.4, ROUND-3.12, ROUND-3.13, ROUND-3.14, ROUND-3.15, ROUND-3.16, ROUND-3.18, ROUND-3.21, ROUND-6.11 |
| `porch_review::reconcile` against the fixture corpus | unit | ROUND-3.5, ROUND-3.6, ROUND-3.9, ROUND-3.10, ROUND-3.11, ROUND-3.19, ROUND-3.20, ROUND-3.23 |
| `porch_review::coverage_state` | unit | ROUND-2.8, ROUND-2.9, ROUND-6.10 |
| `porch_run::run_review_phase` with a PATH-fake producer | integration | ROUND-1.3, ROUND-1.18, ROUND-1.32, ROUND-4.1, ROUND-4.2, ROUND-4.3, ROUND-4.4, ROUND-4.5, ROUND-4.9, ROUND-6.7, ROUND-6.8, ROUND-6.9, ROUND-6.14 |
| daemon startup with fault injection at each boundary | integration | ROUND-4.6, ROUND-4.7, ROUND-4.8, ROUND-6.3, ROUND-6.6, ROUND-7.3, ROUND-7.6 |
| `porch_gate::rpc` snapshot + hunk | integration | ROUND-5.1, ROUND-5.2, ROUND-5.3, ROUND-5.4, ROUND-6.4, ROUND-6.5 |
| `porch` TUI snapshot rendering | unit | ROUND-6.13 |
| `porch-quality` CLI JSON output | integration | ROUND-6.12 |
| `$PORCH_HOME` containment assertion | integration | ROUND-7.2 |
| `porch-gate::id::repo_id_for` | unit | ROUND-6.15 |

Thirteen of these fifteen rows are existing seams; the two new ones are `porch_review::reconcile` and the round
store's public API.

## Coverage check

All 99 live requirement IDs (94 behavioural + 5 NFR) appear in exactly one `Satisfies:` line, and
each appears in at least one seam row. Retired IDs `ROUND-1.6`, `ROUND-1.7`, `ROUND-3.1`,
`ROUND-3.2`, and `ROUND-7.1` are deliberately unmapped — they were superseded and carry no
obligation.

No `## UI design` section: the Step 2b predicate does not hold. No `Satisfies:` ID is delivered
through a browser-rendered surface. `porch/src/tui.rs` is a terminal UI rendered by `ratatui`, not
a page, screen, component, or style in a browser.
