# HydeClaw Upgrade Notes

> These are historical upgrade notes. The current release is v0.27.0.
> If upgrading from v0.20+, no action from these sections is needed.

## Upgrading to v0.20+: toolgate config → Core API single source of truth

**Breaking change:** toolgate no longer reads the following environment variables.
Pre-create equivalent providers via the admin UI (or `POST /api/providers`)
**before** restarting hydeclaw-core, or toolgate will start in **degraded mode**
and capability endpoints will return 503 until providers are configured.

### Removed environment variables

| Deprecated env var | Replacement (in Core provider registry) |
|---|---|
| `WHISPER_URL`, `OLLAMA_API_KEY` (for STT) | Create provider with `type=stt`, `driver=whisper-local`, `base_url=<your whisper URL>` |
| `VISION_URL`, `VISION_MODEL`, `OLLAMA_API_KEY` | Create provider with `type=vision`, `driver=ollama`, `base_url=<vision URL>`, `default_model=<model>` |
| `TTS_BACKEND_URL` | Create provider with `type=tts`, `driver=qwen3-tts`, `base_url=<your Qwen3-TTS URL>` |
| `MINIMAX_API_KEY` (normalize LLM) | Create provider with `type=text`, `provider_type=openai-compatible`, `base_url=<MiniMax URL>`, `api_key=<key>`; then reference its UUID in the TTS provider's `options.normalize_provider_id` |

### Verifying the migration

1. **Before upgrade:** on the current Pi, list env vars:
   ```bash
   systemctl --user show-environment | grep -E 'WHISPER|VISION|OLLAMA|TTS_BACKEND|MINIMAX'
   ```
2. **For each listed var:** create the equivalent provider via UI (Settings → Media Providers → Add Provider).
3. **For the MINIMAX normalize case:** note the UUID of the new `text` provider you create. In the TTS provider editor, set `options.normalize_provider_id = "<that UUID>"` and `options.normalize = true`.
4. **Upgrade:** `./update.sh hydeclaw-v<VERSION>.tar.gz`
5. **Verify:**
   ```bash
   curl -s http://localhost:9011/health | jq .
   ```
   Expected: `"degraded": false`, all used capabilities `true` in the `capabilities` map.

### Rollback

If providers were not pre-created, you can:
1. Revert to previous binary (`~/hydeclaw/hydeclaw-core-aarch64.bak` if kept)
2. **or** create providers retroactively via UI — toolgate will auto-reload on the first matching `PUT /api/providers/{id}`.

### Architectural rationale

See `docs/superpowers/specs/2026-04-18-toolgate-config-sot-design.md` for full
design context (degraded mode, nested `normalize_provider_id`, etc.).

## v0.20.x → v0.20.next — toolgate primitives refactor

Toolgate's `email`, `calendar`, and `bcs_portfolio` routers are replaced by primitive endpoints:

| Old endpoint | New primitive endpoint |
|---|---|
| `POST /email/send` | `POST /primitives/smtp/send` |
| `GET /email/inbox` | `POST /primitives/imap/fetch` |
| `GET /email/search` | `POST /primitives/imap/search` |
| `GET /calendar/today` | `POST /primitives/google_calendar/events/list` |
| `GET /calendar/upcoming` | `POST /primitives/google_calendar/events/list` |
| `POST /calendar/create` | `POST /primitives/google_calendar/events/create` |
| `GET /bcs/portfolio` | `POST /primitives/bcs/portfolio` |

All credentials now flow through the core secrets vault. Before upgrading, add the
following secrets via `POST /api/secrets` (replace values with your own).

> ⚠ The curl commands below contain plaintext passwords. Run them from a shell
> with history disabled (`set +o history` in bash, `setopt no_hist_save` in zsh),
> or clear shell history afterward.

### Secrets to add

