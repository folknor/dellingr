#!/usr/bin/bash
# Differential test: dellingr vs reference Lua 5.2 and 5.4.
#
# Walks `examples/**/*.lua` and runs each script through dellingr, lua5.2,
# and lua5.4. A file passes when its dellingr output matches lua5.4 (or
# matches lua5.2 - we accept either of the two reference outputs). Files
# tagged with `-- DIFF: <reason>` may differ from both and still pass.
# Stress tests and benchmark.lua are skipped (long-running).
#
# Output is intentionally terse: prints "ok" on success, or one
# "FAIL: <path>" line per failing script. Exit 1 on any failure.

set -u

cargo build --release --quiet

OUR_LUA="./target/release/dellingr"
if [ ! -x "$OUR_LUA" ]
then
    echo "FAIL: build"
    exit 1
fi

OUR_OUT=".diff_test_our.out"
OUR_RAW=".diff_test_raw.out"
LUA52_OUT=".diff_test_lua52.out"
LUA54_OUT=".diff_test_lua54.out"
trap "rm -f $OUR_OUT $OUR_RAW $LUA52_OUT $LUA54_OUT" EXIT

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

    timeout 5s "$OUR_LUA" "$f" > "$OUR_RAW" 2>&1
    grep -v "^Cost used:" "$OUR_RAW" > "$OUR_OUT"
    timeout 5s lua5.2 "$f" > "$LUA52_OUT" 2>&1
    timeout 5s lua5.4 "$f" > "$LUA54_OUT" 2>&1

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
