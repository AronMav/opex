> **Language:** English · [Русский](SECURITY.ru.md)

# Security

## Reporting Vulnerabilities

If you discover a security vulnerability, please report it privately via GitHub's [Security Advisories](https://github.com/AronMav/opex/security/advisories/new) feature. Do not open a public issue.

## Security Model

OPEX is designed to run on a private LAN or behind a reverse proxy. It is NOT hardened for direct exposure to the public internet without additional measures (TLS termination, firewall, VPN).

### Authentication

- **HTTP API** — all endpoints require `Authorization: Bearer <token>`, except health, uploads, webhooks, and OAuth callbacks.
- **WebSocket** — one-time tickets (`?ticket=<uuid>`, 30s TTL, consumed on first use) so the static token is never exposed in a URL.
- **Webhooks** — per-webhook Bearer token (generic) or HMAC-SHA256 signature verification (GitHub).
- **Auth rate limiter** — 10 failed attempts from one IP trigger a 5-minute block.
- **Global request rate limiter** — configurable requests-per-minute limit per IP.
- **Constant-time comparison** — all token checks use `subtle::ConstantTimeEq`.

### Secrets Vault

- **Encryption** — ChaCha20-Poly1305 (AEAD) with a unique random 12-byte nonce per secret.
- **Master key** — a 32-byte hex key in the `OPEX_MASTER_KEY` env var. Losing the key destroys all stored secrets.
- **Scoping** — secrets are scoped per-agent `(name, scope)`. Resolution order: agent scope → global → env fallback.
- **Audit** — revealing a secret via `?reveal=true` creates an audit-log entry.
- **Channel credentials** — bot tokens are extracted from the JSON config, stored encrypted in the vault under the channel's UUID scope, and redacted from the `config` column in the database.

### SSRF Protection

YAML tool execution uses a hardened HTTP client (`ssrf_http_client`):

- The DNS resolver blocks private RFC 1918, loopback, and link-local addresses during resolution.
- URL scheme validation blocks `file://`, `ftp://`, and non-HTTP schemes.
- URL path parameters are URL-encoded; request-body templates are JSON-escaped.

### Loopback Restrictions

Requests from `127.0.0.1` / `::1` are allowed without authorization only for specific internal paths:
- `/health`, `/api/mcp/callback`, `/api/channels/notify`, `/api/media/upload`, `/uploads/*`, `/ws`

All other loopback requests (including `/api/secrets`, `/api/backup`) still require Bearer authorization.

### Docker Access

Core connects to Docker over TCP (`tcp://127.0.0.1:2375`). The Docker TCP listener is configured by `setup.sh` to bind to localhost only.

### Container Restart Allowlist

The API can only restart containers on the `docker.rebuild_allowed` list (default: `browser-renderer`, `searxng`). PostgreSQL and other unlisted containers are excluded. MCP containers can be added to the allowlist in `config/opex.toml` if needed.

### Webhook Auth Rate Limiting

Per-webhook failure counter: 5 auth failures within 5 minutes block the webhook for 10 minutes. Prevents brute-forcing webhook secrets.

### Security Headers

Applied globally: `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `X-XSS-Protection: 1; mode=block`, `Referrer-Policy: strict-origin-when-cross-origin`.

### Tool Name Validation

API handlers enforce `[a-zA-Z0-9_-]` on tool, MCP-entry, and skill names to prevent path traversal.

### Code Execution Sandbox

The `code_exec` tool runs user code in an isolated Docker container:

- No network access
- Read-only filesystem (except `/tmp`)
- Memory and CPU limits
- Execution timeout

## Deployment Best Practices

1. **Always use TLS** — run behind nginx/Caddy with HTTPS, or use a VPN.
2. **Generate strong tokens** — `openssl rand -hex 32` for `OPEX_AUTH_TOKEN` and `OPEX_MASTER_KEY`.
3. **Back up the master key** — store it separately from the database. Without it, all vault secrets are unrecoverable.
4. **Restrict network access** — bind to `127.0.0.1` if you only need local access, or use firewall rules.
5. **Keep PostgreSQL local** — the default Docker config binds postgres to `127.0.0.1:5432`.
6. **Review tool definitions** — YAML tools can make arbitrary HTTP requests. Review `workspace/tools/` before deploying.
