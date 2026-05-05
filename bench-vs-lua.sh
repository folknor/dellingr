#!/usr/bin/env bash
# Bench-vs-Lua: wall-clock comparison against reference Lua 5.2 / 5.4.
# Requires hyperfine, lua5.2, lua5.4 on PATH. Builds dellingr release binary
# before running.
#
# Usage: ./bench-vs-lua.sh                  # runs the curated default set
#        ./bench-vs-lua.sh script1.lua ...  # runs the given scripts
#        ./bench-vs-lua.sh --dry-run [...]  # validate inputs, don't run

set -u

DRY_RUN=0
if [ "${1:-}" = "--dry-run" ]
then
    shift
    DRY_RUN=1
fi

DELLINGR="${DELLINGR:-target/release/dellingr}"

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
        examples/fields/same_obj_read.lua
    )
fi

# Validate before doing anything expensive. Cheap; saves a 30s wait on a typo.
errors=()
for tool in hyperfine lua5.2 lua5.4
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
    printf 'dry-run OK: %d script(s), 3 lua versions, all tools present\n' "${#scripts[@]}"
    exit 0
fi

cargo build --release --quiet

# Header block: host + commit context, so pasted results are self-locating.
echo "==== bench-vs-lua start ===="
echo "  host:   $(hostname -s)"
echo "  date:   $(date -Iseconds)"
echo "  commit: $(git rev-parse HEAD 2>/dev/null)"
echo "  branch: $(git rev-parse --abbrev-ref HEAD 2>/dev/null)"
echo "  dirty:  $(git status --porcelain 2>/dev/null | wc -l) file(s)"
echo "============================"

# Collect failures rather than aborting on first - one bad script shouldn't
# wipe out the rest of the run.
failures=()
START=$(date +%s)
for script in "${scripts[@]}"
do
    printf '\n=== %s ===\n' "$script"
    if ! hyperfine --warmup 2 -N \
        -n dellingr "$DELLINGR $script" \
        -n lua5.4 "lua5.4 $script" \
        -n lua5.2 "lua5.2 $script"
    then
        failures+=("$script")
    fi
done
ELAPSED=$(($(date +%s) - START))

echo ""
echo "==== bench-vs-lua end ===="
printf '  elapsed: %ds\n' "$ELAPSED"
printf '  ran:     %d, failed: %d\n' "${#scripts[@]}" "${#failures[@]}"
if [ "${#failures[@]}" -gt 0 ]
then
    printf '  - %s\n' "${failures[@]}"
    exit 1
fi
