#!/usr/bin/env bash
# Inspect the linked release code, including dependencies and LLVM libcalls,
# of the default and snapshot release binaries.
#
# Manual command, not part of `brokkr check`: it builds through `brokkr build`,
# which refuses to run beneath another brokkr command. The gate inspects the
# dev binaries instead (the corpus::libm_symbols integration test).
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
source scripts/lib.sh

for features in '' snapshot; do
    artifact="$(brokkr_artifact release dellingr bin "$features")"
    python3 -B scripts/libm_symbols.py "$artifact"
done
echo ok
