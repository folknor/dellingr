#!/usr/bin/env bash
# Shared build and command-line helpers for the repository scripts.
#
# Script environment overrides:
#   DELLINGR                    use an existing dellingr binary
#   DELLINGR_SKIP_BUILD         diff_test.sh requires DELLINGR when set to 1
#   DELLINGR_DIFF_TIMEOUT       per-process timeout for diff_test.sh (default 5s)
#   LUA52, LUA54                reference Lua executable paths
#   HYPERFINE_WARMUP            hyperfine warmup count (default 2)
#   DELLINGR_HOTPATH_FEATURES   hotbench features; unset defaults to hotpath,
#                               empty disables features
# DELLINGR_TEST_LIMIT_KB is consumed by test_limited.sh. BROKKR_MARKER_FIFO is
# consumed by the hotpath harness and brokkr.

: "${DELLINGR_SKIP_BUILD:=0}"
: "${DELLINGR_DIFF_TIMEOUT:=5s}"
: "${HYPERFINE_WARMUP:=2}"
LUA52="${LUA52:-lua5.2}"
LUA54="${LUA54:-lua5.4}"
REFERENCE_LUAS=("$LUA52" "$LUA54")
REFERENCE_LUA_LABELS=("Lua 5.2" "Lua 5.4")
REFERENCE_LUA_OVERRIDES=(LUA52 LUA54)
DELLINGR_HOTPATH_FEATURES="${DELLINGR_HOTPATH_FEATURES-hotpath}"

hyperfine_warmup() {
    if [ "$#" -ne 0 ]; then
        echo "usage: hyperfine_warmup" >&2
        return 2
    fi
    if [[ ! "$HYPERFINE_WARMUP" =~ ^[0-9]+$ ]]; then
        echo "invalid HYPERFINE_WARMUP: $HYPERFINE_WARMUP (expected a nonnegative integer)" >&2
        return 2
    fi
    printf '%s\n' "$HYPERFINE_WARMUP"
}

cargo_artifact() {
    if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then
        echo "usage: cargo_artifact <debug|release> <name> <bin|example> [features]" >&2
        return 2
    fi

    local profile="$1"
    local target_name="$2"
    local target_kind="$3"
    local features="${4:-}"
    local artifact
    local -a cargo_args

    if ! command -v python3 >/dev/null 2>&1; then
        echo "missing tool: python3 (needed to read Cargo's artifact path)" >&2
        return 1
    fi

    cargo_args=(build --message-format=json-render-diagnostics)
    case "$profile" in
        debug) ;;
        release) cargo_args+=(--release) ;;
        *)
            echo "unsupported build profile: $profile" >&2
            return 2
            ;;
    esac
    case "$target_kind" in
        bin) cargo_args+=(--bin "$target_name") ;;
        example) cargo_args+=(--example "$target_name") ;;
        *)
            echo "unsupported Cargo target kind: $target_kind" >&2
            return 2
            ;;
    esac
    if [ -n "$features" ]; then
        cargo_args+=(--features "$features")
    fi

    if ! artifact=$(
        set -o pipefail
        cargo "${cargo_args[@]}" |
            python3 -c '
import json
import sys

target_name, target_kind = sys.argv[1:]
artifact_path = None
for line in sys.stdin:
    try:
        event = json.loads(line)
    except ValueError:
        continue
    reason = event.get("reason")
    if reason == "compiler-message":
        rendered = event.get("message", {}).get("rendered")
        if rendered:
            sys.stderr.write(rendered)
        continue
    target = event.get("target", {})
    if (
        reason == "compiler-artifact"
        and target.get("name") == target_name
        and target_kind in target.get("kind", [])
        and event.get("executable")
    ):
        artifact_path = event["executable"]

if artifact_path is None:
    sys.exit(1)
print(artifact_path)
' "$target_name" "$target_kind"
    ); then
        echo "failed to build Cargo target: $target_kind $target_name ($profile)" >&2
        return 1
    fi

    if [ -z "$artifact" ]; then
        echo "Cargo did not report an executable for $target_kind $target_name" >&2
        return 1
    fi
    printf '%s\n' "$artifact"
}

dellingr_bin_built() {
    if [ "$#" -ne 1 ]; then
        echo "usage: dellingr_bin_built <debug|release>" >&2
        return 2
    fi
    cargo_artifact "$1" dellingr bin
}

dellingr_bin() {
    if [ "$#" -ne 1 ]; then
        echo "usage: dellingr_bin <debug|release>" >&2
        return 2
    fi
    if [ -n "${DELLINGR:-}" ]; then
        printf '%s\n' "$DELLINGR"
        return 0
    fi
    dellingr_bin_built "$1"
}

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
