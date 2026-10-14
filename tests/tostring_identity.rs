use dellingr::{ArgCount, RetCount, State};

fn eval_one(state: &mut State, source: &str) {
    state.load_string(source).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
}

#[test]
fn tostring_keeps_non_utf8_bytes_from_metamethod() {
    let mut state = State::new();
    eval_one(
        &mut state,
        "return tostring(setmetatable({}, { __tostring = function() return '\\255\\0a' end }))",
    );
    assert_eq!(state.to_bytes(-1).unwrap(), b"\xff\0a");
}

#[test]
fn host_tostring_bytes_preserve_the_value_and_exact_result() {
    let mut state = State::new();
    eval_one(
        &mut state,
        "return setmetatable({}, { __tostring = function() return '\\255\\0a' end })",
    );
    assert_eq!(state.bytes_with_tostring_meta(-1).unwrap(), b"\xff\0a");
    assert_eq!(state.get_top(), 1);
    assert_eq!(state.typ(-1), dellingr::LuaType::Table);
    assert_eq!(state.to_string_with_meta(-1).unwrap(), "\u{fffd}\0a");
}

#[test]
fn rust_functions_render_with_distinct_stable_identities() {
    let mut state = State::new();
    eval_one(
        &mut state,
        "local a, b = tostring(print), tostring(math.floor)
         if a == b then error('distinct builtins render distinctly') end
         if a ~= tostring(print) then error('identity is stable') end
         if a ~= string.format('%s', print) then error('format %s differs') end
         if a ~= 'function: ' .. string.format('%p', print) then error('format %p differs') end
         if not a:match('^function: 0x%x+$') then error(a) end
         return a",
    );
    let first = state.to_string(-1).unwrap();
    state.pop(1).unwrap();
    state.get_global("print").unwrap();
    assert_eq!(state.to_string(-1).unwrap(), first);
    // Identities are state-local and deterministic: a fresh State replays
    // the same sequence.
    let mut again = State::new();
    eval_one(&mut again, "return tostring(print)");
    assert_eq!(again.to_string(-1).unwrap(), first);
}

#[cfg(feature = "snapshot")]
#[test]
fn rust_function_identity_survives_save_and_load() {
    use dellingr::DefaultCallbacks;
    let mut state = State::new();
    state
        .load_string("before = tostring(print); floor_before = tostring(math.floor)")
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
    let save = state.save_state().expect("state saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("state loads");
    assert_eq!(
        loaded.save_state().expect("loaded state resaves").bytes,
        save.bytes
    );
    // Compare bare pointer ids, since table/function prefixes would mask an
    // id collision. Both functions must retain their original identities.
    let probe = "local fresh = string.format('%p', {})
                 return tostring(print) == before
                    and tostring(math.floor) == floor_before
                    and tostring(print) == 'function: ' .. string.format('%p', print)
                    and tostring(math.floor) == 'function: ' .. string.format('%p', math.floor)
                    and fresh == '0x3'";
    eval_one(&mut loaded, probe);
    assert!(loaded.to_boolean(-1));
}
