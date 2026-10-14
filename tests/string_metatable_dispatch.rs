//! Host and script dispatch through the shared string metatable: host
//! `get_table` on a string receiver, `__tostring` and `__call` on strings, and
//! the size preflight on lossy host string conversion.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, LuaType, MAX_STRING_BYTES, RetCount, State};

fn run(state: &mut State, source: &str) {
    state.load_string(source).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
}

#[test]
fn host_get_table_on_string_matches_the_opcode() {
    let mut state = State::new();
    state.push_string("abc").unwrap();
    state.push_string("upper").unwrap();
    state.get_table(-2).unwrap();
    assert_eq!(state.get_top(), 2);
    state.get_global("string").unwrap();
    state.push_string("upper").unwrap();
    state.get_table(-2).unwrap();
    // `("abc").upper` is `string.upper`.
    assert!(state.raw_equal(-1, -3));
}

#[test]
fn host_get_table_on_string_follows_a_redirected_index() {
    let mut state = State::new();
    run(
        &mut state,
        "getmetatable('').__index = function(s, k) return s .. '/' .. k end",
    );
    state.push_string("abc").unwrap();
    state.push_string("key").unwrap();
    state.gc_set_threshold(0);
    state.get_table(1).unwrap();
    assert_eq!(state.to_bytes(-1).unwrap(), b"abc/key");
}

#[test]
fn host_get_table_on_string_without_index_leaves_the_stack() {
    let mut state = State::new();
    run(&mut state, "getmetatable('').__index = nil");
    state.push_string("abc").unwrap();
    state.push_string("upper").unwrap();
    assert!(state.get_table(1).is_err());
    assert_eq!(state.get_top(), 2);
    assert_eq!(state.to_bytes(-1).unwrap(), b"upper");
}

#[test]
fn host_get_table_on_string_preserves_operands_at_the_stack_cap() {
    let mut state = State::new();
    state.push_string("abc").unwrap();
    let cap = dellingr::limits::MAX_STACK_SIZE;
    state.set_top(isize::try_from(cap - 1).unwrap()).unwrap();
    state.push_string("upper").unwrap();
    let error = state.get_table(1).unwrap_err();
    assert!(matches!(error.kind, ErrorKind::StackOverflow { .. }));
    assert_eq!(state.get_top(), cap);
    assert_eq!(state.to_bytes(1).unwrap(), b"abc");
    assert_eq!(state.to_bytes(-1).unwrap(), b"upper");
}

#[test]
fn error_keeps_a_string_message_raw_despite_tostring() {
    let mut state = State::new();
    state
        .load_string(
            "getmetatable('').__tostring = function() return 'wrong' end error('raw message')",
        )
        .unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .unwrap_err();
    assert!(matches!(
        error.kind,
        ErrorKind::ScriptError { message, .. } if message == "raw message"
    ));
}

#[test]
fn string_tostring_metamethod_applies_to_tostring() {
    let mut state = State::new();
    run(
        &mut state,
        "local mt = getmetatable('')
         if tostring('abc') ~= 'abc' then error('default tostring changed') end
         mt.__tostring = function(s) return 'TS<' .. #s .. '>' end
         local r = tostring('abc')
         mt.__tostring = nil
         if r ~= 'TS<3>' then error('string __tostring ignored: ' .. r) end
         if tostring('abc') ~= 'abc' then error('cleared __tostring still applies') end
         mt.__tostring = function(s) return 's' end
         r = tostring('x')
         mt.__tostring = nil
         if r ~= 's' then error('second handler ignored') end",
    );
}

#[test]
fn host_to_string_with_meta_honours_string_tostring() {
    let mut state = State::new();
    run(
        &mut state,
        "getmetatable('').__tostring = function(s) return '<' .. s .. '>' end",
    );
    state.push_string("abc").unwrap();
    assert_eq!(state.to_string_with_meta(-1).unwrap(), "<abc>");
    assert_eq!(state.to_string(-1).unwrap(), "abc");
    assert_eq!(state.typ(-1), LuaType::String);
}

