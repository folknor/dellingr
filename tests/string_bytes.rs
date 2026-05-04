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
