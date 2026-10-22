use dellingr::{ArgCount, RetCount, State};

fn eval_one(state: &mut State, source: &str) {
    dellingr::Program::compile(source, None)
        .and_then(|program| state.load(&program))
        .unwrap();
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
    assert_eq!(state.tostring_bytes(-1).unwrap(), b"\xff\0a");
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
    dellingr::Program::compile(
        "before = tostring(print); floor_before = tostring(math.floor)",
        None,
    )
    .and_then(|program| state.load(&program))
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

/// A string literal reachable only through a saved closure's literal pool keeps
/// its `%p` identity across save and load.
#[cfg(feature = "snapshot")]
#[test]
fn closure_literal_identity_survives_save_and_load() {
    use dellingr::DefaultCallbacks;
    let mut state = State::new();
    dellingr::Program::compile(
        "render = function() return string.format('%p', 'only_in_bytecode') end
             before = render()",
        None,
    )
    .and_then(|program| state.load(&program))
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
    eval_one(&mut loaded, "return render() == before");
    assert!(
        loaded.to_boolean(-1),
        "a closure literal rendered a fresh identity after load"
    );
}

/// Metamethod event names are permanent State strings, so an identity given
/// to one survives collection even when no saved value reaches it. The
/// snapshot must carry that identity, or the loaded State renders anew.
#[cfg(feature = "snapshot")]
#[test]
fn metamethod_name_identity_survives_collection_save_and_load() {
    use dellingr::DefaultCallbacks;
    // Built by concatenation so no bytecode literal holds the name.
    let render = "return string.format('%p', '__c' .. 'all')";
    let mut state = State::new();
    eval_one(&mut state, render);
    let before = state.to_string(-1).unwrap();
    state.pop(1).unwrap();
    state.gc_collect();
    eval_one(&mut state, render);
    assert_eq!(state.to_string(-1).unwrap(), before);
    state.pop(1).unwrap();
    state.gc_collect();

    let save = state.save_state().expect("state saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("state loads");
    assert_eq!(
        loaded.save_state().expect("loaded state resaves").bytes,
        save.bytes
    );
    eval_one(&mut loaded, render);
    assert_eq!(loaded.to_string(-1).unwrap(), before);
}
