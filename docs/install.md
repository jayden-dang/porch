# Install porch

Porch is a local git gate. Consent is `git push porch`. `origin` is never rewritten.

One package installs **both** binaries:

| Binary | Role |
|---|---|
| `porch` | Gate CLI and daemon |
| `porch-quality` | Mandatory **deterministic floor** sibling of `porch`. `engine: quality` is floor-only (no judgment); `engine: agent` still requires this sibling |

Needs **Rust 1.85+** ([rustup](https://rustup.rs)) and **git**. Default bindir is `~/.cargo/bin` — add it to `PATH` if `porch doctor` warns.

## crates.io (recommended)

```sh
cargo install porch --locked
export PATH="$HOME/.cargo/bin:$PATH"
porch setup
porch doctor
```

**Coming from v0.2.1 or earlier?** That release shipped the floor as a separate
package and told you to install it, so cargo now refuses to overwrite a file it
records as belonging to `porch-quality`:

```
error: binary `porch-quality` already exists in destination as part of `porch-quality v0.2.1`
Add --force to overwrite
```

This installs **nothing** — not even `porch`, so you stay on the old release. Either
force the overwrite, or remove the standalone package first:

```sh
cargo install porch --locked --force     # or: cargo uninstall porch-quality, then install
```

`install.sh` and the `--git` command below already pass `--force`, so they overwrite
without asking. Only the plain `cargo install porch --locked` above refuses.

## From GitHub

```sh
curl -fsSL https://raw.githubusercontent.com/jayden-dang/porch/v0.2.3/install.sh | bash
export PATH="$HOME/.cargo/bin:$PATH"
porch setup
porch doctor
```

Or clone and `./install.sh`. Dry-run: `PORCH_INSTALL_DRY_RUN=1 ./install.sh`. Bindir: `PORCH_PREFIX=/usr/local/bin ./install.sh`.

```sh
cargo install --git https://github.com/jayden-dang/porch --tag v0.2.3 --locked --force porch
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

`porch doctor` reports the floor's path and the artifact identity porch observed for
it. That identity is what the assurance record stores, so two installations that
report the same one are running the same floor. Porch does not judge the value: it
has no expected identity to compare against, and the sibling is whatever is next to
the running `porch`.

Removing the floor disarms the gate. `cargo uninstall porch-quality` leaves `porch`
installed and working as a command, while every run fails closed with
`floor_unresolved`. The remedy is to reinstall
(`cargo install porch --locked --force`), not to restart the daemon.

## Upgrading

Before upgrading a machine that already has `$PORCH_HOME` state, see both upgrade fences in
usage: [§R — review-round / mandatory floor](usage.md#r-upgrading-porch-review-round-identity-and-the-mandatory-floor)
and [§S — phase-event history (protocol 3)](usage.md#s-upgrading-porch-phase-event-history).
Finish parked runs when you can, back up `$PORCH_HOME`, and do not expect downgrade after
new-format rounds or phase-events exist. Opening a phase-events binary against an older
`$PORCH_HOME` fail-forwards every still-active run under **protocol 3**. Rollback of an
upgraded state root to a pre-floor or pre-phase-events binary is **unsupported**. Recovery
after a floor failure or fail-forwarded run is `porch rerun --run-id <ULID>` (restart the
daemon first when the floor executable could not be resolved).

**Stop the daemon before upgrading**, then start it again afterwards:

```sh
porch daemon stop
cargo install porch --locked --force
porch daemon start
```

An install replaces its destination by rename, so a daemon that was already running
keeps the old executable while the directory it came from now holds the new floor.
Porch refuses rather than pairing them: the run fails closed as
`floor_launch_replaced`, and `porch doctor` reports `porch was replaced while
running`. Restarting the daemon clears it. Porch detects this by noticing that its
own path no longer names a file, which is how Linux reports a replaced binary; a
platform that reports it differently will not refuse, so stopping the daemon first is
the reliable step rather than the fallback.
