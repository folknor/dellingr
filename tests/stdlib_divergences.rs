//! VM-041 (`print` output cap), STD-014 (`math.random(m, n)` range) and the
//! STD-015 stdlib divergences: `string.sub` argument order and `table.concat`
//! element errors and key-range refusal, including both unpack entry points.

use std::sync::{Arc, Mutex};

use dellingr::error::{Error, ErrorKind};
use dellingr::limits::MAX_STRING_BYTES;
use dellingr::{ArgCount, HostCallbacks, LuaType, RetCount, State};

struct Prints(Arc<Mutex<Vec<Vec<u8>>>>);

impl HostCallbacks for Prints {
    fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, message: &[u8]) {
        self.0
            .lock()
            .expect("print lock is healthy")
            .push(message.to_owned());
    }
}

fn printing_state() -> (State, Arc<Mutex<Vec<Vec<u8>>>>) {
    let prints = Arc::new(Mutex::new(Vec::new()));
    let state = State::with_callbacks(Box::new(Prints(Arc::clone(&prints))));
    (state, prints)
}

fn run(state: &mut State, source: &str) -> Result<(), Error> {
    dellingr::Program::compile(source, None).and_then(|program| state.load(&program))?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
}

fn expect_error(source: &str) -> Error {
    let mut state = State::new();
    match run(&mut state, source) {
        Ok(()) => panic!("expected an error from {source}"),
        Err(err) => err,
    }
}

#[test]
fn print_hands_the_host_exact_bytes() {
    let (mut state, prints) = printing_state();
    state
        .push_bytes(b"a\xffb\xe2\x82")
        .expect("short string fits");
    state.set_global("s").expect("global set");
    run(
        &mut state,
        "print(s, 1, setmetatable({}, {__tostring = function() return s end}))",
    )
    .expect("print succeeds");
    assert_eq!(
        *prints.lock().expect("print lock is healthy"),
        vec![b"a\xffb\xe2\x82\t1\ta\xffb\xe2\x82".to_vec()]
    );
}

#[test]
fn print_bounds_the_raw_joined_output_by_the_cap() {
    // Invalid UTF-8 is no longer expanded, so a half-cap invalid string
    // prints whole, directly and as a byte-string __tostring result.
    for source in [
        "print(s)",
        "print(setmetatable({}, {__tostring = function() return s end}))",
    ] {
        let (mut state, prints) = printing_state();
        let raw = vec![0xffu8; MAX_STRING_BYTES / 2];
        state.push_bytes(&raw).expect("half-cap string fits");
        state.set_global("s").expect("global set");
        run(&mut state, source).expect("raw output within the cap prints");
        assert_eq!(*prints.lock().expect("print lock is healthy"), vec![raw]);
    }

    // The joined message itself is capped on its raw length.
    let (mut state, prints) = printing_state();
    let raw = vec![0xffu8; MAX_STRING_BYTES / 2 + 1];
    state.push_bytes(&raw).expect("half-cap string fits");
    state.set_global("s").expect("global set");
    let err = run(&mut state, "print(s, s)").expect_err("joined output exceeds the cap");
    assert!(
        matches!(err.kind(), ErrorKind::StringSizeExceeded { size, limit }
            if *size == raw.len() * 2 + 1 && *limit == MAX_STRING_BYTES),
        "{err}"
    );
    assert!(prints.lock().expect("print lock is healthy").is_empty());
}

#[test]
fn math_random_widest_range_stays_in_bounds() {
    let mut state = State::new();
    state.set_rng_seed(7);
    run(
        &mut state,
        r"
        local lo, hi = -2^63, 2^63 - 1024
        for _ = 1, 1000 do
            local v = math.random(lo, hi)
            if v < lo or v > hi or v ~= math.floor(v) then
                error('out of range: ' .. tostring(v))
            end
        end
        for _ = 1, 1000 do
            local v = math.random(-3, 3)
            if v < -3 or v > 3 then error('small range escaped') end
        end
        ",
    )
    .expect("math.random stays in range");
}

#[test]
fn math_random_is_deterministic_per_seed() {
    let draw = |seed| {
        let mut state = State::new();
        state.set_rng_seed(seed);
        dellingr::Program::compile(
            "local t = {} for i = 1, 32 do t[i] = math.random(1, 1000000) end \
                 return table.concat(t, ',')",
            None,
        )
        .and_then(|program| state.load(&program))
        .expect("compiles");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(1))
            .expect("runs");
        state.to_string(-1).expect("string result")
    };
    assert_eq!(draw(42), draw(42));
    assert_ne!(draw(42), draw(43));
}

#[test]
fn string_sub_checks_argument_one_first() {
    let err = expect_error("string.sub()");
    let ErrorKind::ArgError(arg) = err.kind() else {
        panic!("expected an argument error, got {err}");
    };
    assert_eq!(arg.arg_number(), 1);
    assert_eq!(arg.expected(), Some(LuaType::String));
}

#[test]
fn table_concat_names_the_bad_element_and_index() {
    for (source, expected) in [
        (
            "table.concat({1, {}, 3})",
            "invalid value (table) at index 2 in table for 'concat'",
        ),
        (
            "table.concat({1, 2, 3}, ',', 1, 5)",
            "invalid value (nil) at index 4 in table for 'concat'",
        ),
    ] {
        let err = expect_error(source);
        assert!(
            matches!(err.kind(), ErrorKind::LibraryError(m) if m.to_string() == expected),
            "{source}: {err}"
        );
    }
}

#[test]
fn sequence_readers_refuse_ranges_past_exact_keys() {
    for function in ["table.concat", "unpack", "table.unpack"] {
        for range in ["2^53, 2^53 + 2", "-2^53 - 2, -2^53"] {
            let sep = if function == "table.concat" {
                "',', "
            } else {
                ""
            };
            let err = expect_error(&format!("{function}({{}}, {sep}{range})"));
            assert!(
                matches!(err.kind(), ErrorKind::LibraryError(m)
                    if m.to_string().contains("exactly representable integer keys")),
                "{function}({range}): {err}"
            );
        }
    }
}

#[test]
fn sequence_readers_accept_exact_boundaries_singletons_and_empty_ranges() {
    let mut state = State::new();
    run(
        &mut state,
        r"
        local b = 2^53
        local t = setmetatable({}, {__index = function() return 'x' end})
        for _, range in ipairs({{b-1, b}, {-b, -b+1}, {b+2, b+2}}) do
            local i, j = range[1], range[2]
            local expected = i == j and 'x' or 'xx'
            if table.concat(t, '', i, j) ~= expected then error('concat keys') end
            for _, unpacker in ipairs({unpack, table.unpack}) do
                local values = {unpacker(t, i, j)}
                if #values ~= j-i+1 then error('unpack key count') end
                if table.concat(values) ~= expected then error('unpack keys') end
            end
        end
        if table.concat(t, '', b+4, b+2) ~= '' then error('empty concat') end
        if select('#', unpack(t, b+4, b+2)) ~= 0 then error('empty unpack') end
        if select('#', table.unpack(t, b+4, b+2)) ~= 0 then error('empty table.unpack') end
        ",
    )
    .expect("exact and empty key ranges remain valid");
}
