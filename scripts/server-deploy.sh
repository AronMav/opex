#!/usr/bin/env bash
# Server-side deploy: build + atomic swap + service restart.
#
# Designed to be invoked via `make remote-deploy` or directly:
#   ssh server '~/hydeclaw-src/scripts/server-deploy.sh'
#
# Assumes:
#   - Rust toolchain installed at ~/.cargo/bin
#   - Source tree at ~/hydeclaw-src (cloned from github.com/AronMav/hydeclaw.git)
#   - Runtime at ~/hydeclaw with systemd --user units enabled
#
# Skip-build mode: pass --skip-build to deploy from existing target/release
# without rebuilding (useful for re-deploy after manual cargo build).

set -euo pipefail

SRC_DIR="${HOME}/hydeclaw-src"
RUN_DIR="${HOME}/hydeclaw"
CRATES=(hydeclaw-core hydeclaw-watchdog hydeclaw-memory-worker)
SUFFIX="x86_64"

# Load Rust env (cargo on PATH)
# shellcheck source=/dev/null
[[ -f "${HOME}/.cargo/env" ]] && . "${HOME}/.cargo/env"

if [[ "${1:-}" != "--skip-build" ]]; then
    echo "==> git pull"
    git -C "${SRC_DIR}" pull --ff-only

    echo "==> cargo build --release"
    (cd "${SRC_DIR}" && cargo build --release -p hydeclaw-core -p hydeclaw-watchdog -p hydeclaw-memory-worker)
fi

echo "==> atomic swap binaries"
for CRATE in "${CRATES[@]}"; do
    SRC_BIN="${SRC_DIR}/target/release/${CRATE}"
    DST_BIN="${RUN_DIR}/${CRATE}-${SUFFIX}"
    if [[ ! -f "${SRC_BIN}" ]]; then
        echo "  MISSING: ${SRC_BIN}"
        exit 1
    fi
    cp "${SRC_BIN}" "${DST_BIN}.new"
    mv -f "${DST_BIN}.new" "${DST_BIN}"
    chmod +x "${DST_BIN}"
    echo "  swapped ${CRATE}"
done

echo "==> restart systemd units"
for SVC in "${CRATES[@]}"; do
    if systemctl --user is-enabled "${SVC}" >/dev/null 2>&1; then
        systemctl --user restart "${SVC}"
        echo "  restarted ${SVC}"
    else
        echo "  ${SVC} not enabled, skipping"
    fi
done

echo "==> done"
