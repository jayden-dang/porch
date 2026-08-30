# Install porch 0.2.0

Porch is a local git gate. Consent is `git push porch`. Slice crates stay unpublished (`publish = false`), so **this release is git/tag install only** — not `cargo install porch` from crates.io.

You need **Rust 1.85+** ([rustup](https://rustup.rs)) and **git**. The installer builds two binaries:

| Binary | Role |
|---|---|
| `porch` | Gate CLI and daemon |
| `porch-quality` | Optional review engine (`porch setup --engine quality`) |

Both land in `~/.cargo/bin` by default. That directory is often **missing from PATH** even when `rustup` works — `porch doctor` will say so.

## Fastest (recommended)

```sh
curl -fsSL https://raw.githubusercontent.com/jayden-dang/porch/v0.2.0/install.sh | bash
export PATH="$HOME/.cargo/bin:$PATH"   # add to ~/.zshrc or ~/.bashrc if doctor warns
porch setup
porch doctor
```

Pin a different tag with `PORCH_GIT_REF`:

```sh
curl -fsSL https://raw.githubusercontent.com/jayden-dang/porch/v0.2.0/install.sh | PORCH_GIT_REF=v0.2.0 bash
```

First run compiles from source (a few minutes). Later upgrades are the same command.

## From a clone

```sh
git clone https://github.com/jayden-dang/porch.git
cd porch
git checkout v0.2.0
./install.sh
```

Dry-run (no writes): `PORCH_INSTALL_DRY_RUN=1 ./install.sh`.

Bindir override: `PORCH_PREFIX=/usr/local/bin ./install.sh`.

## Cargo only

```sh
cargo install --git https://github.com/jayden-dang/porch --tag v0.2.0 --locked --force porch
cargo install --git https://github.com/jayden-dang/porch --tag v0.2.0 --locked --force porch-quality
```

From a checkout: `cargo install --path crates/porch --locked --force` and the same for `crates/porch-quality`.

## PATH

If `command -v porch` fails after install:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
# persist: echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.zshrc
```

`porch doctor` prints `[ok] porch: … (version 0.2.0)` when the binary and `git` are visible.

## After install

```sh
porch setup          # TTY: one screen, Enter; headless: porch setup --yes
porch doctor         # review engine should be quality or agent (not missing)
cd /path/to/your/git/clone
porch init
git push porch HEAD:refs/heads/$(git branch --show-current)
porch                # TUI if a run is active; else list / setup
```

Full loop: [10-operator-checklist.md](10-operator-checklist.md). Review default is a session-free coding agent, or `porch-quality` when that binary is on PATH. Do not set `PORCH_REVIEW_BIN=ocr`.

## What this release is not

Not crates.io. Not a prebuilt GitHub Actions matrix of `.tar.gz` yet (source build via Cargo). Not Windows-first (`install.sh` is macOS/Linux).
