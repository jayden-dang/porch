#!/usr/bin/env bash
# Install the porch binary onto PATH (macOS / Linux).
#
# Default bindir: $CARGO_HOME/bin (usually ~/.cargo/bin).
# Override with PORCH_PREFIX=/path/to/bin.
#
# Dry-run (no writes): PORCH_INSTALL_DRY_RUN=1 ./install.sh
#
# Alternative without this script:
#   cargo install --path crates/porch --locked
#   cargo install --path crates/porch-quality --locked
#
# Not published to crates.io yet (slice crates stay publish = false).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
CARGO_HOME_DEFAULT="${CARGO_HOME:-${HOME}/.cargo}"
PREFIX="${PORCH_PREFIX:-${CARGO_HOME_DEFAULT}/bin}"
# cargo install --root expects the parent of bin/ (e.g. ~/.cargo for ~/.cargo/bin).
INSTALL_ROOT="$(dirname "$PREFIX")"

dry_run() {
  [[ "${PORCH_INSTALL_DRY_RUN:-}" == "1" ]]
}

log() {
  printf '%s\n' "$*"
}

if dry_run; then
  log "dry-run: would install porch + porch-quality to ${PREFIX}"
  log "dry-run: preferred: cargo install --path crates/porch --locked --root ${INSTALL_ROOT}"
  log "dry-run: preferred: cargo install --path crates/porch-quality --locked --root ${INSTALL_ROOT}"
  log "dry-run: fallback: copy target/{release,debug}/porch{,-quality} into ${PREFIX}"
  log "dry-run: ensure ${PREFIX} (typically ~/.cargo/bin) is on PATH"
  log "dry-run:   export PATH=\"${PREFIX}:\$PATH\""
  exit 0
fi

mkdir -p "$PREFIX"

installed=""
quality_installed=""
if command -v cargo >/dev/null 2>&1; then
  log "building with cargo install --path crates/porch --locked --root ${INSTALL_ROOT}"
  cargo install --path "${REPO_ROOT}/crates/porch" --locked --force --root "${INSTALL_ROOT}"
  installed="${PREFIX}/porch"
  log "building with cargo install --path crates/porch-quality --locked --root ${INSTALL_ROOT}"
  cargo install --path "${REPO_ROOT}/crates/porch-quality" --locked --force --root "${INSTALL_ROOT}"
  quality_installed="${PREFIX}/porch-quality"
elif [[ -x "${REPO_ROOT}/target/release/porch" ]]; then
  install -m 755 "${REPO_ROOT}/target/release/porch" "${PREFIX}/porch"
  installed="${PREFIX}/porch"
  if [[ -x "${REPO_ROOT}/target/release/porch-quality" ]]; then
    install -m 755 "${REPO_ROOT}/target/release/porch-quality" "${PREFIX}/porch-quality"
    quality_installed="${PREFIX}/porch-quality"
  fi
elif [[ -x "${REPO_ROOT}/target/debug/porch" ]]; then
  log "note: installing debug binary from target/debug/porch (prefer cargo install or target/release)"
  install -m 755 "${REPO_ROOT}/target/debug/porch" "${PREFIX}/porch"
  installed="${PREFIX}/porch"
  if [[ -x "${REPO_ROOT}/target/debug/porch-quality" ]]; then
    install -m 755 "${REPO_ROOT}/target/debug/porch-quality" "${PREFIX}/porch-quality"
    quality_installed="${PREFIX}/porch-quality"
  fi
else
  log "error: cargo not found and no built binary under target/{release,debug}/porch" >&2
  log "hint: install Rust (https://rustup.rs) or run: cargo build --release -p porch -p porch-quality" >&2
  exit 1
fi

case ":${PATH}:" in
  *":${PREFIX}:"*) ;;
  *)
    log "note: ${PREFIX} is not on PATH — add it to your shell profile:"
    log "  export PATH=\"${PREFIX}:\$PATH\""
    ;;
esac

log "installed: ${installed}"
if [[ -n "${quality_installed}" ]]; then
  log "installed: ${quality_installed}"
fi
if [[ -x "${installed}" ]]; then
  "${installed}" --version || true
fi