```bash
# SMTP + IMAP — required for email_* tools to function.
# Ports are hardcoded to standards (587 for SMTP submission, 993 for IMAPS)
# in the YAML bodies; if you need non-standard ports, edit
# workspace/tools/email_{send,check,search}.yaml directly.
curl -sSf -H "Authorization: Bearer $HYDECLAW_AUTH_TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"SMTP_HOST","scope":"","value":"smtp.gmail.com"}' http://localhost:18789/api/secrets
curl -sSf -H "Authorization: Bearer $HYDECLAW_AUTH_TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"IMAP_HOST","scope":"","value":"imap.gmail.com"}' http://localhost:18789/api/secrets
curl -sSf -H "Authorization: Bearer $HYDECLAW_AUTH_TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"EMAIL_USER","scope":"","value":"you@gmail.com"}' http://localhost:18789/api/secrets
curl -sSf -H "Authorization: Bearer $HYDECLAW_AUTH_TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"EMAIL_PASS","scope":"","value":"YOUR_APP_PASSWORD"}' http://localhost:18789/api/secrets

# Google Calendar — paste the entire service-account JSON as a single string
GSA_JSON=$(cat /path/to/service-account.json | jq -c .)
curl -sSf -H "Authorization: Bearer $HYDECLAW_AUTH_TOKEN" -H "Content-Type: application/json" \
  -d "{\"name\":\"GOOGLE_SA_KEY_JSON\",\"scope\":\"\",\"value\":$(echo "$GSA_JSON" | jq -Rs .)}" \
  http://localhost:18789/api/secrets
curl -sSf -H "Authorization: Bearer $HYDECLAW_AUTH_TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"GOOGLE_CALENDAR_ID","scope":"","value":"primary"}' http://localhost:18789/api/secrets

# BCS — unchanged, only listed for completeness
# (BCS_REFRESH_TOKEN should already be in the vault)
```

### What changed

Tools that previously relied on env-var credentials (`EMAIL_USER`, `EMAIL_PASS`,
`GOOGLE_SA_KEY` as a file path) will now succeed without those env vars set;
the previous env-based path was non-functional in any default deployment and
is removed entirely.

### Dependencies

Toolgate now requires `google-api-python-client` and `google-auth`.
Run `pip install -r toolgate/requirements.txt` on the deploy target (or rely on
`make deploy` to sync).

### Calendar behavior note

`calendar_today` and `calendar_upcoming` now always query the "next 7 days
from now" window (previously there was implicit day-start alignment). The
`days` parameter on `calendar_upcoming` is accepted but ignored — file an
issue if you need the old behavior back.

### Architectural rationale

See `docs/superpowers/specs/2026-04-19-toolgate-primitives-design.md` for full
design context (primitives vs integration routers, `${VAR}` templating in
`body_template`, BCS state carve-out, etc.).

### calendar_create response shape change

The agent-facing response for `calendar_create` has changed shape (this is a
breaking change for agent prompts that referenced specific fields):

- **Old**: `{status, id, link, summary, start, end}` (direct from the GET router)
- **New**: `{id, summary, html_link}` (extracted via `response_transform: $.event`)

Field renames: `link` → `html_link`. Removed: `status`, `start`, `end`, echoed
`summary`. If your agent prompts or skills read these fields, update them
before upgrading.

### Known limitations (follow-ups tracked)

- **BCS refresh token error classification**: bad or expired `BCS_REFRESH_TOKEN`
  now surfaces as 401 from `/primitives/bcs/portfolio` — agents should detect
  this and prompt for token rotation.
- **End-to-end automation gap**: there is no automated test that loads a real
  `workspace/tools/*.yaml`, runs it through core's YAML-tool runtime, and hits
  a mocked primitive. Manual curl smoke test in Task 11 of the implementation
  plan covers happy paths. A future integration-test harness would close this
  gap (tracked as I3 in the plan).
- **Optional `body_template` params without explicit values**: if the LLM
  omits an optional parameter (e.g. `html` in `email_send`), its placeholder
  `{{html}}` is not substituted with the default — the request body becomes
  invalid JSON and the call fails. This is a pre-existing core behavior (not
  introduced by the primitives refactor), but now affects more tools. Agents
  should always pass optional params explicitly until core materialises
  defaults into the substitution map.
