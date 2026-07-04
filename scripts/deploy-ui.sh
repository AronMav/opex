#!/usr/bin/env bash
set -euo pipefail
# Build the UI locally and deploy to the server with an ATOMIC symlink flip.
#
# Why not mv-swap: between `mv out out.old` and `mv out.new out` there is a
# window where core's static server errors; the NPM proxy (assets.conf)
# caches ANY upstream response for 30 min, so a single mid-swap request can
# pin a 502 for a font/chunk until the proxy cache expires (seen live
# 2026-07-04). ~/opex/ui/out is a symlink into releases/; rename(2) of the
# symlink is atomic — there is no moment when the path is unservable.
#
# Union: hashed assets (_next/static) from the previous release are copied in
# (clients with cached HTML/CSS from a prior build keep their fonts/chunks),
# pruned after 3 days. The last 3 releases are retained.
#
# Usage: scripts/deploy-ui.sh [user@host]

HOST="${1:-aronmav@188.246.224.118}"
cd "$(dirname "$0")/../ui"

npm run build
tar czf /tmp/ui-out.tar.gz -C out .
scp -q /tmp/ui-out.tar.gz "$HOST:/tmp/ui-out.tar.gz"
ssh "$HOST" '
  set -e
  REL=~/opex/ui/releases/rel-$(date +%Y%m%d-%H%M%S)
  mkdir -p "$REL"
  tar xzf /tmp/ui-out.tar.gz -C "$REL"
  CUR=$(readlink -f ~/opex/ui/out 2>/dev/null || true)
  if [ -n "$CUR" ] && [ -d "$CUR/_next/static" ] && [ "$CUR" != "$REL" ]; then
    cp -rpn "$CUR/_next/static/." "$REL/_next/static/" 2>/dev/null || true
    find "$REL/_next/static" -type f -mtime +3 -delete 2>/dev/null || true
    find "$REL/_next/static" -type d -empty -delete 2>/dev/null || true
  fi
  # первый запуск: превращаем каталог out в симлинк
  if [ -d ~/opex/ui/out ] && [ ! -L ~/opex/ui/out ]; then
    mv ~/opex/ui/out ~/opex/ui/releases/rel-legacy
  fi
  ln -sfn "$REL" ~/opex/ui/out.tmp
  mv -T ~/opex/ui/out.tmp ~/opex/ui/out
  ls -1dt ~/opex/ui/releases/rel-* | tail -n +4 | xargs -r rm -rf
  rm -f /tmp/ui-out.tar.gz
'
rm -f /tmp/ui-out.tar.gz
echo "UI deployed (atomic symlink flip, prior hashed assets retained)."
