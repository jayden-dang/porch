# Tasks — purge refuses over unforwarded custody tips

**Feature code:** PURGE
**Roadmap item:** ROAD-24 (MILE-4)
**Requirements:** `requirements.md` · **Design:** `design.md`

Waves are ordered by dependency. Each wave ends green on
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo test --workspace`.

## Wave 1 — plumbing (no behavior change to eject yet)

- [x] **1.1** `porch_git`: `is_ancestor` for `GitDir` (`--git-dir`, same 0/1/other
  contract as the work-tree helper). Tests: equal, ancestor, not ancestor,
  missing object is `Err`. *(PURGE-2.3, PURGE-2.4)*
- [x] **1.2** LOOK: keep inspect fail-soft; add strict wrappers for recover refs
  and leftover worktree HEADs that error when the layout exists and listing
  fails, and when a leftover directory's HEAD cannot be read. *(PURGE-2.1,
  PURGE-2.2)*
- [x] **1.3** Drive-by: single `resolve_repo_id` docstring. `home::abandoned_dir`.

## Wave 2 — refuse before detach

- [x] **2.1** Predicate: wrap listers fail-closed; `open_read` + `active_runs`
  for this `repo_id`; per-tip checkout + origin-proof. `eject --purge` calls
  this **before** detach. *(PURGE-1.1, PURGE-1.3, PURGE-2.3 … 2.6, PURGE-3.3,
  PURGE-3.5, PURGE-3.6)*
- [x] **2.2** `Error` path + `run_eject` prints the manifest, exit 1, no
  `ejected repo`. *(PURGE-1.2, PURGE-3.1, PURGE-3.2)*
- [x] **2.3** Unreadable DB without `--abandon` refuses and leaves the `porch`
  remote. Retarget
  `purge_that_cannot_open_the_database_is_reported_and_leaves_the_bare`.
  *(PURGE-3.5)*

## Wave 3 — `--abandon` + log

- [x] **3.1** clap `abandon` `requires = "purge"`; `EjectOptions.abandon`.
  *(PURGE-1.4, PURGE-1.5)*
- [x] **3.2** Write `$PORCH_HOME/abandoned/<repo_id>-<ulid>.json` before detach
  or deletion. Write failure aborts with no mutation. *(PURGE-3.4)*
- [x] **3.3** `--abandon` proceeds despite tips and active runs; then detach +
  purge. Unreadable DB + `--abandon` may detach then `LeftBehind`.

## Wave 4 — ESCAPE-4.3 repair

- [x] **4.1** `remove_dir_all(bare)` propagated; unit test: `bare` is a file →
  `LeftBehind`, file remains, `purged == false`. *(PURGE-4.1)*

## Wave 5 — proof (`m28_*`)

- [x] **5.1** Clean tree `--purge` without `--abandon` still purges (`m13` and
  `eject_purge_removes_only_this_repo_state` stay green). *(PURGE-1.3)*
- [x] **5.2** Recover ref, SHA not ancestor of checkout HEAD, no tracking, no
  forward rows: `--purge` exit 1, remote still `porch`, bare exists, printed
  SHA. `porch status` after refusal still lists the recover ref.
- [x] **5.3** Same + `refs/remotes/origin/main` descendant of the tip, **no**
  `forward_records`: `--purge` succeeds. *(pre-ROAD-7 / `NotAttempted`)*
- [x] **5.4** Stored `reached_origin` / `pushed` for `authorized_sha` equal to
  the tip: `--purge` succeeds even if the tracking ref is missing.
- [x] **5.5** Tip ahead of an `authorized_sha` that reached origin: refuse.
- [x] **5.6** Checkout `HEAD` has the tip as ancestor, origin unproven: `--purge`
  succeeds.
- [x] **5.7** Parked/running/pending row for this repo, no tips: refuse;
  `--purge --abandon` purges. Active run on **other** `repo_id` does not
  block this repo. *(PURGE-2.6)*
- [x] **5.8** `--abandon` without `--purge`: clap failure, remote remains.
- [x] **5.9** Predicate path uses `open_read` only: no `min_writer_protocol`
  bump, no status rewrite on a refused `--purge`. *(PURGE-3.3)*
- [x] **5.10** `eject` without `--purge` still database-free. *(PURGE-4.3)*

## Wave 6 — record

- [x] **6.1** `docs/usage.md` §N: fourth operator-facing case (still attached;
  manifest; exit 1); `--abandon`.
- [x] **6.2** `CONTEXT.md` **Eject**: refusal, `--abandon`, refuse-before-detach;
  purge still outside GOAL-4.
- [x] **6.3** Catalog `PURGE` on `docs/specs/catalog/gate.md`. ESCAPE-5.1
  superseded. LOOK-2.5 consumed. Close ROAD-24 and MILE-4 on the roadmap.
  Held SIGKILL sliver stays named, not a member.
