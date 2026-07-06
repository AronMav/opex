#!/usr/bin/env bash
# Server-side deploy: build + atomic swap + service restart.
#
# Designed to be invoked via `make remote-deploy` or directly:
#   ssh server '~/opex-src/scripts/server-deploy.sh'
#
# Assumes:
#   - Rust toolchain installed at ~/.cargo/bin
#   - Source tree at ~/opex-src (cloned from github.com/AronMav/opex.git)
#   - Runtime at ~/opex with systemd --user units enabled
#
# Skip-build mode: pass --skip-build to deploy from existing target/release
# without rebuilding (useful for re-deploy after manual cargo build).

set -euo pipefail

SRC_DIR="${HOME}/opex-src"
RUN_DIR="${HOME}/opex"
# Build artifact names (cargo crate/binary names — renamed in PR1).
CRATES=(opex-core opex-watchdog opex-memory-worker)
# Install/unit names stay opex-* until PR2 (server dir + systemd units unchanged).
RUN_NAMES=(opex-core opex-watchdog opex-memory-worker)
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
for i in "${!CRATES[@]}"; do
    CRATE="${CRATES[$i]}"
    RUN_NAME="${RUN_NAMES[$i]}"
    SRC_BIN="${SRC_DIR}/target/release/${CRATE}"
    DST_BIN="${RUN_DIR}/${RUN_NAME}-${SUFFIX}"
    if [[ ! -f "${SRC_BIN}" ]]; then
        echo "  MISSING: ${SRC_BIN}"
        exit 1
    fi
    cp "${SRC_BIN}" "${DST_BIN}.new"
    mv -f "${DST_BIN}.new" "${DST_BIN}"
    chmod +x "${DST_BIN}"
    echo "  swapped ${CRATE} -> ${RUN_NAME}-${SUFFIX}"
done

# Migrations are loaded at RUNTIME from ${RUN_DIR}/migrations (main.rs:
# Migrator::new("migrations") with systemd WorkingDirectory=${RUN_DIR}), NOT embedded
# in the binary. `git pull` only updates ${SRC_DIR}, so new .sql files must be copied
# into the runtime dir before restart or they silently never apply.
echo "==> sync migrations to runtime dir"
mkdir -p "${RUN_DIR}/migrations"
cp -f "${SRC_DIR}"/migrations/*.sql "${RUN_DIR}/migrations/"
echo "  synced $(ls "${SRC_DIR}"/migrations/*.sql | wc -l) files (latest: $(basename "$(ls "${SRC_DIR}"/migrations/*.sql | sort | tail -1)"))"

# Toolgate is a managed Python process (NOT Rust, NOT Docker) launched by core
# from ${RUN_DIR}/toolgate. `git pull` only updates ${SRC_DIR}, so its .py files
# must be copied into the runtime dir before the core restart re-spawns toolgate,
# or code changes (e.g. video pipeline) silently never apply. (.env / venv stay.)
echo "==> sync toolgate sources to runtime"
cp -f "${SRC_DIR}"/toolgate/*.py "${RUN_DIR}/toolgate/" 2>/dev/null || true
# Sync every toolgate package subdir (routers/, providers/, handlers/, …),
# including nested dirs like handlers/builtin/. The old cp glob only synced
# top-level children — a missed nested subdir (e.g. builtin/summarize_video.py)
# silently shipped stale handler code.
for sub in "${SRC_DIR}"/toolgate/*/; do
  subname="$(basename "$sub")"
  case "$subname" in
    .venv|__pycache__|tests) continue ;;
  esac
  rsync -a --include='*.py' --include='*/' --exclude='*' "${sub}" "${RUN_DIR}/toolgate/${subname}/" 2>/dev/null || true
done
echo "  synced toolgate .py incl. subpackages (core restart re-spawns toolgate)"

# Toolgate Python deps: `git pull` updates requirements.txt in SRC, but the
# runtime venv is built only once (setup.sh) and the .py sync above never
# touches it. A NEW runtime import (e.g. the its/ router pulling in bs4 +
# markdownify) crash-loops toolgate on startup until the venv is refreshed.
# Reinstall only when requirements.txt actually changed — pip install -r is
# fast when already satisfied, but skipping the no-op keeps deploys quick.
# Fail-soft: a pip hiccup must not abort the deploy after binaries are swapped.
echo "==> sync toolgate python deps"
SRC_REQ="${SRC_DIR}/toolgate/requirements.txt"
RUN_REQ="${RUN_DIR}/toolgate/requirements.txt"
VENV_PY="${RUN_DIR}/toolgate/.venv/bin/python"
if [ -f "$SRC_REQ" ] && ! cmp -s "$SRC_REQ" "$RUN_REQ" 2>/dev/null; then
  if [ -x "$VENV_PY" ] && "$VENV_PY" -m pip install -q -r "$SRC_REQ"; then
    cp -f "$SRC_REQ" "$RUN_REQ"
    echo "  toolgate deps reinstalled (requirements.txt changed)"
  else
    echo "  WARNING: toolgate pip install failed — venv stale, toolgate may crash-loop"
  fi
else
  echo "  toolgate deps unchanged"
fi

# On-demand MCP containers must EXIST (stopped) for core's ContainerManager to
# start them — `ensure_running` only inspect+start, never create. `up --no-start`
# is idempotent: creates any missing container from already-built images. If an
# image is absent, build first: `docker compose --profile on-demand build`.
#
# `--no-recreate` is CRITICAL: without it, `up` reconciles every in-scope
# service — including the profile-less always-on ones (postgres,
# browser-renderer, …) — and RECREATES them on any config-hash drift, leaving
# them stopped (`--no-start`) for ~60s until something starts them again. That
# briefly kills Postgres mid-deploy and used to crash-loop opex-core with
# "pool timed out". `--no-recreate` makes this step create-missing-only and
# never touch running infrastructure. (core also retries its startup DB
# connection now, so this is defence-in-depth, not the sole guard.)
echo "==> ensure on-demand MCP containers exist"
if (cd "${RUN_DIR}/docker" && docker compose --profile on-demand up --no-start --no-recreate >/dev/null 2>&1); then
    echo "  MCP containers ensured ($(docker ps -a --format '{{.Names}}' | grep -c '^mcp-') present)"
else
    echo "  MCP ensure skipped — build images: (cd ${RUN_DIR}/docker && docker compose --profile on-demand build)"
fi

# NOTE: Docker MCP/service IMAGE changes (Dockerfile/app.js/ops.js) still need a
# manual rebuild — sync source + rebuild + recreate:
#   cp ${SRC_DIR}/docker/mcp/<svc>/* ${RUN_DIR}/docker/mcp/<svc>/ &&
#   (cd ${RUN_DIR}/docker && docker compose build mcp-<svc> &&
#    docker compose --profile on-demand up --no-start --force-recreate mcp-<svc>)

echo "==> restart systemd units"
for SVC in "${RUN_NAMES[@]}"; do
    if systemctl --user is-enabled "${SVC}" >/dev/null 2>&1; then
        systemctl --user restart "${SVC}"
        echo "  restarted ${SVC}"
    else
        echo "  ${SVC} not enabled, skipping"
    fi
done

echo "==> done"
