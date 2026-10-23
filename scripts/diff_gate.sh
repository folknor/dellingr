#!/usr/bin/env bash
# Optional debug-build wrapper for the differential corpus.
#
# `brokkr check` runs the corpus through `tests/diff_test.rs` for each feature
# shape. This wrapper supports direct runs with a DEBUG binary. VM semantics are
# the same, but the default per-process timeout can make a slow debug run time
# out where release passes; pass `--timeout` to change the threshold.
#
# The lua5.2 and lua5.4 executables are required for unmarked examples.
# Forwards diff_test.sh's terse output for direct use.

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
cd "$SCRIPT_DIR/.." || exit 1
. "$SCRIPT_DIR/lib.sh"

DEBUG_BIN="$(dellingr_bin_built debug)" || exit 1
exec bash "$SCRIPT_DIR/diff_test.sh" --bin "$DEBUG_BIN" "$@"
