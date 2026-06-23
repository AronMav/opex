# OPEX migration: Raspberry Pi → server (188.246.224.118)

- **Date:** 2026-06-18
- **Status:** Design approved, pending implementation plan
- **Topic:** Full migration of the OPEX agent stack from the Raspberry Pi 4 (`aronmav@192.168.1.82`, aarch64) to the home-lab server (`aronmav@188.246.224.118`, x86_64).

## Goal & context

Move the entire OPEX stack to the server so it is co-located with the media
services (TTS `openedai-speech`, STT `whisper`) it already depends on — removing
the WireGuard tunnel hop for media calls and consolidating onto one host. The
server freed ~13–15 GB RAM after the 1C database was removed (now ~17 GB
available, CPU mostly idle), so it has the headroom.

### Machines

| | Pi (`192.168.1.82`) | Server (`188.246.224.118`) |
| --- | --- | --- |
| Arch | aarch64 (RPi 4) | x86_64 (i7-8700, 12 threads) |
| RAM | 7.6 GB | 31 GB (~17 GB free post-1C) |
| Network | home LAN, reaches server via wg | **public IP** + docker_wg (10.10.1.0/24) + wg-easy + nginx-proxy-manager |
| OPEX | core + memory-worker + watchdog (systemd --user); core spawns toolgate (Python) + channels (Bun); postgres in `docker-postgres-1` (127.0.0.1:5432, db `opex`) | TTS/STT already here |

## Decisions (from brainstorming)

1. **End state:** full migration; Pi retired after a soak period.
2. **Deploy method:** native (systemd --user), mirroring the Pi — *not* containerised.
3. **External exposure:** none for now — **wg-only**. The user will add a
   public domain + TLS via nginx-proxy-manager themselves later.
4. **Cutover:** maintenance window (stop Pi → dump/restore → start server). A
   Telegram bot token can only be polled by one process, so channel cutover is
   inherently exclusive anyway.

## End-state architecture

Native systemd stack under `~/opex/` on the server (same layout as the Pi):

- `opex-core` (x86_64 binary, `opex-core.service`) — spawns **toolgate**
  (Python venv) and **channels** (Bun) as managed child processes; talks to the
  host Docker daemon for `code_exec`/MCP.
- `opex-memory-worker.service`, `opex-watchdog.service`.
- **PostgreSQL 17 + pgvector** — Docker container (mirror of `docker-postgres-1`),
  listening on `127.0.0.1:5434` (5434, not 5432, to avoid clashing with any
  other local pg), db `opex`.
- Media (`openedai-speech` TTS, `whisper` STT) — already on the server; reached
  locally (no wg hop).

### Host prerequisites to install

- `apt install`: **bun** (official installer), **ffmpeg**, **espeak-ng** — these
  are currently absent on the server host (ffmpeg/espeak-ng were only installed
  on the Pi). Needed for toolgate's output denoise (ffmpeg) and G2P
  transliteration (espeak-ng).
- Already present: `python3.13`, `docker` 29.x.
- **No Rust toolchain on the server** → binaries are cross-compiled locally with
  `cargo zigbuild --release --target x86_64-unknown-linux-gnu` and scp'd (same
  workflow as the Pi, x86 target instead of aarch64).

### `.env` (4 keys)

- `OPEX_MASTER_KEY` — **identical to the Pi** (vault decryption depends on it).
- `OPEX_AUTH_TOKEN` — same as the Pi (or rotate).
- `DATABASE_URL` — new, pointing at the local pg on `:5434`.
- `RUST_LOG` — as-is.

## Runbook

### Phase A — Preparation (no downtime; Pi keeps running)

1. Reclaim swap on the server: `sudo swapoff -a && sudo swapon -a`; confirm the
   RAM budget (OPEX ~1–2 GB + its postgres ~1–4 GB fits in ~17 GB free).
