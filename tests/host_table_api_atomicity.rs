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
            err.kind(),
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
    dellingr::Program::compile("return {10, 20, [50] = 'far'}", None)
        .and_then(|program| state.load(&program))
        .unwrap();
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
    dellingr::Program::compile("return {1, 2, 3}", None)
        .and_then(|program| state.load(&program))
        .unwrap();
    state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(1))
        .unwrap();
    state.push_string("abcd").unwrap();
    state.push_number(1.0).unwrap();

    assert_eq!(state.raw_len(1).unwrap(), 3);
    assert_eq!(state.raw_len(2).unwrap(), 4);
    assert!(matches!(
        state.raw_len(3).unwrap_err().kind(),
        ErrorKind::TypeError(TypeError::Length(LuaType::Number))
    ));
    assert!(matches!(
        state.raw_len(9).unwrap_err().kind(),
        ErrorKind::InvalidStackIndex { index: 9 }
    ));
}

#[test]
fn get_table_preserves_the_key_on_dispatch_preflight_failures() {
    for handler in ["5", "t", "function() return 1 end"] {
        let mut state = State::new();
        dellingr::Program::compile(
            format!("local t = {{}}; setmetatable(t, {{__index = {handler}}}); return t"),
            None,
        )
        .and_then(|program| state.load(&program))
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
            "5" => assert!(matches!(err.kind(), ErrorKind::TypeError(_))),
            "t" => assert!(matches!(
                err.kind(),
                ErrorKind::MetamethodDepthExceeded { .. }
            )),
            _ => assert!(matches!(err.kind(), ErrorKind::StackOverflow { .. })),
        }
        assert_eq!(state.get_top(), expected_top);
        assert_eq!(state.to_string(-1).unwrap(), "missing");
    }
}

#[test]
fn get_table_still_consumes_the_key_when_a_handler_errors() {
    let mut state = State::new();
    dellingr::Program::compile(
        "return setmetatable({}, {__index = function() error('handler') end})",
        None,
    )
    .and_then(|program| state.load(&program))
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
    dellingr::Program::compile("return {42}", None)
        .and_then(|program| state.load(&program))
        .unwrap();
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

#[test]
fn chained_index_boundaries_preserve_their_operand_contract() {
    for string in [false, true] {
        for entered in [false, true] {
            let mut state = State::new();
            let terminal = if entered {
                "function() observed = 73 error('handler') end"
            } else {
                "5"
            };
            let source = format!(
                "local chain = setmetatable({{}}, {{__index = {terminal}}})
                chain = setmetatable({{}}, {{__index = chain}})
                {}",
                if string {
                    "getmetatable('').__index = chain return 'receiver'"
                } else {
                    "return setmetatable({}, {__index = chain})"
                }
            );
            let program = dellingr::Program::compile(source, None).unwrap();
            state.load(&program).unwrap();
            state
                .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(1))
                .unwrap();
            let receiver = state.anchor_at(1).unwrap();
            state.push_string("missing").unwrap();
            let error = state.get_table(1).expect_err("terminal index fails");
            assert_eq!(state.get_top(), if entered { 1 } else { 2 });
            if entered {
                assert!(matches!(error.kind(), ErrorKind::ScriptError { .. }));
            } else {
                assert!(matches!(error.kind(), ErrorKind::TypeError(_)));
                assert_eq!(state.to_bytes(-1).unwrap(), b"missing");
            }
            if string {
                assert_eq!(state.to_bytes(1).unwrap(), b"receiver");
            }
            state.push_anchor(&receiver).unwrap();
            assert!(state.raw_equal(1, -1));
            state.pop(1).unwrap();
            drop(receiver);
            state.get_global("observed").unwrap();
            if entered {
                assert_eq!(state.to_number(-1).unwrap(), 73.0);
            } else {
                assert_eq!(state.typ(-1), LuaType::Nil);
            }
            state.pop(1).unwrap();
            state.set_top(0).unwrap();
            #[cfg(feature = "snapshot")]
            state.save_state().unwrap();
            let reuse = dellingr::Program::compile("return 7", None).unwrap();
            state.load(&reuse).unwrap();
            state
                .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(1))
                .unwrap();
            assert_eq!(state.to_number(-1).unwrap(), 7.0);
        }
    }
}

#[test]
fn tostring_handler_failure_preserves_the_host_stack() {
    let mut state = State::new();
    let program = dellingr::Program::compile(
        "return setmetatable({}, {__tostring = function() observed = 73 error('handler') end})",
        None,
    )
    .unwrap();
    state.push_number(73.0).unwrap();
    state.load(&program).unwrap();
    state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(1))
        .unwrap();
    let receiver = state.anchor_at(2).unwrap();
    let error = state.tostring_bytes(2).expect_err("handler fails");
    assert!(matches!(error.kind(), ErrorKind::ScriptError { .. }));
    assert_eq!(state.get_top(), 2);
    assert_eq!(state.to_number(1).unwrap(), 73.0);
    assert_eq!(state.typ(2), LuaType::Table);
    state.push_anchor(&receiver).unwrap();
    assert!(state.raw_equal(2, -1));
    state.pop(1).unwrap();
    drop(receiver);
    state.get_global("observed").unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 73.0);
    state.pop(1).unwrap();
    state.set_top(0).unwrap();
    #[cfg(feature = "snapshot")]
    state.save_state().unwrap();
}