#[test]
fn default_string_conversion_does_not_intern_a_metamethod_key() {
    let mut state = State::new();
    state.push_string("plain").unwrap();
    let strings = state.string_count();
    assert_eq!(state.to_string_with_meta(-1).unwrap(), "plain");
    assert_eq!(state.string_count(), strings);
}

#[test]
fn string_call_roots_the_receiver_and_active_handler_during_gc() {
    fn collect(state: &mut State) -> dellingr::Result<u8> {
        state.gc_collect();
        Ok(0)
    }

    let mut state = State::new();
    state.push_rust_fn(collect).unwrap();
    state.set_global("collect").unwrap();
    run(
        &mut state,
        "local captured = {suffix = '!'}
         getmetatable('').__call = function(s)
             getmetatable('').__call = nil
             collect()
             return s .. captured.suffix
         end",
    );
    // This receiver has no bytecode literal or global keeping it alive.
    state.push_string("host-only receiver").unwrap();
    // Force collection while interning __call, after removing the receiver
    // from the stack, as well as inside the now-detached handler.
    state.gc_set_threshold(0);
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_bytes(-1).unwrap(), b"host-only receiver!");
}

#[test]
fn string_call_metamethod_makes_strings_callable() {
    let mut state = State::new();
    run(
        &mut state,
        "local mt = getmetatable('')
         mt.__call = function(s, a, b) return s .. a .. b end
         local r1 = ('x')(1, 2)
         local s = 'y'
         local r2 = s(3, 4)
         mt.__call = nil
         if r1 ~= 'x12' then error('literal call: ' .. tostring(r1)) end
         if r2 ~= 'y34' then error('local call: ' .. tostring(r2)) end",
    );
}

#[test]
fn string_call_without_metamethod_is_an_error() {
    let mut state = State::new();
    state.load_string("local s = 'x' return s(1)").unwrap();
    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .unwrap_err();
    assert!(err.to_string().contains("string"), "{err}");
}

#[test]
fn string_call_through_a_callable_table_chains() {
    let mut state = State::new();
    run(
        &mut state,
        "local mt = getmetatable('')
         mt.__call = setmetatable({}, { __call = function(self, s, a) return 'nested ' .. s .. a end })
         local r = ('w')(9)
         mt.__call = nil
         if r ~= 'nested w9' then error('chain: ' .. tostring(r)) end",
    );
}

#[test]
fn lossy_host_conversion_is_size_checked() {
    // Each 0xff byte expands to a 3-byte U+FFFD, so this capped string would
    // convert to well over MAX_STRING_BYTES.
    let raw = vec![0xffu8; MAX_STRING_BYTES / 3 + 1];
    let mut state = State::new();
    state.push_bytes(&raw).unwrap();
    for error in [
        state.to_string_with_meta(-1).unwrap_err(),
        state.to_string(-1).unwrap_err(),
    ] {
        assert!(matches!(
            error.kind,
            ErrorKind::StringSizeExceeded { size, limit }
                if size == raw.len() * 3 && limit == MAX_STRING_BYTES
        ));
    }
    assert_eq!(state.get_top(), 1);
    // Valid text at the same length still converts.
    state
        .push_bytes(vec![b'a'; MAX_STRING_BYTES / 3 + 1])
        .unwrap();
    assert_eq!(
        state.to_string_with_meta(-1).unwrap().len(),
        MAX_STRING_BYTES / 3 + 1
    );

    // A small receiver can produce the same over-cap expansion via a handler.
    state.push_value(1).unwrap();
    state.set_global("lossy_result").unwrap();
    run(
        &mut state,
        "getmetatable('').__tostring = function() return lossy_result end",
    );
    state.push_string("small").unwrap();
    assert!(matches!(
        state.to_string_with_meta(-1).unwrap_err().kind,
        ErrorKind::StringSizeExceeded { size, limit }
            if size == raw.len() * 3 && limit == MAX_STRING_BYTES
    ));
    assert_eq!(state.to_string(-1).unwrap(), "small");
}
