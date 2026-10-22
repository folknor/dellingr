//! Recursion limits that bound native-stack growth, driven to their
//! boundaries together on a thread with the stack docs/limits.md states
//! they fit in (`LIMIT_TEST_STACK_BYTES`), unoptimized builds included.
//!
//! Every scenario recurses through one kind of call made from Rust until
//! `MAX_NATIVE_CALL_DEPTH` is exhausted, holds a `__index` table chain
//! across the last entry so the metamethod depth is at its limit too, and
//! at the top runs the deepest leaf work a callback can: a pattern match
//! near `MAXCCALLS`, allocation enough to collect with a maximally nested
//! chunk tree alive, and either one more call from Rust (which must be
//! refused cleanly) or nothing (which must succeed).

use dellingr::error::ErrorKind;
use dellingr::limits::{MAX_METAMETHOD_DEPTH, MAX_NATIVE_CALL_DEPTH};
use dellingr::{ArgCount, RetCount, State};

#[path = "common/stack.rs"]
mod stack;

fn run(source: &str) -> dellingr::Result<()> {
    let mut state = State::new();
    dellingr::Program::compile(source, None).and_then(|program| state.load(&program))?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
}

/// `callthrough(f, n)` calls `f(n)` from Rust through the host API.
fn callthrough(state: &mut State) -> dellingr::Result<u8> {
    state.push_value(1)?;
    state.push_value(2)?;
    state.call(ArgCount::Fixed(1), RetCount::Fixed(0))?;
    Ok(0)
}

/// One level of native recursion: Lua source that calls `deep(n + 1)`
/// through exactly one call made from Rust.
struct Cycle {
    name: &'static str,
    step: &'static str,
    /// Whether each level also holds one unit of metamethod depth.
    metamethod: bool,
}

const CYCLES: &[Cycle] = &[
    Cycle {
        name: "sort_comparator",
        step: "local first = true
            table.sort({2, 1}, function(a, b)
                if first then first = false deep(n + 1) end
                return a < b
            end)",
        metamethod: false,
    },
    Cycle {
        name: "gsub_function",
        step: "string.gsub('a', 'a', function() deep(n + 1) end)",
        metamethod: false,
    },
    Cycle {
        name: "index_function",
        step: "local t = setmetatable({}, {__index = function() deep(n + 1) end})
            local _ = t.x",
        metamethod: true,
    },
    Cycle {
        name: "newindex_function",
        step: "setmetatable({}, {__newindex = function() deep(n + 1) end}).x = 1",
        metamethod: true,
    },
    Cycle {
        name: "tostring_metamethod",
        step: "tostring(setmetatable({}, {__tostring = function() deep(n + 1) return '' end}))",
        metamethod: false,
    },
    Cycle {
        name: "generic_for_iterator",
        step: "local first = true
            for _ in function()
                if first then first = false deep(n + 1) end
                return nil
            end do end",
        metamethod: false,
    },
    Cycle {
        name: "host_callthrough",
        step: "callthrough(deep, n + 1)",
        metamethod: false,
    },
];

/// Source for one scenario. The host call is the first call made from
/// Rust, `deep(0)` runs inside it, and each `deep` level adds one through
/// its cycle; `top` then spends the last one on the chain's function
/// handler, so `leaf` runs with every native call slot in use.
fn scenario(cycle: &Cycle, refuse_one_more: bool) -> String {
    let max_native = MAX_NATIVE_CALL_DEPTH;
    let levels = max_native - 2;
    let used_by_levels = if cycle.metamethod { levels } else { 0 };
    let chain = MAX_METAMETHOD_DEPTH - used_by_levels - 1;
    let nested_functions = 190;
    let finish = if refuse_one_more {
        "table.sort({2, 1}, function(a, b) return a < b end)"
    } else {
        ""
    };
    let step = cycle.step;
    format!(
        r#"
        -- A deeply nested chunk tree, alive with this chunk, for the
        -- collector to walk.
        {open}{close}
        local pattern, subject = "", ""
        for _ = 1, 190 do pattern = pattern .. "a?" subject = subject .. "a" end
        local function leaf()
            -- The matcher recurses once per optional item.
            if string.find(subject, pattern) ~= 1 then error("pattern did not match") end
            -- Enough allocation to run collections at this depth.
            for _ = 1, 500 do local _ = {{}} end
            {finish}
        end
        local chain = setmetatable({{}}, {{__index = function() leaf() end}})
        for _ = 1, {chain} do chain = setmetatable({{}}, {{__index = chain}}) end
        local function top() local _ = chain.missing end
        function deep(n)
            if n == {levels} then top() return end
            {step}
        end
        deep(0)
        "#,
        open = "local function f() ".repeat(nested_functions),
        close = " end".repeat(nested_functions),
    )
}

