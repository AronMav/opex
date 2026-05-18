#!/usr/bin/env bash
# HydeClaw — update an existing installation to a new version.
#
# Usage:
#   ~/hydeclaw/update.sh hydeclaw-v0.2.0.tar.gz
#
# The script:
#   1. Stops all services
#   2. Extracts the archive to a temp directory
#   3. Replaces binaries, UI, channels, toolgate, docker, migrations, scripts
#   4. Preserves .env, config/, workspace/, PostgreSQL data
#   5. Starts services and verifies health
set -euo pipefail

BOLD='\033[1m'; NC='\033[0m'
C_OK='\033[38;2;0;229;204m'
C_WARN='\033[38;2;255;176;32m'
C_ERR='\033[38;2;230;57;70m'
C_MUTED='\033[38;2;90;100;128m'
C_ACCENT='\033[38;2;100;149;237m'

ok()   { echo -e "${C_OK}✓${NC} $*"; }
warn() { echo -e "${C_WARN}!${NC} $*"; }
err()  { echo -e "${C_ERR}✗${NC} $*"; exit 1; }
info() { echo -e "${C_MUTED}·${NC} $*"; }

ARCHIVE="${1:-}"
[[ -n "$ARCHIVE" ]] || err "Usage: $0 <hydeclaw-v*.tar.gz>"
[[ -f "$ARCHIVE" ]] || err "File not found: $ARCHIVE"

# Installation directory = where this script lives
DEST="$(cd "$(dirname "$0")" && pwd)"
[[ -f "$DEST/.env" ]] || err "$DEST/.env not found — not a valid HydeClaw installation"
[[ -f "$DEST/config/hydeclaw.toml" ]] || err "$DEST/config/hydeclaw.toml not found"

OLD_VERSION="unknown"
[[ -f "$DEST/VERSION" ]] && OLD_VERSION="$(tr -d '[:space:]' < "$DEST/VERSION")"

# ── Extract to temp ──
TMPDIR="$(mktemp -d)"
trap "rm -rf '$TMPDIR'" EXIT

info "Extracting archive..."
tar xzf "$ARCHIVE" -C "$TMPDIR"

# Find the extracted directory (hydeclaw/)
SRC="$TMPDIR/hydeclaw"
[[ -d "$SRC" ]] || err "Archive does not contain hydeclaw/ directory"
[[ -f "$SRC/VERSION" ]] || err "VERSION file not found in archive"
NEW_VERSION="$(tr -d '[:space:]' < "$SRC/VERSION")"
ls "$SRC"/hydeclaw-core-* &>/dev/null 2>&1 || err "No binaries found in archive"

# ── Banner ──
echo -e "${BOLD}${C_ACCENT}"
echo "  ╦ ╦╦ ╦╔╦╗╔═╗╔═╗╦  ╔═╗╦ ╦"
echo "  ╠═╣╚╦╝ ║║║╣ ║  ║  ╠═╣║║║"
echo "  ╩ ╩ ╩ ═╩╝╚═╝╚═╝╩═╝╩ ╩╚╩╝"
echo -e "${NC}"
echo -e "  ${C_ACCENT}Update: ${OLD_VERSION} → ${NEW_VERSION}${NC}"
echo ""
echo "  Archive: $ARCHIVE"
echo "  Install: $DEST"
echo ""
echo "  Will replace: binaries, UI, channels, toolgate, docker, migrations, scripts"
echo "  Will keep:    .env, config/, workspace/, PostgreSQL data"
echo ""

echo ""

# ── Detect architecture ──
ARCH=$(uname -m)
case "$ARCH" in
  aarch64|arm64) ARCH_SHORT="aarch64" ;;
  *)             ARCH_SHORT="x86_64" ;;
esac

# ── 0. Backup critical files (.env contains encryption keys — loss = data loss) ──
cp "$DEST/.env" "$DEST/.env.bak"
ok ".env backed up"

