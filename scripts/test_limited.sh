#!/usr/bin/env bash
# Apply a virtual-memory limit before running a command.
# The default is 2 GiB.
if [ "${DELLINGR_TEST_LIMIT_KB+x}" != x ]; then
    DELLINGR_TEST_LIMIT_KB=2097152
fi
if [[ ! "$DELLINGR_TEST_LIMIT_KB" =~ ^[0-9]+$ || "$DELLINGR_TEST_LIMIT_KB" =~ ^0+$ ]]; then
    echo "DELLINGR_TEST_LIMIT_KB must be a positive ulimit -v value in KiB" >&2
    exit 2
fi
if ! ulimit -v "$DELLINGR_TEST_LIMIT_KB" 2>/dev/null; then
    echo "cannot set virtual-memory limit to: $DELLINGR_TEST_LIMIT_KB KiB" >&2
    exit 2
fi
exec "$@"
