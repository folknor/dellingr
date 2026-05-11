#!/usr/bin/bash
# Differential test: dellingr vs reference Lua 5.2 and 5.4.
#
# Walks `examples/**/*.lua` and runs each script through dellingr, lua5.2,
# and lua5.4. A file passes when its dellingr output matches lua5.4 (or
# matches lua5.2 - we accept either of the two reference outputs). Files
# tagged with `-- DIFF: <reason>` may differ from both and still pass.
# Stress tests and benchmark.lua are skipped (long-running).
#
# Env:
#   DELLINGR=/path/to/dellingr    use an already-built binary
#   DELLINGR_SKIP_BUILD=1         do not run cargo build --release first
#   DELLINGR_SKIP_TIMEOUT=1       do not wrap each VM in timeout(1)
#
# Output is intentionally terse: prints "ok" on success, or one
# "FAIL: <path>" line per failing script. Exit 1 on any failure.

set -u

if [ "${DELLINGR_SKIP_BUILD:-0}" != "1" ]
then
    cargo build --release --quiet
fi

OUR_LUA="${DELLINGR:-./target/release/dellingr}"
if [ ! -x "$OUR_LUA" ]
then
    echo "FAIL: build"
    exit 1
fi

OUR_OUT="$(mktemp "${TMPDIR:-/tmp}/dellingr-diff-our.XXXXXX")" || exit 1
LUA52_OUT="$(mktemp "${TMPDIR:-/tmp}/dellingr-diff-lua52.XXXXXX")" || {
    rm -f "$OUR_OUT"
    exit 1
}
LUA54_OUT="$(mktemp "${TMPDIR:-/tmp}/dellingr-diff-lua54.XXXXXX")" || {
    rm -f "$OUR_OUT" "$LUA52_OUT"
    exit 1
}
trap 'rm -f "$OUR_OUT" "$LUA52_OUT" "$LUA54_OUT"' EXIT

shopt -s globstar nullglob
if [ "$#" -gt 0 ]
then
    scripts=()
    for arg in "$@"
    do
        if [ -d "$arg" ]
        then
            for f in "$arg"/**/*.lua
            do
                scripts+=("$f")
            done
        elif [ -f "$arg" ]
        then
            scripts+=("$arg")
        else
            echo "FAIL: missing $arg"
            exit 1
        fi
    done
else
    scripts=(examples/**/*.lua)
fi
failures=()

run_lua() {
    if [ "${DELLINGR_SKIP_TIMEOUT:-0}" = "1" ]
    then
        "$@"
    else
        timeout 5s "$@"
    fi
}

for f in "${scripts[@]}"
do
    name=$(realpath --relative-to=examples "$f")
    case "$name" in
        benchmark.lua|stress_*.lua|upvalue_stress.lua)
            continue
            ;;
    esac

    has_diff=0
    if grep -q "^-- DIFF:" "$f"
    then
        has_diff=1
    fi

    run_lua "$OUR_LUA" --quiet "$f" > "$OUR_OUT" 2>&1
    run_lua lua5.2 "$f" > "$LUA52_OUT" 2>&1
    run_lua lua5.4 "$f" > "$LUA54_OUT" 2>&1

    if diff -q "$OUR_OUT" "$LUA54_OUT" > /dev/null 2>&1
    then
        continue
    fi
    if diff -q "$OUR_OUT" "$LUA52_OUT" > /dev/null 2>&1
    then
        continue
    fi
    if [ "$has_diff" = "1" ]
    then
        continue
    fi
    failures+=("$name")
done

if [ "${#failures[@]}" -eq 0 ]
then
    echo "ok"
else
    for fail in "${failures[@]}"
    do
        echo "FAIL: $fail"
    done
    exit 1
fi
