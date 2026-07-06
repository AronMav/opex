#!/usr/bin/env bash
# sync-youtube-cookies.sh — keep OPEX's YouTube cookies fresh by syncing from
# MeTube's cookie jar (MeTube's web UI lets the user log in to YouTube, which
# refreshes the cookies file). Also checks cookie validity and sends an OPEX
# notification if cookies are stale.
#
# Run via cron: 0 * * * * /home/aronmav/opex/scripts/sync-youtube-cookies.sh
set -euo pipefail

METUBE_COOKIES="/home/aronmav/docker/metube/.metube/cookies.txt"
OPEX_COOKIES="/home/aronmav/docker/metube/.metube/cookies.txt"  # same file — OPEX reads it directly
OPEX_API="http://localhost:18789"
AUTH_TOKEN="$(grep OPEX_AUTH_TOKEN /home/aronmav/opex/.env 2>/dev/null | cut -d= -f2 || true)"

# ── 1. Check cookies file exists ──────────────────────────────────────────────
if [ ! -f "$METUBE_COOKIES" ]; then
    echo "ERROR: cookies file not found: $METUBE_COOKIES"
    exit 1
fi

# ── 2. Check YouTube cookies are present ──────────────────────────────────────
YT_COOKIES=$(grep -c "youtube.com" "$METUBE_COOKIES" || true)
if [ "$YT_COOKIES" -lt 5 ]; then
    echo "WARN: only $YT_COOKIES youtube.com cookies found (need at least 5)"
    # Notify via OPEX API
    if [ -n "$AUTH_TOKEN" ]; then
        curl -s -X POST "$OPEX_API/api/notifications" \
            -H "Authorization: Bearer $AUTH_TOKEN" \
            -H "Content-Type: application/json" \
            -d "{\"type\":\"watchdog_alert\",\"title\":\"YouTube cookies stale\",\"body\":\"YouTube cookies file has only $YT_COOKIES youtube.com entries. Video summaries will fail. Log in to MeTube web UI to refresh cookies.\",\"data\":{}}" 2>/dev/null || true
    fi
    exit 1
fi

# ── 3. Check key auth cookies are present ─────────────────────────────────────
# These are the cookies yt-dlp needs to bypass the "Sign in to confirm you're
# not a bot" check. If any are missing, YouTube will block downloads.
KEY_COOKIES="__Secure-3PSID __Secure-3PAPISID LOGIN_INFO"
MISSING=""
for name in $KEY_COOKIES; do
    if ! grep -q "$name" "$METUBE_COOKIES"; then
        MISSING="$MISSING $name"
    fi
done

if [ -n "$MISSING" ]; then
    echo "WARN: missing key cookies:$MISSING"
    if [ -n "$AUTH_TOKEN" ]; then
        curl -s -X POST "$OPEX_API/api/notifications" \
            -H "Authorization: Bearer $AUTH_TOKEN" \
            -H "Content-Type: application/json" \
            -d "{\"type\":\"watchdog_alert\",\"title\":\"YouTube cookies missing key entries\",\"body\":\"Missing cookies:$MISSING. Video summaries will fail with bot-check. Log in to MeTube web UI (http://metube.aronmav.ru) to refresh cookies.\",\"data\":{}}" 2>/dev/null || true
    fi
    exit 1
fi

# ── 4. Check cookies are not expired ──────────────────────────────────────────
# Parse the expiry timestamps and compare with current time.
NOW=$(date +%s)
EXPIRED_COUNT=$(awk -F'\t' -v now="$NOW" '
    /youtube\.com/ && NF >= 7 {
        exp = $5 + 0
        if (exp > 0 && exp < now) expired++
    }
    END { print expired + 0 }
' "$METUBE_COOKIES" || echo 0)

TOTAL_YT=$(grep "youtube.com" "$METUBE_COOKIES" | wc -l)
if [ "$EXPIRED_COUNT" -gt 0 ] && [ "$EXPIRED_COUNT" -ge "$((TOTAL_YT / 2))" ]; then
    echo "WARN: $EXPIRED_COUNT of $TOTAL_YT youtube cookies are expired"
    if [ -n "$AUTH_TOKEN" ]; then
        curl -s -X POST "$OPEX_API/api/notifications" \
            -H "Authorization: Bearer $AUTH_TOKEN" \
            -H "Content-Type: application/json" \
            -d "{\"type\":\"watchdog_alert\",\"title\":\"YouTube cookies expired\",\"body\":\"$EXPIRED_COUNT of $TOTAL_YT youtube.com cookies have expired. Video summaries may fail. Log in to MeTube web UI to refresh cookies.\",\"data\":{}}" 2>/dev/null || true
    fi
    exit 1
fi

echo "OK: $TOTAL_YT youtube.com cookies, $EXPIRED_COUNT expired, key cookies present"
exit 0