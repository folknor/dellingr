#!/usr/bin/env python3
"""Filter `perf annotate --stdio` output down to its hot instructions.

    perf_hot_lines.py <annotate-output> [min-percent] [context]

Prints every disassembly line whose sample percentage is at least
`min-percent` (default 0.5), with `context` cold lines either side
(default 2), and a `--` separator between non-adjacent runs.
"""

import sys


def percent(line: str) -> float:
    head = line.split(":", 1)[0].strip()
    try:
        return float(head)
    except ValueError:
        return 0.0


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    path = sys.argv[1]
    threshold = float(sys.argv[2]) if len(sys.argv) > 2 else 0.5
    context = int(sys.argv[3]) if len(sys.argv) > 3 else 2
    with open(path, encoding="utf-8", errors="replace") as f:
        lines = f.read().splitlines()
    keep = set()
    for i, line in enumerate(lines):
        if percent(line) >= threshold:
            keep.update(range(max(0, i - context), min(len(lines), i + context + 1)))
    last = None
    for i in sorted(keep):
        if last is not None and i != last + 1:
            print("--")
        print(lines[i])
        last = i
    return 0


if __name__ == "__main__":
    sys.exit(main())
