#!/usr/bin/env python3
"""Check the poison contract of the public State methods.

Two rules, both lexical, read with native_calls' Rust lexer over every
feature configuration (reference/error-reporting.md, "Panics and poisoning",
states them):

1. Entry refusal. Every public fallible State method starts with a check
   that refuses a poisoned State and an unsupported floating-point
   environment on the calling thread, or with a known checked boundary.
   Infallible host configuration and diagnostic methods are explicitly
   reviewed below; they check neither.

2. The execution boundary. A public State method that can run Lua,
   metamethods or host code against its State has exactly one shape: its
   whole body is `self.guarded(|state| ...)`. Which methods those are is
   derived, not chosen: the ones in the caller closure of the execution
   boundaries that native_calls computes, plus the ones whose signature
   hands State-executing code to the method (a closure bound or fn pointer
   taking `&mut Self` or `&mut State`, or a type alias or trait defined in
   src that names one). `guarded` itself appears nowhere else, and only
   src/main.rs, which is host code, calls a guarded method; VM code calls
   the unguarded bodies. The guard turns the swallowed-panic marker into an
   error, which is only correct with no interrupted VM frame between it and
   host code.

This is not a type: a token minted by `guarded` and `invoke_rust_fn` would
have to reach the dispatch loop and every library callback through the
public `RustFunc` signature, and with a crate-visible constructor it would
enforce nothing inside the crate, where every caller of the unguarded bodies
lives. Its coverage is the direct-call convention of
reference/native-calls.md; code that reaches the State through a generic
`fn(&mut S)` or a trait object defined outside src is not seen.
"""

import re
import sys

import native_calls as rust


# These methods cannot return StatePoisoned. They expose host configuration,
# counters or deliberately safe observations, rather than executing Lua.
SAFE_INFALLIBLE = {
    "set_rng_seed", "cost_used", "cost_remaining", "costing",
    "set_user_data", "user_data", "user_data_mut", "clear_user_data",
    "callbacks_mut", "replace_callbacks", "object_count", "string_count",
    "heap_size", "gc_should_run", "gc_threshold", "gc_set_threshold",
    "gc_set_policy", "gc_set_auto_threshold", "gc_disable_auto",
    "current_source", "seal_globals", "globals_sealed", "is_poisoned",
    "error", "get_top", "to_boolean", "typ", "raw_equal", "release_anchor",
    "anchor_type", "anchor_count", "registered_rust_fn",
    # The interrupt handle is host configuration; polling it runs no Lua.
    "interrupt", "set_interrupt",
    # Collection explicitly becomes a no-op after poisoning.
    "gc_collect",
    # Debug-only instrumentation controls, not script execution.
    "arm_fault_injection", "finish_fault_injection",
}

# Public methods whose signature takes State-executing code but which only
# store it, never run it: the registrations. The signature match below is
# what puts a method here; that it does not execute the code is this
# review's claim, not something the script can see. A name that no longer
# takes such code, or that reaches Lua, is an error, so the set cannot rot.
NON_EXECUTING = {"add_rust_fn", "register_rust_fn", "set_global_named_rust_fn"}

# Both prologues refuse a poisoned State and then an unsupported
# floating-point environment; the snapshot entry points report through
# SaveError.
DIRECT = re.compile(r"self\s*\.\s*check_usable\s*\(\s*\)\s*\?\s*;")
SAVE_REFUSAL = re.compile(r"self\s*\.\s*check_save_usable\s*\(\s*\)\s*\?\s*;")
GUARDED = re.compile(r"self\s*\.\s*guarded\s*(\()\s*\|state\|")
# Any spelling of a call to the guard, and any use of it as a value.
GUARD_CALL = re.compile(r"\bguarded\s*(?:::\s*<[^()]*?>\s*)?\(")
GUARD_VALUE = re.compile(r"\b(?:State|Self)\s*::\s*guarded\b(?!\s*(?:::\s*<[^()]*?>\s*)?\()")
GUARD_DEFINITION = re.compile(r"\bfn\s+guarded\b")

RECEIVER = re.compile(r"\(\s*(?:&\s*(?:'\w+\s+)?)?(?:mut\s+)?self\b")
MUT_STATE = r"&\s*(?:'\w+\s+)?mut\s+(?:Self|State)\b"
# A closure bound or fn pointer whose parameters take the State.
STATE_CALLABLE = re.compile(r"(?:\bFn(?:Once|Mut)?|\bfn)\s*\([^()]*" + MUT_STATE)
ALIAS = re.compile(r"\btype\s+([A-Za-z_]\w*)\b[^=;]*=([^;]*);")
TRAIT = re.compile(r"\btrait\s+([A-Za-z_]\w*)\b")


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


def body(fn):
    return fn.src.code[fn.body_open + 1:fn.body_close]


def match_paren(code, open_at):
    depth = 0
    for j in range(open_at, len(code)):
        if code[j] == "(":
            depth += 1
        elif code[j] == ")":
            depth -= 1
            if depth == 0:
                return j
    return None


def whole_body_guarded(fn):
    """The body is one tail expression, `self.guarded(|state| ...)`."""
    text = body(fn)
    m = GUARDED.match(text.lstrip())
    if not m:
        return False
    start = len(text) - len(text.lstrip())
    close = match_paren(text, start + m.start(1))
    return close is not None and not text[close + 1:].strip()


