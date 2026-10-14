#!/usr/bin/env python3
"""Remove whole `## <ID> - ...` sections from a findings document.

Usage: scripts/notes_drop.py <file> <ID> [<ID> ...]

A section runs from its `## ` heading to the next `## ` heading or EOF.
Fails if any named ID has no section, so a typo never silently no-ops.
"""
import re
import sys


def main() -> int:
    path, ids = sys.argv[1], set(sys.argv[2:])
    with open(path, encoding="utf-8") as f:
        lines = f.read().split("\n")
    out, skipping, found = [], False, set()
    for line in lines:
        m = re.match(r"## ([A-Z]+-\d{3}) - ", line)
        if line.startswith("## "):
            skipping = bool(m and m.group(1) in ids)
            if skipping:
                found.add(m.group(1))
        if not skipping:
            out.append(line)
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
