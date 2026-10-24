//! Regression tests for compiler limits that must fail gracefully.

use dellingr::error::{ErrorKind, SyntaxError};
use dellingr::limits::{
    MAX_ASSIGNMENT_TARGETS, MAX_FIXED_ARGS, MAX_LOCALS, MAX_REGISTERS, MAX_TABLE_BATCH_SIZE,
};
use dellingr::{Program, State};

use crate::support;

fn fixed_args(count: usize) -> String {
    std::iter::repeat_n("1", count)
        .collect::<Vec<_>>()
        .join(",")
}

fn expect_syntax_error(code: &str, expected: fn(&SyntaxError) -> bool) {
    let err = Program::compile(code, None).expect_err("source should not compile");
    match err.kind() {
        ErrorKind::SyntaxError(kind) if expected(kind) => {}
        _ => panic!("unexpected error: {err}"),
    }
}

#[test]
fn fixed_call_argument_limit_preserves_dynamic_calls() {
    let mut state = State::new();
    support::eval(
        &mut state,
        format!(
            "local f = function(...) return 1 end return f({})",
            fixed_args(MAX_FIXED_ARGS)
        ),
        1,
    );
    assert_eq!(
        state.to_number(-1).expect("return value should be numeric"),
        1.0
    );

    expect_syntax_error(
        &format!(
            "local f = function(...) end return f({})",
            fixed_args(MAX_FIXED_ARGS + 1)
        ),
        |kind| matches!(kind, SyntaxError::TooManyArguments),
    );

    let mut state = State::new();
    support::eval(
        &mut state,
        format!(
            "local t = {{ m = function(self, ...) return 1 end }} return t:m({})",
            fixed_args(MAX_FIXED_ARGS - 1)
        ),
        1,
    );
    assert_eq!(
        state.to_number(-1).expect("return value should be numeric"),
        1.0
    );

    expect_syntax_error(
        &format!("local t = {{}} return t:m({})", fixed_args(MAX_FIXED_ARGS)),
        |kind| matches!(kind, SyntaxError::TooManyArguments),
    );

    // An explicit method argument count above MAX_FIXED_ARGS must be rejected.
    expect_syntax_error(
        &format!(
            "local t = {{}} return t:m({})",
            fixed_args(MAX_FIXED_ARGS + 1)
        ),
        |kind| matches!(kind, SyntaxError::TooManyArguments),
    );

    let mut state = State::new();
    support::eval(
        &mut state,
        "local function f(...) return select('#', ...) end return f(1, ...)",
        1,
    );
    assert_eq!(
        state.to_number(-1).expect("return value should be numeric"),
        1.0
    );
}

