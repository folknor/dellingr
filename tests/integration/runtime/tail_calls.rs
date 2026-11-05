//! Proper tail calls: `return f(...)` replaces the calling activation. What
//! the replacement must keep (the callee's arguments, varargs and captured
//! values), what it releases (the caller's activation, its varargs and its
//! `__call` chain units), and how errors and tracebacks show it.

use dellingr::State;
use dellingr::error::ErrorKind;
use dellingr::limits::MAX_METAMETHOD_DEPTH;

use crate::support;
use crate::support::PushNewRustFn;

/// A Rust function that collects garbage, then allocates closures and tables
/// so that freed upvalue slots and objects are reused, and returns its
/// argument count.
fn churn(state: &mut State) -> dellingr::Result<u8> {
    let args = state.get_top();
    state.gc_collect();
    support::run(
        state,
        "for i = 1, 50 do
             local v = i
             local f = function() return v end
             local _ = {f, f()}
         end",
    );
    state.gc_collect();
    let args = u32::try_from(args).map_err(|_| {
        dellingr::error::Error::without_location(ErrorKind::HostError("argument count".into()))
    })?;
    state.push_number(f64::from(args))?;
    Ok(1)
}

#[test]
fn a_rust_tail_callee_can_collect_the_retired_caller() {
    // The caller's closure and its captured locals become unreachable while
    // the Rust callee runs (its function slot holds the callee); the
    // callee's own arguments, which capture those locals, stay intact.
    let mut state = State::new();
    state.gc_disable_auto();
    state.push_new_rust_fn(churn).unwrap();
    state.set_global("churn").unwrap();
    support::eval(
        &mut state,
        "local function make(n)
             local captured = {value = n}
             local private = {n}
             local reader = function() return captured.value end
             return function(...)
                 local box = {private[1], ...}
                 return churn(reader, box, ...)
             end, reader
         end
         local results = {}
         for i = 1, 20 do
             local _, reader = make(i)
             -- The caller closure lives only in its call's function slot.
             local count = make(i)(i, i + 1)
             results[#results + 1] = count .. ':' .. reader()
         end
         return table.concat(results, ',')",
        1,
    );
    let expected: Vec<String> = (1..=20).map(|i| format!("4:{i}")).collect();
    assert_eq!(state.to_bytes(-1).unwrap(), expected.join(",").as_bytes());
}

#[test]
fn a_tail_callee_reads_values_captured_from_the_retired_caller() {
    let result = support::eval_bytes(
        "local function apply(f, n) if n == 0 then return f() end return apply(f, n - 1) end
         local function make()
             local count = 0
             local function bump() count = count + 1 return count end
             bump()
             return apply(bump, 5000)
         end
         local function caller(...)
             local first = ...
             return select('#', ...), first, make()
         end
         local function forward(...) return caller(...) end
         local a, b, c = forward(10, 20, 30)
         return a .. ' ' .. b .. ' ' .. c",
    );
    assert_eq!(result, b"3 10 2");
}

#[test]
fn varargs_of_the_retired_caller_are_released_and_the_callees_kept() {
    let result = support::eval_bytes(
        "local function count(n, ...)
             if n == 0 then return select('#', ...), ... end
             return count(n - 1, ...)
         end
         local function grow(n, ...)
             if n == 0 then return count(3, ...) end
             return grow(n - 1, n, ...)
         end
         local total, first, second = grow(100)
         return total .. ' ' .. first .. ' ' .. second",
    );
    assert_eq!(result, b"100 1 2");
}

#[test]
fn a_tail_call_releases_the_retiring_callers_call_chain_units() {
    // Each call through `outer` resolves one non-function `__call` step. A
    // tail call ends the activation that held it, so tail recursion through
    // the chain runs past MAX_METAMETHOD_DEPTH; a non-tail one does not.
    let depth = MAX_METAMETHOD_DEPTH * 3;
    let result = support::eval_bytes(format!(
        "local outer
         local inner = setmetatable({{}}, {{__call = function(_, _, n)
             if n == 0 then return 'reached' end
             return outer(n - 1)
         end}})
         outer = setmetatable({{}}, {{__call = inner}})
         return outer({depth})"
    ));
    assert_eq!(result, b"reached");

    let err = support::run_error(format!(
        "local outer
         local inner = setmetatable({{}}, {{__call = function(_, _, n)
             if n == 0 then return 'reached' end
             return (outer(n - 1))
         end}})
         outer = setmetatable({{}}, {{__call = inner}})
         outer({depth})"
    ));
    assert!(
        matches!(err.kind(), ErrorKind::MetamethodDepthExceeded { .. }),
        "{err}"
    );
}

#[test]
fn tracebacks_mark_the_frames_a_tail_call_replaced() {
    let mut state = State::new();
    let program = dellingr::Program::compile(
        "local function fail() error('deep') end
         local function middle() return fail() end
         local function outer() middle() end
         outer()",
        Some(b"tail".as_slice()),
    )
    .unwrap();
    state.load(&program).unwrap();
    let err = state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(0))
        .expect_err("fail raises");
    let trace = err.stack_trace();
    let shape: Vec<_> = trace
        .iter()
        .map(|frame| (frame.line, frame.tail_called))
        .collect();
    // fail (entered by the tail call that replaced middle), outer, main.
    assert_eq!(shape, [(1, true), (3, false), (4, false)]);
    assert_eq!(
        err.to_string(),
        "tail:1: deep\nstack traceback:\n\ttail:1: in function 'fail'\n\t(...tail calls...)\
         \n\ttail:3: in function 'outer'\n\ttail:4: in main chunk"
    );
}

#[test]
fn error_level_two_in_a_tail_called_helper_blames_the_surviving_caller() {
    // The helper's tail caller is gone, so level 2 is the next activation
    // still running, as in reference Lua.
    let mut state = State::new();
    let program = dellingr::Program::compile(
        "local function check(x) if not x then error('bad value', 2) end return x end
         local function validate(x) return check(x) end
         local ok = validate(1)
         validate(nil)",
        Some(b"levels".as_slice()),
    )
    .unwrap();
    state.load(&program).unwrap();
    let err = state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(0))
        .expect_err("validate(nil) raises");
    assert!(err.to_string().starts_with("levels:4: bad value"), "{err}");
}

#[test]
fn a_refused_tail_call_is_reported_at_the_caller() {
    let mut state = State::new();
    let program = dellingr::Program::compile(
        "local function caller()
             local t = {}
             return t()
         end
         caller()",
        Some(b"refused".as_slice()),
    )
    .unwrap();
    state.load(&program).unwrap();
    let err = state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(0))
        .expect_err("a table is not callable");
    assert!(matches!(err.kind(), ErrorKind::TypeError(_)), "{err}");
    let lines: Vec<_> = err.stack_trace().iter().map(|frame| frame.line).collect();
    assert_eq!(lines, [3, 5]);
    assert!(err.stack_trace().iter().all(|frame| !frame.tail_called));
}
