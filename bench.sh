#!/usr/bin/env bash
# Bench-vs-Lua: wall-clock comparison against reference Lua 5.2 / 5.4 / 5.5
# and LuaJIT. Requires hyperfine, lua5.2, lua5.4, lua5.5, luajit, jq on PATH.
# Builds dellingr release binary before running. Wraps hyperfine to extract
# one-line summaries instead of dumping its full verbose tables.
#
# Usage: ./bench.sh                  # runs the curated default set
#        ./bench.sh script1.lua ...  # runs the given scripts
#        ./bench.sh --dry-run [...]  # validate inputs, don't run

set -u

DRY_RUN=0
if [ "${1:-}" = "--dry-run" ]
then
    shift
    DRY_RUN=1
fi

DELLINGR="${DELLINGR:-target/release/dellingr}"
JSON_FILE=".bench-results.json"

if [ $# -gt 0 ]
then
    scripts=("$@")
else
    scripts=(
        examples/benchmark.lua
        examples/numerics/arithmetic.lua
        examples/iter/pairs.lua
        examples/tables/fill.lua
        examples/strings/mixed.lua
        examples/strings/patterns.lua
        examples/fields/same_obj_read.lua
        examples/alloc/closure.lua
    )
fi

# Validate before doing anything expensive.
errors=()
for tool in hyperfine lua5.2 lua5.4 lua5.5 luajit jq
do
    if ! command -v "$tool" >/dev/null
    then
        errors+=("missing tool: $tool")
    fi
done
for script in "${scripts[@]}"
do
    if [ ! -f "$script" ]
    then
        errors+=("missing script: $script")
    fi
done
if [ "${#errors[@]}" -gt 0 ]
then
    printf '%s\n' "${errors[@]}" >&2
    exit 1
fi

if [ "$DRY_RUN" = "1" ]
then
    printf 'dry-run OK: %d script(s), 5 lua versions, all tools present\n' "${#scripts[@]}"
    exit 0
fi

cargo build --release --quiet

echo "==== bench start ===="
echo "  host:   $(hostname -s)"
echo "  date:   $(date -Iseconds)"
echo "  commit: $(git rev-parse HEAD 2>/dev/null)"
echo "  branch: $(git rev-parse --abbrev-ref HEAD 2>/dev/null)"
echo "  dirty:  $(git status --porcelain 2>/dev/null | wc -l) file(s)"
echo "====================="
echo ""
printf '%-42s %10s %10s %10s %10s %10s\n' "bench" "dellingr" "vs lua5.5" "vs lua5.4" "vs lua5.2" "vs luajit"
printf '%-42s %10s %10s %10s %10s %10s\n' "-----" "--------" "---------" "---------" "---------" "---------"

failures=()
START=$(date +%s)
for script in "${scripts[@]}"
do
    rm -f "$JSON_FILE"
    if ! hyperfine --warmup 2 -N \
        --export-json "$JSON_FILE" \
        -n dellingr "$DELLINGR $script" \
        -n lua5.5 "lua5.5 $script" \
        -n lua5.4 "lua5.4 $script" \
        -n lua5.2 "lua5.2 $script" \
        -n luajit "luajit $script" \
        >/dev/null 2>&1
    then
        failures+=("$script")
        printf '%-42s %10s %10s %10s %10s %10s\n' "$script" "FAIL" "-" "-" "-" "-"
        continue
    fi

    # Extract per-impl mean times and compute ratios entirely inside jq -
    # no piping. Output one line: "<ms> <vs55>x <vs54>x <vs52>x <vsjit>x".
    summary=$(jq -r '
        ([.results[] | select(.command == "dellingr").mean] | first) as $d |
        ([.results[] | select(.command == "lua5.5").mean] | first) as $l5 |
        ([.results[] | select(.command == "lua5.4").mean] | first) as $l4 |
        ([.results[] | select(.command == "lua5.2").mean] | first) as $l2 |
        ([.results[] | select(.command == "luajit").mean] | first) as $jit |
        "\(($d * 1000 + 0.5) | floor)ms \(((($d / $l5) * 100 + 0.5) | floor) / 100)x \(((($d / $l4) * 100 + 0.5) | floor) / 100)x \(((($d / $l2) * 100 + 0.5) | floor) / 100)x \(((($d / $jit) * 100 + 0.5) | floor) / 100)x"
    ' "$JSON_FILE")
    read -r ms vs55 vs54 vs52 vsjit <<< "$summary"
    printf '%-42s %10s %10s %10s %10s %10s\n' "$script" "$ms" "$vs55" "$vs54" "$vs52" "$vsjit"
done
ELAPSED=$(($(date +%s) - START))
rm -f "$JSON_FILE"

echo ""
echo "==== bench end ===="
printf '  elapsed: %ds\n' "$ELAPSED"
printf '  ran:     %d, failed: %d\n' "${#scripts[@]}" "${#failures[@]}"
if [ "${#failures[@]}" -gt 0 ]
then
    printf '  - %s\n' "${failures[@]}"
    exit 1
fi
