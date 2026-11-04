#!/usr/bin/env python3
"""Check that every loop in native library code is an interrupt safepoint or
says why it does not need one.

A host interrupt (src/vm/interrupt.rs) stops running Lua at safepoints. The
dispatch loop and call admission cover bytecode; native library work is
covered only where a loop polls. Whether a loop needs a poll depends on where
its trip count comes from, which a lexer cannot decide, so this check forces
the decision to be written down instead. Every `for`, `while` and `loop` in
the scoped files, outside test code, must either

- call `poll_interrupt(` directly in its body, or
- sit under a comment, in the comment lines directly above it, containing
  `bounded:` (why one pass of it is bounded: a string under
  MAX_STRING_BYTES, a table's storage, a fixed count) or `safepoint:` (where
  it polls instead, such as a callee that polls per step).

Scope: the standard library, the pattern matcher and the table operations
under src/vm/table_ops.rs, which hold the native library work. Iteration
through closures (`for_each`, `fold`, `map`) is not a loop here and is not
checked. Uses native_calls' Rust lexer, so comments and literals cannot
supply a poll. Prints `ok` when every loop is decided.
"""

import re
import sys

import native_calls as rust

SCOPE_DIRS = ("src/lua_std/", "src/patterns/")
SCOPE_FILES = ("src/vm/table_ops.rs",)

# `for` not followed by `<` (a higher-ranked bound), `while` and `loop`.
LOOP = re.compile(r"\b(?:(for)\b(?!\s*<)|while\b|loop\b)")
POLL = re.compile(r"\bpoll_interrupt\s*\(")
DECISION = re.compile(r"//.*\b(?:bounded|safepoint):\s*\S")


def in_scope(source):
    return source.rel.startswith(SCOPE_DIRS) or source.rel in SCOPE_FILES


def header_open(code, start):
    """The offset of the `{` that opens the body of the loop whose keyword
    ends at `start`, skipping parentheses and brackets in its header."""
    depth = 0
    for j in range(start, len(code)):
        c = code[j]
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif depth == 0 and c == "{":
            return j
        elif depth == 0 and c == ";":
            return None
    return None


def decided_above(source, line):
    """Whether the comment lines directly above `line` (1-based) record a
    decision. Attribute lines between them and the loop are skipped."""
    index = line - 2
    while index >= 0:
        raw = source.lines[index].strip()
        if raw.startswith("#["):
            index -= 1
            continue
        if not raw.startswith("//"):
            return False
        if DECISION.search(raw):
            return True
        index -= 1
    return False


def check_sources(sources):
    problems = []
    for source in sources:
        if not in_scope(source):
            continue
        code = source.code
        for m in LOOP.finditer(code):
            if source.in_test(m.start()):
                continue
            open_at = header_open(code, m.end())
            if open_at is None:
                continue
            header = code[m.end():open_at]
            if m.group(1) and not re.search(r"\bin\b", header):
                # `impl Trait for Type` and other non-loop uses of `for`.
                continue
            body = code[open_at:rust.match_brace(code, open_at) + 1]
            line = source.line_of(m.start())
            if POLL.search(body) or decided_above(source, line):
                continue
            keyword = code[m.start():m.end()]
            problems.append(
                f"{source.rel}:{line}: `{keyword}` loop in native library code "
                "neither polls the interrupt nor says why it need not "
                "(`// bounded: <why>` or `// safepoint: <where>` above it)"
            )
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
