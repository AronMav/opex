#!/usr/bin/env bash
# Build versioned release artifacts for HydeClaw.
#
# Usage:
#   ./release.sh 1.0.0        # build for host architecture
#   ./release.sh 1.0.0 --all  # build for aarch64 + x86_64
#
# Version is the single required argument. It is synced to Cargo.toml,
# ui/package.json, and channels/package.json before building.
#
# Output: release/hydeclaw-v{VERSION}.tar.gz
#   hydeclaw-core-{arch}      — Rust binary per platform
#   hydeclaw-ui.tar.gz        — pre-built Next.js static UI
#   config/                   — default config files
#   migrations/               — database schema
#   workspace/                — tools, skills, MCP definitions
#   channels/                 — TypeScript channel adapters (source)
#   toolgate/                 — Python media hub (source)
#   setup.sh                  — interactive installer
#   .env.example              — environment template
#   VERSION                   — version written into archive for setup/update scripts

set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'
info()  { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
err()   { echo -e "${RED}[ERR]${NC}   $*"; exit 1; }

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

# ── Parse args ──
BUILD_ALL=false
VERSION=""
for arg in "$@"; do
  case "$arg" in
    --all) BUILD_ALL=true ;;
    *) VERSION="$arg" ;;
  esac
done

# ── Version ──
[ -n "$VERSION" ] || err "Usage: ./release.sh <version> [--all]  (e.g. ./release.sh 0.2.0 --all)"
RELEASE_DIR="$ROOT/release/hydeclaw"

# ── Sync version across all manifests ──
info "Syncing version ${VERSION} across manifests..."
sed -i "s/^version = \".*\"/version = \"${VERSION}\"/" Cargo.toml
sed -i "s/\"version\": \".*\"/\"version\": \"${VERSION}\"/" ui/package.json
sed -i "s/\"version\": \".*\"/\"version\": \"${VERSION}\"/" channels/package.json

echo -e "${BOLD}"
echo "  Building HydeClaw v${VERSION}"
echo -e "${NC}"

# ── Clean ──
rm -rf "$RELEASE_DIR"
mkdir -p "$RELEASE_DIR"

# ── Determine targets ──
TARGETS=()
if [ "$BUILD_ALL" = true ]; then
  TARGETS=("aarch64-unknown-linux-gnu" "x86_64-unknown-linux-gnu")
else
  # Detect host architecture
  ARCH=$(uname -m 2>/dev/null || echo "x86_64")
  case "$ARCH" in
    aarch64|arm64) TARGETS=("aarch64-unknown-linux-gnu") ;;
    *)             TARGETS=("x86_64-unknown-linux-gnu") ;;
  esac
fi

