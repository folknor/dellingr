#!/usr/bin/env bash
# Bench-vs-Lua: wall-clock comparison against omniLua (a pure-Rust Lua,
# https://github.com/ianm199/omnilua, in its default 5.4 mode), reference Lua
# 5.5 / 5.4 / 5.2 and LuaJIT, run on request to regenerate the README's
# comparison table (it is not how dellingr's own performance is measured; that
# goes through `brokkr dellingr`). Each run records host, commits and dirty
# state in its banner and in provenance.txt beside the JSON. Requires
# hyperfine, lua5.2, lua5.4, lua5.5, luajit, jq on PATH, plus an omniLua
# release binary (path in OMNILUA below) built from a local clone in the
# gitignored research/lua-rs:
#   cargo build --release -p omnilua-cli --manifest-path research/lua-rs/Cargo.toml
# The README footnote names the omniLua commit of its capture, so update the
# clone and the footnote together. Builds
# dellingr release binary before running. Wraps hyperfine to extract one-line
# summaries instead of dumping its full verbose tables. A comparator that
# cannot run a script (one launch, nonzero exit) is left out of that script's
# hyperfine run and shown as n/a.
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

OMNILUA="research/lua-rs/target/release/omnilua"
reference_luas=(lua5.5 lua5.4 lua5.2 luajit)
# Comparator names (hyperfine command names and column order) and commands.
comparators=(omnilua "${reference_luas[@]}")
comparator_commands=("$OMNILUA" "${reference_luas[@]}")

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
[ -x "$OMNILUA" ] || errors+=("missing omniLua binary: $OMNILUA")
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
        "${#scripts[@]}" "$(( ${#comparators[@]} + 1 ))"
    exit 0
fi

HYPERFINE_WARMUP_COUNT="$(hyperfine_warmup)" || exit 2

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
echo "  omnilua: $(git -C research/lua-rs rev-parse HEAD 2>/dev/null)"
echo "====================="
echo ""
printf '%-42s %10s' "bench" "dellingr"
for comparator in "${comparators[@]}"
do
    printf ' %10s' "vs $comparator"
done
printf '\n'
printf '%-42s %10s' "-----" "--------"
for comparator in "${comparators[@]}"
do
    printf ' %10s' "---------"
done
printf '\n'

failures=()
failure_indices=()
START=$(date +%s)
run_id="$(date '+%Y%m%dT%H%M%S%z')-$$"
results_dir=".scratch/bench-results/$run_id"
if ! mkdir -p "$results_dir"
then
    echo "cannot create benchmark results directory: $results_dir" >&2
    exit 1
fi

# Provenance for the saved results: the banner above scrolls away, so the
# commit and host also sit next to the per-script JSON.
{
    echo "host:   $(hostname -s)"
    echo "date:   $(date '+%Y-%m-%dT%H:%M:%S%z')"
    echo "commit: $(git rev-parse HEAD 2>/dev/null)"
    echo "dirty:  $(git status --porcelain 2>/dev/null | wc -l) file(s)"
    echo "omnilua: $(git -C research/lua-rs rev-parse HEAD 2>/dev/null)"
} > "$results_dir/provenance.txt"

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
    error_file="$results_dir/$run_index.err"
    printf '%s\t%s\n' "$run_index" "$script" >> "$results_dir/scripts.tsv"
    SCRIPT_COMMAND="$(shell_quote "$script")"
    benchmark_commands=("$DELLINGR_COMMAND $SCRIPT_COMMAND")
    # A comparator that cannot run this script (a feature it lacks) is left
    # out of the timing and shown as n/a. hyperfine's JSON lists results in
    # command order, so `positions` maps each comparator to its result index.
    positions=()
    for index in "${!comparators[@]}"
    do
        if "${comparator_commands[$index]}" "$script" >/dev/null 2>&1
        then
            positions+=("${#benchmark_commands[@]}")
            benchmark_commands+=("$(shell_quote "${comparator_commands[$index]}") $SCRIPT_COMMAND")
        else
            positions+=("")
        fi
    done
    if ! hyperfine --warmup "$HYPERFINE_WARMUP_COUNT" -N \
        --export-json "$json_file" \
        "${benchmark_commands[@]}" \
        >/dev/null 2>"$error_file"
    then
        failures+=("$script")
        failure_indices+=("$run_index")
        printf '%-42s %10s' "$script" "FAIL"
        for comparator in "${comparators[@]}"
        do
            printf ' %10s' "-"
        done
        printf '\n'
        continue
    fi

    # hyperfine 2's JSON (schema_version 2) keeps the mean wall time under
    # each result's summary.
    dellingr_ms=$(jq -r '.results[0].summary.time_wall_clock.mean * 1000 + 0.5 | floor | tostring + "ms"' "$json_file")
    printf '%-42s %10s' "$script" "$dellingr_ms"
    for position in "${positions[@]}"
    do
        if [ -z "$position" ]
        then
            printf ' %10s' "n/a"
            continue
        fi
        ratio=$(jq -r --argjson i "$position" '
            (.results[0].summary.time_wall_clock.mean / .results[$i].summary.time_wall_clock.mean)
                * 100 + 0.5 | floor / 100 | tostring + "x"
        ' "$json_file")
        printf ' %10s' "$ratio"
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
    for index in "${!failures[@]}"
    do
        error_file="$results_dir/${failure_indices[$index]}.err"
        if [ -s "$error_file" ]; then
            printf '  hyperfine diagnostic for %s:\n' "${failures[$index]}" >&2
            sed 's/^/    /' "$error_file" >&2
        else
            printf '  hyperfine reported failure for %s without diagnostic output\n' "${failures[$index]}" >&2
        fi
    done
    exit 1
fi
