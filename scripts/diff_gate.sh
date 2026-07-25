#!/usr/bin/bash
# Differential gate for `brokkr check`'s script-check phase.
#
# Same comparison as diff_test.sh (every examples/**/*.lua vs reference Lua 5.2
# and 5.4), but built in DEBUG so the gate stays cheap - it runs on every
# `brokkr check`, and a release LTO build each time would dominate. Debug vs
# release does not change the behavior under test (identical VM semantics), only
# speed. Reuses diff_test.sh via its DELLINGR / DELLINGR_SKIP_BUILD env hooks so
# there is a single source of truth for the comparison logic.
#
# Reference Lua (lua5.2 AND lua5.4 on PATH) is a required dellingr dev
# dependency; a host without them is a broken environment, so this fails loudly
# rather than skipping. Emits diff_test.sh's own "ok" / "FAIL: <path>" sentinel
# on stdout - the script_check in brokkr.toml matches the last line against
# "ok".
set -u

# The debug binary path is repo-relative, so anchor to the repo root regardless
# of where this was invoked from (brokkr runs it from there, but the sibling
# script lookup below should not depend on that).
cd "$(dirname "$0")/.." || exit 1

if ! cargo build --quiet
then
    echo "FAIL: debug build"
    exit 1
fi

exec env DELLINGR=./target/debug/dellingr DELLINGR_SKIP_BUILD=1 ./scripts/diff_test.sh "$@"
