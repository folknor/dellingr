//! An `__index` lookup succeeds or fails by the heap alone, never by which
//! receivers its site saw before, nor by how it is spelled.
//!
//! The rule at the metamethod depth limit: a terminal `__index` table step
//! (the handler table holds the key raw, or has no `__index` of its own) takes
//! no depth unit; every step that nests (a handler table that dispatches its
//! own `__index`, a function handler, a string handler) takes one. It holds for
//! `t.k`, `t["k"]` and `t:m()`, for table and string receivers, with a cold
//! inline cache, a warm one, and after a snapshot round trip (which rebuilds
//! every cache cold).
//!
//! The depth is saturated without native recursion: a call through a
//! non-function `__call` step holds one unit for as long as the callee runs, so
//! `limits::MAX_METAMETHOD_DEPTH` such calls leave Lua code running at the
//! limit.

use dellingr::error::{ErrorKind, TypeError};
use dellingr::limits::{MAX_METAMETHOD_DEPTH, MAX_STACK_SIZE};
use dellingr::{ArgCount, LuaType, RetCount, State};

const PRELUDE: &str = r#"
local callable
local function descend(_, _, n)
    if n == 0 then
        local result = PROBE()
        return result
    end
    local result = callable(n - 1)
    return result
end
callable = setmetatable({}, {__call = setmetatable({}, {__call = descend})})

-- Runs f with the metamethod depth at d.
function at_depth(d, f)
    if d == 0 then return f() end
    PROBE = f
    local result = callable(d - 1)
    return result
end

One = {greet = "one"}
One.__index = One
function One.shout() return "one!" end
one = setmetatable({}, One)

Base = {greet = "base"}
Base.__index = Base
function Base.shout() return "base!" end
Derived = setmetatable({}, {__index = Base})
Derived.__index = Derived
two = setmetatable({}, Derived)

Deeper = setmetatable({}, {__index = Derived})
Deeper.__index = Deeper
three = setmetatable({}, Deeper)

by_function = setmetatable({}, {__index = function(_, k) return k end})
function_behind_table = setmetatable({}, {__index = setmetatable({}, {
    __index = function(_, k) return k end,
})})
bad = setmetatable({}, {__index = false})
-- Two table steps that continue, then a handler that is not callable.
bad_chain = setmetatable({}, {__index = setmetatable({}, {
    __index = setmetatable({}, {__index = false}),
})})

function field_site(o) return o.greet end
function key_site(o, k) return o[k] end
function method_site(o) return o:shout() end
function absent_site(o) return o.absent end
function string_site(s) return s:upper() end
function string_key_site(s, k) return s[k](s) end

-- Not armed, the probe only records that its body ran, without reaching its
-- lookup site.
function cap_field(o)
    if not ARMED then entered = true return nil end
    return o.greet
end
function cap_string(s)
    if not ARMED then entered = true return nil end
    return s:upper()
end
"#;

/// Lookups that see a miss, a chain or a handler, run at depth 0 to leave a
/// miss behind in every site's cache before the probes.
const MISS_WARMUP: &str = r#"
field_site(two) field_site(three) field_site(by_function) field_site(function_behind_table)
key_site(two, "greet") key_site(by_function, "greet")
method_site(two) method_site(three)
absent_site(one) absent_site(two)
string_site("x") string_key_site("x", "upper")
"#;

fn state_with(source: &str) -> State {
    let mut state = State::new();
    run(&mut state, PRELUDE);
    run(&mut state, source);
    state
}

