//! `State::raw_field` reads typed fields of a table argument from inside
//! host functions: conversions and their exact edges, absent versus nil, raw
//! (no `__index`) reads, argument errors that name the field, and that a read
//! leaves the stack and the cost meter alone.

use dellingr::error::{ArgDetail, Error, ErrorKind};
use dellingr::{ArgCount, FieldName, LuaType, RetCount, State};

use crate::support;
use crate::support::PushNewRustFn;

/// Defines a host function `$name` that reads field `v` of its first
/// argument as `$ty` and returns the decimal rendering of the value.
macro_rules! reader {
    ($name:ident, $ty:ty) => {
        fn $name(state: &mut State) -> dellingr::Result<u8> {
            let value: $ty = state.raw_field(1, "v")?;
            state.push_string(value.to_string())?;
            Ok(1)
        }
    };
}

reader!(read_f64, f64);
reader!(read_i8, i8);
reader!(read_i16, i16);
reader!(read_i32, i32);
reader!(read_i64, i64);
reader!(read_u8, u8);
reader!(read_u16, u16);
reader!(read_u32, u32);
reader!(read_u64, u64);
reader!(read_bool, bool);
reader!(read_string, String);

fn read_bytes(state: &mut State) -> dellingr::Result<u8> {
    let value: Vec<u8> = state.raw_field(1, "v")?;
    state.push_string(format!("{value:?}"))?;
    Ok(1)
}

/// Borrowed reads: the rendering is owned before the push needs `&mut`.
fn read_str(state: &mut State) -> dellingr::Result<u8> {
    let value: &str = state.raw_field(1, "v")?;
    let rendered = value.to_owned();
    state.push_string(rendered)?;
    Ok(1)
}

fn read_byte_slice(state: &mut State) -> dellingr::Result<u8> {
    let value: &[u8] = state.raw_field(1, "v")?;
    let rendered = format!("{value:?}");
    state.push_string(rendered)?;
    Ok(1)
}

fn read_opt_str(state: &mut State) -> dellingr::Result<u8> {
    let value: Option<&str> = state.raw_field(1, "v")?;
    let rendered = format!("{value:?}");
    state.push_string(rendered)?;
    Ok(1)
}

fn read_opt_u8(state: &mut State) -> dellingr::Result<u8> {
    let value: Option<u8> = state.raw_field(1, "v")?;
    state.push_string(format!("{value:?}"))?;
    Ok(1)
}

fn read_opt_string(state: &mut State) -> dellingr::Result<u8> {
    let value: Option<String> = state.raw_field(1, "v")?;
    state.push_string(format!("{value:?}"))?;
    Ok(1)
}

/// The option-table shape from the ticket: the first argument is `{player, x,
/// flags}`.
fn spawn(state: &mut State) -> dellingr::Result<u8> {
    let player: u32 = state.raw_field(1, "player")?;
    let x: f64 = state.raw_field(1, "x")?;
    let flags: Option<String> = state.raw_field(1, "flags")?;
    state.push_string(format!("{player} {x} {flags:?}"))?;
    Ok(1)
}

/// Reads at argument 0, which is not a valid index.
fn read_arg_zero(state: &mut State) -> dellingr::Result<u8> {
    let value: f64 = state.raw_field(0, "v")?;
    state.push_number(value)?;
    Ok(1)
}

/// Reads at argument 2 so the table is not the first argument.
fn read_second(state: &mut State) -> dellingr::Result<u8> {
    let value: f64 = state.raw_field(2, "v")?;
    state.push_number(value)?;
    Ok(1)
}

/// Returns whether successful and failing reads left the stack height and the
/// cost meter exactly as they were.
fn neutral(state: &mut State) -> dellingr::Result<u8> {
    let top = state.get_top();
    let cost = state.cost_used();
    let number: f64 = state.raw_field(1, "n")?;
    let text: String = state.raw_field(1, "s")?;
    let bytes: Vec<u8> = state.raw_field(1, "s")?;
    let missing: Option<u8> = state.raw_field(1, "absent")?;
    let failed = state.raw_field::<bool>(1, "n").is_err();
    let failed_missing = state.raw_field::<f64>(1, "absent").is_err();
    let ok = number == 7.0
        && text == "hi"
        && bytes == b"hi"
        && missing.is_none()
        && failed
        && failed_missing
        && state.get_top() == top
        && state.cost_used() == cost;
    state.push_boolean(ok)?;
    Ok(1)
}

