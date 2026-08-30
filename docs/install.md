# Install porch

Porch is a local git gate. Consent is `git push porch`. `origin` is never rewritten.

One package installs **both** binaries:

| Binary | Role |
|---|---|
| `porch` | Gate CLI and daemon |
| `porch-quality` | Review engine (`porch setup --engine quality`) |

Needs **Rust 1.85+** ([rustup](https://rustup.rs)) and **git**. Default bindir is `~/.cargo/bin` — add it to `PATH` if `porch doctor` warns.

## crates.io (recommended)

```sh
cargo install porch --locked
export PATH="$HOME/.cargo/bin:$PATH"
porch setup
porch doctor
```

## From GitHub

```sh
curl -fsSL https://raw.githubusercontent.com/jayden-dang/porch/v0.2.2/install.sh | bash
export PATH="$HOME/.cargo/bin:$PATH"
porch setup
porch doctor
```

Or clone and `./install.sh`. Dry-run: `PORCH_INSTALL_DRY_RUN=1 ./install.sh`. Bindir: `PORCH_PREFIX=/usr/local/bin ./install.sh`.

```sh
cargo install --git https://github.com/jayden-dang/porch --tag v0.2.2 --locked --force porch
```

From a checkout: `cargo install --path crates/porch --locked --force`.

## After install

```sh
porch setup          # TTY: one screen; headless: porch setup --yes
porch doctor
cd /path/to/your/git/clone
porch init
git push porch HEAD:refs/heads/$(git branch --show-current)
```

Do not set `PORCH_REVIEW_BIN=ocr`.
