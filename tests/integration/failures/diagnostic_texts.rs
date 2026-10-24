//! Pins the wording of diagnostics that a script author reads: argument
//! numbers of method calls, traceback labels of unnamed functions, the
//! arguments named by `table.move`'s range errors, `error()`'s position
//! prefix, and syntax errors.

use dellingr::error::{
    ArgDetail, Error, ErrorKind, ErrorValue, HostCallApi, PrefixLocation, SyntaxError, TokenKind,
    TypeError,
};
use dellingr::{ArgCount, LuaType, RetCount, State};

fn run_error(code: &str) -> Error {
    let mut state = State::new();
    dellingr::Program::compile(code, Some(b"t".as_slice()))
        .and_then(|program| state.load(&program))
        .expect("test program compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("test program must fail")
}

fn kind_text(code: &str) -> String {
    run_error(code).kind().to_string()
}

#[test]
fn method_call_arguments_are_numbered_from_the_first_typed_argument() {
    assert_eq!(
        kind_text("local s = 'abc'\ns:sub(1.5)"),
        "bad argument #1 to 'sub' (number has no integer representation)"
    );
    assert_eq!(
        kind_text("local s = 'abc'\ns:sub(1, {})"),
        "bad argument #2 to 'sub' (number expected, got table)"
    );
    // The same function called without `:` keeps the callee's own numbering.
    assert_eq!(
        kind_text("string.sub('abc', 1.5)"),
        "bad argument #2 to 'sub' (number has no integer representation)"
    );
}

#[test]
fn method_call_with_a_bad_receiver_reports_bad_self() {
    assert_eq!(
        kind_text("local t = {sub = string.sub}\nt:sub(1)"),
        "calling 'sub' on bad self (string expected, got table)"
    );
    // A dot call passing the same bad value stays an argument error.
    assert_eq!(
        kind_text("local t = {sub = string.sub}\nt.sub(t, 1)"),
        "bad argument #1 to 'sub' (string expected, got table)"
    );
}

#[test]
fn method_call_numbering_covers_table_functions() {
    assert_eq!(
        kind_text("local t = {concat = table.concat}\nt:concat({})"),
        "bad argument #1 to 'concat' (string expected, got table)"
    );
}

#[test]
fn numeric_for_errors_name_the_invalid_control_field() {
    for (code, expected) in [
        (
            "for i = nil, 1 do end",
            "numeric for loop initial value must be a number, got nil",
        ),
        (
            "for i = 1, nil do end",
            "numeric for loop limit must be a number, got nil",
        ),
        (
            "for i = 1, 2, nil do end",
            "numeric for loop step must be a number, got nil",
        ),
    ] {
        assert_eq!(kind_text(code), expected, "{code}");
    }
}

#[test]
fn numeric_for_control_errors_preserve_the_field_in_the_error_kind() {
    enum Field {
        InitialValue,
        Limit,
        Step,
    }

    for (code, field) in [
        ("for i = nil, 1 do end", Field::InitialValue),
        ("for i = 1, nil do end", Field::Limit),
        ("for i = 1, 2, nil do end", Field::Step),
    ] {
        let error = run_error(code);
        match field {
            Field::InitialValue => assert!(matches!(
                error.kind(),
                ErrorKind::TypeError(TypeError::NumericForInitialValue(LuaType::Nil))
            )),
            Field::Limit => assert!(matches!(
                error.kind(),
                ErrorKind::TypeError(TypeError::NumericForLimit(LuaType::Nil))
            )),
            Field::Step => assert!(matches!(
                error.kind(),
                ErrorKind::TypeError(TypeError::NumericForStep(LuaType::Nil))
            )),
        }
        assert!(error.kind().to_string().contains("numeric for loop"));
    }
}

#[test]
fn method_flag_is_exposed_on_the_typed_error() {
    let error = run_error("local s = 'abc'\ns:sub(1.5)");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected an argument error, got {:?}", error.kind());
    };
    assert!(argument.is_method_call());
    assert_eq!(argument.arg_number(), 2);

    let error = run_error("string.sub('abc', 1.5)");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected an argument error, got {:?}", error.kind());
    };
    assert!(!argument.is_method_call());
    assert_eq!(argument.arg_number(), 2);
}

#[test]
fn argument_error_inside_a_callback_keeps_its_own_numbering() {
    // The error is raised by a plain call inside the comparator; the outer
    // `t:sort(...)` method call merely propagates it.
    let text = kind_text(
        "local t = {3, 2, 1, sort = table.sort}\n\
         t:sort(function(a, b) return string.sub(a, 1.5) end)",
    );
    assert_eq!(
        text,
        "bad argument #2 to 'sub' (number has no integer representation)"
    );
}

