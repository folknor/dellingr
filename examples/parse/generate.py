#!/usr/bin/env python3
"""Generate examples/parse/large_source.lua.

The parse-time optimization candidates (token-stamped line numbers, the
Vec::remove in call emission, mem::take in parse_chunk, zero-allocation
identifiers) all have costs that scale with source size - two of them
quadratically. Every other bench script in examples/ is 15-80 lines, which is
far too small for any of that to surface above noise.

The generated file is deliberately WIDE, not deep: many top-level function
definitions, each with several statements, several call sites, and a nested
closure. That shape hits all four candidates at once:

  - statements x lines           -> update_line's linear line_and_col walk
  - call sites                   -> code.remove(mark_idx) tail shifts
  - nested function definitions  -> parse_chunk's two full Bytecode clones,
                                    each O(enclosing chunk size)
  - identifiers                  -> per-token String allocation in lex_word

Functions are globals rather than `local function` on purpose: hundreds of
top-level locals would blow Lua's 200-local-per-function limit in the main
chunk.

FUNCTION_COUNT is bounded by the parser's 255-nested-functions-per-chunk limit,
since every top-level definition is a nested chunk of the main chunk. Size
therefore comes from making each function longer rather than from adding more
of them - which costs nothing in coverage, because the two quadratic candidates
scale with statement and line count, and parse_chunk's clone is O(enclosing
chunk size) per definition, so a bigger main chunk makes each of the 200 clones
more expensive rather than less.

The generated body must stay cheap to RUN, since tests/run_examples.rs executes
every examples/*.lua. Only a handful of the generated functions are ever
called; the rest exist to be parsed.

Usage: python3 examples/parse/generate.py
"""

import pathlib

FUNCTION_COUNT = 200
OUT = pathlib.Path(__file__).with_name("large_source.lua")

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
    parts = [HEADER.format(count=FUNCTION_COUNT)]
    for i in range(1, FUNCTION_COUNT + 1):
        parts.append(TEMPLATE.format(i=i))
    parts.append(FOOTER.format(last=FUNCTION_COUNT))
    body = "".join(parts)
    OUT.write_text(body, encoding="utf-8")
    print(f"wrote {OUT} ({body.count(chr(10))} lines)")


if __name__ == "__main__":
    main()
