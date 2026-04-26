#!/usr/bin/env bash
# HydeClaw — one-script setup for Linux.
# Detects context (pre-built release or git clone), installs everything needed,
# builds if necessary, configures, and starts.
#
# Pre-built release:  tar xzf hydeclaw-v*.tar.gz && cd hydeclaw && ./setup.sh
# From source:        git clone ... && cd hydeclaw && ./setup.sh
#
# Options:
#   --verbose       Show full command output instead of spinners
#   --dry-run       Show install plan without making changes
#   --no-systemd    Skip systemd service installation
#   --help          Show this help
set -euo pipefail

# ── CLI args ──
VERBOSE=0; DRY_RUN=0; NO_SYSTEMD=0
for arg in "$@"; do
  case "$arg" in
    --verbose)     VERBOSE=1 ;;
    --dry-run)     DRY_RUN=1 ;;
    --no-systemd)  NO_SYSTEMD=1 ;;
    --help|-h)
      sed -n '2,/^$/{ s/^# //; s/^#//; p }' "$0"
      exit 0 ;;
  esac
done

# ── Colors ──
BOLD='\033[1m'; NC='\033[0m'
C_INFO='\033[38;2;136;146;176m'    # muted blue-gray
C_OK='\033[38;2;0;229;204m'        # cyan
C_WARN='\033[38;2;255;176;32m'     # amber
C_ERR='\033[38;2;230;57;70m'       # red
C_MUTED='\033[38;2;90;100;128m'    # dim
C_ACCENT='\033[38;2;100;149;237m'  # cornflower

# ── UI helpers (gum with plain fallback) ──
GUM=""

info()    { echo -e "${C_MUTED}·${NC} $*"; }
ok()      { echo -e "${C_OK}✓${NC} $*"; }
warn()    { echo -e "${C_WARN}!${NC} $*"; }
err()     { echo -e "${C_ERR}✗${NC} $*"; }
kv()      { printf "  ${C_MUTED}%-20s${NC} %s\n" "$1" "$2"; }

stage() {
  STAGE_CUR=$((STAGE_CUR + 1))
  local title="[${STAGE_CUR}/${STAGE_TOTAL}] $1"
  if [[ -n "$GUM" ]]; then
    "$GUM" style --bold --foreground "#6495ed" --padding "1 0" "$title"
  else
    echo -e "\n${BOLD}${C_ACCENT}${title}${NC}"
  fi
}


# ── Spinner / quiet step ──
TMPFILES=()
cleanup() { for f in "${TMPFILES[@]:-}"; do rm -rf "$f" 2>/dev/null || true; done; }
trap cleanup EXIT
mktmp() { local f; f="$(mktemp)"; TMPFILES+=("$f"); echo "$f"; }

run_step() {
  local title="$1"; shift

  if [[ "$VERBOSE" == "1" ]]; then
    info "$title"
    "$@"
    return $?
  fi

  local log; log="$(mktmp)"

  # Try gum spinner
  if [[ -n "$GUM" ]] && [[ -t 1 ]]; then
    local cmd_q log_q
    printf -v cmd_q '%q ' "$@"
    printf -v log_q '%q' "$log"
    if "$GUM" spin --spinner dot --title "$title" -- bash -c "${cmd_q} >${log_q} 2>&1"; then
      return 0
    fi
  else
    # Plain: show title, hide output
    echo -en "  ${C_MUTED}${title}...${NC} "
    if "$@" >"$log" 2>&1; then
      echo -e "${C_OK}done${NC}"
      return 0
    fi
  fi

  err "${title} failed"
  [[ -s "$log" ]] && tail -n 30 "$log" >&2
  return 1
}

# ── Ensure common tool paths ──
[[ -d "$HOME/.bun/bin" ]] && export PATH="$HOME/.bun/bin:$PATH"
[[ -d "$HOME/.cargo/bin" ]] && export PATH="$HOME/.cargo/bin:$PATH"
[[ -d "$HOME/.local/bin" ]] && export PATH="$HOME/.local/bin:$PATH"

# ── Detect context ──
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

VERSION=""; [[ -f VERSION ]] && VERSION="$(cat VERSION)"

IS_RELEASE=false
ls hydeclaw-core-* &>/dev/null 2>&1 && IS_RELEASE=true

