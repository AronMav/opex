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
CRATES=(opex-core opex-watchdog opex-memory-worker)
SUFFIX="x86_64"

# Load Rust env (cargo on PATH)
# shellcheck source=/dev/null
[[ -f "${HOME}/.cargo/env" ]] && . "${HOME}/.cargo/env"

if [[ "${1:-}" != "--skip-build" ]]; then
    echo "==> git pull"
    git -C "${SRC_DIR}" pull --ff-only

    echo "==> cargo build --release (with gemini-cloudcode feature)"
    (cd "${SRC_DIR}" && cargo build --release \
        --features opex-core/gemini-cloudcode \
        -p opex-core -p opex-watchdog -p opex-memory-worker)
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

# Migrations are loaded at RUNTIME from ${RUN_DIR}/migrations (main.rs:
# Migrator::new("migrations") with systemd WorkingDirectory=${RUN_DIR}), NOT embedded
# in the binary. `git pull` only updates ${SRC_DIR}, so new .sql files must be copied
# into the runtime dir before restart or they silently never apply.
echo "==> sync migrations to runtime dir"
mkdir -p "${RUN_DIR}/migrations"
cp -f "${SRC_DIR}"/migrations/*.sql "${RUN_DIR}/migrations/"
echo "  synced $(ls "${SRC_DIR}"/migrations/*.sql | wc -l) files (latest: $(basename "$(ls "${SRC_DIR}"/migrations/*.sql | sort | tail -1)"))"

# NOTE: Docker images (browser-renderer, etc.) build from ${RUN_DIR}/docker — NOT
# ${SRC_DIR}. Docker-side changes still need a manual sync + rebuild:
#   cp ${SRC_DIR}/docker/<svc>/* ${RUN_DIR}/docker/<svc>/ &&
#   (cd ${RUN_DIR}/docker && docker compose build <svc> && docker compose up -d <svc>)

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
