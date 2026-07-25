use dellingr::{ArgCount, RetCount, State};

fn run_one(code: &str) -> State {
    let mut state = State::new();
    state.load_string(code).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    state
}

#[test]
fn string_library_coerces_number_subjects_and_patterns() {
    let state = run_one(
        r#"
        local found = string.find(12345, 34)
        local iter = string.gmatch(121, 1)
        local first = iter()
        local replaced = string.gsub(121, 1, "x")
        return string.sub(123, 2) .. ":" .. string.len(42) .. ":"
            .. string.upper(12) .. ":" .. string.lower(12) .. ":"
            .. string.reverse(123) .. ":" .. found .. ":"
            .. string.match(123, 23) .. ":" .. first .. ":" .. replaced
        "#,
    );

    // Verified against Lua 5.4, which prints the same string for this
    // expression. Note gsub("121", "1", "x") is "x2x": both 1s are replaced.
    assert_eq!(state.to_bytes(-1).unwrap(), b"23:2:12:12:321:3:23:1:x2x");
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
fn gmatch_treats_a_leading_caret_as_literal() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            local none = 0
            for _ in string.gmatch("aaa", "^a") do none = none + 1 end
            local found = {}
            for x in string.gmatch("^a^a", "^a") do found[#found + 1] = x end
            return none, #found, found[1], found[2]
            "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(4)).unwrap();
    assert_eq!(state.to_number(-4).unwrap(), 0.0);
    assert_eq!(state.to_number(-3).unwrap(), 2.0);
    assert_eq!(state.to_bytes(-2).unwrap(), b"^a");
    assert_eq!(state.to_bytes(-1).unwrap(), b"^a");
}

#[test]
fn gmatch_returns_a_self_contained_closure() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            local f = string.gmatch("a b", "%w+")
            return type(f), f(), f(), f()
            "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(4)).unwrap();
    assert_eq!(state.to_bytes(-4).unwrap(), b"function");
    assert_eq!(state.to_bytes(-3).unwrap(), b"a");
    assert_eq!(state.to_bytes(-2).unwrap(), b"b");
    assert_eq!(state.typ(-1), dellingr::LuaType::Nil);
}

#[test]
fn gmatch_closures_keep_independent_state() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            local f = string.gmatch("ab", ".")
            local g = string.gmatch("xy", ".")
            return f(), g(), f(), g()
            "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(4)).unwrap();
    assert_eq!(state.to_bytes(-4).unwrap(), b"a");
    assert_eq!(state.to_bytes(-3).unwrap(), b"x");
    assert_eq!(state.to_bytes(-2).unwrap(), b"b");
    assert_eq!(state.to_bytes(-1).unwrap(), b"y");
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
    state
        .push_bytes([0xff, b'a'])
        .expect("short test string fits");

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
fn string_match_returns_all_32_capture_results() {
    for (pattern, subject, expected_position) in [
        ("()".repeat(32), "x".to_owned(), true),
        ("(x)".repeat(32), "x".repeat(32), false),
    ] {
        let mut state = State::new();
        state
            .load_string(format!("return string.match({subject:?}, {pattern:?})"))
            .unwrap();
        state.call(ArgCount::Fixed(0), RetCount::All).unwrap();
        assert_eq!(state.get_top(), 32);
        for index in 1..=32 {
            if expected_position {
                assert_eq!(state.to_number(index).unwrap(), 1.0);
            } else {
                assert_eq!(state.to_bytes(index).unwrap(), b"x");
            }
        }
    }
}

#[test]
fn position_captures_are_numbers_in_all_string_wrappers() {
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
fn resumed_patterns_keep_frontier_context_and_absolute_captures() {
    let mut state = State::new();
    state
        .load_string(
            r#"
        local a, b = string.find("ab", "%f[%a]%a", 2)
        local c, d = string.find("abcd", "%f[%w]%w", 2)
        local m, p, q = string.match("ab", "()(b)()", 2)
        local out, n = string.gsub("abcd", "%f[%w]%w", "X")
        local g
        for x in string.gmatch("ab", ".%f[%z]") do g = x end
        return a, b, c, d, m, p, q, out, n, g
        "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(10)).unwrap();
    assert_eq!(state.typ(-10), dellingr::LuaType::Nil);
    assert_eq!(state.typ(-9), dellingr::LuaType::Nil);
    assert_eq!(state.typ(-8), dellingr::LuaType::Nil);
    assert_eq!(state.typ(-7), dellingr::LuaType::Nil);
    assert_eq!(state.to_number(-6).unwrap(), 2.0);
    assert_eq!(state.to_bytes(-5).unwrap(), b"b");
    assert_eq!(state.to_number(-4).unwrap(), 3.0);
    assert_eq!(state.to_bytes(-3).unwrap(), b"Xbcd");
    assert_eq!(state.to_number(-2).unwrap(), 1.0);
    assert_eq!(state.to_bytes(-1).unwrap(), b"b");
}

#[test]
fn deferred_pattern_validation_keeps_existing_call_timing() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            local f = string.find("abc", "%", 5)
            local m = string.match("abc", "%", 5)
            local g, n = string.gsub("abc", "%", "X", 0)
            local iter = string.gmatch("abc", "%")
            return f, m, g, n, type(iter)
            "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(5)).unwrap();
    assert_eq!(state.typ(-5), dellingr::LuaType::Nil);
    assert_eq!(state.typ(-4), dellingr::LuaType::Nil);
    assert_eq!(state.to_bytes(-3).unwrap(), b"abc");
    assert_eq!(state.to_number(-2).unwrap(), 0.0);
    assert_eq!(state.to_bytes(-1).unwrap(), b"function");
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