is_root() { [[ "$(id -u)" -eq 0 ]]; }
maybe_sudo() { if is_root; then "$@"; else sudo "$@"; fi; }

detect_pkg() {
  if command -v apt-get &>/dev/null; then echo "apt"
  elif command -v dnf &>/dev/null; then echo "dnf"
  elif command -v yum &>/dev/null; then echo "yum"
  elif command -v pacman &>/dev/null; then echo "pacman"
  elif command -v apk &>/dev/null; then echo "apk"
  else echo "unknown"; fi
}

install_pkg() {
  local PKG; PKG=$(detect_pkg)
  case "$PKG" in
    apt)    run_step "apt: $*" maybe_sudo apt-get install -y -qq "$@" ;;
    dnf)    run_step "dnf: $*" maybe_sudo dnf install -y -q "$@" ;;
    yum)    run_step "yum: $*" maybe_sudo yum install -y -q "$@" ;;
    pacman) run_step "pacman: $*" maybe_sudo pacman -S --noconfirm "$@" ;;
    apk)    run_step "apk: $*" maybe_sudo apk add --quiet "$@" ;;
    *)      err "Unknown package manager. Install '$*' manually."; return 1 ;;
  esac
}

ensure_build_tools() {
  for tool in curl gcc git make; do
    command -v "$tool" &>/dev/null && continue
    local PKG; PKG=$(detect_pkg)
    case "$PKG" in
      apt) run_step "Installing build tools" maybe_sudo apt-get update -qq && install_pkg build-essential curl git ;;
      dnf|yum) install_pkg gcc make curl git ;;
      pacman) install_pkg base-devel curl git ;;
      apk) install_pkg build-base curl git ;;
    esac
    return
  done
}

# ── Bootstrap gum (temporary, auto-cleaned) ──
bootstrap_gum() {
  command -v gum &>/dev/null && { GUM="gum"; return 0; }
  [[ ! -t 1 ]] && return 1  # non-interactive

  local gum_ver="0.17.0"
  local os arch
  case "$(uname -s)" in Darwin) os="Darwin" ;; Linux) os="Linux" ;; *) return 1 ;; esac
  case "$(uname -m)" in x86_64|amd64) arch="x86_64" ;; aarch64|arm64) arch="arm64" ;; *) return 1 ;; esac

  local tmpdir; tmpdir="$(mktemp -d)"; TMPFILES+=("$tmpdir")
  local asset="gum_${gum_ver}_${os}_${arch}.tar.gz"
  local url="https://github.com/charmbracelet/gum/releases/download/v${gum_ver}/${asset}"

  curl -fsSL --retry 2 -o "$tmpdir/$asset" "$url" 2>/dev/null || return 1
  tar -xzf "$tmpdir/$asset" -C "$tmpdir" 2>/dev/null || return 1

  local bin; bin="$(find "$tmpdir" -type f -name gum | head -1)"
  [[ -n "$bin" && -x "$bin" ]] || { chmod +x "$bin" 2>/dev/null || return 1; }
  GUM="$bin"
  return 0
}

# ════════════════════════════════════════════════════════════════
# Banner
# ════════════════════════════════════════════════════════════════

bootstrap_gum || true

if [[ -n "$GUM" ]]; then
  local_title="$("$GUM" style --foreground "#6495ed" --bold "⚡ HydeClaw Setup")"
  local_sub="$("$GUM" style --foreground "#5a6480" "${VERSION:+v${VERSION} · }one-script installer")"
  "$GUM" style --border rounded --border-foreground "#6495ed" --padding "1 2" "$(printf '%s\n%s' "$local_title" "$local_sub")"
else
  echo -e "${BOLD}${C_ACCENT}"
  echo "  ╦ ╦╦ ╦╔╦╗╔═╗╔═╗╦  ╔═╗╦ ╦"
  echo "  ╠═╣╚╦╝ ║║║╣ ║  ║  ╠═╣║║║"
  echo "  ╩ ╩ ╩ ═╩╝╚═╝╚═╝╩═╝╩ ╩╚╩╝"
  echo -e "${NC}"
  [[ -n "$VERSION" ]] && echo -e "  ${C_MUTED}Version ${VERSION}${NC}"
fi
echo ""

if [[ "$IS_RELEASE" == true ]]; then
  info "Mode: pre-built release"
