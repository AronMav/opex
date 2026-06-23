#!/bin/bash
# mcp-deploy.sh — Deploy MCP servers for Opex (one-command)
#
# Usage:
#   mcp-deploy.sh stdio-node <source-image> <name> <port>
#   mcp-deploy.sh stdio-python <pip-package> <name> <port> <command>
#   mcp-deploy.sh url <url> <name>
#   mcp-deploy.sh remove <name>
#
# Examples:
#   mcp-deploy.sh stdio-node mcp/fetch:latest fetch 9011
#   mcp-deploy.sh stdio-node mcp/sequentialthinking:latest sequential-thinking 9010
#   mcp-deploy.sh stdio-python mcp-server-git git 9012 mcp-server-git
#   mcp-deploy.sh url https://context7.com/mcp context7
#   mcp-deploy.sh remove fetch

set -euo pipefail

BASE_DIR="${HOME}/opex"
DOCKER_DIR="${BASE_DIR}/docker"
MCP_DIR="${DOCKER_DIR}/mcp"
WORKSPACE_MCP="${BASE_DIR}/workspace/mcp"
BRIDGE_DIR="${DOCKER_DIR}/mcp-bridge"
COMPOSE_FILE="${DOCKER_DIR}/docker-compose.yml"

log() { echo "[mcp-deploy] $*"; }
die() { echo "[mcp-deploy] ERROR: $*" >&2; exit 1; }

# Ensure base bridge image exists
ensure_bridge() {
    if ! docker image inspect opex-mcp-bridge:latest >/dev/null 2>&1; then
        log "Building base bridge image..."
        [ -f "${BRIDGE_DIR}/Dockerfile" ] || die "Missing ${BRIDGE_DIR}/Dockerfile"
        docker build -t opex-mcp-bridge:latest "${BRIDGE_DIR}" || die "Bridge image build failed"
        log "Bridge image built OK"
    fi
}

# Create workspace/mcp YAML
create_yaml() {
    local name="$1" port="${2:-}" url="${3:-}"
    mkdir -p "${WORKSPACE_MCP}"
    if [ -n "$url" ]; then
        cat > "${WORKSPACE_MCP}/${name}.yaml" <<YAML
name: mcp-${name}
url: "${url}"
mode: on-demand
protocol: http
enabled: true
YAML
    else
        cat > "${WORKSPACE_MCP}/${name}.yaml" <<YAML
name: mcp-${name}
container: mcp-${name}
port: ${port}
mode: on-demand
idle_timeout: 5m
protocol: http
enabled: true
YAML
    fi
    log "Created ${WORKSPACE_MCP}/${name}.yaml"
}

# Insert service into docker-compose.yml (before 'networks:' line)
insert_compose_service() {
    local name="$1" port="$2" env_line="${3:-}"
    local svc_name="mcp-${name}"

    # Remove old entry if exists (between service name and next profiles line)
    if grep -q "^  ${svc_name}:" "${COMPOSE_FILE}" 2>/dev/null; then
        log "Removing old ${svc_name} entry from docker-compose.yml"
        sed -i "/^  ${svc_name}:/,/profiles:.*on-demand.*/d" "${COMPOSE_FILE}"
    fi

    local block="  ${svc_name}:\\n"
    block+="    container_name: ${svc_name}\\n"
    block+="    image: opex-${svc_name}:latest\\n"
    block+="    build: ./mcp/${name}\\n"
    block+="    ports:\\n"
    block+="      - \"${port}:8000\"\\n"
    block+="    volumes:\\n"
    block+="      - /etc/localtime:/etc/localtime:ro\\n"
    block+="      - /etc/timezone:/etc/timezone:ro\\n"
    if [ -n "$env_line" ]; then
        block+="    environment:\\n"
        block+="      ${env_line}\\n"
    fi
    block+="    deploy:\\n"
    block+="      resources:\\n"
    block+="        limits:\\n"
    block+="          memory: 256m\\n"
    block+="    profiles: [ \"on-demand\" ]\\n"

    # Insert before 'networks:' line
    sed -i "/^networks:/i\\${block}" "${COMPOSE_FILE}"

    # Validate YAML
    if docker compose -f "${COMPOSE_FILE}" config --quiet 2>/dev/null; then
        log "docker-compose.yml updated OK"
    else
        log "WARNING: docker-compose.yml validation failed, check manually"
    fi
}

