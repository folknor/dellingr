//! Fixed-input charge goldens and coverage of registered native functions.

use std::collections::BTreeSet;

use dellingr::{ArgCount, RetCount, State};

use super::cost_golden::goldens;
use crate::support;

fn call_cost(name: &str, arguments: &str) -> u64 {
    let mut state = State::new();
    let (library, function) = name.split_once('.').expect("qualified native function");
    state
        .get_global(library)
        .expect("standard library table exists");
    state
        .push_string(function)
        .expect("field name fits on the stack");
    state
        .get_table(-2)
        .expect("standard library function exists");
    let mut argument_count = 0_u8;
    if !arguments.is_empty() {
        for argument in arguments.split(';') {
            let (kind, value) = argument.split_once(':').expect("typed golden argument");
            match kind {
                "n" => state.push_number(value.parse().expect("numeric golden argument")),
                "s" => state.push_string(value),
                "b" => state.push_boolean(value.parse().expect("boolean golden argument")),
                "t" => {
                    assert!(value.is_empty(), "table argument must be empty");
                    state.new_table()
                }
                _ => panic!("unknown golden argument kind: {kind}"),
            }
            .expect("golden argument fits on the stack");
            argument_count += 1;
        }
    }
    let before = state.cost_used();
    state
        .call(ArgCount::Fixed(argument_count), RetCount::Fixed(0))
        .expect("golden library call succeeds");
    state.cost_used() - before
}

#[test]
fn native_charges_match_versioned_fixed_inputs() {
    for row in goldens().into_iter().filter(|row| row.kind == "native") {
        assert_eq!(
            call_cost(row.name, row.input),
            row.expected.expect("native golden has an expected cost"),
            "{}",
            row.name
        );
    }
}

#[test]
fn documented_free_base_calls_and_iterators_charge_nothing() {
    for row in goldens()
        .into_iter()
        .filter(|row| matches!(row.kind, "free" | "free_error"))
    {
        let mut state = State::with_callbacks(Box::new(support::Recorder::default()));
        support::load(&mut state, row.input);
        // Supplying the table from Rust keeps opcode allocation out of the
        // native-call probe. The ipairs row also invokes its returned iterator.
        state.new_table().expect("probe table fits on the stack");
        let result = state.call(ArgCount::Fixed(1), RetCount::Fixed(0));
        if row.kind == "free_error" {
            let error = result.expect_err("error probe raises a script error");
            assert!(matches!(
                error.kind(),
                dellingr::error::ErrorKind::ScriptError { .. }
            ));
        } else {
            result.expect("free-call probe succeeds");
        }
        assert_eq!(state.cost_used(), 0, "{}", row.name);
    }
}

#[test]
fn every_registered_function_has_a_charge_probe() {
    let mut state = State::new();
    support::eval(
        &mut state,
        r#"
        local names = ''
        local function visit(prefix, library)
            for name, value in pairs(library) do
                if type(value) == 'function' then
                    names = names .. prefix .. '.' .. name .. '\n'
                end
            end
        end
        visit('_G', _G)
        visit('math', math)
        visit('string', string)
        visit('table', table)
        return names
        "#,
        1,
    );
    let names = state.to_string(-1).expect("registry probe returns names");
    let actual: BTreeSet<_> = names.lines().collect();
    let rows = goldens();
    let expected: BTreeSet<_> = rows
        .iter()
        .filter(|row| matches!(row.kind, "native" | "free" | "free_error"))
        .map(|row| row.name)
        .collect();
    assert_eq!(
        actual, expected,
        "registered functions need a versioned charge probe with valid arguments"
    );
}