fn run(state: &mut State, source: &str) {
    dellingr::Program::compile(source, None)
        .and_then(|program| state.load(&program))
        .expect("test source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .unwrap_or_else(|error| panic!("setup fails: {source}\n{error}"));
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Value(String),
    DepthExceeded,
    TypeError,
    StackOverflow,
}

fn outcome(result: dellingr::Result<()>, state: &mut State) -> Outcome {
    match result {
        Ok(()) => {
            let value = state.to_string(-1).expect("the probe returns a string");
            state.pop(1).expect("the result is on the stack");
            Outcome::Value(value)
        }
        Err(error) => match error.kind() {
            ErrorKind::MetamethodDepthExceeded { depth, limit } => {
                assert_eq!(
                    (*depth, *limit),
                    (MAX_METAMETHOD_DEPTH, MAX_METAMETHOD_DEPTH)
                );
                Outcome::DepthExceeded
            }
            ErrorKind::TypeError(TypeError::TableIndex(_)) => Outcome::TypeError,
            ErrorKind::StackOverflow { .. } => Outcome::StackOverflow,
            other => panic!("unexpected error {other:?}"),
        },
    }
}

/// `expr` evaluated with the metamethod depth at `depth`.
fn probe(state: &mut State, depth: u32, expr: &str) -> Outcome {
    let source = format!("return tostring(at_depth({depth}, function() return {expr} end))");
    dellingr::Program::compile(source, None)
        .and_then(|program| state.load(&program))
        .expect("probe compiles");
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(1));
    let outcome = outcome(result, state);
    assert_eq!(state.get_top(), 0);
    outcome
}

fn value(text: &str) -> Outcome {
    Outcome::Value(text.to_string())
}

/// Every lookup with the units its steps nest, through each spelling.
fn cases() -> Vec<(&'static str, u32, Outcome)> {
    vec![
        // Terminal one-level steps: no unit.
        ("field_site(one)", 0, value("one")),
        ("key_site(one, 'greet')", 0, value("one")),
        ("method_site(one)", 0, value("one!")),
        ("absent_site(one)", 0, value("nil")),
        ("key_site(one, 'absent')", 0, value("nil")),
        // One step that nests (Derived dispatches to Base).
        ("field_site(two)", 1, value("base")),
        ("key_site(two, 'greet')", 1, value("base")),
        ("method_site(two)", 1, value("base!")),
        ("absent_site(two)", 1, value("nil")),
        // Two steps that nest.
        ("field_site(three)", 2, value("base")),
        ("key_site(three, 'greet')", 2, value("base")),
        ("method_site(three)", 2, value("base!")),
        // A function handler, alone and behind a table that dispatches it.
        ("field_site(by_function)", 1, value("greet")),
        ("key_site(by_function, 'greet')", 1, value("greet")),
        ("field_site(function_behind_table)", 2, value("greet")),
        (
            "key_site(function_behind_table, 'greet')",
            2,
            value("greet"),
        ),
        // String receivers: the library table step is terminal.
        ("string_site('x')", 0, value("X")),
        ("string_key_site('x', 'upper')", 0, value("X")),
    ]
}

/// Each case succeeds with exactly its units left and is refused with one
/// fewer.
fn check_limit_edges(state: &mut State, label: &str) {
    for (expr, units, expected) in cases() {
        let fits = MAX_METAMETHOD_DEPTH - units;
        assert_eq!(
            probe(state, fits, expr),
            expected,
            "{label}: {expr} at {fits}"
        );
        if units > 0 {
            assert_eq!(
                probe(state, fits + 1, expr),
                Outcome::DepthExceeded,
                "{label}: {expr} at {}",
                fits + 1
            );
        }
    }
}

#[test]
fn limit_edges_with_cold_caches() {
    let mut state = state_with("");
    check_limit_edges(&mut state, "cold");
}

#[test]
fn limit_edges_after_the_sites_cached_misses() {
    let mut state = state_with(MISS_WARMUP);
    check_limit_edges(&mut state, "warm miss");
}

#[test]
fn limit_edges_after_the_sites_cached_hits() {
    let mut state = state_with(
        "field_site(one) key_site(one, 'greet') method_site(one) absent_site(one)
         string_site('x') string_key_site('x', 'upper')",
    );
    check_limit_edges(&mut state, "warm hit");
}

