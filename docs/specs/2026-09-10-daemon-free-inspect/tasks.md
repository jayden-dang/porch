# Tasks — daemon-free inspect

**Feature code:** LOOK
**Roadmap item:** ROAD-10 (MILE-4), second wave
**Requirements:** `requirements.md` · **Design:** `design.md`

Waves are ordered by dependency. Each wave ends green on
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo test --workspace`.

## Wave 1 — a look cannot write

- [ ] **1.1** `Db::open_read`: `SQLITE_OPEN_READ_ONLY`, no create, no DDL, no
  fence, short busy timeout, `query_only`. *(LOOK-2.1, LOOK-2.2, LOOK-2.3)*
- [ ] **1.2** Store tests: missing file does not create; protocol-skew leaves
  status and `min_writer_protocol` untouched; INSERT fails. *(LOOK-5.1, LOOK-5.2,
  LOOK-5.8)*
- [ ] **1.3** `agent_status` and `agent_sync` inspect half use `open_read`.
  `agent_respond` stays on `Db::open`. *(LOOK-2.4)*

## Wave 2 — custody tips without the database

- [ ] **2.1** `porch_git` helper: `for-each-ref` of a prefix. *(ARCH-2)*
- [ ] **2.2** Promote `eject::resolve_repo_id` to `pub`. List `refs/porch/recover`
  on the layout-derived bare; list leftover worktree HEADs. *(LOOK-3.1, LOOK-3.2)*
- [ ] **2.3** Two pins on one branch: status lists both; sync without `--run-id`
  still shows one. `porch.repo-id` ≠ path hash hits the configured bare.
  *(LOOK-5.6, LOOK-5.7)*

## Wave 3 — forward projection and the inspect command

- [ ] **3.1** `look.rs` join: condition + tips + stored-or-derived verdict.
  Stored wins; derived uses `classify` and never appends. *(LOOK-3.3 … LOOK-3.6)*
- [ ] **3.2** `porch status` / `porch runs` print the join; do not spawn; additive
  JSON (`condition`, `recovery_tips`, `forwards`). *(LOOK-1.1 … LOOK-1.3, LOOK-4.3)*
- [ ] **3.3** Intent-only, daemon dead: undetermined, `operator_note`, no
  “no pull request”. After restart: stored row. *(LOOK-5.4, LOOK-5.5)*
- [ ] **3.4** `porch status` / `runs` against unreachable / refusing / not-answering:
  no spawn, pid unchanged. *(LOOK-5.3)*

## Wave 4 — doctor, docs, ledger

- [ ] **4.1** Doctor: read-only DB check + recover-ref count. *(LOOK-4.2)*
- [ ] **4.2** `docs/usage.md` §H / §M / §U: status does not spawn; verdicts live
  in `forward_reconciliations` and can be derived before restart; `runs.error` is
  not the source of truth.
- [ ] **4.3** `CONTEXT.md` **Forward verdict** / inspect. Catalog LOOK.
  Close ROAD-10's inspect half on the roadmap; MILE-4 stays open on ROAD-24.
