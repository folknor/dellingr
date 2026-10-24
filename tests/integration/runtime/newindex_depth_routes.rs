//! A `__newindex` store counts metamethod depth by the same rule as an
//! `__index` lookup (`tests/index_depth_routes.rs`): a terminal handler
//! table (it holds the key raw, or has no `__newindex` of its own) takes the
//! store into its own contents and no depth unit; a handler table that
//! continues to its own `__newindex`, and a function handler, each take one
//! until the store ends; a handler that is neither a table nor callable is a
//! type error raised without a unit. It holds for `t.k = v`, `t["k"] = v` and
//! table-library writes, with a cold set cache, after the sites stored
//! before, and after a snapshot round trip.
//!
//! The depth is saturated without native recursion: a call through a
//! non-function `__call` step holds one unit for as long as the callee runs.

use dellingr::error::{ErrorKind, TypeError};
use dellingr::limits::MAX_METAMETHOD_DEPTH;
use dellingr::{ArgCount, RetCount, State};

use crate::support;
use crate::support::PushNewRustFn;

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

local function log(_, _, v) Log = v end

-- Terminal: the handler table has no metatable.
Sink = {}
one = setmetatable({}, {__newindex = Sink})
-- Terminal: the handler table holds the key, so its own function
-- `__newindex` is never reached.
Holder = setmetatable({greet = "old"}, {__newindex = function() error("not reached") end})
held = setmetatable({}, {__newindex = Holder})
-- One table step that continues (Mid dispatches to Sink2).
Sink2 = {}
Mid = setmetatable({}, {__newindex = Sink2})
two = setmetatable({}, {__newindex = Mid})
-- Two steps that continue.
Upper = setmetatable({}, {__newindex = Mid})
three = setmetatable({}, {__newindex = Upper})
-- A function handler, alone and behind a table that dispatches it.
by_function = setmetatable({}, {__newindex = log})
function_behind_table = setmetatable({}, {__newindex = setmetatable({}, {__newindex = log})})
bad = setmetatable({}, {__newindex = 5})
-- Two table steps that continue, then a handler that is not callable.
bad_chain = setmetatable({}, {__newindex = setmetatable({}, {
    __newindex = setmetatable({}, {__newindex = 5}),
})})

local sink_of = {[one] = Sink, [held] = Holder, [two] = Sink2, [three] = Sink2}

-- Where the last store through `o` landed.
function landed(o, k)
    local sink = sink_of[o]
    if sink then return rawget(sink, k) end
    return Log
end

function reset()
    rawset(Sink, "greet", nil) rawset(Sink, 1, nil)
    rawset(Sink2, "greet", nil) rawset(Sink2, 1, nil)
    Log = nil
end

function field_site(o) o.greet = "stored" end
function key_site(o, k) o[k] = "stored" end
function insert_site(o) table.insert(o, "stored") end
"#;

/// Stores at depth 0 through every site and receiver, so each site has seen
/// a chain, a function handler and a terminal table before the probes.
const WARMUP: &str = r#"
for _, o in ipairs({one, held, two, three, by_function, function_behind_table}) do
    field_site(o) key_site(o, "greet")
end
for _, o in ipairs({one, two, three, by_function, function_behind_table}) do
    insert_site(o)
end
reset()
"#;

fn state_with(source: &str) -> State {
    let mut state = State::new();
    support::run(&mut state, PRELUDE);
    support::run(&mut state, source);
    state
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Value(String),
    DepthExceeded,
    HandlerTypeError,
    NanKey,
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
            ErrorKind::TypeError(TypeError::TableIndex(_)) => Outcome::HandlerTypeError,
            ErrorKind::TypeError(TypeError::TableKeyNan) => Outcome::NanKey,
            other => panic!("unexpected error {other:?}"),
        },
    }
}

/// `store` run with the metamethod depth at `depth`, then `landed` read
/// back: what the store left where its chain ends.
fn probe(state: &mut State, depth: u32, store: &str, landed: &str) -> Outcome {
    let source = format!(
        "return tostring(at_depth({depth}, function() reset() {store} return {landed} end))"
    );
    support::load(state, source);
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(1));
    let outcome = outcome(result, state);
    assert_eq!(state.get_top(), 0);
    outcome
}

/// Every store with the units its steps nest, through each route.
fn cases() -> Vec<(String, String, u32)> {
    let mut cases = Vec::new();
    for (receiver, units) in [
        ("one", 0),
        ("held", 0),
        ("two", 1),
        ("three", 2),
        ("by_function", 1),
        ("function_behind_table", 2),
    ] {
        let landed = format!("landed({receiver}, 'greet')");
        cases.push((format!("field_site({receiver})"), landed.clone(), units));
        cases.push((format!("key_site({receiver}, 'greet')"), landed, units));
        // The handler table holds `greet`, not the appended key.
        if receiver != "held" {
            cases.push((
                format!("insert_site({receiver})"),
                format!("landed({receiver}, 1)"),
                units,
            ));
        }
    }
    cases
}

