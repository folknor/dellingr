//! Golden `cost_used` values for a fixed corpus.
//!
//! `COST_MODEL_VERSION` covers the cost of a given source: per-opcode charges,
//! native library charges and compiler emission (which opcodes a source turns
//! into, e.g. constant folding). If one of these assertions fails, the cost of
//! identical source changed: bump `COST_MODEL_VERSION`, document the new
//! version in `src/cost_meter.rs`, and update the expected values here.

use dellingr::error::ErrorKind;
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
        345,
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

/// Sources that raise a runtime error after executing costed opcodes. The
/// expected value counts every costed opcode that ran before the error, so it
/// must not depend on the dispatch loop's cost batching interval.
const ERROR_CORPUS: &[(&str, &str, u64)] = &[
    (
        "arith_error_after_add",
        "local a, b = 1, nil local x = a + a local y = a + b",
        // Two adds, including the failing one.
        2,
    ),
    (
        "index_nil_after_add",
        "x = 2 local a = 1 + x local t = nil local b = t.k",
        // One add; global writes and field reads are free.
        1,
    ),
    (
        "write_nil_after_add",
        "local a = 1 local x = a + a local t = nil t.k = x",
        // One add and the failing field write.
        2,
    ),
    (
        "nested_arith_error",
        "local function f(a) local x = a + a return x + nil end local a = 1 local x = a + a f(x)",
        // One caller add, one successful callee add and one failing add.
        3,
    ),
    (
        "arith_error_after_batch_flush",
        "local n = 0 for i = 1, 65 do n = n + 1 end return n + nil",
        // 65 loop adds and the failing add, spanning the 64-unit batch boundary.
        66,
    ),
    (
        "index_handler_error",
        "local t = setmetatable({}, { __index = function(_, k) local n = 1 return n + k end }) local n = 1 local x = n + n local y = t.missing",
        // Two tables, one initializer write, one caller add and one failing add.
        5,
    ),
    (
        "newindex_handler_error",
        "local t = setmetatable({}, { __newindex = function(_, k, v) return v + k end }) local n = 1 local x = n + n t.missing = x",
        // As above, plus the field write that invokes __newindex.
        6,
    ),
    (
        "length_handler_error",
        "local t = setmetatable({}, { __len = function(t) local n = 1 return n + t end }) local n = 1 local x = n + n local y = #t",
        // Two tables, one initializer write, one caller add and one failing add.
        5,
    ),
];

#[test]
fn cost_used_after_runtime_error_matches_golden_corpus() {
    for &(name, source, expected) in ERROR_CORPUS {
        let exact_budget = i64::try_from(expected).expect("golden cost fits in i64");
        for budget in [None, Some(exact_budget), Some(1_000)] {
            let mut state = State::new();
            if let Some(budget) = budget {
                state.set_cost_budget(budget);
            }
            state.load_string(source).expect("golden source compiles");
            let error = state
                .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                .expect_err("golden error source must fail at runtime");
            assert!(
                matches!(error.kind, ErrorKind::TypeError(_)),
                "{name} (budget {budget:?}): expected a type error, got {error:?}"
            );
            assert_eq!(
                state.cost_used(),
                expected,
                "{name} (budget {budget:?}): cost_used after a runtime error must count \
                 the executed costed opcodes; a change here needs a COST_MODEL_VERSION bump"
            );
        }
    }
}

fn cost_of(source: &str) -> u64 {
    let mut state = State::new();
    state.load_string(source).expect("cost source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("cost source runs");
    state.cost_used()
}

#[test]
fn unpack_charges_only_lookups_reached_before_an_index_error() {
    let mut costs = Vec::new();
    for end in [1, 10] {
        let mut state = State::new();
        state
            .load_string(format!(
                "local t = setmetatable({{}}, {{__index = function() return nil + 1 end}}) unpack(t, 1, {end})"
            ))
            .unwrap();
        let err = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("first lookup errors");
        assert!(matches!(err.kind, ErrorKind::TypeError(_)));
        costs.push(state.cost_used());
    }
    assert_eq!(costs[0], costs[1], "unvisited slots must not consume cost");
}

/// The table library charges one per element shifted or visited (min 1).
/// Each pair performs the same costed bytecode operations on different data,
/// so the difference is exactly the data-dependent library charge.
#[test]
fn table_library_charges_scale_with_elements() {
    let setup = "local t = {} for i = 1, 50 do t[i] = i end";
    let pairs: &[(&str, &str, &str, u64)] = &[
        // Insert at the front shifts 50 (cost 50); an append shifts none (1).
        ("insert", "local d = -49", "local d = 1", 49),
        // Removing the front shifts 49 (cost 49); a pop shifts none (1).
        ("remove", "local d = -49", "local d = 0", 48),
        // unpack(t, 1, 10) visits 10; unpack(t, 1, 1) visits 1.
        ("unpack", "local j = 10", "local j = 1", 9),
        // An empty range still costs the minimum 1.
        ("unpack_empty", "local j = 1", "local j = 0", 0),
        // pack stores 10 vs 1, on top of the unpack difference of 9.
        ("pack", "local j = 10", "local j = 1", 18),
    ];
    for &(name, wide, narrow, expected) in pairs {
        let body = match name {
            "insert" => "local p = #t + d table.insert(t, p, 0)",
            "remove" => "local p = #t + d table.remove(t, p)",
            "unpack" | "unpack_empty" => "local a = unpack(t, 1, j)",
            "pack" => "local a = table.pack(unpack(t, 1, j))",
            _ => unreachable!("every pair names a body"),
        };
        let wide_cost = cost_of(&format!("{setup} {wide} {body}"));
        let narrow_cost = cost_of(&format!("{setup} {narrow} {body}"));
        assert_eq!(
            wide_cost - narrow_cost,
            expected,
            "{name}: wide {wide_cost}, narrow {narrow_cost}"
        );
    }
}

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