# ── 1. Stop services ──
info "Stopping services..."
for svc in hydeclaw-core hydeclaw-watchdog hydeclaw-memory-worker; do
  systemctl --user stop "$svc" 2>/dev/null || true
done
sleep 2

# Kill orphaned managed processes (channels, toolgate) that may survive Core shutdown
for pattern in "bun run src/index.ts" "uvicorn app:app.*--port 9011"; do
  pids=$(pgrep -f "$pattern" 2>/dev/null || true)
  if [[ -n "$pids" ]]; then
    warn "Killing orphaned process: $pattern (pids: $pids)"
    echo "$pids" | xargs kill 2>/dev/null || true
  fi
done
sleep 1
ok "Services stopped"

# ── 2. Replace binaries ──
info "Updating binaries..."
for CRATE in hydeclaw-core hydeclaw-watchdog hydeclaw-memory-worker; do
  BIN="$SRC/${CRATE}-${ARCH_SHORT}"
  if [[ -f "$BIN" ]]; then
    cp "$BIN" "$DEST/${CRATE}-${ARCH_SHORT}"
    chmod +x "$DEST/${CRATE}-${ARCH_SHORT}"
    ok "$CRATE ($(du -h "$BIN" | cut -f1))"
  fi
done

# ── 3. Replace UI ──
if [[ -f "$SRC/hydeclaw-ui.tar.gz" ]]; then
  info "Updating UI..."
  rm -rf "$DEST/ui/out"
  mkdir -p "$DEST/ui"
  tar xzf "$SRC/hydeclaw-ui.tar.gz" -C "$DEST/ui"
  ok "UI updated"
fi

# ── 4. Replace channels ──
if [[ -d "$SRC/channels" ]]; then
  info "Updating channels..."
  rm -rf "$DEST/channels/src"
  cp -r "$SRC/channels/src" "$DEST/channels/src"
  cp "$SRC/channels/package.json" "$DEST/channels/package.json"
  [[ -f "$SRC/channels/tsconfig.json" ]] && cp "$SRC/channels/tsconfig.json" "$DEST/channels/tsconfig.json"
  (export PATH="$HOME/.bun/bin:$PATH" && cd "$DEST/channels" && bun install 2>/dev/null) && ok "Channels updated" || warn "Channels copied (bun install failed — run manually)"
fi

# ── 5. Replace toolgate ──
if [[ -d "$SRC/toolgate" ]]; then
  info "Updating toolgate..."
  mkdir -p "$DEST/toolgate"
  # Preserve .venv, replace everything else
  find "$DEST/toolgate" -mindepth 1 -maxdepth 1 ! -name '.venv' -exec rm -rf {} + 2>/dev/null || true
  find "$SRC/toolgate" -mindepth 1 -maxdepth 1 ! -name '.venv' -exec cp -r {} "$DEST/toolgate/" \;
  if [[ -f "$DEST/toolgate/requirements.txt" ]] && [[ -d "$DEST/toolgate/.venv" ]]; then
    "$DEST/toolgate/.venv/bin/pip" install -q -r "$DEST/toolgate/requirements.txt" 2>/dev/null && ok "Toolgate updated" || warn "Toolgate copied (pip install failed — run manually)"
  else
    ok "Toolgate updated"
  fi
fi

