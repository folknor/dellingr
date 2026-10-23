//! Golden `cost_used` values for a fixed corpus.
//!
//! `COST_MODEL_VERSION` covers the cost of a given source: per-opcode charges,
//! native library charges and compiler emission (which opcodes a source turns
//! into, e.g. constant folding). If one of these assertions fails, the cost of
//! identical source changed: bump `COST_MODEL_VERSION`, document the new
//! version in `src/cost_meter.rs`, and update the expected values here.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, COST_MODEL_VERSION, RetCount, State};

/// Fingerprint of `CORPUS` together with `COST_MODEL_VERSION`. Refresh it
/// whenever either changes.
const COST_MODEL_GOLDEN_FINGERPRINT: u64 = 0x1328_bf4a_9cd2_c5fc;

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
        "rawset",
        "local t = {}; rawset(t, 1, 2)",
        // One table creation and one flat rawset charge.
        2,
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
    (
        "gmatch_leading_caret",
        "local n = 0 for w in string.gmatch('^a^a', '^a') do n = n + #w end",
        // The literal caret is charged as the one pattern byte it is, at
        // creation and at first-call compilation.
        26,
    ),
    (
        "sequence_len_handler",
        "local t = setmetatable({}, {__len = function() local n = 1 return n + 1 end}) table.insert(t, 7)",
        // Two tables, one initializer write, one handler add and one append.
        5,
    ),
    (
        "quoted_integral",
        "local s = string.format('%q', 1)",
        // Two format bytes and one emitted decimal digit.
        3,
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
            dellingr::Program::compile(source, None)
                .and_then(|program| state.load(&program))
                .expect("golden source compiles");
            let error = state
                .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                .expect_err("golden error source must fail at runtime");
            assert!(
                matches!(error.kind(), ErrorKind::TypeError(_)),
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
    dellingr::Program::compile(source, None)
        .and_then(|program| state.load(&program))
        .expect("cost source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("cost source runs");
    state.cost_used()
}

#[test]
fn unpack_charges_nothing_for_lookups_reached_before_an_index_error() {
    let mut costs = Vec::new();
    for end in [1, 10] {
        let mut state = State::new();
        dellingr::Program::compile(format!(
                "local t = setmetatable({{}}, {{__index = function() return nil + 1 end}}) unpack(t, 1, {end})"
            ), None).and_then(|program| state.load(&program))
            .unwrap();
        let err = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("first lookup errors");
        assert!(matches!(err.kind(), ErrorKind::TypeError(_)));
        costs.push(state.cost_used());
    }
    // Two tables, one field initializer, and the failing handler add.
    // The lookup itself adds no native charge.
    assert_eq!(costs, [4, 4]);
}

#[test]
fn table_library_native_charges_are_one_or_zero() {
    for (source, expected) in [
        ("table.insert({}, nil)", 2),
        ("table.remove({})", 2),
        ("table.pack()", 1),
        ("table.pack(1, 2, 3)", 1),
        ("unpack({}, 1, 0)", 1),
        ("unpack({}, 1, 10)", 1),
        ("table.unpack({}, 1, 10)", 1),
    ] {
        // Explicit {} costs 1; insert/remove/pack add exactly 1. The table
        // allocated inside pack has no separate opcode creation charge.
        assert_eq!(cost_of(source), expected, "{source}");
    }
}

/// `table.insert`, `table.remove` and `table.pack` charge a flat 1 and
/// `unpack` is free, however many elements are shifted, visited or stored.
/// This is a deliberate game-design choice of the cost model (the budget is
/// not a CPU meter), pinned here so it is not re-filed as a bug. Each pair
/// performs the same costed bytecode operations on different data, so the
/// difference is exactly the data-dependent library charge: zero.
#[test]
fn table_library_charges_are_flat() {
    let setup = "local t = {} for i = 1, 50 do t[i] = i end";
    let pairs: &[(&str, &str, &str, u64)] = &[
        // Front insert and append cost the same.
        ("insert", "local d = -49", "local d = 1", 0),
        // Front remove and pop cost the same.
        ("remove", "local d = -49", "local d = 0", 0),
        // unpack is free whatever the range.
        ("unpack", "local j = 10", "local j = 1", 0),
        ("unpack_empty", "local j = 1", "local j = 0", 0),
        // pack costs 1 whatever the argument count.
        ("pack", "local j = 10", "local j = 1", 0),
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
        dellingr::Program::compile(source, None)
            .and_then(|program| state.load(&program))
            .expect("golden source compiles");
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

fn fingerprint_bytes(fingerprint: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *fingerprint = (*fingerprint ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3);
    }
}

fn golden_corpus_fingerprint() -> u64 {
    let mut fingerprint = 0xcbf2_9ce4_8422_2325;
    fingerprint_bytes(&mut fingerprint, &COST_MODEL_VERSION.to_le_bytes());
    for &(_, source, expected) in CORPUS.iter().chain(ERROR_CORPUS.iter()) {
        fingerprint_bytes(&mut fingerprint, source.as_bytes());
        fingerprint_bytes(&mut fingerprint, &[0]);
        fingerprint_bytes(&mut fingerprint, &expected.to_le_bytes());
    }
    fingerprint
}

#[test]
fn golden_corpus_fingerprint_matches_cost_model_version() {
    assert_eq!(
        golden_corpus_fingerprint(),
        COST_MODEL_GOLDEN_FINGERPRINT,
        "golden sources, expected costs, or COST_MODEL_VERSION changed; review the expected costs, \
         bump the version for cost changes, and refresh the fingerprint in this test"
    );
}

/// A Rust callback running under a Lua caller always sees that caller's opcode
/// cost fully flushed: `OP_CALL` (and every metamethod call out of the
/// dispatch loop) settles the pending batch first. A mid-run
/// `set_cost_budget` therefore resets a complete meter, and `cost_used`
/// afterwards counts only post-reset work, whatever the batching interval
/// made of the prefix.
#[test]
fn mid_run_budget_reset_sees_a_flushed_meter() {
    fn reset(state: &mut State) -> dellingr::Result<u8> {
        let expected = *state
            .user_data::<u64>()
            .expect("test installed expected cost");
        assert_eq!(state.cost_used(), expected, "meter before reset");
        assert_eq!(
            state.cost_remaining(),
            1_000_000 - i64::try_from(expected).map_or(i64::MAX, i64::from)
        );
        state.set_cost_budget(1_000_000);
        Ok(0)
    }

    let mut results = Vec::new();
    let interval = dellingr::limits::COST_CHECK_INTERVAL;
    for (setup, call_site, setup_cost) in [
        ("", "reset()", 0),
        (
            "local t = setmetatable({}, {__index = reset})",
            "local v = t.x",
            3,
        ),
        (
            "local t = setmetatable({}, {__index = reset})",
            "local k = 'x' local v = t[k]",
            3,
        ),
        (
            "local t = setmetatable({}, {__newindex = reset})",
            "t.x = 1",
            4,
        ),
        (
            "local t = setmetatable({}, {__newindex = reset})",
            "local k = 'x' t[k] = 1",
            4,
        ),
        (
            "local t = setmetatable({}, {__len = reset})",
            "local v = #t",
            3,
        ),
        ("local t = setmetatable({}, {__call = reset})", "t()", 3),
        ("getmetatable('').__call = reset", "local s = 'x' s()", 1),
        (
            "getmetatable('').__index = reset",
            "local v = ('x').missing",
            1,
        ),
        ("", "for v in reset, nil, nil do end", 0),
    ] {
        for prefix in [0, 1, 7, interval - 1, interval, interval + 1, 20_000] {
            let mut state = State::new();
            state.set_cost_budget(1_000_000);
            state.set_user_data(prefix + setup_cost);
            state.push_rust_fn(reset).unwrap();
            state.set_global("reset").unwrap();
            dellingr::Program::compile(
                format!(
                    "{setup} local s = 0 for i = 1, {prefix} do s = s + i end \
                     {call_site} local a = {{}} a.x = 1 a.y = 2 local z = a.x + a.y"
                ),
                None,
            )
            .and_then(|program| state.load(&program))
            .unwrap();
            state
                .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                .unwrap_or_else(|error| panic!("{call_site}, prefix {prefix}: {error}"));
            results.push((state.cost_used(), state.cost_remaining()));
        }
    }
    assert!(
        results.windows(2).all(|w| w[0] == w[1]),
        "post-reset cost must not depend on the pre-reset prefix: {results:?}"
    );
    let (used, remaining) = results[0];
    assert_eq!(used, 4, "post-reset table creation, two writes and one add");
    assert_eq!(
        remaining,
        1_000_000 - i64::try_from(used).expect("cost fits in i64")
    );
}
