#!/usr/bin/env python3
"""Interleaved A/B benchmark cells through brokkr.

For each workload, ROUNDS alternating A/B and B/A pairs of
`brokkr bench --commit <ref> wall --workload <w>` runs, one at a time, so
drift between cells lands on both arms. Results land in the bench store;
read them with `brokkr results` (Script benches).

    python3 scripts/ab_pairs.py <commit-a> <commit-b> <rounds> <workload>...
"""

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main() -> int:
    if len(sys.argv) < 5:
        print(__doc__, file=sys.stderr)
        return 2
    a, b, rounds = sys.argv[1], sys.argv[2], int(sys.argv[3])
    for workload in sys.argv[4:]:
        for i in range(1, rounds + 1):
            commits = (a, b) if i % 2 else (b, a)
            for commit in commits:
                print(f"== {workload} round {i} @ {commit}", flush=True)
                proc = subprocess.run(
                    ["brokkr", "bench", "--commit", commit, "wall", "--workload", workload],
                    cwd=ROOT,
                )
                if proc.returncode != 0:
                    print(f"brokkr failed for {workload} @ {commit}", file=sys.stderr)
                    return proc.returncode
    return 0


if __name__ == "__main__":
    sys.exit(main())
