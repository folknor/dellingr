use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

use crate::support::{self, stack};

#[test]
fn cyclic_call_metamethod_returns_depth_error_and_state_recovers() {
    let mut state = State::new();
    support::load(
        &mut state,
        "local callable = {}; setmetatable(callable, { __call = callable }); callable()",
    );

    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("a cyclic __call chain should hit the metamethod depth limit");
    assert!(matches!(
        error.kind(),
        ErrorKind::MetamethodDepthExceeded { .. }
    ));
    assert_eq!(state.get_top(), 0);

    support::eval(&mut state, "return 7", 1);
    assert_eq!(
        state.to_number(-1).expect("the result should be numeric"),
        7.0
    );
}

#[test]
fn recursion_through_a_function_valued_call_metamethod_is_not_capped_at_metamethod_depth() {
    stack::on_limit_test_stack(
        "recursion_through_a_function_valued_call_metamethod",
        function_valued_call_recursion,
    );
}

fn function_valued_call_recursion() {
    let mut state = State::new();
    support::eval(
        &mut state,
        r#"
            local counter = setmetatable({}, {
                __call = function(self, n)
                    if n == 0 then return 0 end
                    return 1 + self(n - 1)
                end,
            })
            return counter(500)
            "#,
        1,
    );
    assert_eq!(state.to_number(-1).expect("numeric result"), 500.0);
}

#[test]
fn refused_lua_frame_is_located_at_the_callers_call_instruction() {
    let mut state = State::new();
    let results = vec!["1"; 64].join(", ");
    dellingr::Program::compile(
        format!("return function()\n return {results}\nend"),
        Some(b"callee".as_slice()),
    )
    .and_then(|program| state.load(&program))
    .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    state.set_global("callee").unwrap();
    let prefix = dellingr::limits::MAX_STACK_SIZE - 16;
    state.set_top(prefix as isize).unwrap();
    dellingr::Program::compile("\n\ncallee()", Some(b"caller".as_slice()))
        .and_then(|program| state.load(&program))
        .unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .unwrap_err();
    assert!(matches!(error.kind(), ErrorKind::StackOverflow { .. }));
    assert_eq!(error.line_num(), 3);
    assert_eq!(error.source_name(), Some(b"caller".as_slice()));
    assert_eq!(error.stack_trace().len(), 1);
    assert_eq!(error.stack_trace()[0].line, 3);
    assert_eq!(
        error.stack_trace()[0].source.as_deref(),
        Some(b"caller".as_slice()),
    );
    assert_eq!(state.get_top(), prefix);
    state.set_top(0).unwrap();
    support::eval(&mut state, "return 7", 1);
    assert_eq!(state.to_number(-1).unwrap(), 7.0);
}