else
  info "Mode: build from source"
fi

# ════════════════════════════════════════════════════════════════
# Install plan
# ════════════════════════════════════════════════════════════════

STAGE_CUR=0
STAGE_TOTAL=5

# Detect what's needed
NEED_DOCKER=false;  command -v docker &>/dev/null || NEED_DOCKER=true
NEED_BUN=false;     command -v bun &>/dev/null || NEED_BUN=true
NEED_PYTHON=false;  command -v python3 &>/dev/null || NEED_PYTHON=true
NEED_RUST=false;    [[ "$IS_RELEASE" == false ]] && ! command -v cargo &>/dev/null && NEED_RUST=true
NEED_NODE=false;    [[ "$IS_RELEASE" == false ]] && ! command -v npm &>/dev/null && NEED_NODE=true

echo ""
kv "Platform" "$(uname -s) $(uname -m)"
kv "Package manager" "$(detect_pkg)"
kv "Docker" "$([[ "$NEED_DOCKER" == true ]] && echo "will install" || echo "$(docker --version 2>/dev/null | grep -oP '\d+\.\d+\.\d+' | head -1)")"
kv "Bun" "$([[ "$NEED_BUN" == true ]] && echo "will install" || echo "$(bun --version 2>/dev/null)")"
kv "Python3" "$([[ "$NEED_PYTHON" == true ]] && echo "will install" || echo "$(python3 --version 2>&1 | cut -d' ' -f2)")"
[[ "$IS_RELEASE" == false ]] && kv "Rust" "$([[ "$NEED_RUST" == true ]] && echo "will install" || echo "$(rustc --version 2>/dev/null | cut -d' ' -f2)")"
[[ "$IS_RELEASE" == false ]] && kv "Node.js" "$([[ "$NEED_NODE" == true ]] && echo "will install" || echo "$(node --version 2>/dev/null)")"
echo ""

if [[ "$DRY_RUN" == "1" ]]; then
  ok "Dry run complete (no changes made)"
  exit 0
fi

# ════════════════════════════════════════════════════════════════
stage "Install dependencies"
# ════════════════════════════════════════════════════════════════

# Docker
if [[ "$NEED_DOCKER" == true ]]; then
  run_step "Installing Docker" bash -c "curl -fsSL https://get.docker.com | sh"
  maybe_sudo usermod -aG docker "$USER" 2>/dev/null || true
  ok "Docker installed (re-login may be needed for group change)"
else
  ok "Docker $(docker --version 2>/dev/null | grep -oP '\d+\.\d+\.\d+' | head -1)"
fi

# Docker Compose
if ! docker compose version &>/dev/null 2>&1; then
  install_pkg docker-compose-plugin 2>/dev/null || {
    run_step "Installing Docker Compose standalone" bash -c \
      "curl -fsSL 'https://github.com/docker/compose/releases/latest/download/docker-compose-$(uname -s)-$(uname -m)' -o /usr/local/bin/docker-compose && chmod +x /usr/local/bin/docker-compose"
  }
  ok "Docker Compose installed"
else
  ok "Docker Compose found"
fi

# Bun
if [[ "$NEED_BUN" == true ]]; then
  run_step "Installing Bun" bash -c "curl -fsSL https://bun.sh/install | bash"
  export BUN_INSTALL="$HOME/.bun"; export PATH="$BUN_INSTALL/bin:$PATH"
  ok "Bun $(bun --version)"
else
  ok "Bun $(bun --version)"
fi

# Python3
if [[ "$NEED_PYTHON" == true ]]; then
  PKG=$(detect_pkg)
  case "$PKG" in
    apt) run_step "apt update" maybe_sudo apt-get update -qq; install_pkg python3-full python3-venv ;;
    *)   install_pkg python3 ;;
  esac
  ok "Python3 installed"
else
  ok "Python3 $(python3 --version 2>&1 | cut -d' ' -f2)"
fi