#[test]
fn parameter_limit_includes_method_self() {
    let params = (0..MAX_LOCALS)
        .map(|i| format!("p{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut state = State::new();
    support::eval(
        &mut state,
        format!(
            "local function f({params}) return p{}, p{} end return f({})",
            MAX_LOCALS - 2,
            MAX_LOCALS - 1,
            fixed_args(MAX_LOCALS - 1)
        ),
        2,
    );
    // The last passed argument holds its value; the final parameter is nil-padded.
    assert_eq!(
        state
            .to_number(-2)
            .expect("the last passed argument should retain its value"),
        1.0
    );
    assert_eq!(state.typ(-1), dellingr::LuaType::Nil);
    state.pop(2).unwrap();

    let too_many = (0..MAX_LOCALS + 1)
        .map(|i| format!("p{i}"))
        .collect::<Vec<_>>()
        .join(",");
    expect_syntax_error(&format!("local function f({too_many}) end"), |kind| {
        matches!(kind, SyntaxError::TooManyLocals)
    });

    let method_params = (0..MAX_LOCALS)
        .map(|i| format!("p{i}"))
        .collect::<Vec<_>>()
        .join(",");
    expect_syntax_error(
        &format!("local t = {{}} function t:m({method_params}) end"),
        |kind| matches!(kind, SyntaxError::TooManyLocals),
    );
}

#[test]
fn long_control_flow_runs_as_a_smoke_test() {
    // Exercise large conditional and loop branch spans. The codegen unit test
    // covers the representable jump boundary.
    let body = "x = 1\n".repeat(40_000);
    for source in [
        format!("if false then\n{body}end return 7"),
        format!("while false do\n{body}end return 7"),
        format!("while true do\n{body}break\nend return 7"),
    ] {
        let mut state = State::new();
        support::eval(&mut state, &source, 1);
        assert_eq!(state.to_number(-1).expect("numeric result"), 7.0);
    }
}

#[test]
fn set_field_sites_past_the_cache_capacity_use_the_uncached_path() {
    // MAX_CACHE_SLOTS is crate-private; these integration probes keep explicit sizes.
    let assignments = "t.x = nil\n".repeat(255);
    let source = format!("local t = {{}}\n{assignments}");
    let program =
        Program::compile(&source, None).expect("255 field assignment sites should compile");
    State::new()
        .load(&program)
        .expect("compiled field assignments should load");

    let uncached = "t.x = 1\n".repeat(256);
    let source = format!("local t = {{}}\n{uncached}return t.x");
    let mut state = State::new();
    support::eval(&mut state, &source, 1);
    assert_eq!(
        state.to_number(-1).expect("field value should be numeric"),
        1.0
    );
}

#[test]
fn table_constructors_flush_array_batches_without_changing_values() {
    let values = std::iter::repeat_n("1", usize::from(MAX_TABLE_BATCH_SIZE) + 1)
        .collect::<Vec<_>>()
        .join(",");
    let mut state = State::new();
    support::eval(
        &mut state,
        format!(
            "local t = {{{values}}} return #t, t[{}]",
            usize::from(MAX_TABLE_BATCH_SIZE) + 1
        ),
        2,
    );
    assert_eq!(
        state.to_number(-2).expect("length should be numeric"),
        f64::from(MAX_TABLE_BATCH_SIZE) + 1.0
    );
    assert_eq!(state.to_number(-1).expect("element should be numeric"), 1.0);

    let prefix = std::iter::repeat_n("1", usize::from(MAX_TABLE_BATCH_SIZE))
        .collect::<Vec<_>>()
        .join(",");
    let mut state = State::new();
    support::eval(
        &mut state,
        format!(
            "local t = {{{prefix}, named = 2, [999] = 3, 4}} return t[{}], t[{}], t.named, t[999]",
            MAX_TABLE_BATCH_SIZE,
            usize::from(MAX_TABLE_BATCH_SIZE) + 1
        ),
        4,
    );
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
    let prefix = std::iter::repeat_n("1", usize::from(MAX_TABLE_BATCH_SIZE))
        .collect::<Vec<_>>()
        .join(",");
    let mut state = State::new();
    support::eval(
        &mut state,
        format!(
            "local function values() return 2, 3 end local t = {{{prefix}, values()}} return t[{}], t[{}], t[{}]",
            MAX_TABLE_BATCH_SIZE,
            usize::from(MAX_TABLE_BATCH_SIZE) + 1,
            usize::from(MAX_TABLE_BATCH_SIZE) + 2
        ),
        3,
    );
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
    support::eval(
        &mut state,
        format!("local t = {{{numbers}}} return t[300]"),
        1,
    );
    assert_eq!(
        state.to_number(-1).expect("array value should be numeric"),
        299.0
    );

    let strings = (0..300)
        .map(|n| format!("'s{n}'"))
        .collect::<Vec<_>>()
        .join(",");
    let mut state = State::new();
    support::eval(
        &mut state,
        format!("local t = {{{strings}}} return t[300]"),
        1,
    );
    assert_eq!(
        state.to_string(-1).expect("array value should be a string"),
        "s299"
    );

    let globals = (0..300)
        .map(|n| format!("g{n} = {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut state = State::new();
    support::eval(&mut state, format!("{globals}\nreturn g299"), 1);
    assert_eq!(
        state.to_number(-1).expect("global should be numeric"),
        299.0
    );

    let fields = (0..300)
        .map(|n| format!("t.f{n} = {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut state = State::new();
    support::eval(
        &mut state,
        format!("local t = {{}}\n{fields}\nreturn t.f299"),
        1,
    );
    assert_eq!(state.to_number(-1).expect("field should be numeric"), 299.0);
}

#[test]
fn uncached_slots_templates_and_wide_set_field_at_execute() {
    // MAX_CACHE_SLOTS is crate-private; these integration probes keep explicit sizes.
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
    support::eval(
        &mut state,
        format!("{globals}\nlocal t = {{}}\n{fields}\n{global_reads}\n{field_reads}\nreturn r, q"),
        2,
    );
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
    support::eval(
        &mut state,
        format!(
            "{prefix}\nlocal a, b = {{}}, {{}}\na.wide, b.wide = 1, 2\nlocal t = {{ one = 1, two = 2, three = 3, four = 4, five = 5 }}\nreturn a.wide, b.wide, t.five"
        ),
        3,
    );
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
    support::eval(
        &mut state,
        format!("local t = {{{named_fields}}} return t.f255"),
        1,
    );
    assert_eq!(
        state
            .to_number(-1)
            .expect("fallback field should be numeric"),
        255.0
    );
}

#[test]
fn assignment_target_count_is_bounded_before_codegen() {
    let targets = std::iter::repeat_n("a", MAX_ASSIGNMENT_TARGETS)
        .collect::<Vec<_>>()
        .join(",");
    Program::compile(format!("local a; {targets} = 1"), None)
        .expect("MAX_ASSIGNMENT_TARGETS fixed assignment targets fit");
    for count in [
        MAX_ASSIGNMENT_TARGETS + 1,
        MAX_REGISTERS + 1,
        MAX_REGISTERS + 2,
        MAX_REGISTERS + 3,
        MAX_REGISTERS + 4,
    ] {
        let targets = std::iter::repeat_n("a", count)
            .collect::<Vec<_>>()
            .join(",");
        for prefix in ["", "local a; "] {
            expect_syntax_error(&format!("{prefix}{targets} = 1"), |kind| {
                matches!(kind, SyntaxError::TooManyAssignmentTargets { count }
                    if *count == MAX_ASSIGNMENT_TARGETS + 1)
            });
        }
    }
}

#[test]
fn compiler_limit_diagnostics_include_observed_counts() {
    assert_eq!(
        SyntaxError::TooManyAssignmentTargets { count: 256 }.to_string(),
        "too many assignment targets (256, limit 255)"
    );
    assert_eq!(
        SyntaxError::TooManyRegisters { count: 65_535 }.to_string(),
        "too many registers (65535, limit 65534)"
    );
}
