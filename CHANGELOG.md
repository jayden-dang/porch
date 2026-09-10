# Changelog

## 0.2.3 — 2026-09-10

Unifies every published crate at **0.2.3** (workspace slices were still 0.2.1 while
`porch` was 0.2.2). Catch-up of `main` since the 30 August crates.io cut.

- **Forward:** durable authorization before any push (FWDAUTH), restart
  reconciliation of interrupted forwards (RECON), and a fault-injection suite
  across that boundary (FAULT). **GOAL-1 / MILE-3 Closed.**
- **Equality (EQUAL):** mutating `commands.format` runs at the end of rebase;
  certify is verify-only; `authorized_forward_sha` binds by equality.
- **Escape:** detach without a healthy daemon (ESCAPE); daemon-free inspect via
  `porch status` / `porch runs` (LOOK); `--purge` refuses unforwarded custody tips
  unless `--abandon` writes a durable manifest first (PURGE). **GOAL-4 / MILE-4 Closed.**
- **Daemon:** named daemon condition, bounded RPC, one daemon per state root
  (DFAULT).
- **Install (ONEBIN):** refuse a floor when `porch` was replaced while running;
  `doctor` reports the floor's observed identity; `--engine quality` resolves the
  sibling; ETXTBSY retry on floor spawn; release smoke uses `porch-quality --help`
  (the floor has no `--version`).
- **Review:** a blank `PORCH_REVIEW_*` override reads as unset; tests do not
  inherit the operator's review environment.
- **This tree:** `.porch.yaml` on `main` so porch can gate porch. That is not
  GOAL-3 / MILE-7 (mailgate then klynt).

## 0.2.2 — 2026-08-30

`cargo install porch` ships **both** `porch` and `porch-quality`. Briefing docs in `docs/` removed except [install.md](docs/install.md) (rewrite pending).

## 0.2.1 — 2026-08-30

Publish the slice graph to crates.io so `cargo install porch` works. Path+version workspace deps. Same operator loop as 0.2.0.

## 0.2.0 — 2026-08-30

Tagged release of the M10–M16 operator loop. Git/tag install only (slices stay `publish = false`).

- **Review:** default is a session-free coding-agent turn; optional `porch-quality` engine; OCR is legacy (`--engine ocr`).
- **Install:** `install.sh` installs `porch` and `porch-quality`; one-liner from the `v0.2.0` tag.
- **Operator:** setup TUI, skill on `init`, park TUI hunks, `eject`, rebase-park, `rerun`, `agent sync`, `agent run --intent --wait`.
- **Docs:** [docs/install.md](docs/install.md).

## 0.1.0 — 2026-08-29

First cut: gate through M9 (doctor, setup OCR wrapper, TUI attach, GitHub deliver).