#[test]
fn a_site_that_saw_an_inherited_method_still_answers_one_level_receivers_at_the_limit() {
    // The site's first receiver resolves two levels up; the next one level
    // up. Both must behave as they would at a fresh site.
    for warmup in [
        "field_site(two)",
        "field_site(by_function)",
        "field_site(bad)",
    ] {
        let mut state = state_with("");
        let _ = probe(&mut state, 0, warmup);
        assert_eq!(
            probe(&mut state, MAX_METAMETHOD_DEPTH, "field_site(one)"),
            value("one"),
            "after {warmup}"
        );
    }
}

#[test]
fn a_handler_that_cannot_be_indexed_is_a_type_error_at_any_depth() {
    let mut state = state_with(MISS_WARMUP);
    for expr in [
        "field_site(bad)",
        "key_site(bad, 'greet')",
        "method_site(bad)",
    ] {
        for depth in [0, MAX_METAMETHOD_DEPTH] {
            assert_eq!(
                probe(&mut state, depth, expr),
                Outcome::TypeError,
                "{expr} at {depth}"
            );
        }
    }
}

#[test]
fn an_invalid_handler_behind_tables_fails_after_their_units() {
    // Its two table steps continue, so they need two units before the type
    // error can be reached.
    let mut state = state_with("");
    for expr in ["field_site(bad_chain)", "key_site(bad_chain, 'greet')"] {
        let fits = MAX_METAMETHOD_DEPTH - 2;
        assert_eq!(probe(&mut state, fits, expr), Outcome::TypeError, "{expr}");
        assert_eq!(
            probe(&mut state, fits + 1, expr),
            Outcome::DepthExceeded,
            "{expr}"
        );
    }
}

/// A Rust callback that reads `receiver.greet` through `State::get_table`
/// and reports whether it failed, swallowing the error.
fn try_index(state: &mut State) -> dellingr::Result<u8> {
    state.push_string("greet")?;
    let failed = state.get_table(1).is_err();
    state.set_top(0)?;
    state.push_boolean(failed)?;
    Ok(1)
}

#[test]
fn a_failed_lookup_returns_every_unit_it_took() {
    // Each lookup fails after taking units, inside a callback that carries
    // on, then a lookup needing the remaining unit runs at the same depth.
    let mut state = state_with("");
    state.push_rust_fn(try_index).expect("callback fits");
    state.set_global("try_index").expect("callback set");
    for receiver in ["bad_chain", "bad", "function_behind_table"] {
        let expr = format!(
            "(function() try_index({receiver}) try_index({receiver}) return field_site(two) end)()"
        );
        let depth = MAX_METAMETHOD_DEPTH - 1;
        assert_eq!(probe(&mut state, depth, &expr), value("base"), "{receiver}");
    }
    // The same after lookups that fail at the stack cap: whatever the
    // outcome there, the units are back afterwards.
    let _ = call_near_cap(&mut state, "cap_field", "function_behind_table", 0);
    let _ = call_near_cap(&mut state, "cap_field", "bad_chain", 0);
    assert_eq!(
        probe(&mut state, MAX_METAMETHOD_DEPTH - 1, "field_site(two)"),
        value("base")
    );
}

#[test]
fn a_string_library_step_that_nests_takes_a_unit() {
    for warm in [false, true] {
        let mut state = state_with(if warm { MISS_WARMUP } else { "" });
        run(
            &mut state,
            r#"getmetatable("").__index = setmetatable({}, {__index = string})"#,
        );
        for expr in ["string_site('x')", "string_key_site('x', 'upper')"] {
            let fits = MAX_METAMETHOD_DEPTH - 1;
            assert_eq!(
                probe(&mut state, fits, expr),
                value("X"),
                "warm {warm}: {expr}"
            );
            assert_eq!(
                probe(&mut state, fits + 1, expr),
                Outcome::DepthExceeded,
                "warm {warm}: {expr}"
            );
        }
    }
}

