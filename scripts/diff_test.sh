#!/usr/bin/env bash
# Differential test: dellingr vs reference Lua 5.2 and 5.4.
#
# Walks examples/**/*.lua, running each script through dellingr and comparing
# ordinary examples with the configured reference runtimes. A file passes when
# its dellingr output matches either reference. Files tagged with -- DIFF:
# <reason> skip the reference runs after dellingr exits successfully.
#
# Accepting either reference is deliberate: dellingr is a Lua 5.2/5.4 hybrid
# (the README's "Won't implement" list removes most of what separates the two),
# and where the references disagree, which one dellingr should follow is a
# per-feature decision, not a global one. A per-file reference pin would need
# upkeep on every example for little gain, since a file where both references
# agree cannot hide a regression, and a deliberate divergence is already
# marked with -- DIFF: <reason>.
#
# Env:
#   DELLINGR=/path/to/dellingr    use an already-built binary
#   DELLINGR_SKIP_BUILD=1         require DELLINGR and do not build
#   DELLINGR_DIFF_TIMEOUT=<time>  per-process timeout (default is in lib.sh)
#   DELLINGR_DIFF_TIMEOUT=0       disable the per-process timeout
#
# Output is intentionally terse: prints "ok" on success, or one failure line
# per failing script. Invalid timeout configuration exits 2; gate failures exit 1.

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
cd "$SCRIPT_DIR/.." || exit 1
ROOT="$(pwd -P)"
. "$SCRIPT_DIR/lib.sh"

DURATION_RE='^\+?([0-9]+([.][0-9]*)?|[.][0-9]+)([smhd])?$'
if [[ ! "$DELLINGR_DIFF_TIMEOUT" =~ $DURATION_RE ]]; then
    echo "invalid DELLINGR_DIFF_TIMEOUT: $DELLINGR_DIFF_TIMEOUT" >&2
    exit 2
fi

TIMEOUT_MODE=""
TIMEOUT_BIN=""
ZERO_DURATION_RE='^\+?(0+([.]0*)?|[.]0+)[smhd]?$'
if [[ "$DELLINGR_DIFF_TIMEOUT" =~ $ZERO_DURATION_RE ]]; then
    TIMEOUT_MODE=none
elif command -v timeout >/dev/null 2>&1; then
    TIMEOUT_BIN="$(command -v timeout)"
    TIMEOUT_MODE=external
else
    echo "missing timeout support; install GNU coreutils or set DELLINGR_DIFF_TIMEOUT=0" >&2
    exit 1
fi

if [ "$TIMEOUT_MODE" = external ]; then
    timeout_status=0
    "$TIMEOUT_BIN" "$DELLINGR_DIFF_TIMEOUT" true >/dev/null 2>&1 || timeout_status=$?
    if [ "$timeout_status" -ne 0 ] && [ "$timeout_status" -ne 124 ]; then
        echo "invalid DELLINGR_DIFF_TIMEOUT: $DELLINGR_DIFF_TIMEOUT" >&2
        exit 2
    fi
fi

if [ "$DELLINGR_SKIP_BUILD" = "1" ]; then
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
references_checked=0
run_lua() {
    if [ "$TIMEOUT_MODE" = none ]; then
        "$@"
    else
        "$TIMEOUT_BIN" "$DELLINGR_DIFF_TIMEOUT" "$@"
    fi
}

report_capture() {
    local label="$1"
    local capture="$2"
    if [ -s "$capture" ]; then
        printf '  %s output:\n' "$label" >&2
        sed 's/^/    /' "$capture" >&2
    fi
}

for f in "${scripts[@]}"
do
    case "$f" in
        "$ROOT/examples/"*) name="${f#"$ROOT/examples/"}" ;;
        examples/*) name="${f#examples/}" ;;
        *) name="$f" ;;
    esac

    if grep -Eq '^-- (DIFF|SKIP-DIFF):[[:space:]]*$' "$f"; then
        failures+=("FAIL: $name (diff marker requires a reason)")
        continue
    fi

    if grep -q '^-- SKIP-DIFF:' "$f"; then
        continue
    fi

    has_diff=0
    if grep -q '^-- DIFF:' "$f"; then
        has_diff=1
    fi

    run_lua "$OUR_LUA" --quiet "$f" > "$OUR_OUT" 2>&1
    our_status=$?
    if [ "$our_status" -eq 124 ] && [ "$TIMEOUT_MODE" != none ]; then
        failures+=("TIMEOUT: $name (dellingr)")
        report_capture "dellingr" "$OUR_OUT"
        continue
    fi
    if [ "$our_status" -ne 0 ]; then
        failures+=("FAIL: $name (dellingr exited $our_status)")
        report_capture "dellingr" "$OUR_OUT"
        continue
    fi

    if [ "$has_diff" = "1" ]; then
        continue
    fi

    if [ "$references_checked" -eq 0 ]; then
        for index in "${!REFERENCE_LUAS[@]}"; do
            tool="${REFERENCE_LUAS[$index]}"
            if ! command -v "$tool" >/dev/null 2>&1; then
                echo "missing reference ${REFERENCE_LUA_LABELS[$index]} executable: $tool (set ${REFERENCE_LUA_OVERRIDES[$index]} to its path)" >&2
                exit 1
            fi
        done
        references_checked=1
    fi

    run_lua "${REFERENCE_LUAS[0]}" "$f" > "$LUA52_OUT" 2>&1
    lua52_status=$?
    if [ "$lua52_status" -eq 124 ] && [ "$TIMEOUT_MODE" != none ]; then
        failures+=("TIMEOUT: $name (${REFERENCE_LUA_LABELS[0]})")
        report_capture "${REFERENCE_LUA_LABELS[0]}" "$LUA52_OUT"
        continue
    fi
    run_lua "${REFERENCE_LUAS[1]}" "$f" > "$LUA54_OUT" 2>&1
    lua54_status=$?
    if [ "$lua54_status" -eq 124 ] && [ "$TIMEOUT_MODE" != none ]; then
        failures+=("TIMEOUT: $name (${REFERENCE_LUA_LABELS[1]})")
        report_capture "${REFERENCE_LUA_LABELS[1]}" "$LUA54_OUT"
        continue
    fi

    if diff -q "$OUR_OUT" "$LUA54_OUT" > /dev/null 2>&1; then
        continue
    fi
    if diff -q "$OUR_OUT" "$LUA52_OUT" > /dev/null 2>&1; then
        continue
    fi
    failures+=("FAIL: $name")
    printf 'differential output mismatch for %s:\n' "$name" >&2
    diff -u --label dellingr --label "${REFERENCE_LUA_LABELS[1]}" "$OUR_OUT" "$LUA54_OUT" >&2 || :
    diff -u --label dellingr --label "${REFERENCE_LUA_LABELS[0]}" "$OUR_OUT" "$LUA52_OUT" >&2 || :
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
