#!/usr/bin/env bash
# Apply a virtual-memory limit before running a command.
# The limit is TEST_LIMIT_KB in lib.sh. This is a transparent exec wrapper, so
# it does not change the caller's working directory.
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
if ! ulimit -v "$TEST_LIMIT_KB" 2>/dev/null; then
    echo "cannot set virtual-memory limit to: $TEST_LIMIT_KB KiB" >&2
    exit 2
fi
exec "$@"
