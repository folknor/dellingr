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

#[test]
fn match_backreferences_compare_captures() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            local mismatch = string.match("ab", "(a)%1")
            local single = string.match("aa", "(a)%1")
            local repeated = string.match("abcabc", "(abc)%1")
            local empty = string.match("b", "^(a*)%1b$")
            local short = string.match("abcab", "^(abc)%1$")
            return mismatch, single, repeated, empty, short
            "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(5)).unwrap();

    assert_eq!(state.typ(-5), dellingr::LuaType::Nil);
    assert_eq!(state.to_bytes(-4).unwrap(), b"a");
    assert_eq!(state.to_bytes(-3).unwrap(), b"abc");
    assert_eq!(state.to_bytes(-2).unwrap(), b"");
    assert_eq!(state.typ(-1), dellingr::LuaType::Nil);
}

#[test]
fn find_backreferences_compare_captures() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            local mismatch_start, mismatch_end = string.find("ab", "(a)%1")
            local start, finish, capture = string.find("zaaz", "(a)%1")
            return mismatch_start, mismatch_end, start, finish, capture
            "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(5)).unwrap();

    assert_eq!(state.typ(-5), dellingr::LuaType::Nil);
    assert_eq!(state.typ(-4), dellingr::LuaType::Nil);
    assert_eq!(state.to_number(-3).unwrap(), 2.0);
    assert_eq!(state.to_number(-2).unwrap(), 3.0);
    assert_eq!(state.to_bytes(-1).unwrap(), b"a");
}

#[test]
fn position_captures_are_numbers_in_all_string_wrappers() {
    // gmatch is a stateful iterator in dellingr, so drive it with a for-loop to
    // grab the first iteration's captures.
    let mut state = State::new();
    state
        .load_string(
            r#"
        local fa, fb, fc, fd, fe = string.find("abc", "()(a)()")
        local m1, m2, m3 = string.match("abc", "()(a)()")
        local g1, g2, g3
        for x, y, z in string.gmatch("abc", "()(a)()") do
            g1, g2, g3 = x, y, z
            break
        end
        return fa, fb, fc, fd, fe, m1, m2, m3, g1, g2, g3
        "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(11)).unwrap();

    // find: start, end, then the () (a) () captures -> 1, 1, 1, "a", 2.
    assert_eq!(state.to_number(-11).unwrap(), 1.0);
    assert_eq!(state.to_number(-10).unwrap(), 1.0);
    assert_eq!(state.to_number(-9).unwrap(), 1.0);
    assert_eq!(state.to_bytes(-8).unwrap(), b"a");
    assert_eq!(state.to_number(-7).unwrap(), 2.0);
    // match and gmatch: the three captures -> 1, "a", 2.
    assert_eq!(state.to_number(-6).unwrap(), 1.0);
    assert_eq!(state.to_bytes(-5).unwrap(), b"a");
    assert_eq!(state.to_number(-4).unwrap(), 2.0);
    assert_eq!(state.to_number(-3).unwrap(), 1.0);
    assert_eq!(state.to_bytes(-2).unwrap(), b"a");
    assert_eq!(state.to_number(-1).unwrap(), 2.0);
}

#[test]
fn position_captures_use_absolute_positions_after_init() {
    let state = run_one(r#"return string.match("abc", "()", 3)"#);
    assert_eq!(state.to_number(-1).unwrap(), 3.0);
}

#[test]
fn end_position_patterns_match_once() {
    let mut state = State::new();
    state
        .load_string(
            r#"
        local a, b = string.find("", "$")
        local c = string.match("", "$")
        local d, e = string.find("abc", "$", 4)
        local n = 0
        for _ in string.gmatch("abc", "$") do n = n + 1 end
        return a, b, c, d, e, n
        "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(6)).unwrap();
    assert_eq!(state.to_number(-6).unwrap(), 1.0);
    assert_eq!(state.to_number(-5).unwrap(), 0.0);
    assert_eq!(state.to_bytes(-4).unwrap(), b"");
    assert_eq!(state.to_number(-3).unwrap(), 4.0);
    assert_eq!(state.to_number(-2).unwrap(), 3.0);
    assert_eq!(state.to_number(-1).unwrap(), 1.0);
}
