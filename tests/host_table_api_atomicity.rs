//! Host table methods validate before they consume operands (VM-006), the
//! positional helpers reject out-of-range positions (VM-007), and `raw_len`
//! reports a bad index or a lengthless value instead of returning 0 (VM-025).

use dellingr::error::{ErrorKind, TypeError};
use dellingr::{LuaType, State};

/// Stack: [number 7, table]. Index 1 is a non-table target.
fn number_then_table() -> State {
    let mut state = State::new();
    state.push_number(7.0).unwrap();
    state.new_table().unwrap();
    state
}

fn assert_stack(state: &State, types: &[LuaType]) {
    assert_eq!(state.get_top(), types.len());
    for (i, typ) in types.iter().enumerate() {
        assert_eq!(state.typ(i as isize + 1), *typ, "slot {}", i + 1);
    }
}

#[test]
fn non_table_targets_leave_every_operand_in_place() {
    let mut state = number_then_table();
    state.push_string("k").unwrap();
    let before = [LuaType::Number, LuaType::Table, LuaType::String];

    assert!(state.get_table(1).is_err());
    assert_stack(&state, &before);
    assert!(state.get_table_raw(1).is_err());
    assert_stack(&state, &before);
    assert!(state.table_next(1).is_err());
    assert_stack(&state, &before);
    assert!(state.table_insert_at(1, 1).is_err());
    assert_stack(&state, &before);
    // Table at 2, string at 3 is a valid metatable target but not a table.
    assert!(state.set_metatable_of(2).is_err());
    assert_stack(&state, &before);

    state.push_number(1.0).unwrap();
    let before = [
        LuaType::Number,
        LuaType::Table,
        LuaType::String,
        LuaType::Number,
    ];
    assert!(state.set_table_raw(1).is_err());
    assert_stack(&state, &before);
    assert!(state.set_metatable_of(1).is_err());
    assert_stack(&state, &before);
}

#[test]
fn set_table_raw_rejects_nil_and_nan_keys_without_popping() {
    for key in [None, Some(f64::NAN)] {
        let mut state = State::new();
        state.new_table().unwrap();
        match key {
            None => state.push_nil().unwrap(),
            Some(n) => state.push_number(n).unwrap(),
        }
        state.push_number(1.0).unwrap();
        let err = state.set_table_raw(1).expect_err("invalid key");
        assert!(matches!(
            err.kind,
            ErrorKind::TypeError(TypeError::TableKeyNil | TypeError::TableKeyNan)
        ));
        assert_eq!(state.get_top(), 3);
        assert_eq!(state.raw_len(1).unwrap(), 0);
    }
}

#[test]
fn table_next_keeps_an_invalid_key_for_the_host() {
    let mut state = State::new();
    state.new_table().unwrap();
    state.push_string("absent").unwrap();
    assert!(state.table_next(1).is_err());
    assert_stack(&state, &[LuaType::Table, LuaType::String]);
}

#[test]
fn table_remove_at_rejects_positions_outside_the_sequence() {
    let mut state = State::new();
    state.load_string("return {10, 20, [50] = 'far'}").unwrap();
    state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(1))
        .unwrap();
    for pos in [0, 4, 50] {
        assert!(state.table_remove_at(1, pos).is_err(), "pos {pos}");
        assert_eq!(state.get_top(), 1);
    }
    // t[50] survived the rejected removal.
    state.push_number(50.0).unwrap();
    state.get_table_raw(1).unwrap();
    assert_eq!(state.to_string(-1).unwrap(), "far");
    state.pop(1).unwrap();

    // len + 1 is accepted and reads nil, as `table.remove(t, #t + 1)` does.
    state.table_remove_at(1, 3).unwrap();
    assert_eq!(state.typ(-1), LuaType::Nil);
    state.pop(1).unwrap();
    state.table_remove_at(1, 1).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 10.0);
    assert_eq!(state.raw_len(1).unwrap(), 1);
}

#[test]
fn table_remove_at_accepts_zero_on_an_empty_table() {
    let mut state = State::new();
    state.new_table().unwrap();
    state.table_remove_at(1, 0).unwrap();
    assert_eq!(state.typ(-1), LuaType::Nil);
}

#[test]
fn raw_len_reports_bad_indices_and_lengthless_values() {
    let mut state = State::new();
    state.load_string("return {1, 2, 3}").unwrap();
    state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(1))
        .unwrap();
    state.push_string("abcd").unwrap();
    state.push_number(1.0).unwrap();

    assert_eq!(state.raw_len(1).unwrap(), 3);
    assert_eq!(state.raw_len(2).unwrap(), 4);
    assert!(matches!(
        state.raw_len(3).unwrap_err().kind,
        ErrorKind::TypeError(TypeError::Length(LuaType::Number))
    ));
    assert!(matches!(
        state.raw_len(9).unwrap_err().kind,
        ErrorKind::InvalidStackIndex { index: 9 }
    ));
}

#[test]
fn get_table_preserves_the_key_on_dispatch_preflight_failures() {
    for handler in ["5", "t", "function() return 1 end"] {
        let mut state = State::new();
        state
            .load_string(format!(
                "local t = {{}}; setmetatable(t, {{__index = {handler}}}); return t"
            ))
            .unwrap();
        state
            .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(1))
            .unwrap();
        let expected_top = if handler.starts_with("function") {
            state
                .set_top((dellingr::limits::MAX_STACK_SIZE - 1) as isize)
                .unwrap();
            dellingr::limits::MAX_STACK_SIZE
        } else {
            2
        };
        state.push_string("missing").unwrap();
        let err = state.get_table(1).expect_err("dispatch must fail");
        match handler {
            "5" => assert!(matches!(err.kind, ErrorKind::TypeError(_))),
            "t" => assert!(matches!(
                err.kind,
                ErrorKind::MetamethodDepthExceeded { .. }
            )),
            _ => assert!(matches!(err.kind, ErrorKind::StackOverflow { .. })),
        }
        assert_eq!(state.get_top(), expected_top);
        assert_eq!(state.to_string(-1).unwrap(), "missing");
    }
}

#[test]
fn get_table_still_consumes_the_key_when_a_handler_errors() {
    let mut state = State::new();
    state
        .load_string("return setmetatable({}, {__index = function() error('handler') end})")
        .unwrap();
    state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(1))
        .unwrap();
    state.push_string("missing").unwrap();
    assert!(state.get_table(1).is_err());
    assert_stack(&state, &[LuaType::Table]);
}

#[test]
fn raw_reads_replace_the_key_even_at_the_stack_cap() {
    let mut state = State::new();
    state.load_string("return {42}").unwrap();
    state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(1))
        .unwrap();
    state
        .set_top((dellingr::limits::MAX_STACK_SIZE - 1) as isize)
        .unwrap();
    state.push_number(1.0).unwrap();
    state.get_table(1).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 42.0);
    state.pop(1).unwrap();
    state.push_number(1.0).unwrap();
    state.get_table_raw(1).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 42.0);
    assert_eq!(state.get_top(), dellingr::limits::MAX_STACK_SIZE);
}
