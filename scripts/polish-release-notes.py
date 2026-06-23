#!/usr/bin/env python3
"""Polish auto-generated release notes into a consistent published style.

The shell generator emits headers like `## V0.2.0 — release build...`.
This pass:
  * normalises the version-tag display ("v0.2.0" lower-case, no caps)
  * truncates over-long themes
  * de-clutters changelog lines (drops `chore:`/`merge:`)
  * adds the closing 'Distribution' footer that's identical across all
    releases (so users see the same standard call-to-action wherever
    they land)
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

ARCHIVE = Path(__file__).resolve().parent.parent / ".release-notes-archive"

# Same footer in every release page — a small "where to download / how
# to install" reminder is more useful than a different snowflake per
# release.
FOOTER = """
### Install / upgrade

Download the architecture-matching tarball from this release page:

* `opex-aarch64-*.tar.gz` — Raspberry Pi 4/5 and other ARM64 Linux hosts.
* `opex-x86_64-*.tar.gz` — generic 64-bit Linux servers.

Fresh install: extract, run `./setup.sh`. Upgrade in place: extract, run
`~/opex/update.sh <tarball>`. Both scripts handle service restart and
migration apply.

Configuration source of truth lives in `~/opex/config/opex.toml`
and `~/opex/.env`; nothing in the tarball overwrites them.
"""


def polish(path: Path) -> None:
    src = path.read_text(encoding="utf-8")
    lines = src.splitlines()

    # ── Normalise the header line ─────────────────────────────────────
    # Pattern: `## V0.2.0 — <theme>` → `## v0.2.0 — <Theme>`
    if lines and lines[0].startswith("## "):
        m = re.match(r"^## (V\d[\w.-]*)\s+(?:—|-)\s+(.+)$", lines[0])
        if m:
            tag = m.group(1).lower()
            theme = m.group(2).strip()
            # Truncate themes that are too long (paragraphs of fixes).
            if len(theme) > 90:
                theme = theme[:87].rstrip(" ,;:") + "…"
            # Capitalize the first letter without lowercasing the rest
            # (preserve ALL-CAPS acronyms like SSE, OTel, WAL, FTS).
            if theme:
                theme = theme[0].upper() + theme[1:]
            lines[0] = f"## {tag} — {theme}"

    # ── Drop noisy chore/merge lines from the change list ─────────────
    cleaned: list[str] = []
    for ln in lines:
        if ln.startswith("* chore"):
            continue
        if ln.startswith("* merge:"):
            continue
        if ln.startswith("* version") or ln.startswith("* bump"):
            continue
        cleaned.append(ln)

    out = "\n".join(cleaned).rstrip() + "\n" + FOOTER
    path.write_text(out, encoding="utf-8")


def main() -> int:
    if not ARCHIVE.is_dir():
        print(f"archive not found: {ARCHIVE}", file=sys.stderr)
        return 1
    for f in sorted(ARCHIVE.glob("v*.md")):
        polish(f)
    print(f"polished {len(list(ARCHIVE.glob('v*.md')))} files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
