# HydeClaw Pi→Server Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the entire HydeClaw stack from the Raspberry Pi (`aronmav@192.168.1.82`, aarch64) to the home-lab server (`aronmav@188.246.224.118`, x86_64), running natively under systemd, exposed only over WireGuard, with a maintenance-window cutover and a clean rollback path.

**Architecture:** Replicate `~/hydeclaw/` from the Pi to the server via rsync (config, workspace, docker/, toolgate/, channels/, migrations/, `.env`, `docker/.env`), then rebuild only the arch-specific parts on the server: the x86_64 core/memory-worker/watchdog binaries (cross-built locally with zigbuild), the toolgate Python venv, the channels Bun deps, and the custom `hydeclaw-pg:17-age-pgvector` Docker image. Data moves via `pg_dump`/`pg_restore` (144 MB). The Pi is left intact (stopped) until the server is verified, giving a one-minute rollback.

**Tech Stack:** Rust (cargo zigbuild, target `x86_64-unknown-linux-gnu`), PostgreSQL 17.9 + Apache AGE + pgvector 0.8.0 (custom Docker image built from `docker/Dockerfile.postgres`), Python 3.13 venv (toolgate), Bun (channels), systemd `--user` units, nftables/ufw firewall, ffmpeg + espeak-ng (toolgate media + G2P).

## Global Constraints

- **Vault key is sacred:** `HYDECLAW_MASTER_KEY` in `.env` MUST be byte-identical to the Pi's, or every encrypted secret (channel `CHANNEL_CREDENTIALS`, provider API keys) becomes undecryptable. It travels via rsync of `.env`; never regenerate it.
- **Public IP — firewall first:** the server (`188.246.224.118`) has a public interface. No HydeClaw port (`18789` API/UI, `9011` toolgate, `5432` pg, `4317` otel) may be reachable from the public interface. The firewall must be in place BEFORE `hydeclaw-core` is ever started on the server.
- **One Telegram poller:** a Telegram bot token can be long-polled by only one process. The server may run channels with credentials ONLY after the Pi's channels are stopped. During prep the server DB is empty (no `CHANNEL_CREDENTIALS`), so channels stay idle — safe.
- **rustls only:** never add OpenSSL; all builds use the existing rustls/zigbuild toolchain.
- **Paths:** server mirrors the Pi exactly — `~/hydeclaw/` (`/home/aronmav/hydeclaw`), systemd `--user` units in `~/.config/systemd/user/`.
- **No destructive Pi action until verified:** the Pi's DB, binaries, and units stay intact through Phase B; only Phase C decommissions it.

## File / Artifact Map

Repo artifacts (committed):
- `deploy/server/firewall-hydeclaw.nft` — Create: nftables ruleset restricting HydeClaw ports to wg/docker_wg/LAN.
- `deploy/server/hydeclaw-core.service`, `hydeclaw-memory-worker.service`, `hydeclaw-watchdog.service` — Create: x86 systemd unit templates (binary names without `-aarch64`).
- `.deploy.env` — Modify: add `SERVER_HOST=aronmav@188.246.224.118`.

Server-side state (NOT in repo, created by runbook tasks):
- `~/hydeclaw/` (rsynced), `~/hydeclaw/hydeclaw-core-x86_64` (+ worker, watchdog), `~/hydeclaw/toolgate/.venv`, `~/hydeclaw/channels/node_modules`, the `hydeclaw-pg:17-age-pgvector` image + `pgdata` volume, the 3 systemd units, the nftables ruleset.

Local build artifacts: `target/x86_64-unknown-linux-gnu/release/{hydeclaw-core,hydeclaw-memory-worker,hydeclaw-watchdog}`.

---

## PHASE A — Preparation (no downtime; Pi keeps running)

### Task A1: Reclaim swap + confirm RAM/CPU budget on the server

**Files:** none (server ops).

- [ ] **Step 1: Snapshot current memory/swap**

