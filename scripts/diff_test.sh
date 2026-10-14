#!/usr/bin/env bash
# Differential test: dellingr vs reference Lua 5.2 and 5.4.
#
# Walks examples/**/*.lua and runs each script through dellingr, lua5.2,
# and lua5.4. A file passes when its dellingr output matches lua5.4 (or
# matches lua5.2 - we accept either of the two reference outputs). Files tagged
# with -- DIFF: <reason> may differ from both after dellingr exits successfully.
#
# Env:
#   DELLINGR=/path/to/dellingr    use an already-built binary
#   DELLINGR_SKIP_BUILD=1         require DELLINGR and do not build
#   DELLINGR_DIFF_TIMEOUT=5s      per-process timeout (default 5s)
#   DELLINGR_SKIP_TIMEOUT=1       legacy switch to disable timeouts
#
# Output is intentionally terse: prints "ok" on success, or one failure line
# per failing script. Exit 1 on any failure.

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
cd "$SCRIPT_DIR/.." || exit 1
ROOT="$(pwd -P)"
. "$SCRIPT_DIR/lib.sh"

for tool in lua5.2 lua5.4
do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "missing reference Lua: $tool" >&2
        exit 1
    fi
done

if [ "${DELLINGR_SKIP_TIMEOUT:-0}" != "1" ]; then
    if command -v timeout >/dev/null 2>&1; then
        TIMEOUT_BIN="$(command -v timeout)"
        TIMEOUT_MODE=external
    elif command -v gtimeout >/dev/null 2>&1; then
        TIMEOUT_BIN="$(command -v gtimeout)"
        TIMEOUT_MODE=external
    elif command -v python3 >/dev/null 2>&1; then
        TIMEOUT_MODE=python
    else
        echo "missing timeout support; install timeout/gtimeout or Python 3, or set DELLINGR_SKIP_TIMEOUT=1" >&2
        exit 1
    fi
fi
DELLINGR_DIFF_TIMEOUT="${DELLINGR_DIFF_TIMEOUT:-5s}"

if [ "${DELLINGR_SKIP_BUILD:-0}" = "1" ]; then
    if [ -z "${DELLINGR:-}" ]; then
        echo "DELLINGR_SKIP_BUILD=1 requires DELLINGR to name the binary" >&2
        exit 1
    fi
    OUR_LUA="$DELLINGR"
else
    OUR_LUA="$(dellingr_bin release)" || exit 1
fi
if [ ! -x "$OUR_LUA" ]; then
    echo "FAIL: dellingr binary is not executable: $OUR_LUA" >&2
    exit 1
fi

SCRATCH_DIR="$ROOT/.scratch"
if ! mkdir -p "$SCRATCH_DIR"; then
    echo "FAIL: cannot create project scratch directory: $SCRATCH_DIR" >&2
    exit 1
fi
OUR_OUT="$(mktemp "$SCRATCH_DIR/dellingr-diff-our.XXXXXX")" || exit 1
LUA52_OUT="$(mktemp "$SCRATCH_DIR/dellingr-diff-lua52.XXXXXX")" || {
    rm -f "$OUR_OUT"
    exit 1
}
LUA54_OUT="$(mktemp "$SCRATCH_DIR/dellingr-diff-lua54.XXXXXX")" || {
    rm -f "$OUR_OUT" "$LUA52_OUT"
    exit 1
}
trap 'rm -f "$OUR_OUT" "$LUA52_OUT" "$LUA54_OUT"' EXIT

scripts=()
append_lua_files() {
    local root="$1"
    local file
    while IFS= read -r file
    do
        scripts+=("$file")
    done < <(find "$root" -type f -name '*.lua' -print | LC_ALL=C sort)
}

if [ "$#" -gt 0 ]; then
    scripts=()
    for arg in "$@"
    do
        if [ -d "$arg" ]; then
            append_lua_files "$arg"
        elif [ -f "$arg" ]; then
            scripts+=("$arg")
        else
            echo "FAIL: missing $arg"
            exit 1
        fi
    done
