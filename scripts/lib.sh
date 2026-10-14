#!/usr/bin/env bash
# Shared build and command-line helpers for the repository scripts.

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

dellingr_bin() {
    if [ "$#" -ne 1 ]; then
        echo "usage: dellingr_bin <debug|release>" >&2
        return 2
    fi
    if [ -n "${DELLINGR:-}" ]; then
        printf '%s\n' "$DELLINGR"
        return 0
    fi
    cargo_artifact "$1" dellingr bin
}

shell_quote() {
    if [ "$#" -ne 1 ]; then
        echo "usage: shell_quote <argument>" >&2
        return 2
    fi
    printf '%q' "$1"
}