Run: `ssh aronmav@188.246.224.118 'free -h; echo "---"; cat /proc/loadavg'`
Expected: ~17 GB available, swap ~4.6 GB used.

- [ ] **Step 2: Reclaim swap (RAM headroom must exceed swap-in-use)**

Run: `ssh aronmav@188.246.224.118 'avail=$(free -m | awk "/^Mem:/{print \$7}"); swap=$(free -m | awk "/^Swap:/{print \$3}"); echo "avail=${avail}MB swap_used=${swap}MB"; if [ "$avail" -gt $((swap + 2000)) ]; then sudo swapoff -a && sudo swapon -a && echo "SWAP RESET"; else echo "ABORT: not enough RAM to reclaim swap safely"; fi'`
Expected: `SWAP RESET`.

- [ ] **Step 3: Verify swap is empty and RAM is healthy**

Run: `ssh aronmav@188.246.224.118 'free -h | grep -E "Mem|Swap"'`
Expected: Swap used ~0; Mem available ≥ 12 GB.

### Task A2: Install server host dependencies (bun, ffmpeg, espeak-ng)

**Files:** none (server ops).

- [ ] **Step 1: Confirm what is missing**

Run: `ssh aronmav@188.246.224.118 'for c in bun ffmpeg espeak-ng python3 docker; do printf "%-10s " $c; command -v $c || echo MISSING; done'`
Expected: `python3`, `docker` present; `bun`, `ffmpeg`, `espeak-ng` MISSING.

- [ ] **Step 2: Install ffmpeg + espeak-ng (apt)**

Run: `ssh aronmav@188.246.224.118 'sudo apt-get update && sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ffmpeg espeak-ng'`
Expected: exit 0.

- [ ] **Step 3: Install Bun for the `aronmav` user**

Run: `ssh aronmav@188.246.224.118 'curl -fsSL https://bun.sh/install | bash'`
Expected: Bun installed to `~/.bun/bin/bun`.

- [ ] **Step 4: Verify all deps + the afftdn/espeak features toolgate needs**

Run: `ssh aronmav@188.246.224.118 'export PATH=$HOME/.bun/bin:$PATH; bun --version; ffmpeg -hide_banner -filters 2>/dev/null | grep -E "\bafftdn\b|\batempo\b" | wc -l; espeak-ng -v en-us -q --ipa "gemini"'`
Expected: bun version prints; afftdn/atempo count = 2; IPA `dʒˈɛmᵻnˌaɪ` prints.

### Task A3: Commit x86 systemd units + firewall ruleset to the repo

**Files:**
- Create: `deploy/server/hydeclaw-core.service`
- Create: `deploy/server/hydeclaw-memory-worker.service`
- Create: `deploy/server/hydeclaw-watchdog.service`
- Create: `deploy/server/firewall-hydeclaw.nft`
- Modify: `.deploy.env`

- [ ] **Step 1: Write `deploy/server/hydeclaw-core.service`** (mirror of the Pi unit, x86 binary name)

```ini
[Unit]
Description=HydeClaw Core
After=network.target docker.service

[Service]
Type=simple
WorkingDirectory=/home/aronmav/hydeclaw
ExecStart=/home/aronmav/hydeclaw/hydeclaw-core-x86_64
EnvironmentFile=/home/aronmav/hydeclaw/.env
Environment=PATH=/home/aronmav/.bun/bin:/home/aronmav/.local/bin:/usr/local/bin:/usr/bin:/bin
StandardOutput=append:/home/aronmav/hydeclaw/logs/core.log
StandardError=append:/home/aronmav/hydeclaw/logs/core.log
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

- [ ] **Step 2: Write `deploy/server/hydeclaw-memory-worker.service`**

```ini
[Unit]
Description=HydeClaw Memory Worker
After=network.target