fn run_scenario(cycle: &Cycle, refuse_one_more: bool) -> dellingr::Result<()> {
    let mut state = State::new();
    state.push_rust_fn(callthrough)?;
    state.set_global("callthrough")?;
    dellingr::Program::compile(scenario(cycle, refuse_one_more), None)
        .and_then(|program| state.load(&program))?;
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    // Whatever happened, the State is usable and back at rest.
    dellingr::Program::compile("return 7", None).and_then(|program| state.load(&program))?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1))?;
    assert_eq!(state.to_number(-1)?, 7.0, "{}: State reuse", cycle.name);
    result
}

#[test]
fn every_limit_at_once_fits_the_limit_test_stack() {
    stack::on_limit_test_stack("every_limit_at_once", || {
        for cycle in CYCLES {
            run_scenario(cycle, false).unwrap_or_else(|error| {
                panic!("{}: the last admitted call failed: {error}", cycle.name)
            });
            let error = run_scenario(cycle, true)
                .expect_err("the call past MAX_NATIVE_CALL_DEPTH must be refused");
            match error.kind() {
                ErrorKind::NativeCallDepthExceeded { depth, limit } => {
                    assert_eq!(*depth, MAX_NATIVE_CALL_DEPTH, "{}", cycle.name);
                    assert_eq!(*limit, MAX_NATIVE_CALL_DEPTH, "{}", cycle.name);
                }
                other => panic!(
                    "{}: expected NativeCallDepthExceeded, got {other}",
                    cycle.name
                ),
            }
        }
    });
}

#[test]
fn recursive_index_function_stops_at_the_native_call_limit() {
    // Every handler is a call made from Rust, and the native limit is the
    // lower one, so it refuses the recursion before the metamethod depth
    // would.
    const { assert!(MAX_NATIVE_CALL_DEPTH < MAX_METAMETHOD_DEPTH) };
    stack::on_limit_test_stack("recursive_index_function", || {
        let error = run(r#"
            local t = setmetatable({}, {__index = function(t, k) return t[k + 1] end})
            local _ = t[1]
            "#)
        .expect_err("unbounded __index recursion must fail");
        match error.kind() {
            ErrorKind::NativeCallDepthExceeded { depth, limit } => {
                assert_eq!(*depth, MAX_NATIVE_CALL_DEPTH);
                assert_eq!(*limit, MAX_NATIVE_CALL_DEPTH);
            }
            other => panic!("expected NativeCallDepthExceeded, got {other}"),
        }
    });
}

/// The parser's deepest accepted nesting, with the innermost function
/// resolving a name from the outermost scope (the upvalue walk recurses
/// through every enclosing function while the parse descent is live), and
/// the first rejected one, both on the limit-test stack.
#[test]
fn deepest_nested_functions_parse_on_the_limit_test_stack() {
    stack::on_limit_test_stack("deepest_nested_functions", || {
        let source = |depth: usize| {
            format!(
                "local outer = 1 {}return outer{}",
                "local function f() ".repeat(depth),
                " end".repeat(depth)
            )
        };
        let mut deepest = 0;
        for depth in 1.. {
            let mut state = State::new();
            match dellingr::Program::compile(source(depth), None)
                .and_then(|program| state.load(&program))
            {
                Ok(()) => deepest = depth,
                Err(error) => {
                    assert!(
                        matches!(error.kind(), ErrorKind::SyntaxError(_)),
                        "depth {depth}: {error}"
                    );
                    break;
                }
            }
        }
        assert!(deepest > 100, "nesting stopped early, at {deepest}");
    });
}
