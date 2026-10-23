//! A Lua frame's admission reserves everything its own instructions can
//! need, so an admitted frame never fails the stack cap in its body except
//! where a call is refused (a callee's admission, a Rust callee's pushes) or
//! open results grow past it.
//!
//! Each probe is a Lua function called with its frame as close to
//! `limits::MAX_STACK_SIZE` as admission allows. Unarmed, it returns before
//! its body's first instruction of interest; armed, at the same height, it
//! must run that instruction to completion. The instructions covered are the
//! ones that once copied operands above the frame or padded results there:
//! closures, template constructors, concatenation, every table read and
//! store route that calls no handler (field and key spellings, string
//! receivers, `__index` and `__newindex` table chains), length through a
//! metatable without `__len`, and a generic `for` over a Rust iterator with
//! more loop variables than it returns.

use dellingr::limits::MAX_STACK_SIZE;
use dellingr::{ArgCount, RetCount, State};

const SETUP: &str = r#"
One = {greet = "one"}
One.__index = One
one = setmetatable({}, One)
Base = {greet = "base"}
Base.__index = Base
Derived = setmetatable({}, {__index = Base})
Derived.__index = Derived
Deeper = setmetatable({}, {__index = Derived})
Deeper.__index = Deeper
three = setmetatable({}, Deeper)
plain = {}
sink = {}
proxy = setmetatable({}, {__newindex = setmetatable({}, {__newindex = sink})})
measured = setmetatable({1, 2, 3}, {})
key = "greet"
"#;

/// A Rust iterator that ends the loop by returning nothing.
fn empty_iterator(_: &mut State) -> dellingr::Result<u8> {
    Ok(0)
}

fn state_with(extra: &str) -> State {
    let mut state = State::new();
    state.push_rust_fn(empty_iterator).expect("iterator fits");
    state.set_global("empty_iterator").expect("iterator set");
    for source in [SETUP, extra] {
        dellingr::Program::compile(source, None)
            .and_then(|program| state.load(&program))
            .expect("setup compiles");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .unwrap_or_else(|error| panic!("setup fails: {error}"));
    }
    state
}

/// What a probe call gave: its string result, or its error's text.
#[derive(Debug, PartialEq)]
enum Outcome {
    Returned(String),
    Failed(String),
}

fn done() -> Outcome {
    Outcome::Returned("done".to_string())
}

/// Defines `probe(armed)` around `body` and runs it armed at the highest
/// stack height that admits it.
fn run_at_cap(state: &mut State, body: &str) -> Outcome {
    let source = format!(
        "function probe(armed)
             if not armed then return 'entered' end
             {body}
             return 'done'
         end"
    );
    dellingr::Program::compile(source, None)
        .and_then(|program| state.load(&program))
        .expect("probe compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("probe defined");
    let call = |state: &mut State, fill: usize, armed: bool| {
        state.set_top(fill as isize).expect("filler fits");
        state.get_global("probe").expect("probe fits");
        state.push_boolean(armed).expect("flag fits");
        let result = state.call(ArgCount::Fixed(1), RetCount::Fixed(1));
        let outcome = match result {
            Ok(()) => Outcome::Returned(state.to_string(-1).expect("a string result")),
            Err(error) => Outcome::Failed(error.to_string()),
        };
        state.set_top(0).expect("stack clears");
        outcome
    };
    let mut fill = MAX_STACK_SIZE - 2;
    while matches!(call(state, fill, false), Outcome::Failed(_)) {
        fill -= 1;
    }
    call(state, fill, true)
}

#[test]
fn an_admitted_frame_runs_every_handler_free_instruction_at_the_cap() {
    let mut state = state_with("");
    for body in [
        "local captured = armed local f = function() return captured end",
        "local t = {x = 1, y = 2, z = 3}",
        "local a, b, c = 'a', 'b', 'c' local s = a .. b .. c .. 1",
        "local v = one.greet",
        "local v = one['greet']",
        "local v = one[key]",
        "local v = three.greet",
        "local v = three[key]",
        "local v = one.absent",
        "local v = three.absent",
        "local v = ('x'):upper()",
        "local s = 'x' local v = s[('upper')]",
        "plain.newkey = 1 plain.newkey = nil",
        "plain[key] = 1 plain[key] = nil",
        "proxy.x = 1",
        "proxy[key] = 1",
        "local n = #measured",
        "for a, b, c, d, e, f in empty_iterator do end",
    ] {
        assert_eq!(run_at_cap(&mut state, body), done(), "{body}");
    }
    assert_eq!(
        (sink_value(&mut state, "x"), sink_value(&mut state, "greet")),
        (1.0, 1.0),
        "the __newindex chain stored into its last table"
    );
}

fn sink_value(state: &mut State, field: &str) -> f64 {
    state.get_global("sink").expect("sink fits");
    state.push_string(field).expect("key fits");
    state.get_table(-2).expect("raw sink read");
    let value = state.to_number(-1).expect("a stored number");
    state.set_top(0).expect("stack clears");
    value
}

#[test]
fn a_string_chain_answers_at_the_cap() {
    let mut state = state_with("getmetatable('').__index = setmetatable({}, {__index = string})");
    for body in [
        "local v = ('x'):upper()",
        "local s = 'x' local v = s[('upper')]",
    ] {
        assert_eq!(run_at_cap(&mut state, body), done(), "{body}");
    }
}

#[test]
fn a_handler_call_can_still_be_refused_at_the_cap() {
    // The window fits; the handler's own frame, a callee, does not.
    let mut state =
        state_with("by_function = setmetatable({}, {__index = function(_, k) return k end})");
    let outcome = run_at_cap(&mut state, "local v = by_function.greet");
    assert!(
        matches!(&outcome, Outcome::Failed(message) if message.contains("stack overflow")),
        "{outcome:?}"
    );
}
