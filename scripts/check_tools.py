#!/usr/bin/env python3
"""Check that this host has every external tool a green `brokkr check` needs.

`brokkr.toml` runs this as a pre-clippy script check, so a missing or wrong
tool is named before anything compiles. Prints one line per problem, then
`ok` only when there are none.

This covers the gate's own requirements and nothing else (brokkr itself, and
the `sh` its script checks run under, are brokkr's). Scripts outside brokkr's
pipelines - `scripts/diff_test.sh`, `scripts/bench.sh`,
`research/bench_luars.sh` - check their own tools when they run.

- python3 3.11 or newer: the script checks, including
  `scripts/crmath_tables.py --check-pins` (3.11 for `tomllib`).
- lua5.2 and lua5.4: the example corpus tests compare every unmarked example
  with both. Their output decides that verdict, so the release and the
  compatibility flags they were built with are pinned: a host whose reference
  prints differently fails here rather than as a corpus mismatch.
- git: `fixtures/lint-policy/check_policy.py` lists the tree with
  `git ls-files`.
"""

import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MINIMUM_PYTHON = (3, 11)
PROBE_TIMEOUT_SECONDS = 10

# Which compatibility globals each reference has: the pinned 5.2 build has
# LUA_COMPAT_ALL's (unpack, loadstring, module, table.maxn) and the pinned 5.4
# build LUA_COMPAT_5_3's math library (math.pow, math.ldexp). Probed with -E,
# as the corpus test runs them, so LUA_INIT cannot change the answer.
COMPAT_PROBE = (
    "print(type(unpack), type(loadstring), type(module), type(table.maxn), "
    "type(bit32), type(math.pow), type(math.ldexp), type(utf8))"
)
REFERENCES = (
    ("lua5.2", "5.2.4", "function function function function table function function nil"),
    ("lua5.4", "5.4.9", "nil nil nil nil nil function function table"),
)


def run(command):
    """Run `command` from the repository root; return (exit code, stdout,
    stderr), or a problem string when it cannot run at all."""
    try:
        result = subprocess.run(
            command,
            cwd=ROOT,
            capture_output=True,
            stdin=subprocess.DEVNULL,
            timeout=PROBE_TIMEOUT_SECONDS,
        )
    except FileNotFoundError:
        return f"missing tool: {command[0]}"
    except subprocess.TimeoutExpired:
        return f"{' '.join(command)} did not finish in {PROBE_TIMEOUT_SECONDS}s"
    except OSError as error:
        return f"cannot run {command[0]}: {error}"
    return result.returncode, result.stdout, result.stderr


def check_reference(program, version, compat):
    found = run([program, "-E", "-v"])
    if isinstance(found, str):
        return [found]
    code, stdout, stderr = found
    tokens = (stdout + stderr).decode(errors="replace").split()
    if code != 0 or tokens[:2] != ["Lua", version]:
        reported = " ".join(tokens[:2]) or "nothing"
        return [f"{program}: expected Lua {version}, found {reported}"]
    found = run([program, "-E", "-e", COMPAT_PROBE])
    if isinstance(found, str):
        return [found]
    code, stdout, stderr = found
    fingerprint = " ".join(stdout.decode(errors="replace").split())
    if code != 0 or fingerprint != compat:
        return [
            f"{program}: compatibility globals differ from the pinned build: "
            f"expected {compat!r}, found {fingerprint!r}"
            + (f" ({stderr.decode(errors='replace').strip()})" if stderr else "")
        ]
    return []


def check_command(command, purpose):
    found = run(command)
    if isinstance(found, str):
        return [f"{found} ({purpose})"]
    code, _, stderr = found
    if code != 0:
        detail = stderr.decode(errors="replace").strip()
        return [f"{' '.join(command)} failed ({purpose}): {detail}"]
    return []


def problems():
    found = []
    if sys.version_info < MINIMUM_PYTHON:
        found.append(
            "python3 {}.{} or newer is required, found {}.{}".format(
                *MINIMUM_PYTHON, *sys.version_info[:2]
            )
        )
    for program, version, compat in REFERENCES:
        if shutil.which(program) is None:
            found.append(f"missing tool: {program} (reference Lua for the example corpus test)")
            continue
        found.extend(check_reference(program, version, compat))
    found.extend(
        check_command(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
            "fixtures/lint-policy/check_policy.py lists the tree",
        )
    )
    return found


def main():
    found = problems()
    for problem in found:
        print(problem)
    if found:
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