fn install(state: &mut State) {
    let functions: [(&str, dellingr::RustFunc); 20] = [
        ("read_f64", read_f64),
        ("read_i8", read_i8),
        ("read_i16", read_i16),
        ("read_i32", read_i32),
        ("read_i64", read_i64),
        ("read_u8", read_u8),
        ("read_u16", read_u16),
        ("read_u32", read_u32),
        ("read_u64", read_u64),
        ("read_bool", read_bool),
        ("read_string", read_string),
        ("read_bytes", read_bytes),
        ("read_str", read_str),
        ("read_byte_slice", read_byte_slice),
        ("read_opt_str", read_opt_str),
        ("read_opt_u8", read_opt_u8),
        ("read_opt_string", read_opt_string),
        ("spawn", spawn),
        ("read_arg_zero", read_arg_zero),
        ("read_second", read_second),
    ];
    for (name, function) in functions {
        state
            .push_named_rust_fn(name, function)
            .expect("function fits");
        state.set_global(name).expect("global is set");
    }
    state.push_new_rust_fn(neutral).expect("function fits");
    state.set_global("neutral").expect("global is set");
}

/// Runs `source` (which returns one value) and renders that value.
fn run(source: &str) -> Result<String, Error> {
    let mut state = State::new();
    install(&mut state);
    support::load(&mut state, source);
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1))?;
    assert_eq!(state.get_top(), 1);
    state.to_string(-1)
}

fn ok(source: &str) -> String {
    run(source).unwrap_or_else(|error| panic!("{source} must succeed: {error}"))
}

fn failure(source: &str) -> Error {
    match run(source) {
        Ok(value) => panic!("{source} must fail, returned {value}"),
        Err(error) => error,
    }
}

/// The message of the argument error `source` raises, with its field.
fn field_failure(source: &str) -> (String, String) {
    let error = failure(source);
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("{source} must raise an argument error, got {error}");
    };
    let field = argument
        .field()
        .unwrap_or_else(|| panic!("{source} must name a field: {argument}"))
        .to_owned();
    (argument.to_string(), field)
}

fn assert_detail(source: &str, expected: &str, detail: &ArgDetail) {
    let error = failure(source);
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("{source} must raise an argument error, got {error}");
    };
    assert_eq!(argument.to_string(), expected, "{source}");
    assert_eq!(argument.field(), Some("v"), "{source}");
    assert_eq!(argument.detail(), detail, "{source}");
}