[Service]
Type=simple
WorkingDirectory=/home/aronmav/hydeclaw
ExecStart=/home/aronmav/hydeclaw/hydeclaw-memory-worker-x86_64
EnvironmentFile=/home/aronmav/hydeclaw/.env
StandardOutput=append:/home/aronmav/hydeclaw/logs/memory-worker.log
StandardError=append:/home/aronmav/hydeclaw/logs/memory-worker.log
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

- [ ] **Step 3: Write `deploy/server/hydeclaw-watchdog.service`**

```ini
[Unit]
Description=HydeClaw Watchdog
After=network.target

[Service]
Type=simple
WorkingDirectory=/home/aronmav/hydeclaw
ExecStart=/home/aronmav/hydeclaw/hydeclaw-watchdog-x86_64
EnvironmentFile=/home/aronmav/hydeclaw/.env
StandardOutput=append:/home/aronmav/hydeclaw/logs/watchdog.log
StandardError=append:/home/aronmav/hydeclaw/logs/watchdog.log
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

- [ ] **Step 4: Write `deploy/server/firewall-hydeclaw.nft`** (drop HydeClaw ports on the public interface; allow wg + docker_wg + LAN + loopback). NOTE: replace `eth0` with the actual public NIC if different (verify with `ip -br addr`).

```nft
#!/usr/sbin/nft -f
# HydeClaw port isolation. Public NIC = eth0 (188.246.224.118).
# Allow only loopback, WireGuard (10.8.0.0/24), docker_wg (10.10.1.0/24),
# and LAN (192.168.0.0/16) to reach HydeClaw ports.
table inet hydeclaw {
    set hydeclaw_ports {
        type inet_service
        elements = { 18789, 9011, 5432, 4317 }
    }
    chain input {
        type filter hook input priority -10; policy accept;
        iifname "lo" accept
        ip saddr { 10.8.0.0/24, 10.10.1.0/24, 192.168.0.0/16 } tcp dport @hydeclaw_ports accept
        iifname "eth0" tcp dport @hydeclaw_ports drop
    }
}
```

- [ ] **Step 5: Add `SERVER_HOST` to `.deploy.env`**

Append the line `SERVER_HOST=aronmav@188.246.224.118` to `.deploy.env` (keep the existing `PI_HOST` line).

- [ ] **Step 6: Commit the repo artifacts**

```bash
git add deploy/server/ .deploy.env
git commit -m "deploy(server): x86 systemd units + HydeClaw firewall ruleset"
```

### Task A4: Apply the firewall on the server (BEFORE any core start)

**Files:** uses `deploy/server/firewall-hydeclaw.nft`.

- [ ] **Step 1: Confirm the public NIC name**

Run: `ssh aronmav@188.246.224.118 'ip -br addr | grep 188.246'`
Expected: shows the interface holding `188.246.224.118` (e.g. `eth0`). If not `eth0`, edit `deploy/server/firewall-hydeclaw.nft` to match, re-commit.

- [ ] **Step 2: Copy and load the ruleset**

Run: `scp deploy/server/firewall-hydeclaw.nft aronmav@188.246.224.118:/tmp/firewall-hydeclaw.nft && ssh aronmav@188.246.224.118 'sudo cp /tmp/firewall-hydeclaw.nft /etc/nftables.d/hydeclaw.nft 2>/dev/null || sudo mkdir -p /etc/nftables.d && sudo cp /tmp/firewall-hydeclaw.nft /etc/nftables.d/hydeclaw.nft; sudo nft -f /etc/nftables.d/hydeclaw.nft && echo LOADED'`
Expected: `LOADED`.

- [ ] **Step 3: Verify the table is active**

Run: `ssh aronmav@188.246.224.118 'sudo nft list table inet hydeclaw | grep -E "drop|accept" | head'`
Expected: the input chain rules are listed.

- [ ] **Step 4: Persist across reboot**

Run: `ssh aronmav@188.246.224.118 'grep -q "include \"/etc/nftables.d/\\*.nft\"" /etc/nftables.conf || echo "include \"/etc/nftables.d/*.nft\"" | sudo tee -a /etc/nftables.conf; sudo systemctl enable nftables 2>/dev/null; echo OK'`
Expected: `OK`.

### Task A5: Replicate `~/hydeclaw/` from Pi to server (skeleton, no arch artifacts)

**Files:** none (rsync).

- [ ] **Step 1: Ensure the Pi can SSH to the server, then dry-run the rsync**

Run: `ssh aronmav@192.168.1.82 'ssh-keygen -F 188.246.224.118 >/dev/null 2>&1 || ssh-keyscan -H 188.246.224.118 >> ~/.ssh/known_hosts 2>/dev/null; ssh -o BatchMode=yes aronmav@188.246.224.118 true 2>/dev/null && echo "Pi->server SSH OK" || echo "Pi->server SSH MISSING: run ssh-copy-id aronmav@188.246.224.118 on the Pi first"'`
Then the dry-run (`-n`): `ssh aronmav@192.168.1.82 'rsync -avn --delete --exclude "hydeclaw-core-aarch64" --exclude "hydeclaw-memory-worker-aarch64" --exclude "hydeclaw-watchdog-aarch64" --exclude "toolgate/.venv" --exclude "channels/node_modules" --exclude "logs/" --exclude "ui/out" ~/hydeclaw/ aronmav@188.246.224.118:~/hydeclaw/' | tail -25`
Expected: `Pi->server SSH OK`, then a transfer preview listing `config/`, `workspace/`, `docker/`, `toolgate/` source, `channels/` source, `migrations/`, `.env`, `docker/.env`.

- [ ] **Step 2: Create target dir + run the real rsync (Pi → server)**

Run from the Pi (it can reach the server over wg/LAN):
```bash
ssh aronmav@192.168.1.82 'ssh -o StrictHostKeyChecking=accept-new aronmav@188.246.224.118 "mkdir -p ~/hydeclaw/logs" && rsync -az --delete \
  --exclude "hydeclaw-core-aarch64" --exclude "hydeclaw-memory-worker-aarch64" --exclude "hydeclaw-watchdog-aarch64" \
  --exclude "toolgate/.venv" --exclude "channels/node_modules" --exclude "logs/" --exclude "ui/out" \
  ~/hydeclaw/ aronmav@188.246.224.118:~/hydeclaw/'
