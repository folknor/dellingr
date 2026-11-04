//! `State::set_table`, the host store that honours `__newindex`: it stores
//! exactly as a script's `t[k] = v` does, leaves the stack unchanged on every
//! failure before a handler is entered, consumes the key and value once one
//! was, returns every metamethod depth unit, and reports each execution error
//! to `on_error` once while host misuse stays unreported.

use std::sync::{Arc, Mutex};

use dellingr::error::{Error, ErrorKind, TypeError};
use dellingr::limits::{
    MAX_CALL_DEPTH, MAX_METAMETHOD_DEPTH, MAX_NATIVE_CALL_DEPTH, MAX_STACK_SIZE,
};
use dellingr::{ArgCount, HostCallbacks, LuaType, RetCount, State};

use crate::support::PushNewRustFn;
use crate::support::{self, stack};

/// Counts `on_error` notifications.
struct ErrorCounter(Arc<Mutex<usize>>);

impl HostCallbacks for ErrorCounter {
    fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, _message: &[u8]) {}

    fn on_error(&mut self, _error: &Error) {
        *self.0.lock().unwrap() += 1;
    }
}

fn counting_state() -> (State, Arc<Mutex<usize>>) {
    let count = Arc::new(Mutex::new(0));
    let state = State::builder()
        .callbacks(Box::new(ErrorCounter(Arc::clone(&count))))
        .build();
    (state, count)
}

/// Push the value `expr` evaluates to.
fn push(state: &mut State, expr: &str) {
    support::eval(state, format!("return {expr}"), 1);
}

/// Global `name` rendered with `tostring`, leaving the stack as it was.
fn global(state: &mut State, name: &str) -> String {
    state.get_global(name).unwrap();
    let rendered = state.to_string_with_meta(-1).unwrap();
    state.pop(1).unwrap();
    rendered
}

#[test]
fn plain_tables_store_update_and_remove_like_a_script() {
    let mut state = State::new();
    push(&mut state, "{a = 1}");
    for (key, value, expected) in [("b", "2", "2"), ("a", "3", "3"), ("a", "nil", "nil")] {
        state.push_string(key).unwrap();
        push(&mut state, value);
        state.set_table(1).unwrap();
        assert_eq!(state.get_top(), 1, "the key and value are consumed");
        state.push_string(key).unwrap();
        state.get_table_raw(1).unwrap();
        assert_eq!(state.to_string_with_meta(-1).unwrap(), expected, "{key}");
        state.pop(1).unwrap();
    }
}

#[test]
fn stores_go_through_newindex_tables_and_functions() {
    let mut state = State::new();
    support::run(
        &mut state,
        "sink = {}
         by_table = setmetatable({}, {__newindex = setmetatable({}, {__newindex = sink})})
         by_function = setmetatable({}, {__newindex = function(t, k, v) seen = {t, k, v} end})
         held = setmetatable({x = 0}, {__newindex = function() error('not for existing keys') end})",
    );
    for (receiver, key) in [("by_table", "x"), ("by_function", "y"), ("held", "x")] {
        state.get_global(receiver).unwrap();
        state.push_string(key).unwrap();
        state.push_number(4.0).unwrap();
        state.set_table(1).unwrap();
        assert_eq!(state.get_top(), 1, "{receiver}");
        state.set_top(0).unwrap();
    }
    support::eval(
        &mut state,
        "return rawget(sink, 'x') == 4 and rawget(by_table, 'x') == nil
            and seen[1] == by_function and seen[2] == 'y' and seen[3] == 4
            and rawget(by_function, 'y') == nil and held.x == 4",
        1,
    );
    assert!(state.to_boolean(-1));
}

/// A nil or NaN key reaches a `__newindex` function, as in a script; only a
/// raw store refuses it, and that refusal leaves the stack unchanged.
#[test]
fn nil_and_nan_keys_reach_a_handler_and_are_refused_raw() {
    for key in ["nil", "0/0"] {
        let mut state = State::new();
        support::run(
            &mut state,
            "got = nil
             by_function = setmetatable({}, {__newindex = function(_, k) got = tostring(k) end})",
        );
        state.get_global("by_function").unwrap();
        push(&mut state, key);
        state.push_number(1.0).unwrap();
        state.set_table(1).unwrap();
        assert_eq!(state.get_top(), 1);
        // The handler ran and saw the key: `got` is its rendering.
        state.get_global("got").unwrap();
        assert_eq!(state.typ(-1), LuaType::String, "{key}");
        state.pop(1).unwrap();

        state.set_top(0).unwrap();
        state.new_table().unwrap();
        push(&mut state, key);
        state.push_number(1.0).unwrap();
        let error = state.set_table(1).expect_err("a raw store refuses the key");
        assert!(matches!(
            error.kind(),
            ErrorKind::TypeError(TypeError::TableKeyNil | TypeError::TableKeyNan)
        ));
        assert_eq!(state.get_top(), 3, "{key}: nothing consumed");
    }
}

