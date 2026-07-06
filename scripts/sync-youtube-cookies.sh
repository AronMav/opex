#!/usr/bin/env bash
# sync-youtube-cookies.sh — validate YouTube cookies stored in the OPEX
# secrets vault and send a notification if they are stale or missing.
#
# Cookies are stored under the `YOUTUBE_COOKIES` secret in the vault
# (managed via `/secrets` UI). Toolgate fetches them at download time via
# `GET /api/internal/youtube-cookies` — no file placement required.
#
# Run via cron: 0 * * * * /home/aronmav/opex/scripts/sync-youtube-cookies.sh
set -euo pipefail

OPEX_API="http://localhost:18789"
AUTH_TOKEN="$(grep OPEX_AUTH_TOKEN /home/aronmav/opex/.env 2>/dev/null | cut -d= -f2 || true)"

if [ -z "$AUTH_TOKEN" ]; then
    echo "ERROR: OPEX_AUTH_TOKEN not set"
    exit 1
fi

# ── 1. Fetch cookies from vault ──────────────────────────────────────────────
COOKIES_FILE=$(mktemp)
trap 'rm -f "$COOKIES_FILE"' EXIT

HTTP_CODE=$(curl -s -o "$COOKIES_FILE" -w "%{http_code}" \
    "$OPEX_API/api/internal/youtube-cookies" \
    -H "Authorization: Bearer $AUTH_TOKEN" 2>/dev/null || echo "000")

if [ "$HTTP_CODE" = "404" ]; then
    echo "WARN: YOUTUBE_COOKIES not set in vault"
    curl -s -X POST "$OPEX_API/api/notifications" \
        -H "Authorization: Bearer $AUTH_TOKEN" \
        -H "Content-Type: application/json" \
        -d '{"type":"watchdog_alert","title":"YouTube cookies not configured","body":"No YOUTUBE_COOKIES secret in vault. Video summaries will fail. Add cookies via /secrets UI (name: YOUTUBE_COOKIES).","data":{}}' 2>/dev/null || true
    exit 1
fi

if [ "$HTTP_CODE" != "200" ]; then
    echo "ERROR: vault returned HTTP $HTTP_CODE"
    exit 1
fi

# Parse cookies from JSON response.
COOKIES=$(python3 -c "import sys,json; print(json.load(open('$COOKIES_FILE'))['cookies'])" 2>/dev/null || true)
if [ -z "$COOKIES" ]; then
    echo "WARN: YOUTUBE_COOKIES is set but empty"
    curl -s -X POST "$OPEX_API/api/notifications" \
        -H "Authorization: Bearer $AUTH_TOKEN" \
        -H "Content-Type: application/json" \
        -d '{"type":"watchdog_alert","title":"YouTube cookies empty","body":"YOUTUBE_COOKIES secret is set but empty. Video summaries will fail. Update cookies via /secrets UI.","data":{}}' 2>/dev/null || true
    exit 1
fi

# Write to temp file for analysis.
echo "$COOKIES" > "$COOKIES_FILE"

# ── 2. Check YouTube cookies are present ──────────────────────────────────────
YT_COOKIES=$(grep -c "youtube.com" "$COOKIES_FILE" || true)
if [ "$YT_COOKIES" -lt 5 ]; then
    echo "WARN: only $YT_COOKIES youtube.com cookies found (need at least 5)"
    curl -s -X POST "$OPEX_API/api/notifications" \
        -H "Authorization: Bearer $AUTH_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"type\":\"watchdog_alert\",\"title\":\"YouTube cookies stale\",\"body\":\"YouTube cookies have only $YT_COOKIES youtube.com entries. Video summaries will fail. Update cookies via /secrets UI (name: YOUTUBE_COOKIES).\",\"data\":{}}" 2>/dev/null || true
    exit 1
fi

# ── 3. Check key auth cookies are present ─────────────────────────────────────
KEY_COOKIES="__Secure-3PSID __Secure-3PAPISID LOGIN_INFO"
MISSING=""
for name in $KEY_COOKIES; do
    if ! grep -q "$name" "$COOKIES_FILE"; then
        MISSING="$MISSING $name"
    fi
done

if [ -n "$MISSING" ]; then
    echo "WARN: missing key cookies:$MISSING"
    curl -s -X POST "$OPEX_API/api/notifications" \
        -H "Authorization: Bearer $AUTH_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"type\":\"watchdog_alert\",\"title\":\"YouTube cookies missing key entries\",\"body\":\"Missing cookies:$MISSING. Video summaries will fail with bot-check. Update cookies via /secrets UI (name: YOUTUBE_COOKIES).\",\"data\":{}}" 2>/dev/null || true
    exit 1
fi

# ── 4. Check cookies are not expired ──────────────────────────────────────────
NOW=$(date +%s)
EXPIRED_COUNT=$(python3 -c "
import sys
now = $NOW
expired = 0
total = 0
for line in open('$COOKIES_FILE'):
    if 'youtube.com' not in line:
        continue
    total += 1
    parts = line.strip().split('\t')
    if len(parts) >= 5:
        try:
            exp = int(parts[4])
            if 0 < exp < now:
                expired += 1
        except ValueError:
            pass
print(expired)
" 2>/dev/null || echo 0)

TOTAL_YT=$(grep "youtube.com" "$COOKIES_FILE" | wc -l)
if [ "$EXPIRED_COUNT" -gt 0 ] && [ "$EXPIRED_COUNT" -ge "$((TOTAL_YT / 2))" ]; then
    echo "WARN: $EXPIRED_COUNT of $TOTAL_YT youtube cookies are expired"
    curl -s -X POST "$OPEX_API/api/notifications" \
        -H "Authorization: Bearer $AUTH_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"type\":\"watchdog_alert\",\"title\":\"YouTube cookies expired\",\"body\":\"$EXPIRED_COUNT of $TOTAL_YT youtube.com cookies have expired. Video summaries may fail. Update cookies via /secrets UI (name: YOUTUBE_COOKIES).\",\"data\":{}}" 2>/dev/null || true
    exit 1
fi

echo "OK: $TOTAL_YT youtube.com cookies, $EXPIRED_COUNT expired, key cookies present"
exit 0