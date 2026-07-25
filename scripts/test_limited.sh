#!/usr/bin/bash
# Limit virtual memory to 2GB to prevent runaway memory usage during testing
ulimit -v 2097152
exec "$@"
