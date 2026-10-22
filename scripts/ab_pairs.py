#!/usr/bin/env python3
"""Interleaved A/B benchmark cells through brokkr.

For each workload, ROUNDS alternating `--bench --commit A` and
`--bench --commit B` runs, one at a time, so drift between cells lands on
both arms. Results land in .brokkr/results.db; read them with
`brokkr results -n <cells>`.

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
            for commit in (a, b):
                print(f"== {workload} round {i} @ {commit}", flush=True)
                proc = subprocess.run(
                    ["brokkr", "dellingr", "--lua", workload, "--bench", "--commit", commit],
                    cwd=ROOT,
                )
                if proc.returncode != 0:
                    print(f"brokkr failed for {workload} @ {commit}", file=sys.stderr)
                    return proc.returncode
    return 0


if __name__ == "__main__":
    sys.exit(main())
