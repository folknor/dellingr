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
fn set_field_sites_past_the_cache_capacity_use_the_uncached_path() {
    let assignments = "t.x = nil\n".repeat(255);
    let source = format!("local t = {{}}\n{assignments}");
    State::new()
        .load_string(&source)
        .expect("255 field assignment sites should compile");
    Engine::new()
        .compile(&source)
        .expect("Engine::compile should accept 255 field assignment sites");

    let uncached = "t.x = 1\n".repeat(256);
    let source = format!("local t = {{}}\n{uncached}return t.x");
    let mut state = State::new();
    state
        .load_string(&source)
        .expect("256 field assignment sites should compile with an uncached tail");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("uncached set-field site should execute");
    assert_eq!(
        state.to_number(-1).expect("field value should be numeric"),
        1.0
    );
    Engine::new()
        .compile(&source)
        .expect("Engine::compile should accept uncached set-field sites");
}

#[test]
fn table_constructors_flush_array_batches_without_changing_values() {
    let values = std::iter::repeat_n("1", 300).collect::<Vec<_>>().join(",");
    let mut state = State::new();
    state
        .load_string(format!("local t = {{{values}}} return #t, t[300]"))
        .expect("300 array entries should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .expect("300 array entries should execute");
    assert_eq!(
        state.to_number(-2).expect("length should be numeric"),
        300.0
    );
    assert_eq!(state.to_number(-1).expect("element should be numeric"), 1.0);

    let prefix = std::iter::repeat_n("1", 255).collect::<Vec<_>>().join(",");
    let mut state = State::new();
    state
        .load_string(format!(
            "local t = {{{prefix}, named = 2, [999] = 3, 4}} return t[255], t[256], t.named, t[999]"
        ))
        .expect("mixed fields after a batch boundary should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(4))
        .expect("mixed fields after a batch boundary should execute");
    assert_eq!(
        state.to_number(-4).expect("batch tail should be numeric"),
        1.0
    );
    assert_eq!(
        state
            .to_number(-3)
            .expect("next array element should be numeric"),
        4.0
    );
    assert_eq!(
        state.to_number(-2).expect("named field should be numeric"),
        2.0
    );
    assert_eq!(
        state
            .to_number(-1)
            .expect("computed field should be numeric"),
        3.0
    );
}

#[test]
fn dynamic_table_tail_uses_the_batch_after_flushed_values() {
    let prefix = std::iter::repeat_n("1", 255).collect::<Vec<_>>().join(",");
    let mut state = State::new();
    state
        .load_string(format!(
            "local function values() return 2, 3 end local t = {{{prefix}, values()}} return t[255], t[256], t[257]"
        ))
        .expect("dynamic tail after a flushed batch should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(3))
        .expect("dynamic tail after a flushed batch should execute");
    assert_eq!(
        state.to_number(-3).expect("batch tail should be numeric"),
        1.0
    );
    assert_eq!(
        state
            .to_number(-2)
            .expect("first dynamic value should be numeric"),
        2.0
    );
    assert_eq!(
        state
            .to_number(-1)
            .expect("second dynamic value should be numeric"),
        3.0
    );
}

#[test]
fn literal_pools_and_wide_field_keys_execute() {
    let numbers = (0..300)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut state = State::new();
    state
        .load_string(format!("local t = {{{numbers}}} return t[300]"))
        .expect("300 numeric literals should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("numeric literals should run");
    assert_eq!(
        state.to_number(-1).expect("array value should be numeric"),
        299.0
    );

    let strings = (0..300)
        .map(|n| format!("'s{n}'"))
        .collect::<Vec<_>>()
        .join(",");
    let mut state = State::new();
    state
        .load_string(format!("local t = {{{strings}}} return t[300]"))
        .expect("300 string literals should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("string literals should run");
    assert_eq!(
        state.to_string(-1).expect("array value should be a string"),
        "s299"
    );

    let globals = (0..300)
        .map(|n| format!("g{n} = {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut state = State::new();
    state
        .load_string(format!("{globals}\nreturn g299"))
        .expect("wide global names should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("wide global names should run");
    assert_eq!(
        state.to_number(-1).expect("global should be numeric"),
        299.0
    );

    let fields = (0..300)
        .map(|n| format!("t.f{n} = {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut state = State::new();
    state
        .load_string(format!("local t = {{}}\n{fields}\nreturn t.f299"))
        .expect("wide field names should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("wide field names should run");
    assert_eq!(state.to_number(-1).expect("field should be numeric"), 299.0);
}

#[test]
fn uncached_slots_templates_and_wide_set_field_at_execute() {
    let globals = (0..256)
        .map(|n| format!("g{n} = {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let global_reads = (0..256)
        .map(|n| format!("r = g{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let field_reads = (0..256)
        .map(|n| format!("q = t.f{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let fields = (0..256)
        .map(|n| format!("t.f{n} = {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut state = State::new();
    state
        .load_string(format!(
            "{globals}\nlocal t = {{}}\n{fields}\n{global_reads}\n{field_reads}\nreturn r, q"
        ))
        .expect("uncached cache candidates should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .expect("uncached cache candidates should run");
    assert_eq!(
        state.to_number(-2).expect("global read should be numeric"),
        255.0
    );
    assert_eq!(
        state.to_number(-1).expect("field read should be numeric"),
        255.0
    );

    let prefix = (0..256)
        .map(|n| format!("p{n} = 0"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut state = State::new();
    state
        .load_string(format!("{prefix}\nlocal a, b = {{}}, {{}}\na.wide, b.wide = 1, 2\nlocal t = {{ one = 1, two = 2, three = 3, four = 4, five = 5 }}\nreturn a.wide, b.wide, t.five"))
        .expect("wide template and SET_FIELD_AT keys should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(3))
        .expect("wide template and SET_FIELD_AT should run");
    assert_eq!(
        state
            .to_number(-3)
            .expect("first assignment should be numeric"),
        1.0
    );
    assert_eq!(
        state
            .to_number(-2)
            .expect("second assignment should be numeric"),
        2.0
    );
    assert_eq!(
        state
            .to_number(-1)
            .expect("template field should be numeric"),
        5.0
    );

    let named_fields = (0..256)
        .map(|n| format!("f{n} = {n}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut state = State::new();
    state
        .load_string(format!("local t = {{{named_fields}}} return t.f255"))
        .expect("constructors beyond the template-entry cap should fall back");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("fallback constructor should run");
    assert_eq!(
        state
            .to_number(-1)
            .expect("fallback field should be numeric"),
        255.0
    );
}
