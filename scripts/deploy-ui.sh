#!/usr/bin/env bash
set -euo pipefail
# Build the UI locally and atomically swap ~/opex/ui/out on the server.
#
# Union-swap: hashed assets (_next/static) from the previous build are copied
# into the new out/ before the swap. Clients holding cached HTML/CSS from the
# prior build (nginx serves static with ~20h max-age) would otherwise 404 on
# fonts/chunks and fall back to system fonts. Old assets are pruned after 3
# days (content-hashed, so coexistence is safe; CSS cache tops out at ~20h).
#
# Usage: scripts/deploy-ui.sh [user@host]

HOST="${1:-aronmav@188.246.224.118}"
cd "$(dirname "$0")/../ui"

npm run build
tar czf /tmp/ui-out.tar.gz -C out .
scp -q /tmp/ui-out.tar.gz "$HOST:/tmp/ui-out.tar.gz"
ssh "$HOST" '
  set -e
  rm -rf ~/opex/ui/out.new && mkdir -p ~/opex/ui/out.new
  tar xzf /tmp/ui-out.tar.gz -C ~/opex/ui/out.new
  if [ -d ~/opex/ui/out/_next/static ]; then
    cp -rpn ~/opex/ui/out/_next/static/. ~/opex/ui/out.new/_next/static/ 2>/dev/null || true
    find ~/opex/ui/out.new/_next/static -type f -mtime +3 -delete
    find ~/opex/ui/out.new/_next/static -type d -empty -delete
  fi
  mv ~/opex/ui/out ~/opex/ui/out.old
  mv ~/opex/ui/out.new ~/opex/ui/out
  rm -rf ~/opex/ui/out.old /tmp/ui-out.tar.gz
'
rm -f /tmp/ui-out.tar.gz
echo "UI deployed (union-swap, previous hashed assets retained)."