#[test]
fn the_ticket_example_reads_an_option_table() {
    assert_eq!(
        ok(r#"return spawn{player = 1, x = 2.5, flags = "Structure"}"#),
        "1 2.5 Some(\"Structure\")"
    );
    assert_eq!(ok("return spawn{player = 1, x = 2}"), "1 2 None");
    assert_eq!(
        ok("return spawn{player = 1, x = 2, flags = nil}"),
        "1 2 None"
    );
}

#[test]
fn f64_takes_any_number() {
    assert_eq!(ok("return read_f64{v = 1.5}"), "1.5");
    assert_eq!(ok("return read_f64{v = -3}"), "-3");
    assert_eq!(ok("return read_f64{v = 1/0}"), "inf");
    assert_eq!(ok("return read_f64{v = -1/0}"), "-inf");
    assert_eq!(ok("return read_f64{v = 0/0}"), "NaN");
}

#[test]
fn integers_take_in_range_integral_numbers() {
    assert_eq!(ok("return read_i8{v = -128}"), "-128");
    assert_eq!(ok("return read_i8{v = 127}"), "127");
    assert_eq!(ok("return read_i16{v = -32768}"), "-32768");
    assert_eq!(ok("return read_i16{v = 32767}"), "32767");
    assert_eq!(ok("return read_i32{v = -2^31}"), "-2147483648");
    assert_eq!(ok("return read_i32{v = 2^31 - 1}"), "2147483647");
    assert_eq!(ok("return read_u8{v = 0}"), "0");
    assert_eq!(ok("return read_u8{v = 255}"), "255");
    assert_eq!(ok("return read_u16{v = 65535}"), "65535");
    assert_eq!(ok("return read_u32{v = 2^32 - 1}"), "4294967295");
    assert_eq!(ok("return read_i64{v = 5}"), "5");
    assert_eq!(ok("return read_u64{v = 5}"), "5");
    assert_eq!(ok("return read_u64{v = 2^53}"), "9007199254740992");
}

#[test]
fn i64_edges() {
    assert_eq!(ok("return read_i64{v = -2^63}"), "-9223372036854775808");
    assert_out_of_range("read_i64", "2^63");
    assert_out_of_range("read_i64", "-2^63 - 2048");
    // The largest f64 below 2^63 is 2^63 - 1024.
    assert_eq!(
        ok("return read_i64{v = 2^63 - 1024}"),
        "9223372036854774784"
    );
}

#[test]
fn u64_edges() {
    assert_eq!(
        ok("return read_u64{v = 2^64 - 2048}"),
        "18446744073709549568"
    );
    assert_eq!(ok("return read_u64{v = -0.0}"), "0");
    assert_out_of_range("read_u64", "2^64");
    assert_out_of_range("read_u64", "-1");
}

#[test]
fn small_integer_edges() {
    assert_eq!(ok("return read_u8{v = -0.0}"), "0");
    assert_out_of_range("read_u8", "256");
    assert_out_of_range("read_u8", "-1");
    assert_out_of_range("read_i8", "128");
    assert_out_of_range("read_i8", "-129");
    assert_out_of_range("read_u16", "65536");
    assert_out_of_range("read_i16", "32768");
    assert_out_of_range("read_u32", "2^32");
    assert_out_of_range("read_i32", "2^31");
    assert_out_of_range("read_i32", "-2^31 - 1");
    assert_out_of_range("read_u8", "1e300");
}

fn assert_out_of_range(function: &str, value: &str) {
    assert_detail(
        &format!("return {function}{{v = {value}}}"),
        &format!("bad argument #1 to '{function}' (field 'v': value out of range)"),
        &ArgDetail::ValueOutOfRange,
    );
}

#[test]
fn non_integral_numbers_are_no_integer() {
    for function in [
        "read_i8", "read_u8", "read_i32", "read_u32", "read_i64", "read_u64",
    ] {
        for value in ["0.5", "-0.5", "0/0", "1/0", "-1/0"] {
            assert_detail(
                &format!("return {function}{{v = {value}}}"),
                &format!(
                    "bad argument #1 to '{function}' (field 'v': number has no integer representation)"
                ),
                &ArgDetail::NoInteger,
            );
        }
    }
}

#[test]
fn bool_takes_only_a_boolean() {
    assert_eq!(ok("return read_bool{v = true}"), "true");
    assert_eq!(ok("return read_bool{v = false}"), "false");
    for value in ["0", "1", "\"true\"", "{}", "nil"] {
        let (message, field) = field_failure(&format!("return read_bool{{v = {value}}}"));
        assert_eq!(field, "v");
        assert!(
            message
                .starts_with("bad argument #1 to 'read_bool' (field 'v': boolean expected, got "),
            "{message}"
        );
    }
}

#[test]
fn strings_require_utf8_and_bytes_are_exact() {
    assert_eq!(ok(r#"return read_string{v = "héllo"}"#), "héllo");
    assert_eq!(ok(r#"return read_string{v = ""}"#), "");
    assert_eq!(ok(r#"return read_string{v = "a\0b"}"#), "a\0b");
    assert_detail(
        r#"return read_string{v = "\255"}"#,
        "bad argument #1 to 'read_string' (field 'v': invalid UTF-8)",
        &ArgDetail::InvalidUtf8,
    );
    assert_eq!(ok(r#"return read_bytes{v = "\255\0a"}"#), "[255, 0, 97]");
    assert_eq!(ok(r#"return read_bytes{v = ""}"#), "[]");
}

#[test]
fn borrowed_strings_read_like_owned_ones() {
    assert_eq!(ok(r#"return read_str{v = "héllo"}"#), "héllo");
    assert_eq!(ok(r#"return read_str{v = "a\0b"}"#), "a\0b");
    assert_detail(
        r#"return read_str{v = "\255"}"#,
        "bad argument #1 to 'read_str' (field 'v': invalid UTF-8)",
        &ArgDetail::InvalidUtf8,
    );
    assert_eq!(
        field_failure("return read_str{v = 1}").0,
        "bad argument #1 to 'read_str' (field 'v': string expected, got number)"
    );
    assert_eq!(
        ok(r#"return read_byte_slice{v = "\255\0a"}"#),
        "[255, 0, 97]"
    );
    assert_eq!(ok("return read_opt_str{}"), "None");
    assert_eq!(ok(r#"return read_opt_str{v = "x"}"#), r#"Some("x")"#);
}

#[test]
fn numbers_and_strings_do_not_coerce() {
    assert_eq!(
        field_failure(r#"return read_f64{v = "1"}"#).0,
        "bad argument #1 to 'read_f64' (field 'v': number expected, got string)"
    );
    assert_eq!(
        field_failure(r#"return read_u8{v = "1"}"#).0,
        "bad argument #1 to 'read_u8' (field 'v': number expected, got string)"
    );
    assert_eq!(
        field_failure("return read_string{v = 1}").0,
        "bad argument #1 to 'read_string' (field 'v': string expected, got number)"
    );
    assert_eq!(
        field_failure("return read_bytes{v = 1}").0,
        "bad argument #1 to 'read_bytes' (field 'v': string expected, got number)"
    );
}

#[test]
fn missing_and_nil_are_the_same() {
    assert_eq!(ok("return read_opt_u8{}"), "None");
    assert_eq!(ok("return read_opt_u8{v = nil}"), "None");
    assert_eq!(ok("return read_opt_u8{v = 3}"), "Some(3)");
    assert_eq!(ok("return read_opt_string{}"), "None");
    assert_eq!(ok("return read_opt_string{v = \"a\"}"), "Some(\"a\")");
    for source in ["return read_f64{}", "return read_f64{v = nil}"] {
        let (message, field) = field_failure(source);
        assert_eq!(field, "v");
        assert_eq!(
            message,
            "bad argument #1 to 'read_f64' (field 'v': number expected, got nil)"
        );
    }
    // An Option still checks a present value.
    assert_eq!(
        field_failure("return read_opt_u8{v = \"x\"}").0,
        "bad argument #1 to 'read_opt_u8' (field 'v': number expected, got string)"
    );
    assert_eq!(
        field_failure("return read_opt_u8{v = 256}").0,
        "bad argument #1 to 'read_opt_u8' (field 'v': value out of range)"
    );
}

#[test]
fn a_field_from_index_reads_as_absent() {
    let source = "local base = {v = 5}\n\
                  local t = setmetatable({}, {__index = base})\n\
                  if t.v ~= 5 then error('index works') end\n";
    assert_eq!(ok(&format!("{source}return read_opt_u8(t)")), "None");
    assert_eq!(
        field_failure(&format!("{source}return read_u8(t)")).0,
        "bad argument #1 to 'read_u8' (field 'v': number expected, got nil)"
    );
    // An own field still wins over the metatable.
    assert_eq!(
        ok("local t = setmetatable({v = 2}, {__index = {v = 5}})\nreturn read_u8(t)"),
        "2"
    );
    // __index functions never run.
    assert_eq!(
        ok(
            "local t = setmetatable({}, {__index = function() error('ran') end})\n\
            return read_opt_u8(t)"
        ),
        "None"
    );
}

#[test]
fn a_key_no_string_spells_reads_as_absent() {
    // The field name is only ever spelled in the host function, never in the
    // script, so it may not exist in the string pool at all.
    fn read_unspelled(state: &mut State) -> dellingr::Result<u8> {
        let value: Option<f64> = state.raw_field(1, "never_interned_anywhere_8c1f")?;
        state.push_boolean(value.is_none())?;
        Ok(1)
    }
    let mut state = State::new();
    state
        .push_new_rust_fn(read_unspelled)
        .expect("function fits");
    state.set_global("read_unspelled").unwrap();
    support::load(&mut state, "return read_unspelled{a = 1}");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("the read succeeds");
    assert!(state.to_boolean(-1));
}

#[test]
fn a_non_table_or_missing_argument_is_the_ordinary_table_error() {
    for (source, received) in [
        ("return read_f64()", None),
        ("return read_f64(nil)", Some(LuaType::Nil)),
        ("return read_f64(5)", Some(LuaType::Number)),
        ("return read_f64('v')", Some(LuaType::String)),
        ("return read_f64(print)", Some(LuaType::Function)),
    ] {
        let error = failure(source);
        let ErrorKind::ArgError(argument) = error.kind() else {
            panic!("{source} must raise an argument error, got {error}");
        };
        assert_eq!(argument.field(), None, "{source}");
        assert_eq!(argument.arg_number(), 1, "{source}");
        assert_eq!(argument.func_name(), Some("read_f64"), "{source}");
        assert_eq!(argument.expected(), Some(LuaType::Table), "{source}");
        assert_eq!(argument.received(), received, "{source}");
        assert!(
            argument
                .to_string()
                .starts_with("bad argument #1 to 'read_f64' (table expected, got "),
            "{argument}"
        );
    }
}

#[test]
fn the_table_may_be_a_later_argument() {
    assert_eq!(ok("return read_second(1, {v = 4})"), "4");
    let error = failure("return read_second(1, {v = 'x'})");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected an argument error, got {error}");
    };
    assert_eq!(argument.arg_number(), 2);
    assert_eq!(argument.field(), Some("v"));
    assert_eq!(
        argument.to_string(),
        "bad argument #2 to 'read_second' (field 'v': number expected, got string)"
    );
    let error = failure("return read_second(1)");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected an argument error, got {error}");
    };
    assert_eq!(argument.arg_number(), 2);
    assert_eq!(argument.field(), None);
    assert_eq!(argument.expected(), Some(LuaType::Table));
}

#[test]
fn argument_zero_is_an_invalid_stack_index() {
    let error = failure("return read_arg_zero{v = 1}");
    assert!(
        matches!(error.kind(), ErrorKind::InvalidStackIndex { index: 0 }),
        "{error}"
    );
}

#[test]
fn field_errors_carry_the_field_and_describe_it() {
    let error = failure("return spawn{player = 1, x = 'far'}");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected an argument error, got {error}");
    };
    assert_eq!(argument.field(), Some("x"));
    assert_eq!(argument.arg_number(), 1);
    assert_eq!(argument.func_name(), Some("spawn"));
    assert_eq!(argument.detail(), &ArgDetail::TypeMismatch);
    assert_eq!(argument.expected(), Some(LuaType::Number));
    assert_eq!(argument.received(), Some(LuaType::String));
    assert!(!argument.is_method_call());
    assert_eq!(
        argument.to_string(),
        "bad argument #1 to 'spawn' (field 'x': number expected, got string)"
    );
    assert_eq!(
        field_failure("return spawn{player = 1.5, x = 1}"),
        (
            "bad argument #1 to 'spawn' (field 'player': number has no integer representation)"
                .to_owned(),
            "player".to_owned()
        )
    );
    assert_eq!(
        field_failure("return spawn{player = -1, x = 1}").0,
        "bad argument #1 to 'spawn' (field 'player': value out of range)"
    );
    assert_eq!(
        field_failure("return spawn{player = 1, x = 1, flags = 4}").0,
        "bad argument #1 to 'spawn' (field 'flags': string expected, got number)"
    );
    assert_eq!(
        field_failure("return spawn{x = 1}").0,
        "bad argument #1 to 'spawn' (field 'player': number expected, got nil)"
    );
}

#[test]
fn a_method_call_uses_the_self_wording() {
    let (message, field) =
        field_failure("local obj = {x = 'far', player = 1, spawn = spawn}\nreturn obj:spawn()");
    assert_eq!(field, "x");
    assert_eq!(
        message,
        "calling 'spawn' on bad self (field 'x': number expected, got string)"
    );
    let error = failure("local obj = {x = 'far', player = 1, spawn = spawn}\nreturn obj:spawn()");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected an argument error, got {error}");
    };
    assert!(argument.is_method_call());
    assert_eq!(argument.arg_number(), 1);
}

#[test]
fn reads_leave_the_stack_and_cost_unchanged() {
    assert_eq!(ok("return neutral{n = 7, s = 'hi'}"), "true");

    // Also measured from outside: a call that reads costs exactly what a call
    // that does not read costs.
    fn nop(state: &mut State) -> dellingr::Result<u8> {
        state.push_string("3")?;
        Ok(1)
    }
    let cost_of = |source: &str| {
        let mut state = State::new();
        install(&mut state);
        state.push_new_rust_fn(nop).expect("function fits");
        state.set_global("nop").unwrap();
        support::run(&mut state, source);
        state.cost_used()
    };
    let reading = cost_of("local r = read_u8{v = 3}\nlocal s = read_string{v = 'abc'}");
    let idle = cost_of("local r = nop{v = 3}\nlocal s = nop{v = 'abc'}");
    assert!(idle > 0);
    assert_eq!(reading, idle);
}

/// A field error from a host function stored in a table is located at the
/// script line that called it, in that script's chunk.
#[test]
fn field_errors_report_the_calling_script_line() {
    let mut state = State::new();
    install(&mut state);
    let program = dellingr::Program::compile(
        "local game = {spawn = read_u8}\nlocal ok = 1\ngame.spawn{v = 'x'}",
        Some(b"map.lua"),
    )
    .expect("the map compiles");
    state.load(&program).expect("the map loads");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("the bad field fails");
    assert_eq!(error.line_num(), 3);
    assert_eq!(error.source_name(), Some(b"map.lua".as_slice()));
    assert_eq!(
        error.to_string().lines().next(),
        Some("map.lua:3: bad argument #1 to 'read_u8' (field 'v': number expected, got string)")
    );
}

// A descriptor holds only a slot hint, which every read validates, so a hint
// left by another test changes no result.
// process-global: a validated slot hint only, see above.
static HINTED_V: FieldName = FieldName::new("v");
// process-global: as above.
static HINTED_ONE: FieldName = FieldName::new("1");

/// Reads fields `v` and `"1"` of its first argument through shared
/// descriptors and by plain name, fails unless the two agree, and returns
/// the reading of `v`.
fn hinted_agrees(state: &mut State) -> dellingr::Result<u8> {
    let hinted: Option<f64> = state.raw_field(1, &HINTED_V)?;
    let plain: Option<f64> = state.raw_field(1, "v")?;
    let hinted_one: Option<f64> = state.raw_field(1, &HINTED_ONE)?;
    let plain_one: Option<f64> = state.raw_field(1, "1")?;
    if hinted != plain || hinted_one != plain_one {
        return Err(Error::without_location(ErrorKind::HostError(
            format!(
                "descriptor read {hinted:?}/{hinted_one:?}, plain read {plain:?}/{plain_one:?}"
            )
            .into(),
        )));
    }
    match hinted {
        Some(value) => state.push_number(value)?,
        None => state.push_nil()?,
    }
    Ok(1)
}

/// A descriptor's hint goes stale as tables differ, lose the field, regain
/// it and rebuild; every read still matches the plain name.
#[test]
fn a_descriptor_reads_what_the_plain_name_reads_across_layouts() {
    let mut state = State::new();
    state
        .push_named_rust_fn("check", hinted_agrees)
        .expect("function fits");
    state.set_global("check").expect("global is set");
    support::load(
        &mut state,
        r#"
        local sum = 0
        local function read(t)
            local v = check(t)
            if v then sum = sum + v end
        end
        local a = {a = 1, b = 2, c = 3, d = 4, e = 5, v = 10}
        local b = {v = 20, a = 1, b = 2, c = 3, d = 4, e = 5}
        for _ = 1, 3 do read(a) read(b) end
        -- The hint points at a's slot for v when it becomes a tombstone.
        read(a)
        a.v = nil
        read(a)
        a.v = 30
        read(a)
        for i = 1, 64 do a["k" .. i] = i end
        read(a)
        -- Enough tombstones that the next insert compacts, moving v to
        -- another slot under a warm hint.
        local c = {a = 1, b = 2, c = 3, d = 4, e = 5, v = 40}
        read(c)
        c.a, c.b, c.c, c.d, c.e = nil, nil, nil, nil, nil
        c.z = 1
        read(c)
        read({v = 1})
        read({w = 1})
        read({5, 6, 7})
        read({["1"] = 8, v = 2})
        read({1, 2, v = 3})
        return sum
        "#,
    );
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("descriptor and plain reads agree");
    assert_eq!(state.to_number(-1).expect("a number"), 246.0);
}

/// Reads field `v` as `f64` through a descriptor.
fn read_hinted_f64(state: &mut State) -> dellingr::Result<u8> {
    let value: f64 = state.raw_field(1, &HINTED_V)?;
    state.push_number(value)?;
    Ok(1)
}

/// A failed descriptor read raises the error the plain name raises.
#[test]
fn a_descriptor_fails_with_the_plain_name_error() {
    let mut state = State::new();
    install(&mut state);
    state
        .push_named_rust_fn("read_hinted_f64", read_hinted_f64)
        .expect("function fits");
    state.set_global("read_hinted_f64").expect("global is set");
    let mut message = |source: &str| {
        support::load(&mut state, source);
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("the read fails");
        let ErrorKind::ArgError(argument) = error.kind() else {
            panic!("{source} must raise an argument error, got {error}");
        };
        argument.to_string()
    };
    let hinted = message("read_hinted_f64({v = 'x', a = 1, b = 2, c = 3, d = 4})");
    let plain = message("read_f64({v = 'x', a = 1, b = 2, c = 3, d = 4})");
    assert_eq!(hinted.replace("read_hinted_f64", "read_f64"), plain);
    assert!(plain.contains("field 'v'"), "{plain}");
}

/// `spawn` read as one batch: two descriptors and a plain name.
fn spawn_batch(state: &mut State) -> dellingr::Result<u8> {
    let (player, x, flags): (u32, f64, Option<String>) =
        state.raw_fields(1, (&BATCH_PLAYER, &BATCH_X, "flags"))?;
    state.push_string(format!("{player} {x} {flags:?}"))?;
    Ok(1)
}

/// A batch read at argument 0.
fn batch_arg_zero(state: &mut State) -> dellingr::Result<u8> {
    let (value,): (f64,) = state.raw_fields(0, ("v",))?;
    state.push_number(value)?;
    Ok(1)
}

// process-global: a validated slot hint only, see above.
static BATCH_PLAYER: FieldName = FieldName::new("player");
// process-global: as above.
static BATCH_X: FieldName = FieldName::new("x");

/// The outcome of `source` with `CALL` replaced by `function`: the result,
/// or the error kind with the function's name masked so spellings compare.
fn outcome(state: &mut State, source: &str, function: &str) -> String {
    support::load(state, source.replace("CALL", function));
    match state.call(ArgCount::Fixed(0), RetCount::Fixed(1)) {
        Ok(()) => {
            let value = state.to_string(-1).expect("a string result");
            state.pop(1).expect("the result pops");
            value
        }
        Err(error) => format!("{:?}", error.kind()).replace(function, "F"),
    }
}

/// A batch reads what separate reads read and fails with the error the
/// first failing one raises, in tuple order.
#[test]
fn a_batch_reads_and_fails_like_separate_reads_in_order() {
    let mut state = State::new();
    install(&mut state);
    let functions: [(&str, dellingr::RustFunc); 2] = [
        ("spawn_batch", spawn_batch),
        ("batch_arg_zero", batch_arg_zero),
    ];
    for (name, function) in functions {
        state
            .push_named_rust_fn(name, function)
            .expect("function fits");
        state.set_global(name).expect("global is set");
    }
    for source in [
        "return CALL{player = 1, x = 2.5, flags = 'Structure'}",
        "return CALL{player = 1, x = 2.5}",
        "return CALL{x = 2.5, player = 3, other = true}",
        "return CALL{player = -1, x = 'far'}",
        "return CALL{player = 1, x = 'far', flags = 5}",
        "return CALL{player = 1, x = 2, flags = 5}",
        "return CALL{}",
        "return CALL(5)",
        "return CALL()",
    ] {
        let scalar = outcome(&mut state, source, "spawn");
        let batch = outcome(&mut state, source, "spawn_batch");
        assert_eq!(batch, scalar, "{source}");
    }
    let error = outcome(&mut state, "return CALL{v = 1}", "batch_arg_zero");
    assert!(error.contains("InvalidStackIndex"), "{error}");
    assert_eq!(state.get_top(), 0);
}