# Source-only: Rust + Node.js
if [[ "$IS_RELEASE" == false ]]; then
  ensure_build_tools

  if [[ "$NEED_RUST" == true ]]; then
    run_step "Installing Rust" bash -c "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable"
    source "$HOME/.cargo/env"
    ok "Rust $(rustc --version | cut -d' ' -f2)"
  else
    ok "Rust $(rustc --version | cut -d' ' -f2)"
  fi

  if [[ "$NEED_NODE" == true ]]; then
    PKG=$(detect_pkg)
    if [[ "$PKG" == "apt" ]]; then
      run_step "Adding NodeSource repo" bash -c "curl -fsSL https://deb.nodesource.com/setup_22.x | sudo -E bash -"
    fi
    install_pkg nodejs
    ok "Node.js $(node --version)"
  else
    ok "Node.js $(node --version)"
  fi
fi

# ════════════════════════════════════════════════════════════════
stage "Prepare binary and UI"
# ════════════════════════════════════════════════════════════════

BINARY_CORE=""
BINARY_WATCHDOG=""
BINARY_WORKER=""

if [[ "$IS_RELEASE" == true ]]; then
  ARCH=$(uname -m)
  case "$ARCH" in
    aarch64|arm64) ARCH_SHORT="aarch64" ;;
    *)             ARCH_SHORT="x86_64" ;;
  esac

  for CRATE in hydeclaw-core hydeclaw-watchdog hydeclaw-memory-worker; do
    BIN="$ROOT/${CRATE}-${ARCH_SHORT}"
    if [[ -f "$BIN" ]]; then
      chmod +x "$BIN"
      ok "$CRATE ($(du -h "$BIN" | cut -f1))"
    else
      warn "$CRATE not found (optional)"
    fi
  done
  BINARY_CORE="$ROOT/hydeclaw-core-${ARCH_SHORT}"
  BINARY_WATCHDOG="$ROOT/hydeclaw-watchdog-${ARCH_SHORT}"
  BINARY_WORKER="$ROOT/hydeclaw-memory-worker-${ARCH_SHORT}"
  [[ -f "$BINARY_CORE" ]] || { err "hydeclaw-core binary not found"; exit 1; }

  if [[ -f hydeclaw-ui.tar.gz ]] && [[ ! -d ui/out ]]; then
    mkdir -p ui && tar xzf hydeclaw-ui.tar.gz -C ui
    ok "UI extracted"
  elif [[ -d ui/out ]]; then
    ok "UI ready"
  fi
else
  for CRATE in hydeclaw-core hydeclaw-watchdog hydeclaw-memory-worker; do
    run_step "Compiling $CRATE" cargo build --release -p "$CRATE"
    ok "$CRATE ($(du -h "target/release/$CRATE" | cut -f1))"
  done
  BINARY_CORE="$ROOT/target/release/hydeclaw-core"
  BINARY_WATCHDOG="$ROOT/target/release/hydeclaw-watchdog"
  BINARY_WORKER="$ROOT/target/release/hydeclaw-memory-worker"

  run_step "Building Next.js UI" bash -c "cd ui && npm install --silent && npm run build"
  ok "UI built"
fi

# Runtime deps
if [[ -f channels/package.json ]]; then
  run_step "Installing channel adapters" bash -c "export PATH=\"$HOME/.bun/bin:\$PATH\" && cd channels && bun install"
  ok "Channels ready"
fi

if [[ -f toolgate/requirements.txt ]]; then
  if run_step "Setting up toolgate" bash -c "cd toolgate && python3 -m venv .venv 2>/dev/null && .venv/bin/pip install -q -r requirements.txt"; then
    ok "Toolgate ready"
  else
    warn "Toolgate setup failed (STT/TTS/Vision unavailable until fixed)"
  fi
fi

# ════════════════════════════════════════════════════════════════
stage "Configure"
# ════════════════════════════════════════════════════════════════

# .env
SKIP_ENV=""
if [[ -f .env ]]; then
  warn ".env already exists — keeping existing keys"
  SKIP_ENV=1
fi

if [[ "$SKIP_ENV" != "1" ]]; then
  AUTH_TOKEN=$(openssl rand -hex 32)
  MASTER_KEY=$(openssl rand -hex 32)
  cat > .env << EOF
HYDECLAW_AUTH_TOKEN=${AUTH_TOKEN}
HYDECLAW_MASTER_KEY=${MASTER_KEY}
DATABASE_URL=postgresql://hydeclaw:hydeclaw@localhost:5432/hydeclaw
EOF
  ok "Generated .env"