#[test]
fn unnamed_functions_are_labelled_by_where_they_were_defined() {
    let error = run_error("local f = function()\n  error('x')\nend\nf()");
    let frames: Vec<String> = error
        .stack_trace()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        frames,
        ["t:2: in function <t:1>", "t:4: in main chunk"],
        "{error}"
    );
}

#[test]
fn immediately_invoked_function_is_not_a_main_chunk() {
    let error = run_error("(function() error('x') end)()");
    let rendered = error.to_string();
    assert_eq!(rendered.matches("main chunk").count(), 1, "{rendered}");
    assert!(rendered.contains("t:1: in function <t:1>"), "{rendered}");
}

#[test]
fn named_functions_keep_their_names_in_tracebacks() {
    let error = run_error("local function boom()\n  error('x')\nend\nboom()");
    let frames: Vec<String> = error
        .stack_trace()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        frames,
        ["t:2: in function 'boom'", "t:4: in main chunk"],
        "{error}"
    );
}

#[test]
fn table_move_blames_the_destination_for_wrap_around() {
    let error = run_error("table.move({10, 20, 30}, 1, 2000, 9223372036854774784)");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected an argument error, got {:?}", error.kind());
    };
    assert_eq!(argument.arg_number(), 4);
    assert_eq!(argument.func_name(), Some("move"));
    assert_eq!(argument.detail(), &ArgDetail::DestinationWrapAround);
    assert_eq!(
        error.kind().to_string(),
        "bad argument #4 to 'move' (destination wrap around)"
    );
}

/// The first line of an uncaught error: prefix plus rendered value.
fn first_line(code: &str) -> String {
    let rendered = run_error(code).to_string();
    rendered
        .lines()
        .next()
        .expect("an error renders at least one line")
        .to_owned()
}

#[test]
fn error_prefixes_only_string_values() {
    // As Lua 5.4 does: position information is added only to a string, at
    // any level. Lua 5.2 also prefixes numbers.
    for (code, expected) in [
        ("error('m')", "t:1: m"),
        ("error('m', 1)", "t:1: m"),
        ("error('m', 0)", "m"),
        ("local function f() error('m', 2) end\nf()", "t:2: m"),
        ("error('42')", "t:1: 42"),
        ("error(42)", "42"),
        ("error(1.5, 1)", "1.5"),
        ("local function f() error(42, 2) end\nf()", "42"),
        ("error(true)", "true"),
        ("error(nil)", "nil"),
        ("error()", "nil"),
        ("error({})", "table: 0x1"),
        (
            "error(setmetatable({}, {__tostring = function() return 'custom' end}), 2)",
            "custom",
        ),
        ("error(print)", "function: 0x1"),
    ] {
        assert_eq!(first_line(code), expected, "{code}");
    }
}

#[test]
fn error_records_suppressed_prefix_for_non_strings() {
    for code in ["error(42)", "error({}, 2)", "error(false, 1)", "error()"] {
        let error = run_error(code);
        let ErrorKind::ScriptError { prefix, .. } = error.kind() else {
            panic!("expected a script error, got {:?}", error.kind());
        };
        assert!(matches!(prefix, PrefixLocation::Suppressed), "{code}");
    }
    let error = run_error("error('m')");
    let ErrorKind::ScriptError { value, prefix } = error.kind() else {
        panic!("expected a script error, got {:?}", error.kind());
    };
    assert_eq!(value, &ErrorValue::String(b"m".to_vec()));
    assert!(matches!(prefix, PrefixLocation::Current));
}

#[test]
fn error_validates_its_level_for_every_value() {
    // The level is checked whatever the value, as in both references. A
    // table's `__tostring` runs first, so its own error wins.
    assert_eq!(
        kind_text("error(42, 'x')"),
        "bad argument #2 to 'error' (number expected, got string)"
    );
    assert_eq!(
        kind_text("error({}, 1.5)"),
        "bad argument #2 to 'error' (number has no integer representation)"
    );
    assert_eq!(
        kind_text("error(setmetatable({}, {__tostring = function() error('inner', 0) end}), 'x')"),
        "inner"
    );
}

fn syntax_error(code: &str) -> Error {
    dellingr::Program::compile(code, Some(b"t".as_slice()))
        .expect_err("test source must not compile")
}

#[test]
fn syntax_errors_name_what_reference_lua_names() {
    for (code, expected) in [
        ("local x = )", "unexpected symbol near ')'"),
        ("x = 1 )", "unexpected symbol near ')'"),
        ("= 1", "unexpected symbol near '='"),
        ("if x then ) end", "unexpected symbol near ')'"),
        ("a, ) = 1, 2", "unexpected symbol near ')'"),
        ("return )", "unexpected symbol near ')'"),
        ("do return ) end", "unexpected symbol near ')'"),
        ("(x) = 1", "syntax error near '='"),
        ("f() = 1", "syntax error near '='"),
        ("a, f() = 1, 2", "syntax error near '='"),
        ("x;", "syntax error near ';'"),
        ("x + 1", "syntax error near '+'"),
        ("for i do end", "'=' or 'in' expected near 'do'"),
        ("a:b z", "function arguments expected near 'z'"),
        ("function 1() end", "<name> expected near '1'"),
        ("x = 1 end", "<eof> expected near 'end'"),
        ("return; )", "<eof> expected near ')'"),
        ("for i = 1, 2 x end", "'do' expected near 'x'"),
        ("t = {[1] 2}", "'=' expected near '2'"),
    ] {
        assert_eq!(syntax_error(code).kind().to_string(), expected, "{code}");
    }
}

