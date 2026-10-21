#!/usr/bin/env bash
# Apply a virtual-memory limit before running a command where `ulimit -v` works.
# The default is 2 GiB; set DELLINGR_TEST_LIMIT_KB=unlimited to disable it.
if [ "${DELLINGR_TEST_LIMIT_KB+x}" = x ]; then
    test_limit_explicit=true
else
    DELLINGR_TEST_LIMIT_KB=2097152
    test_limit_explicit=false
fi
if [ "$DELLINGR_TEST_LIMIT_KB" = unlimited ]; then
    exec "$@"
fi
if [ -z "$DELLINGR_TEST_LIMIT_KB" ]; then
    echo "DELLINGR_TEST_LIMIT_KB must be a ulimit -v value in KiB" >&2
    exit 2
fi
if ! ulimit -v "$DELLINGR_TEST_LIMIT_KB" 2>/dev/null; then
    if [ "$test_limit_explicit" = true ]; then
        echo "cannot set virtual-memory limit to: $DELLINGR_TEST_LIMIT_KB KiB" >&2
        exit 2
    fi
fi
exec "$@"
