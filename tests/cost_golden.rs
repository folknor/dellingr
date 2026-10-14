//! Golden `cost_used` values for a fixed corpus.
//!
//! `COST_MODEL_VERSION` covers the cost of a given source: per-opcode charges,
//! native library charges and compiler emission (which opcodes a source turns
//! into, e.g. constant folding). If one of these assertions fails, the cost of
//! identical source changed: bump `COST_MODEL_VERSION`, document the new
//! version in `src/cost_meter.rs`, and update the expected values here.

use dellingr::{ArgCount, COST_MODEL_VERSION, RetCount, State};

const CORPUS: &[(&str, &str, u64)] = &[
    (
        "folded_literals",
        "local x = 60 * 1000 + -5 local y = x * 2",
        1,
    ),
    ("literal_pow", "local x = 2 ^ 10 local y = -3 ^ 2 + 1", 4),
    (
        "loop_and_calls",
        "local function f(a) return a + 1 end local s = 0 for i = 1, 100 do s = f(s) end",
        100,
    ),
    (
        "tables",
        "local t = {} for i = 1, 50 do t[i] = i end local n = 0 for _, v in ipairs(t) do n = n + v end table.insert(t, 1, 0) table.concat(t, ',')",
        296,
    ),
    (
        "strings_and_patterns",
        "local s = 'hello world from lua' local a = s:find('wor') local b = s:gsub('o', '0') local c = s:match('(%a+) (%a+)') local u = s:upper()",
        193,
    ),
    (
        "gmatch",
        "local n = 0 for w in string.gmatch('one two three four', '%a+') do n = n + #w end for w in string.gmatch('a b', '%a+') do n = n + 1 end",
        103,
    ),
];

#[test]
fn cost_used_matches_golden_corpus() {
    let mut drift = Vec::new();
    for &(name, source, expected) in CORPUS {
        let mut state = State::new();
        state.load_string(source).expect("golden source compiles");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("golden source runs");
        let actual = state.cost_used();
        if actual != expected {
            drift.push(format!("{name}: expected {expected}, got {actual}"));
        }
    }
    assert!(
        drift.is_empty(),
        "cost_used for identical source changed under COST_MODEL_VERSION {COST_MODEL_VERSION}; \
         bump COST_MODEL_VERSION, document the new version, and update this table:\n{}",
        drift.join("\n")
    );
}