#[cfg(feature = "snapshot")]
#[test]
fn a_snapshot_round_trip_keeps_every_limit_edge() {
    let mut state = state_with(MISS_WARMUP);
    let save = state.save_state().expect("state saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(dellingr::DefaultCallbacks), |_| {
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("save loads");
    for (expr, units, _) in cases() {
        for depth in [
            MAX_METAMETHOD_DEPTH - units,
            MAX_METAMETHOD_DEPTH - units + 1,
        ] {
            if depth > MAX_METAMETHOD_DEPTH {
                continue;
            }
            assert_eq!(
                probe(&mut state, depth, expr),
                probe(&mut loaded, depth, expr),
                "{expr} at {depth}: the continuing and the loaded State diverge"
            );
        }
    }
    check_limit_edges(&mut loaded, "loaded");
}

/// Calls `function(receiver)` with its frame `headroom` slots below the stack
/// cap. The frame's size is found by calling it unarmed, which runs no
/// lookup, from the cap downwards until admission accepts it.
fn call_near_cap(state: &mut State, function: &str, receiver: &str, headroom: usize) -> Outcome {
    let mut fill = MAX_STACK_SIZE - 2;
    loop {
        state.push_boolean(false).expect("flag fits");
        state.set_global("entered").expect("flag set");
        state.set_top(fill as isize).expect("filler fits");
        state.get_global(function).expect("probe function fits");
        state.get_global(receiver).expect("receiver fits");
        let entered = state.call(ArgCount::Fixed(1), RetCount::Fixed(1)).is_ok();
        state.set_top(0).expect("stack clears");
        if entered {
            state.get_global("entered").expect("flag fits");
            assert!(state.to_boolean(-1), "the unarmed probe ran its body");
            state.pop(1).expect("flag pops");
            break;
        }
        fill -= 1;
    }
    state.push_boolean(true).expect("flag fits");
    state.set_global("ARMED").expect("armed");
    state
        .set_top((fill - headroom) as isize)
        .expect("filler fits");
    state.get_global(function).expect("probe function fits");
    state.get_global(receiver).expect("receiver fits");
    let result = state.call(ArgCount::Fixed(1), RetCount::Fixed(1));
    let outcome = match result {
        Ok(()) => {
            let value = if state.typ(-1) == LuaType::Nil {
                "nil".to_string()
            } else {
                state.to_string(-1).expect("a string result")
            };
            Outcome::Value(value)
        }
        Err(error) => outcome(Err(error), state),
    };
    state.set_top(0).expect("stack clears");
    state.push_boolean(false).expect("flag fits");
    state.set_global("ARMED").expect("disarmed");
    outcome
}

#[test]
fn a_cached_miss_expires_when_the_lookup_becomes_direct() {
    // Each site first learns that the register path cannot answer (the
    // method table lacks the key, or the handler is a function), then the
    // heap changes so that it can. At the stack cap only the register path
    // succeeds, so a stale miss would show as StackOverflow.
    for (change, receiver, expected) in [
        ("Derived.greet = 'derived'", "two", "derived"),
        (
            "Derived.extra = nil Derived.greet = 'derived'",
            "two",
            "derived",
        ),
        (
            "getmetatable(by_function).__index = {greet = 'table'}",
            "by_function",
            "table",
        ),
    ] {
        let mut state = state_with(&format!(
            "Derived.extra = 1 ARMED = true cap_field({receiver}) cap_field({receiver}) ARMED = false"
        ));
        run(&mut state, change);
        assert_eq!(
            call_near_cap(&mut state, "cap_field", receiver, 0),
            value(expected),
            "{change}"
        );
    }
}

#[test]
fn stack_need_at_the_cap_does_not_depend_on_cache_history() {
    for warm in [false, true] {
        let mut state = state_with(if warm {
            "ARMED = true cap_field(two) cap_string('y') ARMED = false"
        } else {
            ""
        });
        // A one-level field read answers in its register, with no scratch,
        // even after its site resolved an inherited method.
        assert_eq!(
            call_near_cap(&mut state, "cap_field", "one", 0),
            value("one"),
            "warm {warm}"
        );
        // A string method read needs the receiver copy and its result, the
        // same two slots cold as with the string-method cache warm.
        state.push_string("x").expect("receiver fits");
        state.set_global("str").expect("receiver set");
        assert_eq!(
            call_near_cap(&mut state, "cap_string", "str", 1),
            value("X"),
            "warm {warm}"
        );
    }
}
