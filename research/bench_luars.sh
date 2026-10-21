#!/usr/bin/env bash
# Bench dellingr vs lua-rs (https://github.com/ianm199/lua-rs), both pure-Rust
# Lua VMs, anchored to reference C Lua 5.4 and the LuaJIT floor. Same scripts,
# same host, one hyperfine pass per script. Mirrors ../bench.sh's jq extraction.
#
# Run from the dellingr repo root:  ./research/bench_luars.sh
#
# Requires: hyperfine, jq, lua5.4, luajit on PATH; the dellingr release binary
# and the lua-rs release binary built (see header check below).
#
# Usage: ./research/bench_luars.sh                  # curated default set
#        ./research/bench_luars.sh script1.lua ...  # explicit scripts
#        ./research/bench_luars.sh --dry-run [...]   # validate, don't run

set -u

DRY_RUN=0
if [ "${1:-}" = "--dry-run" ]
then
    shift
    DRY_RUN=1
fi

DELLINGR="${DELLINGR:-target/release/dellingr}"
LUARS="${LUARS:-research/lua-rs/target/release/lua-rs}"
JSON_FILE=".bench-luars.json"

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
        examples/strings/literal_find.lua
        examples/fields/same_obj_read.lua
        examples/alloc/closure.lua
        examples/alloc/record_tables.lua
    )
fi

# Validate before doing anything expensive.
errors=()
for tool in hyperfine jq lua5.4 luajit
do
    if ! command -v "$tool" >/dev/null
    then
        errors+=("missing tool: $tool")
    fi
done
[ -x "$DELLINGR" ] || errors+=("missing/!x dellingr binary: $DELLINGR (build with: cargo build --release)")
[ -x "$LUARS" ]    || errors+=("missing/!x lua-rs binary: $LUARS (build with: cargo build --release --bin lua-rs --manifest-path research/lua-rs/Cargo.toml)")
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
    printf 'dry-run OK: %d script(s), 4 arms (dellingr, lua-rs, lua5.4, luajit)\n' "${#scripts[@]}"
    exit 0
fi

echo "==== bench start ===="
echo "  host:    $(hostname -s)"
echo "  date:    $(date -Iseconds)"
echo "  dellingr:$(git rev-parse --short HEAD 2>/dev/null)"
echo "  lua-rs:  $(git -C research/lua-rs rev-parse --short HEAD 2>/dev/null)"
echo "====================="
echo ""
printf '%-34s %10s %10s %12s %12s %12s\n' "bench" "dellingr" "lua-rs" "lrs/dlgr" "dlgr/lua5.4" "lrs/lua5.4"
printf '%-34s %10s %10s %12s %12s %12s\n' "-----" "--------" "------" "--------" "-----------" "----------"

failures=()
START=$(date +%s)
for script in "${scripts[@]}"
do
    rm -f "$JSON_FILE"
    if ! hyperfine --warmup 2 -N \
        --export-json "$JSON_FILE" \
        -n dellingr "$DELLINGR $script" \
        -n lua-rs "$LUARS $script" \
        -n lua5.4 "lua5.4 $script" \
        -n luajit "luajit $script" \
        >/dev/null 2>&1
    then
        failures+=("$script")
        printf '%-34s %10s %10s %12s %12s %12s\n' "$script" "FAIL" "-" "-" "-" "-"
        continue
    fi

    summary=$(jq -r '
        ([.results[] | select(.command == "dellingr").mean] | first) as $d |
        ([.results[] | select(.command == "lua-rs").mean] | first) as $r |
        ([.results[] | select(.command == "lua5.4").mean] | first) as $l4 |
        "\(($d * 1000 + 0.5) | floor)ms \(($r * 1000 + 0.5) | floor)ms \((($r / $d) * 100 + 0.5 | floor) / 100)x \((($d / $l4) * 100 + 0.5 | floor) / 100)x \((($r / $l4) * 100 + 0.5 | floor) / 100)x"
    ' "$JSON_FILE")
    read -r dms rms lrs_dlgr dlgr_l4 lrs_l4 <<< "$summary"
    printf '%-34s %10s %10s %12s %12s %12s\n' "$script" "$dms" "$rms" "$lrs_dlgr" "$dlgr_l4" "$lrs_l4"
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
