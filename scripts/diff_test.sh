#!/usr/bin/env bash
# Differential test: dellingr vs reference Lua 5.2 and 5.4.
#
# Walks examples/**/*.lua, running each script through dellingr and comparing
# ordinary examples with the fixed reference runtimes. A file passes when
# its dellingr output matches either reference. Files tagged with -- DIFF:
# <reason> are expected to differ from both references; matching either one
# reports a stale marker after dellingr exits successfully.
#
# Accepting either reference is deliberate: dellingr is a Lua 5.2/5.4 hybrid
# (the README's "Won't implement" list removes most of what separates the two),
# and where the references disagree, which one dellingr should follow is a
# per-feature decision, not a global one. A per-file reference pin would need
# upkeep on every example for little gain, since a file where both references
# agree cannot hide a regression, and a deliberate divergence is already
# marked with -- DIFF: <reason>.
#
# Usage: diff_test.sh [--bin <path>] [--timeout <time>] [file-or-dir...]
#   --bin <path>                  use an already-built binary instead of building
#   --timeout <time>              per-process timeout in timeout(1) syntax
#                                 (default is in lib.sh); 0 disables it
#
# Internal input mode:
#   --list-examples               print the default example paths and exit
#
# Output is intentionally terse: prints "ok" on success, or one failure line
# per failing script. Invalid timeout configuration exits 2; gate failures exit 1.

set -u

# dellingr's character classes are fixed ASCII; reference Lua follows the
# locale. Pin the C locale so a Latin-1 host locale cannot make the references
# disagree on bytes 0x80..0xFF.
export LC_ALL=C

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
cd "$SCRIPT_DIR/.." || exit 1
ROOT="$(pwd -P)"
. "$SCRIPT_DIR/lib.sh"

scripts=()
append_lua_files() {
    local root="$1"
    local file
    while IFS= read -r file
    do
        scripts+=("$file")
    done < <(sorted_lua_files "$root")
}

if [ "$#" -eq 1 ] && [ "$1" = "--list-examples" ]; then
    append_lua_files examples
    if [ "${#scripts[@]}" -gt 0 ]; then
        printf '%s\n' "${scripts[@]}"
    fi
    exit 0
fi

OUR_LUA=""
DIFF_TIMEOUT="$DEFAULT_DIFF_TIMEOUT"
while [ "$#" -gt 0 ]
do
    case "$1" in
        --bin|--timeout)
            if [ "$#" -lt 2 ]; then
                echo "$1 requires a value" >&2
                exit 2
            fi
            if [ "$1" = --bin ]; then
                OUR_LUA="$2"
            else
                DIFF_TIMEOUT="$2"
            fi
            shift 2
            ;;
        --)
            shift
            break
            ;;
        *) break ;;
    esac
done

DURATION_RE='^\+?([0-9]+([.][0-9]*)?|[.][0-9]+)([smhd])?$'
if [[ ! "$DIFF_TIMEOUT" =~ $DURATION_RE ]]; then
    echo "invalid --timeout: $DIFF_TIMEOUT" >&2
    exit 2
fi

TIMEOUT_MODE=""
TIMEOUT_BIN=""
ZERO_DURATION_RE='^\+?(0+([.]0*)?|[.]0+)[smhd]?$'
if [[ "$DIFF_TIMEOUT" =~ $ZERO_DURATION_RE ]]; then
    TIMEOUT_MODE=none
elif command -v timeout >/dev/null 2>&1; then
    TIMEOUT_BIN="$(command -v timeout)"
    TIMEOUT_MODE=external
else
    echo "missing timeout support; install GNU coreutils or pass --timeout 0" >&2
    exit 1
fi

if [ "$TIMEOUT_MODE" = external ]; then
    timeout_status=0
    "$TIMEOUT_BIN" "$DIFF_TIMEOUT" true >/dev/null 2>&1 || timeout_status=$?
    if [ "$timeout_status" -ne 0 ] && [ "$timeout_status" -ne 124 ]; then
        echo "invalid --timeout: $DIFF_TIMEOUT" >&2
        exit 2
    fi
fi

if [ -z "$OUR_LUA" ]; then
    OUR_LUA="$(dellingr_bin_built release)" || exit 1
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
        "$TIMEOUT_BIN" "$DIFF_TIMEOUT" "$@"
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
    # 124 is timeout(1)'s "timed out" status. The CLI's handled paths use 0 or
    # 1; unhandled panics and aborts are also failures, but do not use 124.
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

    if [ "$references_checked" -eq 0 ]; then
        for index in "${!REFERENCE_LUAS[@]}"; do
            tool="${REFERENCE_LUAS[$index]}"
            if ! command -v "$tool" >/dev/null 2>&1; then
                echo "missing reference ${REFERENCE_LUA_LABELS[$index]} executable: $tool" >&2
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

    if [ "$has_diff" = "1" ]; then
        stale_matches=""
        if diff -q "$OUR_OUT" "$LUA54_OUT" > /dev/null 2>&1; then
            stale_matches="${REFERENCE_LUA_LABELS[1]}"
        fi
        if diff -q "$OUR_OUT" "$LUA52_OUT" > /dev/null 2>&1; then
            if [ -n "$stale_matches" ]; then
                stale_matches+=" and "
            fi
            stale_matches+="${REFERENCE_LUA_LABELS[0]}"
        fi
        if [ -n "$stale_matches" ]; then
            failures+=("FAIL: $name (stale DIFF marker; matches $stale_matches)")
        fi
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
