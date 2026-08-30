#!/usr/bin/env bash
# Install porch (and porch-quality) onto PATH (macOS / Linux).
#
# Preferred:
#   cargo install porch --locked
#
# One-liner from git:
#   curl -fsSL https://raw.githubusercontent.com/jayden-dang/porch/v0.2.2/install.sh | bash
#
# From a clone:
#   ./install.sh
#
# Default bindir: $CARGO_HOME/bin (usually ~/.cargo/bin).
# Override with PORCH_PREFIX=/path/to/bin.
# Pin a git ref with PORCH_GIT_REF=v0.2.2 (when not run from a clone).
# Dry-run: PORCH_INSTALL_DRY_RUN=1 ./install.sh

set -euo pipefail

PORCH_GIT_URL="${PORCH_GIT_URL:-https://github.com/jayden-dang/porch}"
PORCH_GIT_REF="${PORCH_GIT_REF:-v0.2.2}"

script_dir=""
if [[ -n "${BASH_SOURCE[0]:-}" && -f "${BASH_SOURCE[0]}" ]]; then
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fi

in_clone=0
if [[ -n "${script_dir}" && -f "${script_dir}/Cargo.toml" && -d "${script_dir}/crates/porch" ]]; then
  in_clone=1
fi

CARGO_HOME_DEFAULT="${CARGO_HOME:-${HOME}/.cargo}"
PREFIX="${PORCH_PREFIX:-${CARGO_HOME_DEFAULT}/bin}"
INSTALL_ROOT="$(dirname "$PREFIX")"

dry_run() {
  [[ "${PORCH_INSTALL_DRY_RUN:-}" == "1" ]]
}

log() {
  printf '%s\n' "$*"
}

if dry_run; then
  log "dry-run: would install porch + porch-quality to ${PREFIX}"
  if [[ "${in_clone}" -eq 1 ]]; then
    log "dry-run: preferred: cargo install --path crates/porch --locked --root ${INSTALL_ROOT}"
  else
    log "dry-run: cargo install --git ${PORCH_GIT_URL} --tag ${PORCH_GIT_REF} --locked --force --root ${INSTALL_ROOT} porch"
  fi
  log "dry-run: fallback: copy target/{release,debug}/porch{,-quality} into ${PREFIX}"
  log "dry-run: ensure ${PREFIX} (typically ~/.cargo/bin) is on PATH"
  log "dry-run:   export PATH=\"${PREFIX}:\$PATH\""
  exit 0
fi

if ! command -v cargo >/dev/null 2>&1 && [[ "${in_clone}" -eq 0 ]]; then
  log "error: cargo is required (https://rustup.rs) — Rust 1.85+" >&2
  exit 1
fi

mkdir -p "$PREFIX"

installed=""
quality_installed=""

if command -v cargo >/dev/null 2>&1; then
  if [[ "${in_clone}" -eq 1 ]]; then
    log "building with cargo install --path crates/porch --locked --root ${INSTALL_ROOT}"
    cargo install --path "${script_dir}/crates/porch" --locked --force --root "${INSTALL_ROOT}"
  else
    log "building with cargo install --git ${PORCH_GIT_URL} --tag ${PORCH_GIT_REF} --locked porch"
    cargo install --git "${PORCH_GIT_URL}" --tag "${PORCH_GIT_REF}" --locked --force --root "${INSTALL_ROOT}" porch
  fi
  installed="${PREFIX}/porch"
  quality_installed="${PREFIX}/porch-quality"
elif [[ "${in_clone}" -eq 1 && -x "${script_dir}/target/release/porch" ]]; then
  install -m 755 "${script_dir}/target/release/porch" "${PREFIX}/porch"
  installed="${PREFIX}/porch"
  if [[ -x "${script_dir}/target/release/porch-quality" ]]; then
    install -m 755 "${script_dir}/target/release/porch-quality" "${PREFIX}/porch-quality"
    quality_installed="${PREFIX}/porch-quality"
  fi
elif [[ "${in_clone}" -eq 1 && -x "${script_dir}/target/debug/porch" ]]; then
  log "note: installing debug binary from target/debug (prefer cargo install or target/release)"
  install -m 755 "${script_dir}/target/debug/porch" "${PREFIX}/porch"
  installed="${PREFIX}/porch"
  if [[ -x "${script_dir}/target/debug/porch-quality" ]]; then
    install -m 755 "${script_dir}/target/debug/porch-quality" "${PREFIX}/porch-quality"
    quality_installed="${PREFIX}/porch-quality"
  fi
else
  log "error: cargo not found and no built binary under target/{release,debug}/porch" >&2
  log "hint: install Rust (https://rustup.rs) then re-run this script" >&2
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
if [[ -x "${quality_installed}" ]]; then
  log "installed: ${quality_installed}"
fi
if [[ -x "${installed}" ]]; then
  "${installed}" --version || true
fi
log "next: export PATH=\"${PREFIX}:\$PATH\"  # if needed"
log "next: porch setup && porch doctor"