/// Failures before any handler runs: the stack is exactly as the host left
/// it, and only execution errors are reported.
#[test]
fn failures_before_entry_leave_the_stack_and_report_only_execution_errors() {
    let cases: [(&str, &str, Option<&str>, bool); 5] = [
        // (receiver, key, expected kind, reported)
        ("7", "'k'", Some("type"), true),
        (
            "setmetatable({}, {__newindex = 5})",
            "'k'",
            Some("type"),
            true,
        ),
        (
            "(function() local t = {} setmetatable(t, {__newindex = t}) return t end)()",
            "'k'",
            None,
            true,
        ),
        ("{}", "nil", Some("key"), true),
        ("{}", "0/0", Some("key"), true),
    ];
    for (receiver, key, kind, reported) in cases {
        let (mut state, count) = counting_state();
        state.push_number(73.0).unwrap();
        push(&mut state, receiver);
        push(&mut state, key);
        state.push_string("v").unwrap();
        let receiver_type = state.typ(2);
        let error = state.set_table(2).expect_err(receiver);
        match kind {
            Some("type") => assert!(
                matches!(error.kind(), ErrorKind::TypeError(_)),
                "{receiver}"
            ),
            Some("key") => assert!(matches!(
                error.kind(),
                ErrorKind::TypeError(TypeError::TableKeyNil | TypeError::TableKeyNan)
            )),
            _ => assert!(
                matches!(error.kind(), ErrorKind::MetamethodDepthExceeded { .. }),
                "{receiver}: {error}"
            ),
        }
        assert_eq!(state.get_top(), 4, "{receiver}");
        assert_eq!(state.to_number(1).unwrap(), 73.0);
        assert_eq!(state.typ(2), receiver_type);
        assert_eq!(state.to_bytes(4).unwrap(), b"v");
        assert_eq!(*count.lock().unwrap(), usize::from(reported), "{receiver}");
    }
}

#[test]
fn host_misuse_is_refused_unreported() {
    let (mut state, count) = counting_state();
    state.new_table().unwrap();
    state.push_string("k").unwrap();
    // Only one operand above the table.
    assert!(matches!(
        state.set_table(1).unwrap_err().kind(),
        ErrorKind::InvalidStackIndex { .. }
    ));
    state.push_string("v").unwrap();
    assert!(matches!(
        state.set_table(9).unwrap_err().kind(),
        ErrorKind::InvalidStackIndex { index: 9 }
    ));
    assert_eq!(state.get_top(), 3);
    assert_eq!(*count.lock().unwrap(), 0);
}

/// A handler whose Lua frame cannot fit under the stack cap is refused at
/// admission, before entry: the call window fits, the frame does not, and
/// the operands stay.
#[test]
fn a_handler_frame_that_cannot_fit_leaves_the_operands() {
    let mut state = State::new();
    let results = vec!["1"; 64].join(", ");
    push(
        &mut state,
        &format!("setmetatable({{}}, {{__newindex = function() return {results} end}})"),
    );
    state.set_top((MAX_STACK_SIZE - 8) as isize).unwrap();
    state.push_string("k").unwrap();
    state.push_string("v").unwrap();
    let top = state.get_top();
    let error = state.set_table(1).unwrap_err();
    assert!(
        matches!(error.kind(), ErrorKind::StackOverflow { .. }),
        "{error}"
    );
    assert_eq!(state.get_top(), top);
    assert_eq!(state.to_bytes(-1).unwrap(), b"v");
}

