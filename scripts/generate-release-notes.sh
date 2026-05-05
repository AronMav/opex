#!/usr/bin/env bash
# Generate release notes in a unified style for every published release.
#
# Output: .release-notes-archive/v<X.Y.Z>.md per release.
# After review, push to GitHub via:
#   gh release edit v<X.Y.Z> --notes-file .release-notes-archive/v<X.Y.Z>.md
#
# Style:
#   ## <Theme line — auto-derived from top commits>
#   <One-sentence overview>
#
#   ### Changes
#   * <commit subject>
#   * ...
#
#   ### Upgrade notes
#   <only if migrations / breaking changes detected>
# Don't use `set -e`: grep returns 1 when there are no matches, and the
# pipeline `git log | grep -v ... | head -1` then trips pipefail; we
# treat empty headline / empty migrations list as legitimate states.
set -uo pipefail || true

cd "$(dirname "$0")/.."

ARCHIVE=".release-notes-archive"
mkdir -p "$ARCHIVE"

# Walk every tag in chronological order.
TAGS=($(git tag --sort=creatordate))
PREV=""

for TAG in "${TAGS[@]}"; do
    if [ -z "$PREV" ]; then
        RANGE="$TAG"
    else
        RANGE="${PREV}..${TAG}"
    fi

    OUT="$ARCHIVE/${TAG}.md"

    # Pull commits, drop pure chore/release/typo lines for the headline
    # but keep them in the full list.
    HEADLINE_COMMIT=$(git log --pretty=format:'%s' "$RANGE" 2>/dev/null \
        | grep -vE '^(chore|release|version|bump|ci|merge):' \
        | grep -vE '^(chore|release|ci)\(' \
        | head -1)

    HEADLINE_COMMIT="${HEADLINE_COMMIT:-Release ${TAG}}"

    # Strip leading "feat: " / "fix: " etc. to get the human theme.
    THEME=$(echo "$HEADLINE_COMMIT" | sed -E 's/^[a-z]+(\([^)]+\))?[!:]?\s*//')

    HAS_MIGRATION=$(git diff --name-only "$RANGE" 2>/dev/null | grep -E '^migrations/' | head -3)

    {
        echo "## ${TAG^} — ${THEME}"
        echo
        # One-sentence narrative based on commit count + migration presence.
        N_COMMITS=$(git log --oneline "$RANGE" 2>/dev/null | wc -l)
        if [ -n "$HAS_MIGRATION" ]; then
            echo "Schema-affecting release covering ${N_COMMITS} commit(s); migrations included."
        else
            echo "Iterative release covering ${N_COMMITS} commit(s)."
        fi
        echo
        echo "### Changes"
        echo
        git log --pretty=format:'* %s' "$RANGE" 2>/dev/null \
            | grep -vE '^\* (chore|release|version|bump):' \
            | head -25
        echo
        echo
        if [ -n "$HAS_MIGRATION" ]; then
            echo "### Upgrade notes"
            echo
            echo "* New SQL migrations are auto-applied at startup via sqlx (no manual step required)."
            echo "* Files changed:"
            echo
            echo "$HAS_MIGRATION" | sed 's/^/  * /'
            echo
        fi
    } > "$OUT"

    PREV="$TAG"
done

echo "Generated $(ls "$ARCHIVE" | wc -l) release-notes files in $ARCHIVE/"
