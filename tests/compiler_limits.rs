//! Regression tests for compiler limits that must fail gracefully.

use dellingr::error::{ErrorKind, SyntaxError};
use dellingr::{ArgCount, Engine, RetCount, State};

fn fixed_args(count: usize) -> String {
    std::iter::repeat_n("1", count)
        .collect::<Vec<_>>()
        .join(",")
}

fn expect_syntax_error(code: &str, expected: fn(&SyntaxError) -> bool) {
    let err = State::new()
        .load_string(code)
        .expect_err("source should not compile");
    match &err.kind {
        ErrorKind::SyntaxError(kind) if expected(kind) => {}
        _ => panic!("unexpected error: {err}"),
    }
}

#[test]
fn fixed_call_argument_limit_preserves_dynamic_calls() {
    let mut state = State::new();
    state
        .load_string(format!(
            "local f = function(...) return 1 end return f({})",
            fixed_args(254)
        ))
        .expect("254 fixed arguments should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("254 fixed arguments should run");
    assert_eq!(
        state.to_number(-1).expect("return value should be numeric"),
        1.0
    );

    expect_syntax_error(
        &format!("local f = function(...) end return f({})", fixed_args(255)),
        |kind| matches!(kind, SyntaxError::TooManyArguments),
    );

    let mut state = State::new();
    state
        .load_string(format!(
            "local t = {{ m = function(self, ...) return 1 end }} return t:m({})",
            fixed_args(253)
        ))
        .expect("253 explicit method arguments should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("253 explicit method arguments should run");
    assert_eq!(
        state.to_number(-1).expect("return value should be numeric"),
        1.0
    );

    expect_syntax_error(
        &format!("local t = {{}} return t:m({})", fixed_args(254)),
        |kind| matches!(kind, SyntaxError::TooManyArguments),
    );

    // 255 explicit method arguments would make the old `num_args + 1` wrap past
    // u8; it must be rejected, not silently misframed.
    expect_syntax_error(
        &format!("local t = {{}} return t:m({})", fixed_args(255)),
        |kind| matches!(kind, SyntaxError::TooManyArguments),
    );

    let mut state = State::new();
    state
        .load_string("local function f(...) return select('#', ...) end return f(1, ...)")
        .expect("dynamic vararg tail should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("dynamic vararg tail should run");
    assert_eq!(
        state.to_number(-1).expect("return value should be numeric"),
        1.0
    );
}

#[test]
fn parameter_limit_includes_method_self() {
    let params = (0..255)
        .map(|i| format!("p{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut state = State::new();
    state
        .load_string(format!(
            "local function f({params}) return p253, p254 end return f({})",
            fixed_args(254)
        ))
        .expect("255 parameters should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .expect("255-parameter function should run");
    // Called with 254 arguments: the last explicit slot holds its value and the
    // 255th parameter is nil-padded (num_params did not wrap).
    assert_eq!(
        state
            .to_number(-2)
            .expect("p253 should be the last passed argument"),
        1.0
    );
    assert_eq!(state.typ(-1), dellingr::LuaType::Nil);
    state.pop(2).unwrap();

    let too_many = (0..256)
        .map(|i| format!("p{i}"))
        .collect::<Vec<_>>()
        .join(",");
    expect_syntax_error(&format!("local function f({too_many}) end"), |kind| {
        matches!(kind, SyntaxError::TooManyLocals)
    });

    let method_params = (0..255)
        .map(|i| format!("p{i}"))
        .collect::<Vec<_>>()
        .join(",");
    expect_syntax_error(
        &format!("local t = {{}} function t:m({method_params}) end"),
        |kind| matches!(kind, SyntaxError::TooManyLocals),
    );
}

#[test]
fn jumps_beyond_i16_are_rejected_before_execution() {
    let body = "x = 1\n".repeat(17_000);
    expect_syntax_error(&format!("if false then\n{body}end"), |kind| {
        matches!(kind, SyntaxError::JumpTooFar)
    });

    expect_syntax_error(&format!("while false do\n{body}end"), |kind| {
        matches!(kind, SyntaxError::JumpTooFar)
    });

    expect_syntax_error(&format!("while true do\n{body}break\nend"), |kind| {
        matches!(kind, SyntaxError::JumpTooFar)
    });
}

#[test]
fn set_field_cache_limit_is_checked_recursively() {
    let assignments = "t.x = nil\n".repeat(255);
    let source = format!("local t = {{}}\n{assignments}");
    State::new()
        .load_string(&source)
        .expect("255 field assignment sites should compile");
    Engine::new()
        .compile(&source)
        .expect("Engine::compile should accept 255 field assignment sites");

    let too_many = "t.x = nil\n".repeat(256);
    expect_syntax_error(
        &format!("local function f() local t = {{}}\n{too_many}end"),
        |kind| matches!(kind, SyntaxError::TooManyFieldAssignments),
    );
    let err = Engine::new()
        .compile(&format!("local t = {{}}\n{too_many}"))
        .expect_err("Engine::compile should reject 256 field assignment sites");
    assert!(matches!(
        err.kind,
        ErrorKind::SyntaxError(SyntaxError::TooManyFieldAssignments)
    ));
}
