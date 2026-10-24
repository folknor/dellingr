#!/usr/bin/env python3
"""Remove whole `## <ID> - ...` sections from a findings document.

Usage: scripts/notes_drop.py <file> <ID> [<ID> ...]

A section runs from its `## ` heading to the next `## ` heading outside a
fenced code block, or EOF. Fails if any named ID has no section, so a typo
never silently no-ops.
This is a standalone maintainer helper and is invoked directly when needed.
"""
import re
import sys

from notes_sections import section_heading_flags


def drop_sections(lines, ids):
    out, skipping, found = [], False, set()
    for is_heading, line in zip(section_heading_flags(lines), lines):
        m = re.match(r"## ([A-Z]+-\d{3}) - ", line) if is_heading else None
        if is_heading:
            skipping = bool(m and m.group(1) in ids)
            if skipping:
                found.add(m.group(1))
        if not skipping:
            out.append(line)
    return out, found


def main() -> int:
    if len(sys.argv) < 3:
        print(f"usage: {sys.argv[0]} <file> <ID> [<ID> ...]", file=sys.stderr)
        return 2

    path, ids = sys.argv[1], set(sys.argv[2:])
    with open(path, encoding="utf-8") as f:
        lines = f.read().split("\n")
    out, found = drop_sections(lines, ids)
    missing = ids - found
    if missing:
        print(f"no section for: {', '.join(sorted(missing))}", file=sys.stderr)
        return 1
    text = "\n".join(out).rstrip("\n") + "\n"
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
