#!/usr/bin/env bash
# Optional debug-build wrapper for the differential corpus.
#
# `brokkr check` runs the corpus through `tests/diff_test.rs` for each feature
# shape. This wrapper supports direct runs with a DEBUG binary; debug vs release
# does not change the behavior under test (identical VM semantics, only speed).
#
# Reference Lua executables are required for unmarked examples. Set LUA52 and
# LUA54 to their paths when their default command names are unavailable.
# Forwards diff_test.sh's terse output for direct use.

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
cd "$SCRIPT_DIR/.." || exit 1
. "$SCRIPT_DIR/lib.sh"

DEBUG_BIN="$(dellingr_bin debug)" || exit 1
exec env DELLINGR="$DEBUG_BIN" DELLINGR_SKIP_BUILD=1 bash "$SCRIPT_DIR/diff_test.sh" "$@"