else
  AUTH_TOKEN=$(grep '^HYDECLAW_AUTH_TOKEN=' .env | cut -d= -f2)
  MASTER_KEY=$(grep '^HYDECLAW_MASTER_KEY=' .env | cut -d= -f2)
  ok "Using existing .env"
fi

# Docker .env
[[ ! -f docker/.env ]] && {
  [[ -f docker/.env.example ]] && cp docker/.env.example docker/.env || \
  printf 'POSTGRES_USER=hydeclaw\nPOSTGRES_PASSWORD=hydeclaw\n' > docker/.env
}

# Configure Docker TCP listener (Core connects via bollard HTTP, not Unix socket)
if ! curl -sf http://127.0.0.1:2375/version > /dev/null 2>&1; then
  info "Configuring Docker TCP listener on 127.0.0.1:2375"
  if [[ -f /etc/docker/daemon.json ]]; then
    warn "Overwriting existing /etc/docker/daemon.json (backup: /etc/docker/daemon.json.bak)"
    maybe_sudo cp /etc/docker/daemon.json /etc/docker/daemon.json.bak
  fi
  maybe_sudo tee /etc/docker/daemon.json > /dev/null << 'DJEOF'
{"hosts": ["unix:///var/run/docker.sock", "tcp://127.0.0.1:2375"]}
DJEOF
  maybe_sudo mkdir -p /etc/systemd/system/docker.service.d
  maybe_sudo tee /etc/systemd/system/docker.service.d/override.conf > /dev/null << 'DOEOF'
[Service]
ExecStart=
ExecStart=/usr/bin/dockerd
DOEOF
  maybe_sudo systemctl daemon-reload
  maybe_sudo systemctl restart docker
  sleep 3
  curl -sf http://127.0.0.1:2375/version > /dev/null && ok "Docker TCP ready" || warn "Docker TCP not responding"
else
  ok "Docker TCP already configured"
fi

# Docker images (postgres, browser-renderer, sandbox)
info "Building Docker images..."
if [[ "$VERBOSE" == "1" ]]; then
  docker compose -f docker/docker-compose.yml build postgres browser-renderer
else
  docker compose -f docker/docker-compose.yml build postgres browser-renderer 2>&1 | tail -5
fi
# Agent sandbox image
if [[ -f docker/Dockerfile.sandbox ]]; then
  info "Building sandbox image (this may take a few minutes)..."
  docker build -f docker/Dockerfile.sandbox -t hydeclaw-sandbox:latest . 2>&1 | \
    grep -E '^(Step |#[0-9]+ (DONE|ERROR)|Successfully|WARN)' || true
  ok "Docker images built (postgres, browser-renderer, sandbox)"
else
  ok "Docker images built (postgres, browser-renderer)"
fi

info "Starting Docker infrastructure..."
if [[ "$VERBOSE" == "1" ]]; then
  docker compose -f docker/docker-compose.yml up -d postgres searxng browser-renderer
else
  docker compose -f docker/docker-compose.yml up -d postgres searxng browser-renderer 2>/dev/null
fi
for i in $(seq 1 30); do
  docker compose -f docker/docker-compose.yml exec -T postgres pg_isready -q 2>/dev/null && { ok "PostgreSQL ready"; break; }
  [[ "$i" -eq 30 ]] && { err "PostgreSQL failed to start after 30s"; exit 1; }
  sleep 1
done
ok "Docker infrastructure started"

# ── Phase 64 SEC-05: nginx Content-Security-Policy-Report-Only header ──────
# v0.19.0 ships CSP in *observation* mode: browsers will POST violations to
# /api/csp-report, core will log + count them, and we'll flip to enforce in
# v0.19.1 once the 7-day observation window shows what CodeMirror / Mermaid /
# KaTeX / shiki workers actually need.
#
# Locked directive (Phase 64 CONTEXT D-CSP-01) — copy-paste-compatible:
#   default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
#   connect-src 'self' ws: wss:; style-src 'self' 'unsafe-inline'; \
#   img-src 'self' data: blob:; font-src 'self' data:
#
# If you front hydeclaw-core with nginx, add this INSIDE the `location /` block:
#
#   # Phase 64 SEC-05: CSP observation window (v0.19.0). Flip to enforce in v0.19.1.
#   add_header Content-Security-Policy-Report-Only "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self' ws: wss:; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; report-uri /api/csp-report" always;
#
# Use `always` so the header is emitted on 4xx/5xx responses too — browsers
# need it to fire reports for blocked subresources even when the document
# itself is non-2xx.