# Verify MCP server responds to tools/list
verify() {
    local port="$1" name="$2"
    log "Verifying mcp-${name} on port ${port}..."
    local resp
    local attempt
    for attempt in 1 2 3 4 5; do
        resp=$(curl -s --max-time 10 -X POST "http://localhost:${port}/mcp" \
            -H 'Content-Type: application/json' \
            -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' 2>&1) || true
        if echo "$resp" | grep -q '"tools"'; then
            local count
            count=$(echo "$resp" | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('result',{}).get('tools',[])))" 2>/dev/null || echo "?")
            log "OK: mcp-${name} has ${count} tools"
            return 0
        fi
        log "Attempt ${attempt}/5 — waiting for server..."
        sleep 2
    done
    log "FAIL: mcp-${name} verification failed after 5 attempts. Response: ${resp:0:200}"
    return 1
}

# ═══════════════════════════════════════════════════════════════════════
# Command: stdio-node — wrap an official Node.js MCP image
# ═══════════════════════════════════════════════════════════════════════
cmd_stdio_node() {
    local source_image="$1" name="$2" port="$3" env_vars="${4:-}"
    local build_dir="${MCP_DIR}/${name}"

    ensure_bridge

    log "Deploying stdio-node MCP: ${name} from ${source_image} on port ${port}"

    # Pull source image (cross-platform: force amd64 for node.js content)
    log "Pulling ${source_image}..."
    docker pull --platform linux/amd64 "${source_image}" 2>/dev/null || \
    docker pull "${source_image}" || die "Cannot pull ${source_image}"

    # Detect entrypoint
    local entrypoint workdir
    entrypoint=$(docker inspect "${source_image}" --format '{{json .Config.Entrypoint}}' 2>/dev/null || echo "null")
    workdir=$(docker inspect "${source_image}" --format '{{.Config.WorkingDir}}' 2>/dev/null || echo "/app")
    [ "$workdir" = "" ] && workdir="/app"
    log "Source image entrypoint: ${entrypoint}, workdir: ${workdir}"

    # Detect runtime type and file locations
    local is_python=false
    local has_node_modules=false
    local copy_from="${workdir}"
    local mcp_cmd=""

    docker create --name "mcp-inspect-$$" --platform linux/amd64 "${source_image}" >/dev/null 2>&1

    # Check for Python venv (pip-based MCP)
    if docker cp "mcp-inspect-$$:${workdir}/.venv/bin/" - >/dev/null 2>&1; then
        is_python=true
    fi

    # Check for global node_modules (npm -g installed, e.g. notion)
    if [ "$is_python" = false ]; then
        if docker cp "mcp-inspect-$$:/usr/local/lib/node_modules/" - >/dev/null 2>&1; then
            # Check if workdir has content; if not, use global node_modules
            if ! docker cp "mcp-inspect-$$:${workdir}/dist/" - >/dev/null 2>&1 && \
               ! docker cp "mcp-inspect-$$:${workdir}/node_modules/" - >/dev/null 2>&1; then
                has_node_modules=true
            fi
        fi
    fi

    docker rm -f "mcp-inspect-$$" >/dev/null 2>&1

    if [ "$is_python" = true ]; then
        # Python MCP — pip install the package natively on arm64
        local cmd_name
        cmd_name=$(echo "$entrypoint" | python3 -c "import sys,json; ep=json.load(sys.stdin); print(ep[-1] if ep else '')" 2>/dev/null || echo "")

        # If entrypoint is null/empty, scan .venv/bin/ for mcp-server-* commands
        if [ -z "$cmd_name" ]; then
            docker create --name "mcp-pyscan-$$" --platform linux/amd64 "${source_image}" >/dev/null 2>&1
            cmd_name=$(docker cp "mcp-pyscan-$$:${workdir}/.venv/bin/" - 2>/dev/null | tar -tf - 2>/dev/null | \
                grep -oE 'mcp-server-[^/]+$' | head -1)
            docker rm -f "mcp-pyscan-$$" >/dev/null 2>&1
        fi

        [ -z "$cmd_name" ] && die "Cannot determine Python MCP command for ${source_image}"
        log "Detected Python MCP, switching to pip install mode (command: ${cmd_name})"

        mkdir -p "${build_dir}"
        cat > "${build_dir}/Dockerfile" <<DOCKERFILE
FROM opex-mcp-bridge:latest
RUN pip install --no-cache-dir ${cmd_name}
ENV MCP_COMMAND='["${cmd_name}"]'
DOCKERFILE
        log "Created ${build_dir}/Dockerfile (pip install)"

    elif [ "$has_node_modules" = true ]; then
        # Node.js global install (npm -g) — copy /usr/local/lib/node_modules
        # Find the entry point: check /usr/local/bin/ for the command, or scan for dist/index.js
        # Find entry point from package.json bin field or dist/index.js
        local entry_path=""
        docker create --name "mcp-scan-$$" --platform linux/amd64 "${source_image}" >/dev/null 2>&1

        # Find the main package (not npm/corepack)
        local pkg_path
        pkg_path=$(docker cp "mcp-scan-$$:/usr/local/lib/node_modules/" - 2>/dev/null | tar -tf - 2>/dev/null | \
            grep -E '^node_modules/(@[^/]+/)?[^/]+/package\.json$' | \
            grep -v 'node_modules/npm/' | grep -v 'node_modules/corepack/' | head -1)

        if [ -n "$pkg_path" ]; then
            local pkg_base="${pkg_path%/package.json}"
            # Strip node_modules/ prefix (tar archive root) since COPY puts contents directly into /mcp_modules/
            pkg_base="${pkg_base#node_modules/}"
            # Read bin field from package.json
            local bin_entry
            bin_entry=$(docker cp "mcp-scan-$$:/usr/local/lib/${pkg_path}" - 2>/dev/null | \
                tar -xO 2>/dev/null | \
                python3 -c "import sys,json; d=json.load(sys.stdin); b=d.get('bin',{}); print(list(b.values())[0] if isinstance(b,dict) and b else d.get('main',''))" 2>/dev/null)

            if [ -n "$bin_entry" ]; then
                entry_path="/mcp_modules/${pkg_base}/${bin_entry}"
            fi
        fi

        docker rm -f "mcp-scan-$$" >/dev/null 2>&1

        if [ -n "$entry_path" ]; then
            mcp_cmd="[\"node\", \"${entry_path}\"]"
        else
            mcp_cmd='["node", "/mcp_modules/dist/index.js"]'
        fi
        log "Detected Node.js global package, command: ${mcp_cmd}"

        mkdir -p "${build_dir}"
        cat > "${build_dir}/Dockerfile" <<DOCKERFILE
FROM --platform=linux/amd64 ${source_image} AS mcp
FROM opex-mcp-bridge:latest
COPY --from=mcp /usr/local/lib/node_modules /mcp_modules
ENV MCP_COMMAND='${mcp_cmd}'
DOCKERFILE
        log "Created ${build_dir}/Dockerfile (global node_modules)"

    else
        # Node.js workdir-based — multi-stage copy from workdir
        if echo "$entrypoint" | grep -q "node"; then
            local script
            script=$(echo "$entrypoint" | python3 -c "import sys,json; ep=json.load(sys.stdin); print(ep[-1] if ep else 'dist/index.js')" 2>/dev/null || echo "dist/index.js")
            if [[ "$script" == /* ]]; then
                script="${script#$workdir}"
                script="${script#/}"
            fi
            mcp_cmd="[\"node\", \"/mcp_server/${script}\"]"
        else
            mcp_cmd='["node", "/mcp_server/dist/index.js"]'
        fi
        log "Detected Node.js workdir MCP, command: ${mcp_cmd}"

        mkdir -p "${build_dir}"
        cat > "${build_dir}/Dockerfile" <<DOCKERFILE
FROM --platform=linux/amd64 ${source_image} AS mcp
FROM opex-mcp-bridge:latest
COPY --from=mcp ${workdir} /mcp_server
ENV MCP_COMMAND='${mcp_cmd}'
DOCKERFILE
        log "Created ${build_dir}/Dockerfile (multi-stage)"
    fi

    # Build image
    log "Building opex-mcp-${name}:latest..."
    docker build -t "opex-mcp-${name}:latest" "${build_dir}" || die "Build failed"

    # Remove old container if exists
    docker rm -f "mcp-${name}" 2>/dev/null || true

    # Create container via docker-compose or docker create
    insert_compose_service "${name}" "${port}" "${env_vars}"
    docker compose -f "${COMPOSE_FILE}" create --no-recreate "mcp-${name}" 2>/dev/null || \
    docker create --name "mcp-${name}" \
        -p "${port}:8000" \
        --network opex \
        --memory=256m \
        --restart unless-stopped \
        "opex-mcp-${name}:latest"

    # Create workspace YAML
    create_yaml "${name}" "${port}"

    # Start and verify
    docker start "mcp-${name}"
    sleep 3
    verify "${port}" "${name}"

    # Stop (on-demand — will be started by core when needed)
    docker stop "mcp-${name}" 2>/dev/null || true
    log "Done: mcp-${name} deployed and verified"
}

# ═══════════════════════════════════════════════════════════════════════
# Command: stdio-python — install Python MCP package via pip
# ═══════════════════════════════════════════════════════════════════════
cmd_stdio_python() {
    local pip_pkg="$1" name="$2" port="$3" cmd_name="${4:-$pip_pkg}"
    local build_dir="${MCP_DIR}/${name}"

    ensure_bridge

    log "Deploying stdio-python MCP: ${name} (pip: ${pip_pkg}) on port ${port}"

    mkdir -p "${build_dir}"
    cat > "${build_dir}/Dockerfile" <<DOCKERFILE
FROM opex-mcp-bridge:latest
RUN pip install --no-cache-dir ${pip_pkg}
ENV MCP_COMMAND='["${cmd_name}"]'
DOCKERFILE

    docker build -t "opex-mcp-${name}:latest" "${build_dir}" || die "Build failed"
    docker rm -f "mcp-${name}" 2>/dev/null || true
    insert_compose_service "${name}" "${port}"
    docker compose -f "${COMPOSE_FILE}" create --no-recreate "mcp-${name}" 2>/dev/null || \
    docker create --name "mcp-${name}" -p "${port}:8000" --network opex --memory=256m "opex-mcp-${name}:latest"
    create_yaml "${name}" "${port}"
    docker start "mcp-${name}"
    sleep 3
    verify "${port}" "${name}"
    docker stop "mcp-${name}" 2>/dev/null || true
    log "Done: mcp-${name} deployed"
}

# ═══════════════════════════════════════════════════════════════════════
# Command: url — register an external HTTP MCP server
# ═══════════════════════════════════════════════════════════════════════
cmd_url() {
    local url="$1" name="$2"
    log "Registering external MCP: ${name} at ${url}"
    create_yaml "${name}" "" "${url}"

    # Verify
    local resp
    resp=$(curl -sf --max-time 10 -X POST "${url}" \
        -H 'Content-Type: application/json' \
        -H 'Accept: application/json, text/event-stream' \
        -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' 2>&1) || true

    if echo "$resp" | grep -q '"tools"\|"result"'; then
        log "OK: external MCP ${name} reachable"
    else
        log "WARNING: external MCP ${name} not reachable (may need auth or different protocol)"
    fi
    log "Done: mcp-${name} registered"
}

# ═══════════════════════════════════════════════════════════════════════
# Command: remove — clean up an MCP server
# ═══════════════════════════════════════════════════════════════════════
cmd_remove() {
    local name="$1"
    log "Removing MCP: ${name}"
    docker stop "mcp-${name}" 2>/dev/null || true
    docker rm -f "mcp-${name}" 2>/dev/null || true
    rm -rf "${MCP_DIR}/${name}"
    rm -f "${WORKSPACE_MCP}/${name}.yaml"
    # Remove from docker-compose (between mcp-NAME: and next service or networks:)
    # Best-effort: just warn, manual cleanup may be needed
    log "Note: docker-compose.yml entry for mcp-${name} may need manual removal"
    log "Done: mcp-${name} removed"
}

# ═══════════════════════════════════════════════════════════════════════
# Main dispatcher
# ═══════════════════════════════════════════════════════════════════════
case "${1:-}" in
    stdio-node)
        [ $# -ge 4 ] || die "Usage: $0 stdio-node <source-image> <name> <port> [env_vars]"
        cmd_stdio_node "$2" "$3" "$4" "${5:-}"
        ;;
    stdio-python)
        [ $# -ge 4 ] || die "Usage: $0 stdio-python <pip-package> <name> <port> [command]"
        cmd_stdio_python "$2" "$3" "$4" "${5:-}"
        ;;
    url)
        [ $# -ge 3 ] || die "Usage: $0 url <url> <name>"
        cmd_url "$2" "$3"
        ;;
    remove)
        [ $# -ge 2 ] || die "Usage: $0 remove <name>"
        cmd_remove "$2"
        ;;
    *)
        die "Unknown command: ${1:-}. Use: stdio-node, stdio-python, url, remove"
        ;;
esac
