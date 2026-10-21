#!/usr/bin/env bash
# Bench-vs-Lua: wall-clock comparison against reference Lua 5.2 / 5.4 / 5.5
# and LuaJIT. Requires hyperfine, lua5.2, lua5.4, lua5.5, luajit, jq on PATH.
# Builds dellingr release binary before running. Wraps hyperfine to extract
# one-line summaries instead of dumping its full verbose tables.
#
# Usage: ./scripts/bench.sh                  # runs the curated default set
#        ./scripts/bench.sh script1.lua ...  # runs the given scripts
#        ./scripts/bench.sh --dry-run [...]  # validate inputs, don't run

set -u

# Script paths below are repo-relative, so anchor to the repo root regardless of
# where this was invoked from.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
cd "$SCRIPT_DIR/.." || exit 1
. "$SCRIPT_DIR/lib.sh"

DRY_RUN=0
if [ "${1:-}" = "--dry-run" ]
then
    shift
    DRY_RUN=1
fi

reference_luas=(lua5.5 lua5.4 lua5.2 luajit)

if [ $# -gt 0 ]
then
    scripts=("$@")
else
    scripts=()
    default_scripts="$(workload_hotpath_files)" || exit 1
    while IFS= read -r script
    do
        if [ -n "$script" ]; then
            scripts+=("$script")
        fi
    done <<< "$default_scripts"
fi

# Validate before doing anything expensive.
errors=()
if [ "${#scripts[@]}" -eq 0 ]
then
    errors+=("no benchmark scripts found")
fi
for tool in hyperfine "${reference_luas[@]}" jq python3
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
    printf 'dry-run OK: %d script(s), %d Lua implementations, all tools present\n' \
        "${#scripts[@]}" "$(( ${#reference_luas[@]} + 1 ))"
    exit 0
fi

DELLINGR_BIN="$(dellingr_bin_built release)" || exit 1
if [ ! -x "$DELLINGR_BIN" ]; then
    echo "dellingr binary is not executable: $DELLINGR_BIN" >&2
    exit 1
fi
DELLINGR_COMMAND="$(shell_quote "$DELLINGR_BIN")"

echo "==== bench start ===="
echo "  host:   $(hostname -s)"
echo "  date:   $(date '+%Y-%m-%dT%H:%M:%S%z')"
echo "  commit: $(git rev-parse HEAD 2>/dev/null)"
echo "  branch: $(git rev-parse --abbrev-ref HEAD 2>/dev/null)"
echo "  dirty:  $(git status --porcelain 2>/dev/null | wc -l) file(s)"
echo "====================="
echo ""
printf '%-42s %10s' "bench" "dellingr"
for reference_lua in "${reference_luas[@]}"
do
    printf ' %10s' "vs $reference_lua"
done
printf '\n'
printf '%-42s %10s' "-----" "--------"
for reference_lua in "${reference_luas[@]}"
do
    printf ' %10s' "---------"
done
printf '\n'

failures=()
START=$(date +%s)
run_id="$(date '+%Y%m%dT%H%M%S%z')-$$"
results_dir=".scratch/bench-results/$run_id"
if ! mkdir -p "$results_dir"
then
    echo "cannot create benchmark results directory: $results_dir" >&2
    exit 1
fi

# Benchmark run files are scratch output; retain only the ten newest runs.
prune_bench_results() {
    local results_root="$1"
    local keep="$2"
    local count=0
    local modified name
    while read -r modified name
    do
        if [[ ! "$name" =~ ^[0-9]{8}T[0-9]{6}[+-][0-9]{4}-[0-9]+$ ]]
        then
            continue
        fi
        count=$((count + 1))
        if [ "$count" -gt "$keep" ]
        then
            if ! rm -rf -- "$results_root/$name"
            then
                echo "cannot remove old benchmark results: $results_root/$name" >&2
                return 1
            fi
        fi
    done < <(find "$results_root" -mindepth 1 -maxdepth 1 -type d -printf '%T@ %f\n' | sort -rn)
}

if ! prune_bench_results ".scratch/bench-results" 10
then
    exit 1
fi

run_index=0
for script in "${scripts[@]}"
do
    run_index=$((run_index + 1))
    json_file="$results_dir/$run_index.json"
    printf '%s\t%s\n' "$run_index" "$script" >> "$results_dir/scripts.tsv"
    SCRIPT_COMMAND="$(shell_quote "$script")"
    benchmark_commands=(-n dellingr "$DELLINGR_COMMAND $SCRIPT_COMMAND")
    for reference_lua in "${reference_luas[@]}"
    do
        benchmark_commands+=(-n "$reference_lua" "$reference_lua $SCRIPT_COMMAND")
    done
    if ! hyperfine --warmup 2 -N \
        --export-json "$json_file" \
        "${benchmark_commands[@]}" \
        >/dev/null 2>&1
    then
        failures+=("$script")
        printf '%-42s %10s' "$script" "FAIL"
        for reference_lua in "${reference_luas[@]}"
        do
            printf ' %10s' "-"
        done
        printf '\n'
        continue
    fi

    summary=$(jq -r '
        ([.results[] | select(.command == "dellingr").mean] | first) as $d |
        [
            ((($d * 1000 + 0.5) | floor | tostring) + "ms"),
            (.results[] | select(.command != "dellingr") |
                (((($d / .mean) * 100 + 0.5) | floor / 100 | tostring) + "x"))
        ] | join(" ")
    ' "$json_file")
    values=()
    read -r -a values <<< "$summary"
    if [ "${#values[@]}" -ne "$(( ${#reference_luas[@]} + 1 ))" ]
    then
        failures+=("$script")
        printf '%-42s %10s' "$script" "FAIL"
        for reference_lua in "${reference_luas[@]}"
        do
            printf ' %10s' "-"
        done
        printf '\n'
        continue
    fi
    printf '%-42s %10s' "$script" "${values[0]}"
    for index in "${!reference_luas[@]}"
    do
        printf ' %10s' "${values[$((index + 1))]}"
    done
    printf '\n'
done
ELAPSED=$(($(date +%s) - START))

echo ""
echo "==== bench end ===="
printf '  elapsed: %ds\n' "$ELAPSED"
printf '  ran:     %d, failed: %d\n' "${#scripts[@]}" "${#failures[@]}"
printf '  results: %s\n' "$results_dir"
if [ "${#failures[@]}" -gt 0 ]
then
    printf '  - %s\n' "${failures[@]}"
    exit 1
fi