/// An entered handler's error consumes the key and value, keeps the
/// handler's writes, is reported once, and returns its depth units: a
/// callback that catches it can store again, many times over.
#[test]
fn an_entered_handler_error_consumes_the_operands_and_returns_its_units() {
    let (mut state, count) = counting_state();
    support::run(
        &mut state,
        "writes = 0
         failing = setmetatable({}, {__newindex = setmetatable({}, {
             __newindex = function() writes = writes + 1 error('handler') end,
         })})",
    );
    state.push_number(73.0).unwrap();
    state.get_global("failing").unwrap();
    state.push_string("k").unwrap();
    state.push_string("v").unwrap();
    let error = state.set_table(2).expect_err("the handler fails");
    assert!(
        matches!(error.kind(), ErrorKind::ScriptError { .. }),
        "{error}"
    );
    assert_eq!(state.get_top(), 2);
    assert_eq!(state.to_number(1).unwrap(), 73.0);
    assert_eq!(state.typ(2), LuaType::Table);
    assert_eq!(*count.lock().unwrap(), 1);

    // Each failed store held two units; leaked units would exhaust the
    // depth long before the loop ends.
    state.set_top(0).unwrap();
    state.push_new_rust_fn(store_and_swallow).unwrap();
    state.set_global("store_and_swallow").unwrap();
    support::run(
        &mut state,
        format!(
            "for _ = 1, {} do store_and_swallow(failing) end",
            MAX_METAMETHOD_DEPTH * 2
        ),
    );
    let failures = usize::try_from(MAX_METAMETHOD_DEPTH * 2 + 1).unwrap();
    assert_eq!(global(&mut state, "writes"), failures.to_string());
    assert_eq!(*count.lock().unwrap(), failures);
}

fn store_and_swallow(state: &mut State) -> dellingr::Result<u8> {
    state.push_string("k")?;
    state.push_string("v")?;
    let top = state.get_top();
    let error = state.set_table(1).expect_err("the handler fails");
    assert!(matches!(error.kind(), ErrorKind::ScriptError { .. }));
    assert_eq!(state.get_top(), top - 2);
    Ok(0)
}

fn probe_native_store_refusal(state: &mut State) -> dellingr::Result<u8> {
    state.push_string("k")?;
    state.push_string("v")?;
    let top = state.get_top();
    let error = state.set_table(1).unwrap_err();
    assert!(matches!(
        error.kind(),
        ErrorKind::NativeCallDepthExceeded { .. }
    ));
    assert_eq!(state.get_top(), top);
    assert_eq!(state.to_bytes(-1)?, b"v");
    Ok(0)
}

fn recurse_host_store(state: &mut State) -> dellingr::Result<u8> {
    let remaining = state.to_number(1)?;
    state.get_global("recurse")?;
    state.push_number(remaining)?;
    state.call(ArgCount::Fixed(1), RetCount::Fixed(0))?;
    Ok(0)
}

fn host_store_native_refusal() -> dellingr::Result<()> {
    let mut state = State::new();
    state.push_new_rust_fn(probe_native_store_refusal)?;
    state.set_global("probe")?;
    state.push_new_rust_fn(recurse_host_store)?;
    state.set_global("step")?;
    dellingr::Program::compile(
        format!(
            "local t = setmetatable({{}}, {{__newindex = function() end}})
             function recurse(n)
                 if n == 0 then probe(t) else step(n - 1) end
             end
             recurse({})",
            MAX_NATIVE_CALL_DEPTH - 1,
        ),
        None,
    )
    .and_then(|program| state.load(&program))?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))?;
    assert_eq!(state.get_top(), 0);
    Ok(())
}

#[test]
fn the_operands_stay_at_the_native_call_depth_limit() {
    stack::on_limit_test_stack("host_store_native_refusal", || {
        host_store_native_refusal().unwrap();
    });
}

fn probe_lua_store_refusal(state: &mut State) -> dellingr::Result<u8> {
    state.push_string("k")?;
    state.push_string("v")?;
    let top = state.get_top();
    let error = state.set_table(1).unwrap_err();
    assert!(matches!(error.kind(), ErrorKind::CallDepthExceeded { .. }));
    assert_eq!(state.get_top(), top);
    assert_eq!(state.to_bytes(-1)?, b"v");
    Ok(0)
}

#[test]
fn the_operands_stay_at_the_lua_call_depth_limit() {
    let mut state = State::new();
    state.push_new_rust_fn(probe_lua_store_refusal).unwrap();
    state.set_global("probe").unwrap();
    support::run(
        &mut state,
        format!(
            "local t = setmetatable({{}}, {{__newindex = function() end}})
             local function recurse(n)
                 if n == 0 then probe(t) else recurse(n - 1) end
             end
             recurse({})",
            MAX_CALL_DEPTH - 2,
        ),
    );
    assert_eq!(state.get_top(), 0);
}

#[test]
fn a_store_charges_no_cost() {
    let mut state = State::new();
    support::run(
        &mut state,
        "t = setmetatable({}, {__newindex = function(t, k, v) rawset(t, k, v) end})",
    );
    state.get_global("t").unwrap();
    let before = state.cost_used();
    state.push_string("k").unwrap();
    state.push_number(1.0).unwrap();
    state.set_table(1).unwrap();
    // The handler's own rawset is the only charge.
    assert_eq!(state.cost_used() - before, 1);
}
