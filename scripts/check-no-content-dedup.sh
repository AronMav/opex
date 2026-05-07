#!/usr/bin/env bash
set -euo pipefail

# Forbid content-based dedup patterns in production stream-message code.
# Identity-first contract: dedup is by ID, never by content hash.
# See docs/architecture/2026-05-05-id-based-dedup.md
#
# Patterns selected to catch ADR-2026-05-05 heuristic-dedup remnants WITHOUT
# false-positive on legitimate Rust content-addressable storage
# (e.g., skill_versions.content_hash, history.rs::prune_old_tool_results
# context_hashes — different concerns, not stream-message dedup).

FORBIDDEN_PATTERNS=(
  # Specific historical names from removed heuristics (ADR 2026-05-05 §"Phase 2")
  'lastHistAssistantTexts'
  'dedupeWithinSteps'
  'dedupeBubbleTextParts'
  'historyEndsWithNewUserTurn'
  # Generic dedup-content semantics — strong signal regardless of language
  'dedupeContent'
  'dedupe_content'
  # Frontend stream-message contentHash function — must NOT be reintroduced
  # after T1b deletion of chat-reconciliation.ts
  'contentHash'
)

EXCLUDE=(
  --exclude-dir=tests
  --exclude-dir=__tests__
  --exclude-dir=target
  --exclude-dir=node_modules
  --exclude-dir=.next
  --exclude-dir=.venv
  --exclude='*.test.ts'
  --exclude='*.test.tsx'
  --exclude='*_test.rs'
  --exclude='*.md'
)

for pattern in "${FORBIDDEN_PATTERNS[@]}"; do
  if grep -rEn "$pattern" "${EXCLUDE[@]}" \
       crates/ ui/src/ toolgate/ channels/src/ 2>&1 \
       | grep -v '^Binary file'; then
    echo ""
    echo "FAIL: forbidden content-dedup pattern '$pattern' in non-test code."
    echo "Identity-first contract: dedup is by ID, never by content hash."
    echo "See docs/architecture/2026-05-05-id-based-dedup.md"
    exit 1
  fi
done

echo "OK: no content-dedup patterns in production code."
