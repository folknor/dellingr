use dellingr::State;

use crate::support;

#[test]
fn tostring_keeps_non_utf8_bytes_from_metamethod() {
    let mut state = State::new();
    support::eval(
        &mut state,
        "return tostring(setmetatable({}, { __tostring = function() return '\\255\\0a' end }))",
        1,
    );
    assert_eq!(state.to_bytes(-1).unwrap(), b"\xff\0a");
}

#[test]
fn host_tostring_bytes_preserve_the_value_and_exact_result() {
    let mut state = State::new();
    support::eval(
        &mut state,
        "return setmetatable({}, { __tostring = function() return '\\255\\0a' end })",
        1,
    );
    assert_eq!(state.tostring_bytes(-1).unwrap(), b"\xff\0a");
    assert_eq!(state.get_top(), 1);
    assert_eq!(state.typ(-1), dellingr::LuaType::Table);
    assert_eq!(state.to_string_with_meta(-1).unwrap(), "\u{fffd}\0a");
}

#[test]
fn rust_functions_render_with_distinct_stable_identities() {
    let mut state = State::new();
    support::eval(
        &mut state,
        "local a, b = tostring(print), tostring(math.floor)
         if a == b then error('distinct builtins render distinctly') end
         if a ~= tostring(print) then error('identity is stable') end
         if a ~= string.format('%s', print) then error('format %s differs') end
         if a ~= 'function: ' .. string.format('%p', print) then error('format %p differs') end
         if not a:match('^function: 0x%x+$') then error(a) end
         return a",
        1,
    );
    let first = state.to_string(-1).unwrap();
    state.pop(1).unwrap();
    state.get_global("print").unwrap();
    assert_eq!(state.to_string(-1).unwrap(), first);
    // Identities are state-local and deterministic: a fresh State replays
    // the same sequence.
    let mut again = State::new();
    support::eval(&mut again, "return tostring(print)", 1);
    assert_eq!(again.to_string(-1).unwrap(), first);
}

#[cfg(feature = "snapshot")]
#[test]
fn rust_function_identity_survives_save_and_load() {
    use dellingr::DefaultCallbacks;
    let mut state = State::new();
    support::run(
        &mut state,
        "before = tostring(print); floor_before = tostring(math.floor)",
    );
    let save = state.save_state().expect("state saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup)
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
    support::eval(&mut loaded, probe, 1);
    assert!(loaded.to_boolean(-1));
}

/// A string literal reachable only through a saved closure's literal pool keeps
/// its `%p` identity across save and load.
#[cfg(feature = "snapshot")]
#[test]
fn closure_literal_identity_survives_save_and_load() {
    use dellingr::DefaultCallbacks;
    let mut state = State::new();
    support::run(
        &mut state,
        "render = function() return string.format('%p', 'only_in_bytecode') end
             before = render()",
    );

    let save = state.save_state().expect("state saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup)
        .expect("state loads");
    assert_eq!(
        loaded.save_state().expect("loaded state resaves").bytes,
        save.bytes
    );
    support::eval(&mut loaded, "return render() == before", 1);
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
    support::eval(&mut state, render, 1);
    let before = state.to_string(-1).unwrap();
    state.pop(1).unwrap();
    state.gc_collect();
    support::eval(&mut state, render, 1);
    assert_eq!(state.to_string(-1).unwrap(), before);
    state.pop(1).unwrap();
    state.gc_collect();

    let save = state.save_state().expect("state saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup)
        .expect("state loads");
    assert_eq!(
        loaded.save_state().expect("loaded state resaves").bytes,
        save.bytes
    );
    support::eval(&mut loaded, render, 1);
    assert_eq!(loaded.to_string(-1).unwrap(), before);
}

#[cfg(feature = "snapshot")]
#[test]
fn hidden_environment_identities_survive_save_and_load() {
    use dellingr::DefaultCallbacks;
    let mut state = State::new();
    // Keep only rendered strings, so no saved global reaches either table.
    support::eval(
        &mut state,
        "meta_before = tostring(getmetatable(''))
         library_before = tostring(string)
         key_ids = ''
         for key in pairs(string) do key_ids = key_ids .. string.format('%p', key) .. ',' end
         string = false
         return true",
        1,
    );
    state.pop(1).unwrap();
    let save = state.save_state().unwrap();
    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();
    assert_eq!(loaded.save_state().unwrap().bytes, save.bytes);
    support::eval(
        &mut loaded,
        "local library = getmetatable('').__index
         if meta_before ~= tostring(getmetatable('')) then error('metatable identity') end
         if library_before ~= tostring(library) then error('library identity') end
         local after = ''
         for key in pairs(library) do after = after .. library.format('%p', key) .. ',' end
         if key_ids ~= after then error('key identity') end
         return string == false",
        1,
    );
    assert!(loaded.to_boolean(-1));
}
