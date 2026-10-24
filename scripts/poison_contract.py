#!/usr/bin/env python3
"""Check public State receivers for an entry-time poison refusal.

This is a lexical guard, not a proof of arbitrary control flow. New fallible
entry points must start with a check or a known checked boundary. Infallible
host configuration and diagnostic methods are explicitly reviewed below.
All feature configurations are scanned, using native_calls' Rust lexer.
"""

import re
import sys

import native_calls as rust


# These methods cannot return StatePoisoned. They expose host configuration,
# counters or deliberately safe observations, rather than executing Lua.
SAFE_INFALLIBLE = {
    "set_rng_seed", "set_cost_budget", "cost_used", "cost_remaining",
    "set_user_data", "user_data", "user_data_mut", "clear_user_data",
    "callbacks_mut", "replace_callbacks", "object_count", "string_count",
    "heap_size", "gc_should_run", "gc_threshold", "gc_set_threshold",
    "gc_set_policy", "gc_set_auto_threshold", "gc_disable_auto",
    "current_source", "seal_globals", "globals_sealed", "is_poisoned",
    "error", "get_top", "to_boolean", "typ", "raw_equal", "release_anchor",
    "anchor_type", "anchor_count",
    # Collection explicitly becomes a no-op after poisoning.
    "gc_collect",
    # Debug-only instrumentation controls, not script execution.
    "arm_fault_injection", "finish_fault_injection",
}

DIRECT = re.compile(r"self\s*\.\s*check_poisoned\s*\(\s*\)\s*\?\s*;")
SAVE_REFUSAL = re.compile(
    r"if\s+self\s*\.\s*poisoned\s*\{\s*return\s+Err\s*\(\s*"
    r"(?:save_state\s*::\s*)?SaveError\s*::\s*StatePoisoned\s*\)\s*;\s*\}")
GUARDED = re.compile(r"(?:return\s+)?self\s*\.\s*guarded\s*\(\s*\|state\|")
STRING_CALL = r"self\s*\.\s*tostring_bytes\s*\(\s*idx\s*\)"
# The lexer blanks the wrapper's edge-id string. Only this known macro may
# precede the checked renderer; arbitrary wrappers can run other work first.
STRING_DELEGATE = re.compile(
    r"let\s+raw\s*=\s*(?:" + STRING_CALL
    + r"|crate\s*::\s*native_call!\s*\(\s*,\s*" + STRING_CALL
    + r"\s*,?\s*\))\s*\?\s*;")

RECEIVER = re.compile(r"\(\s*(?:&\s*(?:'\w+\s+)?)?(?:mut\s+)?self\b")


def public_receiver(fn):
    """Exclude restricted visibility, constructors and unrelated impls."""
    prefix = fn.src.code[:fn.start]
    return (fn.owner == "State"
            and re.search(r"\bpub\s+(?:(?:async|unsafe|const)\s+)*$", prefix)
            and RECEIVER.search(fn.sig))


def returns_result(fn):
    """Read the outer return type, not a callback's return type."""
    receiver = RECEIVER.search(fn.sig)
    depth = 0
    for offset in range(receiver.start(), len(fn.sig)):
        token = fn.sig[offset]
        if token == "(":
            depth += 1
        elif token == ")":
            depth -= 1
            if depth == 0:
                return bool(re.match(
                    r"\s*->\s*(?:[\w]+\s*::\s*)*Result\s*<", fn.sig[offset + 1:]))
    return False


def check_sources(sources, safe_infallible=SAFE_INFALLIBLE):
    problems = []
    methods = [fn for src in sources for fn in src.fns if public_receiver(fn)]
    definitions = rust.definitions(sources)

    def body(fn):
        return fn.src.code[fn.body_open + 1:fn.body_close].lstrip()

    def checked_boundary(name):
        found = definitions.get(("State", name), [])
        return bool(found) and all(DIRECT.match(body(fn)) for fn in found)

    for fn in methods:
        where = f"{fn.src.rel}:{fn.line}: {fn.qual}"
        if not returns_result(fn):
            if fn.name not in safe_infallible:
                problems.append(f"{where}: review this infallible poisoned-State API")
            continue
        entry = body(fn)
        if DIRECT.match(entry) or SAVE_REFUSAL.match(entry):
            continue
        if GUARDED.match(entry) and checked_boundary("guarded"):
            continue
        # This conversion's first operation propagates the checked renderer's
        # error. Do not accept arbitrary calls or arbitrary with_* helpers.
        if fn.name == "to_string_with_meta" and STRING_DELEGATE.match(entry):
            renderers = definitions.get(("State", "tostring_bytes"), [])
            if renderers and all(
                DIRECT.match(body(renderer))
                or (GUARDED.match(body(renderer)) and checked_boundary("guarded"))
                for renderer in renderers
            ):
                continue
        problems.append(f"{where}: must refuse poisoning before other work")
    if not methods:
        problems.append("no public State receiver methods found")
    return problems


def main():
    if sys.argv[1:]:
        sys.exit(f"usage: {sys.argv[0]}")
    problems = check_sources(rust.production_sources())
    for problem in problems:
        print(problem)
    if problems:
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