/// Each case lands its store with exactly its units left and is refused
/// with one fewer.
fn check_limit_edges(state: &mut State, label: &str) {
    for (store, landed, units) in cases() {
        let fits = MAX_METAMETHOD_DEPTH - units;
        assert_eq!(
            probe(state, fits, &store, &landed),
            Outcome::Value("stored".to_string()),
            "{label}: {store} at {fits}"
        );
        if units > 0 {
            assert_eq!(
                probe(state, fits + 1, &store, &landed),
                Outcome::DepthExceeded,
                "{label}: {store} at {}",
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
fn limit_edges_after_the_sites_stored() {
    let mut state = state_with(WARMUP);
    check_limit_edges(&mut state, "warm");
}

#[test]
fn a_handler_that_cannot_be_stored_into_is_a_type_error_at_any_depth() {
    let mut state = state_with(WARMUP);
    for store in [
        "field_site(bad)",
        "key_site(bad, 'greet')",
        "insert_site(bad)",
    ] {
        for depth in [0, MAX_METAMETHOD_DEPTH] {
            assert_eq!(
                probe(&mut state, depth, store, "nil"),
                Outcome::HandlerTypeError,
                "{store} at {depth}"
            );
        }
    }
}

#[test]
fn an_invalid_handler_behind_tables_fails_after_their_units() {
    // Its two table steps continue, so they need two units before the type
    // error can be reached.
    let mut state = state_with("");
    for store in [
        "field_site(bad_chain)",
        "key_site(bad_chain, 'greet')",
        "insert_site(bad_chain)",
    ] {
        let fits = MAX_METAMETHOD_DEPTH - 2;
        assert_eq!(
            probe(&mut state, fits, store, "nil"),
            Outcome::HandlerTypeError,
            "{store}"
        );
        assert_eq!(
            probe(&mut state, fits + 1, store, "nil"),
            Outcome::DepthExceeded,
            "{store}"
        );
    }
}

#[test]
fn a_terminal_insert_error_is_not_masked_by_the_depth_limit() {
    // The terminal table's raw insert rejects a NaN key. It takes no unit,
    // so at the limit that rejection is what reports.
    let mut state = state_with("");
    assert_eq!(
        probe(
            &mut state,
            MAX_METAMETHOD_DEPTH,
            "key_site(one, 0/0)",
            "nil"
        ),
        Outcome::NanKey
    );
    // Behind one continuing step, the rejection needs that step's unit.
    assert_eq!(
        probe(
            &mut state,
            MAX_METAMETHOD_DEPTH - 1,
            "key_site(two, 0/0)",
            "nil"
        ),
        Outcome::NanKey
    );
    assert_eq!(
        probe(
            &mut state,
            MAX_METAMETHOD_DEPTH,
            "key_site(two, 0/0)",
            "nil"
        ),
        Outcome::DepthExceeded
    );
}

/// A Rust callback that runs the global function `store_probe` with its
/// first argument through `State::call` and reports whether it failed,
/// swallowing the error.
fn try_store(state: &mut State) -> dellingr::Result<u8> {
    state.get_global("store_probe")?;
    state.push_value(1)?;
    let failed = state.call(ArgCount::Fixed(1), RetCount::Fixed(0)).is_err();
    state.set_top(0)?;
    state.push_boolean(failed)?;
    Ok(1)
}

#[test]
fn a_failed_store_returns_every_unit_it_took() {
    // Each store fails after taking units, inside a callback that carries
    // on, then a store needing the remaining unit runs at the same depth.
    let mut state = state_with(
        "function store_probe(o) if o == 'nan' then key_site(two, 0/0) else field_site(o) end end",
    );
    state.push_new_rust_fn(try_store).expect("callback fits");
    state.set_global("try_store").expect("callback set");
    for receiver in ["bad_chain", "bad", "'nan'"] {
        let store = format!(
            "local a = try_store({receiver}) local b = try_store({receiver}) \
             if not (a and b) then error('the store succeeded') end field_site(two)"
        );
        assert_eq!(
            probe(
                &mut state,
                MAX_METAMETHOD_DEPTH - 1,
                &store,
                "landed(two, 'greet')"
            ),
            Outcome::Value("stored".to_string()),
            "{receiver}"
        );
    }
}

#[cfg(feature = "snapshot")]
#[test]
fn a_snapshot_round_trip_keeps_every_limit_edge() {
    let mut state = state_with(WARMUP);
    let save = state.save_state().expect("state saves");
    let mut loaded = State::load_state(
        &save.bytes,
        Box::new(dellingr::DefaultCallbacks),
        support::no_setup,
    )
    .expect("save loads");
    for (store, landed, units) in cases() {
        for depth in [
            MAX_METAMETHOD_DEPTH - units,
            MAX_METAMETHOD_DEPTH - units + 1,
        ] {
            if depth > MAX_METAMETHOD_DEPTH {
                continue;
            }
            assert_eq!(
                probe(&mut state, depth, &store, &landed),
                probe(&mut loaded, depth, &store, &landed),
                "{store} at {depth}: the continuing and the loaded State diverge"
            );
        }
    }
    check_limit_edges(&mut loaded, "loaded");
}