# ── 6. Replace docker compose ──
if [[ -d "$SRC/docker" ]]; then
  info "Updating docker compose..."
  # Preserve docker/.env and Docker-owned config files
  [[ -f "$DEST/docker/.env" ]] && cp "$DEST/docker/.env" /tmp/hydeclaw-docker-env.bak
  # Use rsync to skip Docker-owned files (searxng config written by container at runtime)
  if command -v rsync &>/dev/null; then
    rsync -a --exclude 'config/searxng/' "$SRC/docker/" "$DEST/docker/"
  else
    cp -r "$SRC/docker/"* "$DEST/docker/" 2>/dev/null || true
  fi
  [[ -f /tmp/hydeclaw-docker-env.bak ]] && mv /tmp/hydeclaw-docker-env.bak "$DEST/docker/.env"
  # Rebuild images if Dockerfiles changed
  info "Rebuilding Docker images..."
  (cd "$DEST" && docker compose -f docker/docker-compose.yml build postgres browser-renderer 2>&1 | tail -3) || true
  [[ -f "$DEST/docker/Dockerfile.sandbox" ]] && \
    (cd "$DEST" && docker build -f docker/Dockerfile.sandbox -t hydeclaw-sandbox:latest . 2>&1 | tail -3) || true
  # MCP bridge base image (required by on-demand MCP containers)
  [[ -f "$DEST/docker/mcp-bridge/Dockerfile" ]] && \
    (cd "$DEST" && docker build -f docker/mcp-bridge/Dockerfile -t hydeclaw-mcp-bridge:latest docker/mcp-bridge/ 2>&1 | tail -3) || true
  # Re-create on-demand MCP containers (--no-recreate skips already-created ones)
  (cd "$DEST" && docker compose -f docker/docker-compose.yml --profile on-demand create --no-recreate 2>&1 | grep -E '(Created|Error|error)') || true
  ok "Docker compose and images updated"
fi

# ── 7. Replace migrations ──
if [[ -d "$SRC/migrations" ]]; then
  info "Updating migrations..."
  cp -r "$SRC/migrations/"* "$DEST/migrations/"
  ok "Migrations updated (will apply on next start)"
fi

# ── 8. Replace scripts ──
if [[ -d "$SRC/scripts" ]]; then
  info "Updating scripts..."
  mkdir -p "$DEST/scripts"
  cp -r "$SRC/scripts/"* "$DEST/scripts/"
  chmod +x "$DEST/scripts/"*.sh 2>/dev/null || true
  ok "Scripts updated"
fi

# ── 9. Update metadata ──
echo "$NEW_VERSION" > "$DEST/VERSION"
[[ -f "$SRC/setup.sh" ]] && cp "$SRC/setup.sh" "$DEST/setup.sh" && chmod +x "$DEST/setup.sh"
[[ -f "$SRC/update.sh" ]] && cp "$SRC/update.sh" "$DEST/update.sh" && chmod +x "$DEST/update.sh"
[[ -f "$SRC/uninstall.sh" ]] && cp "$SRC/uninstall.sh" "$DEST/uninstall.sh" && chmod +x "$DEST/uninstall.sh"
[[ -f "$SRC/.env.example" ]] && cp "$SRC/.env.example" "$DEST/.env.example"
[[ -f "$SRC/README.md" ]] && cp "$SRC/README.md" "$DEST/README.md"

# ── 10. Verify .env integrity (master key change = all secrets lost) ──
if ! diff -q "$DEST/.env" "$DEST/.env.bak" > /dev/null 2>&1; then
  warn ".env was modified during update — restoring from backup (master key change = secrets lost)"
  cp "$DEST/.env.bak" "$DEST/.env"
  ok ".env restored from backup"
fi
rm -f "$DEST/.env.bak"

# ── 11. Restart services ──
info "Starting services..."
systemctl --user daemon-reload 2>/dev/null || true
for svc in hydeclaw-core hydeclaw-watchdog hydeclaw-memory-worker; do
  if [[ -f ~/.config/systemd/user/${svc}.service ]]; then
    systemctl --user start "$svc" 2>/dev/null && ok "$svc started" || warn "$svc failed to start"
  fi
done

# ── 12. Health check ──
info "Waiting for core..."
for i in $(seq 1 20); do
  if curl -sf http://localhost:18789/health > /dev/null 2>&1; then
    ok "Core is healthy"
    break
  fi
  [[ "$i" -eq 20 ]] && warn "Core not responding yet — check logs: journalctl --user -u hydeclaw-core -f"
  sleep 1
done

# ── Done ──
echo ""
echo -e "${BOLD}${C_OK}Update complete: v${OLD_VERSION} → v${NEW_VERSION}${NC}"
echo ""
info "Logs: journalctl --user -u hydeclaw-core -f"