#[test]
fn unsupported_environment_name_is_a_located_syntax_error() {
    for code in [
        "local _ENV = {}",
        "local value, _ENV = 1, 2",
        "local function f(_ENV) end",
        "function f(_ENV) end",
        "local function _ENV() end",
        "function _ENV() end",
        "for _ENV = 1, 2 do end",
        "for key, _ENV in iterator do end",
        "_ENV = 1",
        "print(_ENV)",
    ] {
        let error = syntax_error(code);
        assert!(
            matches!(
                error.kind(),
                ErrorKind::SyntaxError(SyntaxError::UnsupportedEnvironmentName)
            ),
            "{code}: {error}"
        );
        assert_eq!(
            error.kind().to_string(),
            "the special name '_ENV' is not supported",
            "{code}"
        );
    }

    let error = syntax_error("print(1)\nlocal _ENV = {}");
    assert_eq!(error.line_num(), 2);

    // `_ENV` remains an ordinary field spelling; only variable-name uses
    // would imply the unsupported environment behavior.
    dellingr::Program::compile("local t = {_ENV = 1}; return t._ENV", None)
        .expect("field names named _ENV remain valid");
}

#[test]
fn syntax_error_variants_are_typed() {
    let found = |code: &str| match syntax_error(code).into_kind() {
        ErrorKind::SyntaxError(error) => error,
        other => panic!("expected a syntax error for {code}, got {other:?}"),
    };
    assert!(matches!(
        found("local x = )"),
        SyntaxError::UnexpectedSymbol { found } if found == b")"
    ));
    assert!(matches!(
        found("x + 1"),
        SyntaxError::MalformedStatement { found } if found == b"+"
    ));
    assert!(matches!(
        found("for i do end"),
        SyntaxError::ForSeparatorExpected { found } if found == b"do"
    ));
    assert!(matches!(
        found("a:b z"),
        SyntaxError::FunctionArgumentsExpected { found } if found == b"z"
    ));
    assert!(matches!(
        found("local 1 = 2"),
        SyntaxError::ExpectedToken { expected: TokenKind::Identifier, found } if found == b"1"
    ));
    assert!(matches!(
        found("while x y"),
        SyntaxError::ExpectedToken {
            expected: TokenKind::Do,
            ..
        }
    ));
}

#[test]
fn parser_reaching_end_of_input_stays_recoverable() {
    // Whatever the parser was looking for, reaching the end-of-input token is
    // `UnexpectedEof`, which a REPL reads as "keep reading".
    for code in ["local x =", "x", "(x)", "a:b", "x = 1 +", "if x then", "f("] {
        let error = syntax_error(code);
        assert!(
            matches!(
                error.kind(),
                ErrorKind::SyntaxError(SyntaxError::UnexpectedEof)
            ),
            "{code}: {:?}",
            error.kind()
        );
        assert!(error.is_recoverable(), "{code}");
        assert_eq!(error.kind().to_string(), "unexpected <eof>", "{code}");
    }
}

#[test]
fn token_kinds_render_with_reference_quoting() {
    for (token, expected) in [
        (TokenKind::End, "'end'"),
        (TokenKind::Assign, "'='"),
        (TokenKind::DotDotDot, "'...'"),
        (TokenKind::Identifier, "<name>"),
        (TokenKind::LiteralNumber, "<number>"),
        (TokenKind::LiteralString, "<string>"),
        (TokenKind::EndOfFile, "<eof>"),
    ] {
        assert_eq!(token.to_string(), expected);
    }
}

#[test]
fn host_call_api_names_the_method() {
    assert_eq!(HostCallApi::Call.to_string(), "call");
    assert_eq!(HostCallApi::CallAnchor.to_string(), "call_anchor");
}

#[test]
fn table_move_names_the_argument_with_too_many_elements() {
    let error = run_error("table.move({}, -1024, 9223372036854774784, 1)");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected an argument error, got {:?}", error.kind());
    };
    assert_eq!(argument.arg_number(), 3);
    assert_eq!(argument.func_name(), Some("move"));
    assert_eq!(
        error.kind().to_string(),
        "bad argument #3 to 'move' (too many elements to move)"
    );
}
