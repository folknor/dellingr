#!/usr/bin/env python3
"""Generate examples/parse/large_source.lua.

Parse and codegen costs that scale with source size cannot surface in the
other bench scripts in examples/, which are 15-80 lines - far too small to rise
above noise. This one is large enough to.

The generated file is deliberately WIDE, not deep: many top-level function
definitions, each with several statements, several call sites, a nested
closure, and plenty of identifiers, so lexing, statement parsing, call
emission and nested-chunk finalization all get exercised at scale.

Functions are globals rather than `local function` on purpose: hundreds of
top-level locals would exceed the per-function local limit (`MAX_LOCALS` in
src/limits.rs, and reference Lua's own 200) in the main chunk. Every top-level
function is a nested chunk of the main chunk, so the executable guard below
keeps them within `MAX_NESTED`, read from src/limits.rs on every run
(including --check) so the two cannot drift. Size therefore comes from making
each function longer rather than from adding more of them.

The generated body must stay cheap to RUN, since the example runner executes
every Lua file under examples/. Only a handful of the generated functions are
ever called; the rest exist to be parsed.

Usage: python3 examples/parse/generate.py [--check]
"""

import pathlib
import re
import sys

FUNCTION_COUNT = 200
# Top-level functions besides the generated nodes: mix, scale, tag, _bench.
EXTRA_TOP_LEVEL_FUNCTIONS = 4
# The footer calls node_100_step.
MIN_FUNCTION_COUNT = 100
OUT = pathlib.Path(__file__).with_name("large_source.lua")
LIMITS = pathlib.Path(__file__).resolve().parents[2] / "src" / "limits.rs"

INTEGER_MAX = {"u8": (1 << 8) - 1, "u16": (1 << 16) - 1, "u32": (1 << 32) - 1}


def read_limit(name):
    """Evaluate `pub const NAME: usize = ...;` from src/limits.rs.

    Only an integer literal or `uN::MAX as usize` is understood; anything else
    fails loudly rather than guessing.
    """
    text = LIMITS.read_text(encoding="utf-8")
    match = re.search(rf"^pub const {name}: usize = (.+);$", text, re.MULTILINE)
    if match is None:
        raise SystemExit(f"{LIMITS}: no `pub const {name}: usize` declaration")
    expr = match.group(1).strip()
    if re.fullmatch(r"[0-9_]+", expr):
        return int(expr.replace("_", ""))
    max_match = re.fullmatch(r"(u8|u16|u32)::MAX as usize", expr)
    if max_match:
        return INTEGER_MAX[max_match.group(1)]
    raise SystemExit(f"{LIMITS}: cannot evaluate {name} = {expr}")

HEADER = '''-- GENERATED FILE - do not edit by hand.
-- Regenerate with: python3 examples/parse/generate.py
--
-- Probe: parse and codegen time on a large chunk. Unlike every other bench in
-- examples/, the interesting cost here is paid BEFORE _bench() ever runs, so
-- read the harness's parse_us phase rather than warm_avg_us, and expect the
-- standalone wall time to be dominated by parsing rather than execution.
--
-- Shape: {count} top-level function definitions, each with several statements,
-- several call sites, and a nested closure. See examples/parse/generate.py for
-- why this shape and why the functions are globals.

local function mix(a, b)
    return a + b * 2
end

local function scale(v)
    return v * 3
end

local function tag(v)
    return v + 1
end

'''

TEMPLATE = '''function node_{i}_step(alpha, beta)
    local acc = mix(alpha, beta)
    local scaled = scale(acc)
    local label = "node_{i}"
    local bump = function(v)
        return tag(v) + acc
    end
    scaled = bump(scaled)
    acc = mix(scaled, tag(acc))
    local width = scale(tag(acc))
    local height = mix(width, scaled)
    local depth = tag(mix(height, width))
    acc = mix(acc, depth)
    if acc > 0 then
        acc = scale(acc)
        acc = mix(acc, tag(width))
    else
        acc = tag(acc)
        acc = mix(acc, scale(height))
    end
    local total = mix(acc, mix(width, mix(height, depth)))
    total = scale(tag(total))
    return total + #label
end

'''

FOOTER = '''function _bench()
    local total = 0
    for i = 1, 50 do
        total = total + node_1_step(i, 2)
        total = total + node_100_step(i, 3)
        total = total + node_{last}_step(i, 4)
    end
    return total
end

-- Parsing dominates, so the standalone runner loops only enough to keep the
-- executed portion non-trivial without hiding the parse cost.
for i = 1, 20 do _bench() end
print("parse/large_source: true")
'''


def main():
    max_nested = read_limit("MAX_NESTED")
    if FUNCTION_COUNT + EXTRA_TOP_LEVEL_FUNCTIONS > max_nested:
        raise SystemExit(f"top-level functions exceed MAX_NESTED ({max_nested})")
    if FUNCTION_COUNT < MIN_FUNCTION_COUNT:
        raise SystemExit(f"FUNCTION_COUNT must be at least {MIN_FUNCTION_COUNT}")

    parts = [HEADER.format(count=FUNCTION_COUNT)]
    for i in range(1, FUNCTION_COUNT + 1):
        parts.append(TEMPLATE.format(i=i))
    parts.append(FOOTER.format(last=FUNCTION_COUNT))
    body = "".join(parts)

    if sys.argv[1:] == ["--check"]:
        if not OUT.exists() or OUT.read_text(encoding="utf-8") != body:
            raise SystemExit(f"{OUT} does not match generated output")
        print("ok")
        return
    if sys.argv[1:]:
        raise SystemExit("usage: python3 examples/parse/generate.py [--check]")

    OUT.write_text(body, encoding="utf-8")
    print(f"wrote {OUT} ({body.count(chr(10))} lines)")


if __name__ == "__main__":
    main()