#[test]
fn get_table_preserves_the_key_when_the_lua_handler_frame_cannot_fit() {
    for string in [false, true] {
        for chained in [false, true] {
            let mut state = State::new();
            let results = vec!["1"; 64].join(", ");
            let handler = if chained {
                "setmetatable({}, {__index = handler})"
            } else {
                "handler"
            };
            let receiver = if string {
                format!("getmetatable('').__index = {handler}; return 'receiver'")
            } else {
                format!("return setmetatable({{}}, {{__index = {handler}}})")
            };
            dellingr::Program::compile(
                format!("local handler = function() return {results} end; {receiver}"),
                None,
            )
            .and_then(|program| state.load(&program))
            .unwrap();
            state
                .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(1))
                .unwrap();
            state
                .set_top((dellingr::limits::MAX_STACK_SIZE - 4) as isize)
                .unwrap();
            state.push_string("missing").unwrap();
            let top = state.get_top();
            let error = state.get_table(1).unwrap_err();
            assert!(matches!(error.kind(), ErrorKind::StackOverflow { .. }));
            assert_eq!(state.get_top(), top);
            assert_eq!(state.to_string(-1).unwrap(), "missing");
            assert_eq!(
                state.typ(1),
                if string {
                    LuaType::String
                } else {
                    LuaType::Table
                },
            );
        }
    }
}

fn probe_native_read_refusal(state: &mut State) -> dellingr::Result<u8> {
    state.push_string("missing")?;
    let top = state.get_top();
    let error = state.get_table(1).unwrap_err();
    assert!(matches!(
        error.kind(),
        ErrorKind::NativeCallDepthExceeded { .. }
    ));
    assert_eq!(state.get_top(), top);
    assert_eq!(state.to_string(-1)?, "missing");
    Ok(0)
}

fn recurse_host_read(state: &mut State) -> dellingr::Result<u8> {
    let remaining = state.to_number(1)?;
    state.get_global("recurse")?;
    state.push_number(remaining)?;
    state.call(dellingr::ArgCount::Fixed(1), dellingr::RetCount::Fixed(0))?;
    Ok(0)
}

#[path = "common/stack.rs"]
mod stack;

fn host_read_native_refusal() -> dellingr::Result<()> {
    for handler in ["function() return 1 end", "tostring"] {
        let mut state = State::new();
        state.push_rust_fn(probe_native_read_refusal)?;
        state.set_global("probe")?;
        state.push_rust_fn(recurse_host_read)?;
        state.set_global("step")?;
        dellingr::Program::compile(
            format!(
                "local t = setmetatable({{}}, {{__index = {handler}}})
                 function recurse(n)
                     if n == 0 then probe(t) else step(n - 1) end
                 end
                 recurse({})",
                dellingr::limits::MAX_NATIVE_CALL_DEPTH - 1,
            ),
            None,
        )
        .and_then(|program| state.load(&program))?;
        state.call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(0))?;
        assert_eq!(state.get_top(), 0);
    }
    Ok(())
}

#[test]
fn get_table_preserves_the_key_at_the_native_call_depth_limit() {
    stack::on_limit_test_stack("host_read_native_refusal", || {
        host_read_native_refusal().unwrap();
    });
}

fn probe_lua_read_refusal(state: &mut State) -> dellingr::Result<u8> {
    state.push_string("missing")?;
    let top = state.get_top();
    let error = state.get_table(1).unwrap_err();
    assert!(matches!(error.kind(), ErrorKind::CallDepthExceeded { .. }));
    assert_eq!(state.get_top(), top);
    assert_eq!(state.to_string(-1)?, "missing");
    Ok(0)
}

#[test]
fn get_table_preserves_the_key_at_the_lua_call_depth_limit() {
    let mut state = State::new();
    state.push_rust_fn(probe_lua_read_refusal).unwrap();
    state.set_global("probe").unwrap();
    dellingr::Program::compile(
        format!(
            "local t = setmetatable({{}}, {{__index = function() return 1 end}})
             local function recurse(n)
                 if n == 0 then probe(t) else recurse(n - 1) end
             end
             recurse({})",
            dellingr::limits::MAX_CALL_DEPTH - 2,
        ),
        None,
    )
    .and_then(|program| state.load(&program))
    .unwrap();
    state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(0))
        .unwrap();
    assert_eq!(state.get_top(), 0);
}