2. Install host deps: bun, ffmpeg, espeak-ng; verify python3.13 + docker.
3. Stand up the `postgres17+pgvector` container on `127.0.0.1:5434`; create db
   `opex` and `CREATE EXTENSION vector`.
4. Cross-build x86_64 binaries locally (core, memory-worker, watchdog) → scp to
   `~/opex/`.
5. Copy `config/`, `workspace/` (rsync, ~353 MB), `.env` (new DATABASE_URL);
   copy `toolgate/` + `python -m venv` + `pip install`; copy `channels/` +
   `bun install`.
6. Create the 3 systemd --user units. Let core run migrations against the empty
   db; smoke-test `/api/doctor` → 200 to confirm it boots.

### Phase B — Cutover (maintenance window; downtime ≈ minutes)

1. Stop OPEX on the Pi (the 3 units) — frees the Telegram token, quiesces
   the db.
2. `pg_dump` (custom format) on the Pi → scp → `pg_restore` into the server db.
3. Final `rsync` of `workspace/` + `config/agents/` (capture any runtime edits).
4. Start OPEX on the server (core brings up toolgate + channels).
5. Run the verification checklist below.

### Phase C — Decommission Pi

After ~a day of soak: stop + disable the Pi units, take a final backup, free the
Pi for other use.

## Security / exposure (wg-only)

The server has a **public IP**, unlike the isolated Pi — so a firewall is
mandatory:

- nftables/ufw on the public interface (eth0/188.x): **drop** all OPEX ports
  (`18789` API/UI, `9011` toolgate, `5434` pg, WS ports) from the public
  interface; **allow** from `10.8.0.0/24` (wg) + `10.10.1.0/24` (docker_wg) + LAN.
  Prefer an explicit firewall over bind-address juggling (easier to audit).
- **Post-cutover check:** external `nmap` of `188.246.224.118` from another host —
  confirm `18789/9011/5434` are closed from outside.
- Channels are **outbound only** (Telegram/Discord) → zero inbound surface.
- Postgres `127.0.0.1:5434` only; toolgate `:9011` loopback only; `.env` mode 600.
- Public domain + TLS via nginx-proxy-manager (reverse-proxy to
  `127.0.0.1:18789` + Let's Encrypt + auth) is **deferred to the user**. Until
  then, access the UI over wg at `10.x:18789`.

## Data integrity, verification, rollback

- **Vault (item #1):** `OPEX_MASTER_KEY` must match the Pi verbatim — without
  it, every secret (channel `CHANNEL_CREDENTIALS`, provider API keys) is
  undecryptable.
- **pgvector:** install the `vector` extension on the target **before**
  `pg_restore` (the dump references `halfvec` types); keep the pgvector major
  version compatible with the Pi's.
- **Verification checklist (Phase B step 5):**
  1. `/api/doctor` → 200.
  2. Vault decrypt: `GET /api/channels?reveal=true` returns credentials → master
     key works.
  3. Memory: semantic search over `memory_chunks` (pgvector halfvec) returns hits.
  4. Chat: SSE round-trip on `/api/chat`.
  5. Telegram: message → agent reply → **voice reply** (TTS now local) → polling
     active on the server only.
- **Rollback:** the Pi is left intact (db + units, just stopped) until verified.
  On failure, restart the Pi units (its db was untouched by the dump); the
  Telegram token returns to the Pi. Full rollback in ~a minute.

## Risks

1. pg / pgvector version mismatch → verify versions before Phase B.
2. Master-key mistake → verbatim copy + verification test #2.
3. Firewall hole exposing `:18789` publicly → external nmap after cutover.
4. `config/agents/*.toml` reference `tts_provider = "qwen3-tts-local"`; that
   provider lives in the db and travels with the dump — OK.

## Out of scope / follow-ups

- Public domain + TLS exposure via nginx-proxy-manager (user will do).
- Optional: repoint the TTS provider `base_url` from `http://10.10.1.42:8000` to
  a local address once OPEX is on the server (drops the wg hop; minor).
- Repurposing the freed Pi.
