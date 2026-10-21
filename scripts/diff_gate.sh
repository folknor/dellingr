#!/usr/bin/env bash
# Optional debug-build wrapper for the differential corpus.
#
# `brokkr check` runs the corpus through `tests/diff_test.rs` for each feature
# shape. This wrapper supports direct runs with a DEBUG binary; debug vs release
# does not change the behavior under test (identical VM semantics, only speed).
#
# Reference Lua (lua5.2 AND lua5.4 on PATH) is required for unmarked examples;
# a host without them fails loudly rather than skipping those comparisons.
# Forwards diff_test.sh's terse output for direct use.

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
cd "$SCRIPT_DIR/.." || exit 1
. "$SCRIPT_DIR/lib.sh"

DEBUG_BIN="$(dellingr_bin debug)" || exit 1
exec env DELLINGR="$DEBUG_BIN" DELLINGR_SKIP_BUILD=1 bash "$SCRIPT_DIR/diff_test.sh" "$@"
