#!/usr/bin/env bash
# Differential gate for `brokkr check`'s script-check phase.
#
# Same comparison as diff_test.sh (every examples/**/*.lua vs reference Lua 5.2
# and 5.4), but built in DEBUG so the gate stays cheap - it runs on every
# `brokkr check`, and a release LTO build each time would dominate. Debug vs
# release does not change the behavior under test (identical VM semantics, only
# speed). Reuses diff_test.sh via its DELLINGR / DELLINGR_SKIP_BUILD env hooks.
#
# Reference Lua (lua5.2 AND lua5.4 on PATH) is a required dellingr dev
# dependency; a host without them is a broken environment, so this fails loudly
# rather than skipping. Emits diff_test.sh's own "ok" / failure sentinel on
# stdout; brokkr.toml matches the last line against "ok".

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
cd "$SCRIPT_DIR/.." || exit 1
. "$SCRIPT_DIR/lib.sh"

DEBUG_BIN="$(dellingr_bin debug)" || exit 1
exec env DELLINGR="$DEBUG_BIN" DELLINGR_SKIP_BUILD=1 "$SCRIPT_DIR/diff_test.sh" "$@"
