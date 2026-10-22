#!/usr/bin/env python3
"""Diff two `scripts/frame_sizes.py --body` dumps with the noise removed.

Usage: scripts/asm_diff.py [--slots] <before.asm> <after.asm> [max_lines]

Jump offsets inside the function and RIP-relative data offsets change with
every rebuild; they are blanked so the diff shows only instructions that
changed: spills, reloads, moved stores. Prints a unified diff with two lines
of context, capped at max_lines (default 400).
"""

import difflib
import re
import sys


def normalize(line: str) -> str:
    line = re.sub(r"\+0x[0-9a-f]+>", "+OFF>", line)
    line = re.sub(r"-?(?:0x[0-9a-f]+|\d+)\(%rip\)", "RIPREL", line)
    return line


SLOT = re.compile(r"(-?0x[0-9a-f]+)?\(%rsp\)")


def slots(lines: list[str]) -> dict[int, list[str]]:
    """Distinct `%rsp` offsets used, each with the instructions touching it."""
    found: dict[int, list[str]] = {}
    for line in lines:
        for m in SLOT.finditer(line):
            off = int(m.group(1), 16) if m.group(1) else 0
            found.setdefault(off, []).append(line)
    return found


def main() -> int:
    args = sys.argv[1:]
    slot_mode = False
    if args and args[0] == "--slots":
        slot_mode = True
        args = args[1:]
    if len(args) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    cap = int(args[2]) if len(args) > 2 else 400
    with open(args[0], encoding="utf-8") as f:
        before = [normalize(l) for l in f.read().splitlines()]
    with open(args[1], encoding="utf-8") as f:
        after = [normalize(l) for l in f.read().splitlines()]
    if slot_mode:
        # Per slot: how many instructions touch it, and the first few, so a
        # slot present in only one build (after undoing a uniform shift)
        # stands out.
        for label, lines in (("before", before), ("after", after)):
            print(f"== {label}: {len(slots(lines))} distinct rsp slots")
            for off, uses in sorted(slots(lines).items()):
                sample = "; ".join(uses[:2])
                print(f"{off:#06x}\t{len(uses)}\t{sample}")
        return 0
    diff = list(difflib.unified_diff(before, after, lineterm="", n=2))
    print(f"{len(diff)} diff lines")
    print("\n".join(diff[:cap]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
