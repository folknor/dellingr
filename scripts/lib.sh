#!/usr/bin/env bash
# Shared build and command-line helpers for the repository scripts.
#
# The scripts read no environment variables of their own; their tunables are
# the constants below, and per-run choices are command-line flags. The hotpath
# crate consumes HOTPATH_OUTPUT_PATH set by brokkr, and the harness reads
# BROKKR_MARKER_FIFO, also set by brokkr.

# Per-process timeout for manual diff_test.sh runs, in timeout(1) syntax.
readonly DEFAULT_DIFF_TIMEOUT=5s
# Virtual-memory cap applied by test_limited.sh, in KiB (2 GiB). The example
# runner reads this exact line (tests/integration/support/process.rs), so keep
# it a bare `readonly TEST_LIMIT_KB=<integer>` assignment.
readonly TEST_LIMIT_KB=2097152
readonly HYPERFINE_WARMUP=2
REFERENCE_LUAS=("lua5.2" "lua5.4")
REFERENCE_LUA_LABELS=("Lua 5.2" "Lua 5.4")

hyperfine_warmup() {
    if [ "$#" -ne 0 ]; then
        echo "usage: hyperfine_warmup" >&2
        return 2
    fi
    printf '%s\n' "$HYPERFINE_WARMUP"
}

brokkr_artifact() {
    if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then
        echo "usage: brokkr_artifact <debug|release> <name> <bin|example> [features]" >&2
        return 2
    fi

    local profile="$1"
    local target_name="$2"
    local target_kind="$3"
    local features="${4:-}"
    local artifact
    local -a brokkr_args

    if ! command -v brokkr >/dev/null 2>&1; then
        echo "missing tool: brokkr (needed to build $target_kind $target_name)" >&2
        return 1
    fi

    brokkr_args=(build "$target_name")
    case "$profile" in
        debug) brokkr_args+=(--debug) ;;
        release) brokkr_args+=(--release) ;;
        *)
            echo "unsupported build profile: $profile" >&2
            return 2
            ;;
    esac
    case "$target_kind" in
        bin|example) ;;
        *)
            echo "unsupported target kind: $target_kind" >&2
            return 2
            ;;
    esac
    if [ -n "$features" ]; then
        brokkr_args+=(-F "$features")
    fi

    if ! artifact="$(brokkr "${brokkr_args[@]}")"; then
        echo "failed to build target: $target_kind $target_name ($profile)" >&2
        return 1
    fi

    if [ -z "$artifact" ]; then
        echo "brokkr did not print an executable for $target_kind $target_name" >&2
        return 1
    fi
    printf '%s\n' "$artifact"
}

dellingr_bin_built() {
    if [ "$#" -ne 1 ]; then
        echo "usage: dellingr_bin_built <debug|release>" >&2
        return 2
    fi
    brokkr_artifact "$1" dellingr bin
}

# This is a curated benchmark subset from brokkr.toml, not an examples corpus walk.
workload_hotpath_files() {
    if [ "$#" -gt 1 ]; then
        echo "usage: workload_hotpath_files [workload-name]" >&2
        return 2
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        echo "missing tool: python3 (needed to read brokkr.toml)" >&2
        return 1
    fi

    python3 - "$@" <<'PY'
import sys

if sys.version_info < (3, 11):
    print("Python 3.11 or newer is required to read brokkr.toml", file=sys.stderr)
    raise SystemExit(1)

import tomllib
from pathlib import Path

try:
    with Path("brokkr.toml").open("rb") as registry_file:
        registry = tomllib.load(registry_file)
except OSError as error:
    print(f"cannot read brokkr.toml: {error}", file=sys.stderr)
    raise SystemExit(1)
except tomllib.TOMLDecodeError as error:
    print(f"cannot parse brokkr.toml: {error}", file=sys.stderr)
    raise SystemExit(1)

workloads = registry.get("dellingr", {}).get("workloads")
if not isinstance(workloads, dict) or not workloads:
    print("no workloads found in brokkr.toml", file=sys.stderr)
    raise SystemExit(1)

def hotpath_file(name, workload):
    if not isinstance(workload, dict):
        print(f"invalid workload entry in brokkr.toml: {name}", file=sys.stderr)
        raise SystemExit(1)
    path = workload.get("hotpath_file")
    if not isinstance(path, str):
        print(f"workload has no hotpath_file: {name}", file=sys.stderr)
        raise SystemExit(1)
    parts = path.split("/")
    if (
        not path.startswith("examples/")
        or not path.endswith(".lua")
        or any(part in ("", ".", "..") for part in parts)
    ):
        print(
            f"workload hotpath_file must be a Lua file under examples/: {name}",
            file=sys.stderr,
        )
        raise SystemExit(1)
    return path

if len(sys.argv) == 1:
    for name, workload in workloads.items():
        print(hotpath_file(name, workload))
else:
    name = sys.argv[1]
    if name not in workloads:
        print(f"unknown workload: {name}", file=sys.stderr)
        print("available workloads: " + ", ".join(workloads), file=sys.stderr)
        raise SystemExit(1)
    print(hotpath_file(name, workloads[name]))
PY
}

shell_quote() {
    if [ "$#" -ne 1 ]; then
        echo "usage: shell_quote <argument>" >&2
        return 2
    fi
    printf '%q' "$1"
}

# Regular files only, in C-locale byte order (the order of the Rust corpus
# list in tests/common/examples.rs).
sorted_lua_files() {
    if [ "$#" -ne 1 ]; then
        echo "usage: sorted_lua_files <directory>" >&2
        return 2
    fi

    find "$1" -type f -name '*.lua' -print | LC_ALL=C sort
}