```
Expected: exit 0. (If Pi→server SSH keys are not set up, transfer the SSH pubkey first, or rsync Pi→local→server.)

- [ ] **Step 3: Verify the skeleton + that `.env` carried the master key**

Run: `ssh aronmav@188.246.224.118 'ls ~/hydeclaw; echo "---"; grep -c "^HYDECLAW_MASTER_KEY=" ~/hydeclaw/.env; grep "^DATABASE_URL=" ~/hydeclaw/.env | sed -E "s|//[^@]*@|//<creds>@|"'`
Expected: dirs `config workspace docker toolgate channels migrations`; master key count = 1; DATABASE_URL points at `localhost:5432/hydeclaw`.

### Task A6: Bring up PostgreSQL (custom AGE+pgvector image) on the server

**Files:** uses existing `~/hydeclaw/docker/docker-compose.yml` + `docker/.env`.

- [ ] **Step 1: Confirm host port 5432 is free (keeps DATABASE_URL identical)**

Run: `ssh aronmav@188.246.224.118 'ss -ltn | grep ":5432 " && echo "5432 BUSY -> use override" || echo "5432 FREE"'`
Expected: `5432 FREE`. (If BUSY: create `~/hydeclaw/docker/docker-compose.override.yml` mapping `127.0.0.1:5434:5432` and change `DATABASE_URL` port to 5434 in `~/hydeclaw/.env`.)

- [ ] **Step 2: Build the custom image + start postgres only**

Run: `ssh aronmav@188.246.224.118 'cd ~/hydeclaw/docker && sudo docker compose up -d --build postgres'`
Expected: image `hydeclaw-pg:17-age-pgvector` built; container starts.

- [ ] **Step 3: Verify pg is healthy with AGE + pgvector**

Run: `ssh aronmav@188.246.224.118 'cd ~/hydeclaw/docker && sudo docker compose ps postgres; cid=$(sudo docker compose ps -q postgres); sudo docker exec $cid psql -U hydeclaw -d hydeclaw -tAc "select version()" | head -1; sudo docker exec $cid psql -U hydeclaw -d hydeclaw -tAc "create extension if not exists vector; create extension if not exists age; select extname from pg_extension order by 1"'`
Expected: PostgreSQL 17.x; extensions list includes `age`, `vector`.

### Task A7: Cross-build x86_64 binaries locally and deploy them

**Files:** local build → `~/hydeclaw/*-x86_64` on the server.

- [ ] **Step 1: Cross-build all three binaries (local dev machine)**

Run: `cargo zigbuild --release --target x86_64-unknown-linux-gnu -p hydeclaw-core -p hydeclaw-memory-worker -p hydeclaw-watchdog`
Expected: `Finished release` for all three; binaries under `target/x86_64-unknown-linux-gnu/release/`.

- [ ] **Step 2: Verify they are x86_64 ELF**

Run: `file target/x86_64-unknown-linux-gnu/release/hydeclaw-core`
Expected: `ELF 64-bit LSB ... x86-64`.

- [ ] **Step 3: scp the three binaries to the server with `-x86_64` suffix**

```bash
scp target/x86_64-unknown-linux-gnu/release/hydeclaw-core       aronmav@188.246.224.118:~/hydeclaw/hydeclaw-core-x86_64
scp target/x86_64-unknown-linux-gnu/release/hydeclaw-memory-worker aronmav@188.246.224.118:~/hydeclaw/hydeclaw-memory-worker-x86_64
scp target/x86_64-unknown-linux-gnu/release/hydeclaw-watchdog    aronmav@188.246.224.118:~/hydeclaw/hydeclaw-watchdog-x86_64
ssh aronmav@188.246.224.118 'chmod +x ~/hydeclaw/hydeclaw-*-x86_64'
```
Expected: 3 files copied, executable.

- [ ] **Step 4: Verify the core binary runs on the server**

Run: `ssh aronmav@188.246.224.118 '~/hydeclaw/hydeclaw-core-x86_64 --version 2>&1 | head -1 || ldd ~/hydeclaw/hydeclaw-core-x86_64 | head -3'`
Expected: a version line, or `ldd` shows resolved libs (no "not found"). A `not found` here means a missing shared lib — install it before proceeding.

### Task A8: Build toolgate venv + channels deps on the server

**Files:** `~/hydeclaw/toolgate/.venv`, `~/hydeclaw/channels/node_modules` (server).

- [ ] **Step 1: Create the toolgate venv and install requirements**

Run: `ssh aronmav@188.246.224.118 'cd ~/hydeclaw/toolgate && python3 -m venv .venv && .venv/bin/pip install -q --upgrade pip && .venv/bin/pip install -q -r requirements.txt && echo VENV_OK'`
Expected: `VENV_OK`.

- [ ] **Step 2: Verify toolgate imports (incl. normalize/espeak path)**

Run: `ssh aronmav@188.246.224.118 'cd ~/hydeclaw/toolgate && .venv/bin/python -c "import app; from normalize import transliterate_latin as t; print(t(\"test Gemini\"))"'`
Expected: prints `тест джемини` (espeak G2P + dict working through the deployed code).

- [ ] **Step 3: Install channels deps with Bun**

Run: `ssh aronmav@188.246.224.118 'export PATH=$HOME/.bun/bin:$PATH; cd ~/hydeclaw/channels && bun install --frozen-lockfile && echo CHANNELS_OK'`
Expected: `CHANNELS_OK`.

### Task A9: Install systemd units + smoke-test core on the EMPTY DB

**Files:** uses `deploy/server/*.service`.

- [ ] **Step 1: Copy the unit files into the user systemd dir**

```bash
scp deploy/server/hydeclaw-core.service deploy/server/hydeclaw-memory-worker.service deploy/server/hydeclaw-watchdog.service aronmav@188.246.224.118:~/.config/systemd/user/
ssh aronmav@188.246.224.118 'systemctl --user daemon-reload && loginctl enable-linger aronmav'
```
Expected: exit 0 (`enable-linger` lets user services run without an active login).

- [ ] **Step 2: Start ONLY core (empty DB → no channel creds → no Telegram conflict with the live Pi)**

Run: `ssh aronmav@188.246.224.118 'systemctl --user start hydeclaw-core; sleep 8; systemctl --user is-active hydeclaw-core'`
Expected: `active`.

- [ ] **Step 3: Smoke-test the API + confirm migrations ran**

Run: `ssh aronmav@188.246.224.118 'TOKEN=$(grep ^HYDECLAW_AUTH_TOKEN= ~/hydeclaw/.env | cut -d= -f2-); curl -s -o /dev/null -w "doctor=%{http_code}\n" -H "Authorization: Bearer $TOKEN" http://localhost:18789/api/doctor; cd ~/hydeclaw/docker && cid=$(sudo docker compose ps -q postgres); sudo docker exec $cid psql -U hydeclaw -d hydeclaw -tAc "select count(*) from information_schema.tables where table_schema=\$\$public\$\$"'`
Expected: `doctor=200`; table count > 20 (migrations applied).

- [ ] **Step 4: Verify port 18789 is NOT reachable from the public IP**

Run (from the local dev machine, hitting the public IP): `curl -s -o /dev/null -w "public=%{http_code}\n" --max-time 6 http://188.246.224.118:18789/api/doctor || echo "public=BLOCKED"`
Expected: `public=BLOCKED` (connection refused/timeout) — the firewall works.

- [ ] **Step 5: Stop core (ready for cutover)**

Run: `ssh aronmav@188.246.224.118 'systemctl --user stop hydeclaw-core; systemctl --user is-active hydeclaw-core || echo stopped'`
Expected: `stopped`.

---

## PHASE B — Cutover (maintenance window; downtime ≈ minutes)

### Task B1: Stop HydeClaw on the Pi (frees the Telegram token)

**Files:** none.

- [ ] **Step 1: Stop the three Pi units**

Run: `ssh aronmav@192.168.1.82 'systemctl --user stop hydeclaw-core hydeclaw-memory-worker hydeclaw-watchdog; for u in core memory-worker watchdog; do echo "$u: $(systemctl --user is-active hydeclaw-$u)"; done'`
Expected: all three `inactive`.

- [ ] **Step 2: Confirm no stray core process holds the Telegram token**

Run: `ssh aronmav@192.168.1.82 'pgrep -af hydeclaw-core-aarch64 || echo "no core process — token freed"'`
Expected: `no core process — token freed`.

### Task B2: Dump the Pi DB and restore it on the server

**Files:** none.

- [ ] **Step 1: pg_dump on the Pi (custom format) → local file on the Pi**

Run: `ssh aronmav@192.168.1.82 'sudo docker exec docker-postgres-1 pg_dump -U hydeclaw -d hydeclaw -Fc -f /tmp/hydeclaw.dump && sudo docker cp docker-postgres-1:/tmp/hydeclaw.dump ~/hydeclaw-cutover.dump && ls -la ~/hydeclaw-cutover.dump'`
Expected: dump file ~50–150 MB.

- [ ] **Step 2: Transfer the dump Pi → server**

Run: `ssh aronmav@192.168.1.82 'rsync -az ~/hydeclaw-cutover.dump aronmav@188.246.224.118:~/hydeclaw-cutover.dump'`
Expected: exit 0.

- [ ] **Step 3: Drop + recreate the server DB, then restore (overwrites the empty migrated schema)**

Run:
```bash
ssh aronmav@188.246.224.118 'cd ~/hydeclaw/docker && cid=$(sudo docker compose ps -q postgres); \
  sudo docker cp ~/hydeclaw-cutover.dump $cid:/tmp/hydeclaw.dump; \
  sudo docker exec $cid psql -U hydeclaw -d postgres -c "DROP DATABASE IF EXISTS hydeclaw WITH (FORCE); CREATE DATABASE hydeclaw OWNER hydeclaw;"; \
  sudo docker exec $cid sh -c "psql -U hydeclaw -d hydeclaw -c \"create extension if not exists age; create extension if not exists vector;\" && pg_restore -U hydeclaw -d hydeclaw --no-owner /tmp/hydeclaw.dump"; echo RESTORE_DONE'
```
Expected: `RESTORE_DONE` (pg_restore may print non-fatal warnings about existing extensions — acceptable).

- [ ] **Step 4: Verify row counts match a known table**

Run: `ssh aronmav@188.246.224.118 'cd ~/hydeclaw/docker && cid=$(sudo docker compose ps -q postgres); for t in sessions messages memory_chunks providers agent_channels secrets; do echo "$t: $(sudo docker exec $cid psql -U hydeclaw -d hydeclaw -tAc "select count(*) from $t")"; done'`
Expected: non-trivial counts (compare against the Pi: `ssh Pi 'sudo docker exec docker-postgres-1 psql -U hydeclaw -d hydeclaw -tAc "select count(*) from messages"'`).

### Task B3: Final sync of workspace + agent configs

**Files:** none.

- [ ] **Step 1: Re-rsync workspace/ and config/agents/ (capture last-minute runtime edits)**

Run: `ssh aronmav@192.168.1.82 'rsync -az --delete ~/hydeclaw/workspace/ aronmav@188.246.224.118:~/hydeclaw/workspace/ && rsync -az ~/hydeclaw/config/agents/ aronmav@188.246.224.118:~/hydeclaw/config/agents/'`
Expected: exit 0.

### Task B4: Start HydeClaw on the server

**Files:** none.

- [ ] **Step 1: Enable + start the three units**

Run: `ssh aronmav@188.246.224.118 'systemctl --user enable hydeclaw-core hydeclaw-memory-worker hydeclaw-watchdog; systemctl --user start hydeclaw-core hydeclaw-memory-worker hydeclaw-watchdog; sleep 12; for u in core memory-worker watchdog; do echo "$u: $(systemctl --user is-active hydeclaw-$u)"; done'`
Expected: all three `active`.

- [ ] **Step 2: Confirm core spawned toolgate + channels**

Run: `ssh aronmav@188.246.224.118 'curl -s -o /dev/null -w "toolgate=%{http_code}\n" http://localhost:9011/health; pgrep -af "bun .*channels" >/dev/null && echo "channels: running" || echo "channels: NOT running"'`
Expected: `toolgate=200`; `channels: running`.

### Task B5: Verification checklist

**Files:** none.

- [ ] **Step 1: API health**

Run: `ssh aronmav@188.246.224.118 'TOKEN=$(grep ^HYDECLAW_AUTH_TOKEN= ~/hydeclaw/.env | cut -d= -f2-); curl -s -o /dev/null -w "doctor=%{http_code}\n" -H "Authorization: Bearer $TOKEN" http://localhost:18789/api/doctor'`
Expected: `doctor=200`.

- [ ] **Step 2: Vault decrypt (proves the master key carried over)**

Run: `ssh aronmav@188.246.224.118 'TOKEN=$(grep ^HYDECLAW_AUTH_TOKEN= ~/hydeclaw/.env | cut -d= -f2-); curl -s -H "Authorization: Bearer $TOKEN" "http://localhost:18789/api/channels?reveal=true" | head -c 200; echo'`
Expected: channel JSON with a non-empty `bot_token` (or equivalent) — NOT an error/empty. A decrypt failure here means the master key did not match — STOP and roll back (Task B6).

- [ ] **Step 3: Memory semantic search (pgvector)**

Run: `ssh aronmav@188.246.224.118 'TOKEN=$(grep ^HYDECLAW_AUTH_TOKEN= ~/hydeclaw/.env | cut -d= -f2-); curl -s -H "Authorization: Bearer $TOKEN" "http://localhost:18789/api/memory/search?q=test&limit=3" -o /dev/null -w "memory=%{http_code}\n"'`
Expected: `memory=200`.

- [ ] **Step 4: Chat round-trip (SSE)**

Run: `ssh aronmav@188.246.224.118 'TOKEN=$(grep ^HYDECLAW_AUTH_TOKEN= ~/hydeclaw/.env | cut -d= -f2-); curl -s -N --max-time 60 -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" -d "{\"agent\":\"Hyde\",\"messages\":[{\"role\":\"user\",\"content\":\"скажи одно слово\"}]}" http://localhost:18789/api/chat | grep -m1 -E "text-delta|finish" && echo "CHAT OK"'`
Expected: a `text-delta`/`finish` event, then `CHAT OK`.

- [ ] **Step 5: Telegram round-trip (manual, with the user)**

Action: the user sends a message to the bot in Telegram and requests a voice reply ("ответь голосом …").
Expected: the agent replies AND a voice message arrives (TTS is now local on the server). Confirm in the logs: `ssh aronmav@188.246.224.118 'tail -20 ~/hydeclaw/logs/core.log'`.

### Task B6: Rollback path (only if a verification step fails)

**Files:** none.

- [ ] **Step 1: Stop the server, restart the Pi**

Run: `ssh aronmav@188.246.224.118 'systemctl --user stop hydeclaw-core hydeclaw-memory-worker hydeclaw-watchdog' && ssh aronmav@192.168.1.82 'systemctl --user start hydeclaw-core hydeclaw-memory-worker hydeclaw-watchdog; sleep 8; systemctl --user is-active hydeclaw-core'`
Expected: Pi core `active`; Telegram token returns to the Pi; the Pi DB was never modified by the dump. Investigate the failed step before retrying the cutover.

---

## PHASE C — Decommission the Pi (after ~1 day soak)

### Task C1: External firewall verification (post-cutover, from outside)

**Files:** none.

- [ ] **Step 1: External port scan of the public IP**

Run (from a host OUTSIDE the home network / over mobile data): `nmap -Pn -p 18789,9011,5432,4317 188.246.224.118`
Expected: all four ports `filtered`/`closed` — none `open`.

### Task C2: Decommission the Pi

**Files:** none.

- [ ] **Step 1: Take a final Pi DB backup (safety net)**

Run: `ssh aronmav@192.168.1.82 'sudo docker exec docker-postgres-1 pg_dump -U hydeclaw -d hydeclaw -Fc -f /tmp/hydeclaw-final.dump && sudo docker cp docker-postgres-1:/tmp/hydeclaw-final.dump ~/hydeclaw-final-$(date +%Y%m%d).dump && ls -la ~/hydeclaw-final-*.dump'`
Expected: a dated final dump on the Pi.

- [ ] **Step 2: Stop + disable the Pi units**

Run: `ssh aronmav@192.168.1.82 'systemctl --user stop hydeclaw-core hydeclaw-memory-worker hydeclaw-watchdog; systemctl --user disable hydeclaw-core hydeclaw-memory-worker hydeclaw-watchdog; echo DISABLED'`
Expected: `DISABLED`. The Pi is now free to repurpose; keep the final dump until the server has run cleanly for a while.

---

## Post-migration follow-ups (out of scope for this plan)

- Public domain + TLS for the UI via nginx-proxy-manager (user will configure).
- Optional: repoint the TTS provider `base_url` from `http://10.10.1.42:8000` to a server-local address (drops the wg hop) via `PUT /api/providers/{id}`.
- Update the memory note [[reference_pi_deploy]] / [[reference_deploy_env]] to the new server target once stable.
