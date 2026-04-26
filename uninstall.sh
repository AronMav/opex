#!/usr/bin/env bash
# HydeClaw — complete uninstaller.
# After this script, NOTHING hydeclaw-related remains on the system
# (except Docker engine itself and system packages).
#
# Usage: ./uninstall.sh [--yes]
#   --yes   Skip all confirmations (dangerous)
set -uo pipefail  # no -e: we handle errors ourselves

YES=0
for arg in "$@"; do [[ "$arg" == "--yes" ]] && YES=1; done

BOLD='\033[1m'; NC='\033[0m'
C_OK='\033[38;2;0;229;204m'
C_WARN='\033[38;2;255;176;32m'
C_ERR='\033[38;2;230;57;70m'
C_MUTED='\033[38;2;90;100;128m'

ok()   { echo -e "${C_OK}✓${NC} $*"; }
warn() { echo -e "${C_WARN}!${NC} $*"; }
err()  { echo -e "${C_ERR}✗${NC} $*"; }
info() { echo -e "${C_MUTED}·${NC} $*"; }

# Determine hydeclaw root: prefer directory containing this script,
# but if script is in /tmp or other external location, use $PWD
_SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
if [[ -f "$_SCRIPT_DIR/config/hydeclaw.toml" ]]; then
  ROOT="$_SCRIPT_DIR"
elif [[ -f "$PWD/config/hydeclaw.toml" ]]; then
  ROOT="$PWD"
else
  echo "Error: cannot find hydeclaw installation. Run from the hydeclaw directory."
  exit 1
fi
confirm() {
  [[ "$YES" == "1" ]] && return 0
  read -r -p "  Type 'yes' to confirm: " ans
  [[ "$ans" == "yes" ]]
}

echo -e "${BOLD}${C_ERR}"
echo "  ╦ ╦╔╗╔╦╔╗╔╔═╗╔╦╗╔═╗╦  ╦  "
echo "  ║ ║║║║║║║║╚═╗ ║ ╠═╣║  ║  "
echo "  ╚═╝╝╚╝╩╝╚╝╚═╝ ╩ ╩ ╩╩═╝╩═╝"
echo -e "${NC}"
echo -e "  ${C_ERR}This will permanently remove HydeClaw and all its data.${NC}"
echo ""
echo "  Directory: $ROOT"
echo ""
echo "  Will remove:"
echo "    • All hydeclaw systemd services"
echo "    • ALL Docker containers (compose infra + bollard-managed MCP/agents)"
echo "    • PostgreSQL data (Docker volume)"
echo "    • The entire $ROOT directory"
echo ""

if ! confirm "Proceed with complete uninstall?"; then
  echo "Cancelled."
  exit 0
fi
echo ""

# ════════════════════════════════════════════════════════════
# 1. Systemd — stop, disable, remove unit files
# ════════════════════════════════════════════════════════════
info "Stopping systemd services..."

# Use wildcard glob for any hydeclaw-* unit
for unit_file in ~/.config/systemd/user/hydeclaw*.service; do
  [[ -f "$unit_file" ]] || continue
  svc="$(basename "$unit_file" .service)"
  systemctl --user stop "$svc" 2>/dev/null || true
  systemctl --user disable "$svc" 2>/dev/null || true
  rm -f "$unit_file"
done

systemctl --user daemon-reload 2>/dev/null || true
systemctl --user reset-failed 2>/dev/null || true

# Kill remaining processes (match binary name prefix — covers both
# hydeclaw-core and hydeclaw-core-aarch64 release naming)
for proc in hydeclaw-core hydeclaw-watchdog hydeclaw-memory-worker; do
  pkill -f "/${proc}" 2>/dev/null || true
done
# Also kill managed child processes (bun channels, uvicorn toolgate)
pkill -f "bun.*channels/src" 2>/dev/null || true
pkill -f "uvicorn.*app:app.*9011" 2>/dev/null || true
sleep 1

ok "Systemd services and processes stopped"

# ════════════════════════════════════════════════════════════
# 2. Docker — stop EVERYTHING hydeclaw-related
# ════════════════════════════════════════════════════════════
if command -v docker &>/dev/null; then
  info "Stopping Docker containers..."

  # 2a. Compose-managed (postgres, searxng, browser-renderer)
  # Must run BEFORE we delete any files (needs docker-compose.yml and docker/.env)
  if [[ -f "$ROOT/docker/docker-compose.yml" ]]; then
    docker compose -f "$ROOT/docker/docker-compose.yml" down -v --remove-orphans 2>/dev/null || true
  fi

  # 2b. Bollard-managed containers (hc-agent-*, hc-docker-*, mcp-*)
  # AND any compose containers that survived (docker-postgres-1, docker-searxng-1, etc.)
  for c in $(docker ps -a --format '{{.Names}}' 2>/dev/null | grep -E '^(hc-|mcp-|docker-)' || true); do
    docker rm -f "$c" 2>/dev/null || true
  done

  # 2c. Remove Docker network and volumes
  docker network rm hydeclaw 2>/dev/null || true
  docker volume rm docker_pgdata 2>/dev/null || true

  # 2d. Remove HydeClaw Docker images (hydeclaw-*, browser-renderer, searxng)
  for img in $(docker images --format '{{.Repository}}:{{.Tag}}' 2>/dev/null | grep -E '^(hydeclaw-|browser-renderer|searxng/)' || true); do
    docker rmi "$img" 2>/dev/null || true
  done

  ok "Docker containers, volumes, network, and images removed"
else
  warn "Docker not found — skipping container cleanup"
fi

# ════════════════════════════════════════════════════════════
# 3. Remove the entire directory
# ════════════════════════════════════════════════════════════
info "Removing $ROOT ..."

# Safety: never delete / or /home or /home/user
case "$ROOT" in
  /|/home|/home/*)
    # Only allow if it's at least 3 levels deep (/home/user/hydeclaw)
    depth=$(echo "$ROOT" | tr -cd '/' | wc -c)
    if [[ "$depth" -lt 3 ]]; then
      err "Refusing to delete $ROOT (too shallow). Remove manually."
      exit 1
    fi
    ;;
esac

# Move to a safe dir before deleting
cd /tmp 2>/dev/null || cd /

# Try without sudo first; use sudo only if needed (Docker-owned files)
if rm -rf "$ROOT" 2>/dev/null; then
  ok "Directory removed"
elif sudo rm -rf "$ROOT" 2>/dev/null; then
  ok "Directory removed (with sudo)"
else
  err "Could not remove $ROOT — try: sudo rm -rf $ROOT"
  exit 1
fi

# ════════════════════════════════════════════════════════════
# 4. Clean up stray files outside the directory
# ════════════════════════════════════════════════════════════
rm -f /tmp/hydeclaw-watchdog.json /tmp/hydeclaw-docker-env.bak 2>/dev/null || true

echo ""
echo -e "${C_OK}${BOLD}HydeClaw completely uninstalled.${NC}"
echo ""
echo -e "${C_MUTED}Remaining on system (not removed):${NC}"
echo -e "${C_MUTED}  • Docker engine${NC}"
echo -e "${C_MUTED}  • Bun, Python, Node.js${NC}"
echo -e "${C_MUTED}  • To remove those: apt remove docker-ce / rm -rf ~/.bun${NC}"