configure_nginx_csp() {
  # Only act when nginx is present AND a hydeclaw site config exists.
  [[ -d /etc/nginx ]] || return 0
  local site=""
  for candidate in /etc/nginx/sites-available/hydeclaw /etc/nginx/conf.d/hydeclaw.conf; do
    if [[ -f "$candidate" ]]; then site="$candidate"; break; fi
  done
  [[ -z "$site" ]] && { info "nginx present but no hydeclaw site config — skipping CSP header"; return 0; }

  # Skip if already configured (idempotent).
  if grep -q 'Content-Security-Policy-Report-Only' "$site" 2>/dev/null; then
    ok "nginx CSP Report-Only header already configured in $site"
    return 0
  fi

  info "Injecting Content-Security-Policy-Report-Only into $site"
  maybe_sudo cp "$site" "${site}.bak"

  # Insert the add_header directive right after the first `location / {` line.
  # Locked directive string — do not drift from Phase 64 CONTEXT D-CSP-01.
  local csp_line='        # Phase 64 SEC-05: CSP observation window (v0.19.0). Flip to enforce in v0.19.1.\n        add_header Content-Security-Policy-Report-Only "default-src '"'"'self'"'"'; script-src '"'"'self'"'"' '"'"'wasm-unsafe-eval'"'"'; connect-src '"'"'self'"'"' ws: wss:; style-src '"'"'self'"'"' '"'"'unsafe-inline'"'"'; img-src '"'"'self'"'"' data: blob:; font-src '"'"'self'"'"' data:; report-uri /api/csp-report" always;'

  # Use sed with the FIRST `location / {` match. Falls back to a warning if the
  # site config does not contain one (operator layout is unusual).
  if grep -q 'location / {' "$site"; then
    maybe_sudo sed -i "0,/location \/ {/{s|location / {|location / {\n${csp_line}|}" "$site"
    if maybe_sudo nginx -t 2>/dev/null; then
      maybe_sudo systemctl reload nginx 2>/dev/null && ok "nginx reloaded with CSP Report-Only header"
    else
      warn "nginx -t failed — reverted from backup"
      maybe_sudo cp "${site}.bak" "$site"
    fi
  else
    warn "no 'location / {' block in $site — paste the CSP snippet manually (see docs/DEPLOYMENT.md)"
  fi
}

if [[ -d /etc/nginx ]]; then configure_nginx_csp; fi

# Public URL (auto-detect LAN IP)
LAN_IP=$(hostname -I 2>/dev/null | awk '{print $1}' || echo "localhost")
PUBLIC_URL="http://${LAN_IP}:18789"

[[ -f config/hydeclaw.toml ]] && \
  sed -i "s|public_url = \"http://your-server:18789\"|public_url = \"${PUBLIC_URL}\"|" config/hydeclaw.toml
ok "public_url = ${PUBLIC_URL}"

# ════════════════════════════════════════════════════════════════
stage "Systemd service"
# ════════════════════════════════════════════════════════════════

if [[ "$NO_SYSTEMD" != "1" ]]; then
    mkdir -p ~/.config/systemd/user

    # Core (main gateway + agent engine)
    cat > ~/.config/systemd/user/hydeclaw-core.service << SEOF
[Unit]
Description=HydeClaw Core
After=network.target

[Service]
Type=simple
WorkingDirectory=${ROOT}
ExecStart=${BINARY_CORE}
EnvironmentFile=${ROOT}/.env
Environment=PATH=${HOME}/.bun/bin:${HOME}/.local/bin:/usr/local/bin:/usr/bin:/bin
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
SEOF

    # Watchdog (health monitor + auto-restart + alerting)
    if [[ -f "$BINARY_WATCHDOG" ]]; then
      cat > ~/.config/systemd/user/hydeclaw-watchdog.service << SEOF
[Unit]
Description=HydeClaw Watchdog
After=hydeclaw-core.service

[Service]
Type=notify
WorkingDirectory=${ROOT}
ExecStart=${BINARY_WATCHDOG} config/watchdog.toml
EnvironmentFile=${ROOT}/.env
Environment=HYDECLAW_CORE_URL=http://localhost:18789
WatchdogSec=120
Restart=always
RestartSec=10

