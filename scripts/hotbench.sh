#!/usr/bin/env bash
# Hotpath bench: phase KVs from the hotpath harness + hyperfine wall-time
# from the regular dellingr binary on the same target's standalone runner.
#
# Usage: ./scripts/hotbench.sh <workload-name> [hyperfine-args...]
#   The workload name is resolved to its hotpath_file in brokkr.toml.
#
# Why two binaries:
#   The hotpath feature instruments every traced fn and prints a stats
#   table at exit; that overhead skews wall-time measurements. We use the
#   hotpath harness only for the KV breakdown (parse/cold/warm phasing)
#   and the regular dellingr binary on the bench's standalone runner for
#   the hyperfine-driven wall time.
#
# Env:
#   DELLINGR_HOTPATH_FEATURES=hotpath-alloc  # KV side; empty disables features
#   HYPERFINE_WARMUP=2                       # wall-time warmup calls

set -u

# Binary and example paths below are repo-relative, so anchor to the repo root
# regardless of where this was invoked from.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
cd "$SCRIPT_DIR/.." || exit 1
. "$SCRIPT_DIR/lib.sh"

if [ $# -lt 1 ]
then
    echo "usage: $0 <workload-name> [hyperfine-args...]" >&2
    exit 2
fi

TARGET="$1"
shift
HYPERFINE_WARMUP_COUNT="$(hyperfine_warmup)" || exit 2
SCRIPT="$(workload_hotpath_files "$TARGET")" || exit 1

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

DELLINGR_BIN="$(dellingr_bin_built release)" || exit 1
HOTPATH_BIN="$(cargo_artifact release hotpath example "$DELLINGR_HOTPATH_FEATURES")" || exit 1

if [ ! -x "$DELLINGR_BIN" ] || [ ! -x "$HOTPATH_BIN" ]
then
    echo "build did not produce expected binaries" >&2
    exit 1
fi
DELLINGR_COMMAND="$(shell_quote "$DELLINGR_BIN")"
SCRIPT_COMMAND="$(shell_quote "$SCRIPT")"

echo "==== harness output ===="
# Everything the harness emits: the phase KVs on stderr, and the hotpath
# crate's own per-function stats table (p50/p95/p99 over every
# #[hotpath::measure] site) at exit. The per-function table is the reason the
# annotations exist - do not filter it out.
"$HOTPATH_BIN" "$SCRIPT" 2>&1

echo ""
echo "==== hyperfine ===="
hyperfine --warmup "$HYPERFINE_WARMUP_COUNT" -N "$@" -n "$TARGET" "$DELLINGR_COMMAND $SCRIPT_COMMAND"