def executing_types(sources):
    """Names of src type aliases and traits that name State-executing code,
    to a fixpoint so an alias of an alias counts."""
    items = []
    for s in sources:
        for m in ALIAS.finditer(s.code):
            if not s.in_test(m.start()):
                items.append((m.group(1), m.group(2)))
        for m in TRAIT.finditer(s.code):
            if s.in_test(m.start()):
                continue
            j = m.end()
            while j < len(s.code) and s.code[j] not in "{;":
                j += 1
            if j < len(s.code) and s.code[j] == "{":
                items.append((m.group(1), s.code[m.end():rust.match_brace(s.code, j)]))
    names = set()
    changed = True
    while changed:
        changed = False
        for name, text in items:
            if name in names:
                continue
            if re.search(MUT_STATE, text) or any(
                    re.search(rf"\b{re.escape(n)}\b", text) for n in names):
                names.add(name)
                changed = True
    return names


def takes_state_code(fn, types):
    sig = fn.sig[RECEIVER.search(fn.sig).end():]
    return bool(STATE_CALLABLE.search(sig)) or any(
        re.search(rf"\b{re.escape(name)}\b", sig) for name in types)


def check_sources(sources, safe_infallible=SAFE_INFALLIBLE, non_executing=NON_EXECUTING):
    problems = []
    methods = [fn for src in sources for fn in src.fns if public_receiver(fn)]
    definitions = rust.definitions(sources)

    def checked_boundary(name):
        found = definitions.get(("State", name), [])
        return bool(found) and all(DIRECT.match(body(fn).lstrip()) for fn in found)

    # Rule 1: entry refusal.
    for fn in methods:
        where = f"{fn.src.rel}:{fn.line}: {fn.qual}"
        if not returns_result(fn):
            if fn.name not in safe_infallible:
                problems.append(f"{where}: review this infallible poisoned-State API")
            continue
        entry = body(fn).lstrip()
        if DIRECT.match(entry) or SAVE_REFUSAL.match(entry):
            continue
        if GUARDED.match(entry) and checked_boundary("guarded"):
            continue
        problems.append(f"{where}: must refuse poisoning before other work")
    if not methods:
        problems.append("no public State receiver methods found")

    # Rule 2: the execution boundary.
    reached, _, _ = rust.closure(sources)
    types = executing_types(sources)
    guarded = {fn.key for fn in methods if whole_body_guarded(fn)}
    hosting = {fn.name for fn in methods if takes_state_code(fn, types)}
    for fn in methods:
        where = f"{fn.src.rel}:{fn.line}: {fn.qual}"
        runs_lua = fn.key in reached
        runs_host = fn.name in hosting and fn.name not in non_executing
        if (runs_lua or runs_host) and not whole_body_guarded(fn):
            why = "can run Lua" if runs_lua else "runs host code against the State"
            problems.append(f"{where}: {why}, so its whole body must be "
                            "self.guarded(|state| ...)")
        if fn.name in non_executing and runs_lua:
            problems.append(f"{where}: reaches Lua, so it cannot be reviewed as non-executing")
    for name in sorted(non_executing - hosting):
        problems.append(f"poison_contract.py: {name} takes no State-executing code; "
                        "drop it from NON_EXECUTING")

    for s in sources:
        for m in GUARD_CALL.finditer(s.code):
            if s.in_test(m.start()) or GUARD_DEFINITION.search(
                    s.code[max(0, m.start() - 12):m.end()]):
                continue
            enc = s.enclosing(m.start())
            # The one call a whole-body-guarded method makes is its first;
            # a second guard nested inside it is as wrong as one elsewhere.
            if not (enc is not None and enc.key in guarded
                    and m.start() == first_guard_offset(enc)):
                problems.append(f"{s.rel}:{s.line_of(m.start())}: guarded is only the whole "
                                "body of a public State method")
        for m in GUARD_VALUE.finditer(s.code):
            if not s.in_test(m.start()):
                problems.append(f"{s.rel}:{s.line_of(m.start())}: guarded used as a value")

    # VM code calls the unguarded bodies; only the CLI's host code calls a
    # guarded entry.
    entries = {key for key in guarded}
    for s in sources:
        if s.rel == "src/main.rs":
            continue
        for name, offset, enc, owner in s.calls():
            for key in entries:
                if rust.resolves(key, name, owner) and enc is not None:
                    problems.append(f"{s.rel}:{s.line_of(offset)}: {enc.qual} calls the "
                                    f"guarded entry State::{name}; call its unguarded "
                                    "body instead")
        for key in entries:
            value = re.compile(rf"\b(?:State|Self)\s*::\s*{key[1]}\b(?!\s*(?:::\s*<[^()]*?>\s*)?\()")
            for m in value.finditer(s.code):
                if not s.in_test(m.start()):
                    problems.append(f"{s.rel}:{s.line_of(m.start())}: guarded entry "
                                    f"State::{key[1]} used as a value")
    return problems


def first_guard_offset(fn):
    text = body(fn)
    m = GUARD_CALL.search(text)
    return fn.body_open + 1 + m.start() if m else None


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
