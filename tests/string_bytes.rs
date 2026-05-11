use dellingr::{ArgCount, RetCount, State};

fn run_one(code: &str) -> State {
    let mut state = State::new();
    state.load_string(code).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    state
}

#[test]
fn gsub_dot_matches_utf8_bytes() {
    let mut state = State::new();
    state
        .load_string(r#"return string.gsub("⚠", ".", "X")"#)
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2)).unwrap();

    assert_eq!(state.to_bytes(-2).unwrap(), b"XXX");
    assert_eq!(state.to_number(-1).unwrap(), 3.0);
}

#[test]
fn gsub_can_remove_multibyte_string_bytewise() {
    let mut state = State::new();
    state
        .load_string(r#"return string.gsub("⚠", ".", "")"#)
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2)).unwrap();

    assert_eq!(state.to_bytes(-2).unwrap(), b"");
    assert_eq!(state.to_number(-1).unwrap(), 3.0);
}

#[test]
fn gsub_can_produce_invalid_utf8() {
    let state = run_one(
        r#"
        return string.gsub("⚠", ".", function()
            return string.format("%c", 255)
        end)
        "#,
    );

    assert_eq!(state.to_bytes(-1).unwrap(), &[0xff, 0xff, 0xff]);
}

#[test]
fn empty_pattern_find_and_match_respect_init_bounds() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            local f1, f2 = string.find("abc", "", 4)
            local f3 = string.find("abc", "", 5)
            local m1 = string.match("abc", "", 4)
            local m2 = string.match("abc", "", 5)
            return f1, f2, f3, m1, m2
        "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(5)).unwrap();

    assert_eq!(state.to_number(-5).unwrap(), 4.0);
    assert_eq!(state.to_number(-4).unwrap(), 3.0);
    assert_eq!(state.typ(-3), dellingr::LuaType::Nil);
    assert_eq!(state.to_bytes(-2).unwrap(), b"");
    assert_eq!(state.typ(-1), dellingr::LuaType::Nil);
}

#[test]
fn empty_pattern_gsub_replaces_boundaries() {
    let mut state = State::new();
    state
        .load_string(r#"return string.gsub("abc", "", "-")"#)
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2)).unwrap();

    assert_eq!(state.to_string(-2).unwrap(), "-a-b-c-");
    assert_eq!(state.to_number(-1).unwrap(), 4.0);
}

#[test]
fn empty_pattern_gsub_respects_replacement_limit() {
    let mut state = State::new();
    state
        .load_string(r#"return string.gsub("abc", "", "-", 2)"#)
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2)).unwrap();

    assert_eq!(state.to_string(-2).unwrap(), "-a-bc");
    assert_eq!(state.to_number(-1).unwrap(), 2.0);
}

#[test]
fn empty_pattern_gmatch_visits_each_boundary() {
    let state = run_one(
        r#"
        local count = 0
        for match in string.gmatch("abc", "") do
            if match ~= "" then return -1 end
            count = count + 1
        end
        return count
        "#,
    );

    assert_eq!(state.to_number(-1).unwrap(), 4.0);
}

#[test]
fn sub_and_reverse_are_bytewise() {
    let state = run_one(
        r#"
        local first = string.sub("⚠", 1, 1)
        local reversed = string.reverse("⚠")
        return first .. reversed
        "#,
    );

    assert_eq!(state.to_bytes(-1).unwrap(), &[0xe2, 0xa0, 0x9a, 0xe2]);
}

#[test]
fn push_bytes_and_to_bytes_preserve_invalid_utf8() {
    let mut state = State::new();
    state.push_bytes([0xff, b'a']);

    assert_eq!(state.to_bytes(-1).unwrap(), &[0xff, b'a']);
    assert_eq!(state.to_string(-1).unwrap(), "�a");
}