# ── Build Rust binaries ──
for TARGET in "${TARGETS[@]}"; do
  ARCH_SHORT="${TARGET%%-*}"  # aarch64 or x86_64
  info "Building hydeclaw-core for ${TARGET}..."

  # Build all 3 binaries for this target
  for CRATE in hydeclaw-core hydeclaw-watchdog hydeclaw-memory-worker; do
    info "  ${CRATE} for ${TARGET}..."
    if [ "$TARGET" = "$(rustc -vV 2>/dev/null | grep host | cut -d' ' -f2)" ]; then
      cargo build --release -p "$CRATE"
      cp "target/release/${CRATE}" "$RELEASE_DIR/${CRATE}-${ARCH_SHORT}"
    else
      if command -v cargo-zigbuild &>/dev/null; then
        cargo zigbuild --release --target "$TARGET" -p "$CRATE"
      else
        cargo build --release --target "$TARGET" -p "$CRATE"
      fi
      cp "target/${TARGET}/release/${CRATE}" "$RELEASE_DIR/${CRATE}-${ARCH_SHORT}"
    fi
    chmod +x "$RELEASE_DIR/${CRATE}-${ARCH_SHORT}"
  done

  for BIN in "$RELEASE_DIR"/*-"${ARCH_SHORT}"; do
    SIZE=$(du -h "$BIN" | cut -f1)
    ok "$(basename "$BIN") (${SIZE})"
  done
done

# ── Build UI ──
info "Building Next.js UI..."
(cd ui && npm install --silent && npm run build) || err "UI build failed"
tar czf "$RELEASE_DIR/hydeclaw-ui.tar.gz" -C ui out
UI_SIZE=$(du -h "$RELEASE_DIR/hydeclaw-ui.tar.gz" | cut -f1)
ok "hydeclaw-ui.tar.gz (${UI_SIZE})"

# ── Copy runtime files ──
info "Packaging runtime files..."

# Config
cp -r config "$RELEASE_DIR/config"

# Migrations
cp -r migrations "$RELEASE_DIR/migrations"

# Workspace (tools, skills, MCP — exclude uploads and user data)
mkdir -p "$RELEASE_DIR/workspace"
for dir in tools skills mcp agents prompts; do
  [ -d "workspace/$dir" ] && cp -r "workspace/$dir" "$RELEASE_DIR/workspace/$dir"
done
# Copy workspace root docs (AGENTS.md, TOOLS.md, etc.)
find workspace -maxdepth 1 -name '*.md' -exec cp {} "$RELEASE_DIR/workspace/" \;

# Channel adapters (source — user runs bun install)
mkdir -p "$RELEASE_DIR/channels"
cp channels/package.json channels/bun.lock* "$RELEASE_DIR/channels/" 2>/dev/null || true
cp -r channels/src "$RELEASE_DIR/channels/src"
[ -f channels/tsconfig.json ] && cp channels/tsconfig.json "$RELEASE_DIR/channels/"

# Toolgate (source — user runs pip install)
# Copy entire toolgate dir except venv, __pycache__, .pyc
rsync -a --exclude '.venv' --exclude '__pycache__' --exclude '*.pyc' \
  toolgate/ "$RELEASE_DIR/toolgate/" 2>/dev/null || {
  # Fallback if rsync not available
  cp -r toolgate "$RELEASE_DIR/toolgate.tmp"
  rm -rf "$RELEASE_DIR/toolgate.tmp/.venv" "$RELEASE_DIR/toolgate.tmp/__pycache__"
  find "$RELEASE_DIR/toolgate.tmp" -name '*.pyc' -delete
  mv "$RELEASE_DIR/toolgate.tmp" "$RELEASE_DIR/toolgate"
}

# Docker
cp -r docker "$RELEASE_DIR/docker"

# Scripts (mcp-deploy.sh etc.)
cp -r scripts "$RELEASE_DIR/scripts"
chmod +x "$RELEASE_DIR/scripts/"*.sh 2>/dev/null || true

# Setup script and templates
cp setup.sh "$RELEASE_DIR/setup.sh"
chmod +x "$RELEASE_DIR/setup.sh"
cp .env.example "$RELEASE_DIR/.env.example"

# Update script
cp update.sh "$RELEASE_DIR/update.sh"
chmod +x "$RELEASE_DIR/update.sh"

# Uninstall script
cp uninstall.sh "$RELEASE_DIR/uninstall.sh"
chmod +x "$RELEASE_DIR/uninstall.sh"

# README
cp README.md "$RELEASE_DIR/README.md" 2>/dev/null || true

# Version file
echo "$VERSION" > "$RELEASE_DIR/VERSION"

# ── Create archive ──
ARCHIVE="$ROOT/release/hydeclaw-v${VERSION}.tar.gz"
info "Creating archive..."
tar czf "$ARCHIVE" -C "$ROOT/release" "hydeclaw"
ARCHIVE_SIZE=$(du -h "$ARCHIVE" | cut -f1)

# Clean up — only the archive remains
rm -rf "$RELEASE_DIR"

# ── Summary ──
echo ""
echo -e "${BOLD}━━━ Release v${VERSION} ━━━${NC}"
echo ""
echo -e "  Archive: ${BOLD}${ARCHIVE_SIZE}${NC} → release/hydeclaw-v${VERSION}.tar.gz"
echo ""
info "Fresh install:"
echo ""
echo "  scp release/hydeclaw-v${VERSION}.tar.gz user@server:~/"
echo "  ssh user@server"
echo "  tar xzf hydeclaw-v${VERSION}.tar.gz && cd hydeclaw && ./setup.sh"
echo ""
info "Update existing:"
echo ""
echo "  scp release/hydeclaw-v${VERSION}.tar.gz user@server:~/"
echo "  ssh user@server"
echo "  ~/hydeclaw/update.sh hydeclaw-v${VERSION}.tar.gz"
