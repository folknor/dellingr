#!/usr/bin/env bash
# Limit virtual memory to the configured amount (in KiB) before running a command.
DELLINGR_TEST_LIMIT_KB="${DELLINGR_TEST_LIMIT_KB-2097152}"
if [ -z "$DELLINGR_TEST_LIMIT_KB" ]; then
    echo "DELLINGR_TEST_LIMIT_KB must be a ulimit -v value in KiB" >&2
    exit 2
fi
if ! ulimit -v "$DELLINGR_TEST_LIMIT_KB"; then
    echo "cannot set virtual-memory limit to: $DELLINGR_TEST_LIMIT_KB KiB" >&2
    exit 2
fi
exec "$@"