else
    append_lua_files examples
fi
set +u
script_count="${#scripts[@]}"
set -u
if [ "$script_count" -eq 0 ]; then
    echo "FAIL: no Lua scripts found"
    exit 1
fi

failures=()
run_lua() {
    if [ "${DELLINGR_SKIP_TIMEOUT:-0}" = "1" ]; then
        "$@"
    elif [ "$TIMEOUT_MODE" = "python" ]; then
        python3 -c '
import math
import subprocess
import sys

raw = sys.argv[1]
units = {"ns": 1e-9, "us": 1e-6, "ms": 1e-3, "s": 1, "m": 60, "h": 3600, "d": 86400}
unit = next((item for item in sorted(units, key=len, reverse=True) if raw.endswith(item)), "")
value = raw[:-len(unit)] if unit else raw
try:
    seconds = float(value) * units.get(unit, 1)
except ValueError:
    print("invalid DELLINGR_DIFF_TIMEOUT: " + raw, file=sys.stderr)
    sys.exit(125)
if not math.isfinite(seconds) or seconds < 0:
    print("invalid DELLINGR_DIFF_TIMEOUT: " + raw, file=sys.stderr)
    sys.exit(125)
try:
    result = subprocess.run(sys.argv[2:], timeout=None if seconds == 0 else seconds)
except subprocess.TimeoutExpired:
    sys.exit(124)
sys.exit(result.returncode if result.returncode >= 0 else 128 - result.returncode)
' "$DELLINGR_DIFF_TIMEOUT" "$@"
    else
        "$TIMEOUT_BIN" "$DELLINGR_DIFF_TIMEOUT" "$@"
    fi
}

for f in "${scripts[@]}"
do
    case "$f" in
        "$ROOT/examples/"*) name="${f#"$ROOT/examples/"}" ;;
        examples/*) name="${f#examples/}" ;;
        *) name="$f" ;;
    esac

    if grep -q '^-- SKIP-DIFF: .' "$f"; then
        continue
    fi

    has_diff=0
    if grep -q '^-- DIFF:' "$f"; then
        has_diff=1
    fi

    run_lua "$OUR_LUA" --quiet "$f" > "$OUR_OUT" 2>&1
    our_status=$?
    if [ "$our_status" -eq 124 ] && [ "${DELLINGR_SKIP_TIMEOUT:-0}" != "1" ]; then
        failures+=("TIMEOUT: $name (dellingr)")
        continue
    fi
    if [ "$our_status" -ne 0 ]; then
        failures+=("FAIL: $name (dellingr exited $our_status)")
        continue
    fi

    run_lua lua5.2 "$f" > "$LUA52_OUT" 2>&1
    lua52_status=$?
    if [ "$lua52_status" -eq 124 ] && [ "${DELLINGR_SKIP_TIMEOUT:-0}" != "1" ]; then
        failures+=("TIMEOUT: $name (lua5.2)")
        continue
    fi
    run_lua lua5.4 "$f" > "$LUA54_OUT" 2>&1
    lua54_status=$?
    if [ "$lua54_status" -eq 124 ] && [ "${DELLINGR_SKIP_TIMEOUT:-0}" != "1" ]; then
        failures+=("TIMEOUT: $name (lua5.4)")
        continue
    fi

    if [ "$has_diff" = "1" ]; then
        continue
    fi
    if diff -q "$OUR_OUT" "$LUA54_OUT" > /dev/null 2>&1; then
        continue
    fi
    if diff -q "$OUR_OUT" "$LUA52_OUT" > /dev/null 2>&1; then
        continue
    fi
    failures+=("FAIL: $name")
done

set +u
failure_count="${#failures[@]}"
set -u
if [ "$failure_count" -eq 0 ]; then
    echo "ok"
else
    printf '%s\n' "${failures[@]}"
    exit 1
fi
