//! VM-041 (`print` output cap), STD-014 (`math.random(m, n)` range) and the
//! STD-015 stdlib divergences: `string.sub` argument order and `table.concat`
//! element errors and key-range refusal, including both unpack entry points.

use dellingr::error::{Error, ErrorKind};
use dellingr::limits::MAX_STRING_BYTES;
use dellingr::{LuaType, State};

use crate::support::{self, Recorder};

/// The first error compiling, loading or running `source` raises.
fn expect_error(source: &str) -> Error {
    match support::try_run(&mut State::new(), source) {
        Ok(()) => panic!("expected an error from {source}"),
        Err(err) => err,
    }
}

#[test]
fn print_hands_the_host_exact_bytes() {
    let (mut state, recorder) = Recorder::state();
    state
        .push_bytes(b"a\xffb\xe2\x82")
        .expect("short string fits");
    state.set_global("s").expect("global set");
    support::try_run(
        &mut state,
        "print(s, 1, setmetatable({}, {__tostring = function() return s end}))",
    )
    .expect("print succeeds");
    assert_eq!(
        recorder.prints(),
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
        let (mut state, recorder) = Recorder::state();
        let raw = vec![0xffu8; MAX_STRING_BYTES / 2];
        state.push_bytes(&raw).expect("half-cap string fits");
        state.set_global("s").expect("global set");
        support::try_run(&mut state, source).expect("raw output within the cap prints");
        assert_eq!(recorder.prints(), vec![raw]);
    }

    // The joined message itself is capped on its raw length.
    let (mut state, recorder) = Recorder::state();
    let raw = vec![0xffu8; MAX_STRING_BYTES / 2 + 1];
    state.push_bytes(&raw).expect("half-cap string fits");
    state.set_global("s").expect("global set");
    let err =
        support::try_run(&mut state, "print(s, s)").expect_err("joined output exceeds the cap");
    assert!(
        matches!(err.kind(), ErrorKind::StringSizeExceeded { size, limit }
            if *size == raw.len() * 2 + 1 && *limit == MAX_STRING_BYTES),
        "{err}"
    );
    assert!(recorder.prints().is_empty());
}

#[test]
fn math_random_widest_range_stays_in_bounds() {
    let mut state = State::new();
    state.set_rng_seed(7);
    support::try_run(
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
        support::eval(
            &mut state,
            "local t = {} for i = 1, 32 do t[i] = math.random(1, 1000000) end \
                 return table.concat(t, ',')",
            1,
        );
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
fn optional_math_nil_arguments_use_their_defaults() {
    let mut state = State::new();
    support::try_run(
        &mut state,
        r#"
        if math.atan(0.75, nil) ~= math.atan(0.75) then error('atan nil default') end
        if math.log(8, nil) ~= math.log(8) then error('log nil default') end
        "#,
    )
    .expect("nil optional math arguments use the default");
}

#[test]
fn table_concat_coerces_a_numeric_separator() {
    let mut state = State::new();
    support::try_run(
        &mut state,
        r#"if table.concat({1, 2}, 0) ~= '102' then error('numeric separator') end"#,
    )
    .expect("number separator is converted to its string representation");
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
    support::try_run(
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
