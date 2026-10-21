//! Fixed-input charge goldens for native standard-library calls.

use dellingr::{ArgCount, RetCount, State};

#[derive(Clone, Copy)]
enum Argument {
    Boolean(bool),
    Number(f64),
    String(&'static str),
    EmptyTable,
}

fn call_cost(library: &str, function: &str, arguments: &[Argument]) -> u64 {
    let mut state = State::new();
    state
        .get_global(library)
        .expect("standard library table exists");
    state
        .push_string(function)
        .expect("library field name fits on the stack");
    state
        .get_table(-2)
        .expect("standard library function exists");
    for argument in arguments {
        match argument {
            Argument::Boolean(value) => state
                .push_boolean(*value)
                .expect("golden argument fits on the stack"),
            Argument::Number(value) => state
                .push_number(*value)
                .expect("golden argument fits on the stack"),
            Argument::String(value) => state
                .push_string(value)
                .expect("golden argument fits on the stack"),
            Argument::EmptyTable => state
                .new_table()
                .expect("golden table argument fits on the stack"),
        }
    }

    let argument_count =
        u8::try_from(arguments.len()).expect("golden call arity fits in a fixed argument count");
    let before = state.cost_used();
    state
        .call(ArgCount::Fixed(argument_count), RetCount::Fixed(0))
        .expect("golden library call succeeds");
    state.cost_used() - before
}

#[test]
fn every_math_function_has_its_flat_charge() {
    for function in [
        "abs", "acos", "asin", "atan", "ceil", "cos", "deg", "exp", "floor", "log", "rad", "sin",
        "sqrt", "tan",
    ] {
        assert_eq!(
            call_cost("math", function, &[Argument::Number(0.5)]),
            1,
            "math.{function}"
        );
    }
    for function in ["atan2", "fmod"] {
        assert_eq!(
            call_cost(
                "math",
                function,
                &[Argument::Number(1.0), Argument::Number(2.0)]
            ),
            1,
            "math.{function}"
        );
    }
    for function in ["min", "max"] {
        assert_eq!(
            call_cost(
                "math",
                function,
                &[Argument::Number(1.0), Argument::Number(2.0)]
            ),
            1,
            "math.{function}"
        );
    }
    assert_eq!(call_cost("math", "modf", &[Argument::Number(1.5)]), 1);
    assert_eq!(call_cost("math", "random", &[]), 1);
}

#[test]
fn string_function_charges_match_fixed_inputs() {
    assert_eq!(
        call_cost(
            "string",
            "sub",
            &[
                Argument::String("abc"),
                Argument::Number(2.0),
                Argument::Number(3.0),
            ]
        ),
        2
    );
    assert_eq!(
        call_cost(
            "string",
            "find",
            &[
                Argument::String("abc"),
                Argument::String("a"),
                Argument::Number(1.0),
                Argument::Boolean(true),
            ]
        ),
        6
    );
    assert_eq!(call_cost("string", "format", &[Argument::String("")]), 1);
    assert_eq!(call_cost("string", "len", &[Argument::String("abc")]), 0);
    for function in ["upper", "lower", "reverse"] {
        assert_eq!(
            call_cost("string", function, &[Argument::String("abc")]),
            3,
            "string.{function}"
        );
    }
    assert_eq!(
        call_cost(
            "string",
            "match",
            &[Argument::String(""), Argument::String("")]
        ),
        2
    );
    assert_eq!(
        call_cost(
            "string",
            "gmatch",
            &[Argument::String(""), Argument::String("")]
        ),
        1
    );
    assert_eq!(
        call_cost(
            "string",
            "gsub",
            &[
                Argument::String(""),
                Argument::String(""),
                Argument::String(""),
            ]
        ),
        2
    );
}

#[test]
fn table_function_charges_match_fixed_inputs() {
    assert_eq!(
        call_cost(
            "table",
            "insert",
            &[Argument::EmptyTable, Argument::Number(1.0)]
        ),
        1
    );
    assert_eq!(
        call_cost(
            "table",
            "remove",
            &[Argument::EmptyTable, Argument::Number(1.0)]
        ),
        1
    );
    assert_eq!(
        call_cost(
            "table",
            "pack",
            &[Argument::Number(1.0), Argument::Number(2.0)]
        ),
        1
    );
    assert_eq!(
        call_cost(
            "table",
            "move",
            &[
                Argument::EmptyTable,
                Argument::Number(1.0),
                Argument::Number(0.0),
                Argument::Number(1.0),
            ]
        ),
        1
    );
    assert_eq!(
        call_cost(
            "table",
            "concat",
            &[Argument::EmptyTable, Argument::String("")]
        ),
        1
    );
    assert_eq!(call_cost("table", "sort", &[Argument::EmptyTable]), 1);
}
