#!/usr/bin/env bash
# Hotpath bench: phase KVs from the hotpath harness + hyperfine wall-time
# from the regular dellingr binary on the same target's standalone runner.
#
# Usage: ./hotbench.sh <category/name> [hyperfine-args...]
#   ./hotbench.sh tables/numeric_index
#   ./hotbench.sh tables/numeric_index --runs 20
#
# Why two binaries:
#   The hotpath feature instruments every traced fn and prints a stats
#   table at exit; that overhead skews wall-time measurements. We use the
#   hotpath harness only for the KV breakdown (parse/cold/warm phasing)
#   and the regular dellingr binary on the bench's standalone runner for
#   the hyperfine-driven wall time.
#
# Env:
#   FEATURES=hotpath  # KV side; switch to hotpath-alloc for alloc tracking

set -u

if [ $# -lt 1 ]
then
    echo "usage: $0 <category/name> [hyperfine-args...]" >&2
    exit 2
fi

TARGET="$1"
shift
FEATURES="${FEATURES:-hotpath}"
SCRIPT="examples/${TARGET}.lua"
HOTPATH_BIN="target/release/examples/hotpath"
DELLINGR_BIN="target/release/dellingr"

if [ ! -f "$SCRIPT" ]
then
    echo "missing bench script: $SCRIPT" >&2
    exit 1
fi

if ! command -v hyperfine >/dev/null
then
    echo "missing tool: hyperfine" >&2
    exit 1
fi

cargo build --release --quiet
cargo build --release --example hotpath --features "$FEATURES" --quiet

if [ ! -x "$DELLINGR_BIN" ] || [ ! -x "$HOTPATH_BIN" ]
then
    echo "build did not produce expected binaries" >&2
    exit 1
fi

echo "==== KVs ===="
"$HOTPATH_BIN" "$TARGET" 2>&1 \
    | grep -E '^(target|state_new_us|parse_us|cold_call_us|cold_cost|warm_iterations|warm_avg_us|warm_avg_cost|cost_per_us|setup_|final_)='

echo ""
echo "==== hyperfine ===="
hyperfine --warmup 2 -N "$@" -n "$TARGET" "$DELLINGR_BIN $SCRIPT"
