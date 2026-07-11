#!/usr/bin/env python3
"""One-shot migration: enforce access control = restricted for existing agents.

Access control is now enabled by default ("restricted") for every newly
created agent. This script brings ALREADY-EXISTING agent config files in line
with that policy:

  * agents with an [agent.access] section whose  mode = "open"  → "restricted"
  * agents with NO [agent.access] section at all → an  [agent.access]
    section with  mode = "restricted"  is appended

Agents that already have  mode = "restricted"  are left untouched.

The  mode = "open"  replacement is applied ONLY inside the [agent.access]
section, so an unrelated  mode = "..."  key in another section (e.g. a
webhook's mode) is never touched.

Every file that changes is backed up next to it as  <name>.toml.bak  before
being rewritten. Re-running the script is safe (idempotent).

SECURITY NOTE: switching an agent to "restricted" without an owner_id means
NO channel user is allowed until the owner re-pairs (or owner_id is set).
This is intentional — it is the whole point of "secure by default". Review the
printed summary and set owner_id / re-pair as needed after running.

Usage (on the server):

    python3 scripts/migrate-agent-access-restricted.py            # ~/opex/config/agents
    python3 scripts/migrate-agent-access-restricted.py --dir DIR  # custom dir
    python3 scripts/migrate-agent-access-restricted.py --dry-run  # preview only

After running, restart core so the new configs take effect:

    systemctl --user restart opex-core
"""

from __future__ import annotations

import argparse
import glob
import os
import shutil
import sys

SECTION_HEADER = "[agent.access]"


def migrate_text(text: str) -> tuple[str, str]:
    """Return (new_text, action). action ∈ {unchanged, mode_open, added_section}."""
    lines = text.splitlines(keepends=True)
    in_access = False
    has_access = False
    changed_mode = False

    for i, line in enumerate(lines):
        stripped = line.strip()
        # Track section boundaries. Any `[...]` header that is not the access
        # header ends the access section.
        if stripped.startswith("[") and stripped.endswith("]"):
            in_access = stripped == SECTION_HEADER
            if in_access:
                has_access = True
            continue
        if in_access:
            # Replace only  mode = "open"  inside the access section.
            no_ws = stripped.replace(" ", "")
            if no_ws == 'mode="open"':
                lines[i] = line.replace('"open"', '"restricted"')
                changed_mode = True

    if changed_mode:
        return "".join(lines), "mode_open"

    if not has_access:
        new_text = text
        if new_text and not new_text.endswith("\n"):
            new_text += "\n"
        new_text += '\n[agent.access]\nmode = "restricted"\n'
        return new_text, "added_section"

    return text, "unchanged"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--dir",
        default=os.path.expanduser("~/opex/config/agents"),
        help="directory containing agent *.toml files",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="show what would change without writing",
    )
    args = parser.parse_args()

    pattern = os.path.join(args.dir, "*.toml")
    paths = sorted(glob.glob(pattern))
    if not paths:
        print(f"no agent config files found in {args.dir}", file=sys.stderr)
        return 1

    changed = 0
    for path in paths:
        with open(path, encoding="utf-8") as fh:
            original = fh.read()
        new_text, action = migrate_text(original)
        name = os.path.basename(path)
        if action == "unchanged":
            print(f"  ok       {name}")
            continue
        changed += 1
        label = "open→restricted" if action == "mode_open" else "added restricted"
        if args.dry_run:
            print(f"  WOULD    {name}  ({label})")
            continue
        shutil.copy2(path, path + ".bak")
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(new_text)
        print(f"  MIGRATED {name}  ({label}, backup: {name}.bak)")

    print(
        f"\n{changed} file(s) {'would be ' if args.dry_run else ''}migrated, "
        f"{len(paths) - changed} already restricted."
    )
    if changed and not args.dry_run:
        print("Restart core to apply:  systemctl --user restart opex-core")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
