# Changelog

## 0.2.0 — 2026-08-30

Tagged release of the M10–M16 operator loop. Git/tag install only (slices stay `publish = false`).

- **Review:** default is a session-free coding-agent turn; optional `porch-quality` engine; OCR is legacy (`--engine ocr`).
- **Install:** `install.sh` installs `porch` and `porch-quality`; one-liner from the `v0.2.0` tag.
- **Operator:** setup TUI, skill on `init`, park TUI hunks, `eject`, rebase-park, `rerun`, `agent sync`, `agent run --intent --wait`.
- **Docs:** [docs/install.md](docs/install.md), [docs/10-operator-checklist.md](docs/10-operator-checklist.md).

## 0.1.0 — 2026-08-29

First cut: gate through M9 (doctor, setup OCR wrapper, TUI attach, GitHub deliver).