[Install]
WantedBy=default.target
SEOF
    fi

    # Memory Worker (async embedding + reindex tasks)
    if [[ -f "$BINARY_WORKER" ]]; then
      cat > ~/.config/systemd/user/hydeclaw-memory-worker.service << SEOF
[Unit]
Description=HydeClaw Memory Worker
After=hydeclaw-core.service

[Service]
Type=notify
WorkingDirectory=${ROOT}
ExecStart=${BINARY_WORKER}
EnvironmentFile=${ROOT}/.env
WatchdogSec=300
Restart=always
RestartSec=10

[Install]
WantedBy=default.target
SEOF
    fi

    systemctl --user daemon-reload
    systemctl --user enable hydeclaw-core
    ok "hydeclaw-core service enabled"
    [[ -f "$BINARY_WATCHDOG" ]] && { systemctl --user enable hydeclaw-watchdog; ok "hydeclaw-watchdog service enabled (Type=notify, WatchdogSec=120)"; }
    [[ -f "$BINARY_WORKER" ]] && { systemctl --user enable hydeclaw-memory-worker; ok "hydeclaw-memory-worker service enabled"; }
else
  info "Skipped (--no-systemd)"
fi

# ════════════════════════════════════════════════════════════════
stage "Verify & launch"
# ════════════════════════════════════════════════════════════════

# Stop any existing hydeclaw processes before starting fresh
for svc in hydeclaw-core hydeclaw-watchdog hydeclaw-memory-worker; do
  systemctl --user stop "$svc" 2>/dev/null || true
done
# Kill orphaned managed processes (channels, toolgate) that may survive Core shutdown
for pattern in "bun run src/index.ts" "uvicorn app:app.*--port 9011"; do
  pids=$(pgrep -f "$pattern" 2>/dev/null || true)
  if [[ -n "$pids" ]]; then
    warn "Killing orphaned process: $pattern (pids: $pids)"
    echo "$pids" | xargs kill 2>/dev/null || true
  fi
done
sleep 1

# Quick verification
VERIFY_OK=true
[[ -x "$BINARY_CORE" ]] && ok "hydeclaw-core" || { err "hydeclaw-core not executable"; VERIFY_OK=false; }
[[ -f "$BINARY_WATCHDOG" ]] && ok "hydeclaw-watchdog" || warn "hydeclaw-watchdog missing (optional)"
[[ -f "$BINARY_WORKER" ]] && ok "hydeclaw-memory-worker" || warn "hydeclaw-memory-worker missing (optional)"
[[ -d ui/out ]] && ok "UI present" || warn "UI missing — web interface unavailable"
[[ -f .env ]] && ok ".env configured" || { err ".env missing"; VERIFY_OK=false; }
docker compose -f docker/docker-compose.yml exec -T postgres pg_isready -q 2>/dev/null && ok "PostgreSQL reachable" || { err "PostgreSQL unreachable"; VERIFY_OK=false; }
[[ -d channels/node_modules ]] && ok "Channel adapters installed" || warn "Channel adapters not installed"
[[ -d toolgate/.venv ]] && ok "Toolgate venv ready" || warn "Toolgate not installed"

echo ""
if [[ "$VERIFY_OK" == false ]]; then
  err "Some checks failed — fix issues above before starting"
  exit 1
fi

ok "All checks passed!"
echo ""
kv "Web UI" "${PUBLIC_URL}"
kv "Auth token" "${AUTH_TOKEN}"
kv "Config" "${ROOT}/config/hydeclaw.toml"
echo ""

if [[ -f ~/.config/systemd/user/hydeclaw-core.service ]]; then
  systemctl --user start hydeclaw-core
  ok "hydeclaw-core started"
  [[ -f ~/.config/systemd/user/hydeclaw-watchdog.service ]] && { systemctl --user start hydeclaw-watchdog; ok "hydeclaw-watchdog started"; }
  [[ -f ~/.config/systemd/user/hydeclaw-memory-worker.service ]] && { systemctl --user start hydeclaw-memory-worker; ok "hydeclaw-memory-worker started"; }
else
  info "Starting core... (Ctrl+C to stop)"
  echo ""
  exec "$BINARY_CORE"
fi

echo ""
info "View logs:       journalctl --user -u hydeclaw-core -f"
