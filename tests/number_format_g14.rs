//! STD-005: number-to-string coercion is reference Lua 5.2's `%.14g`.
//! Expected spellings were captured from `lua5.2` (except NaN, which dellingr
//! prints unsigned on every host where x86-64 C Lua prints `-nan` for `0/0`).

use dellingr::State;

fn render(number: f64) -> String {
    let mut state = State::new();
    state
        .push_number(number)
        .expect("the test stack has room for a number");
    state.to_string(-1).expect("a number converts to a string")
}

#[test]
fn number_rendering_matches_reference_g14() {
    let cases: &[(f64, &str)] = &[
        (0.0, "0"),
        (-0.0, "-0"),
        (3.0, "3"),
        (-3.0, "-3"),
        (100.0, "100"),
        (99_999_999_999_999.0, "99999999999999"),
        (1e14, "1e+14"),
        (-1e14, "-1e+14"),
        (99_999_999_999_999.5, "1e+14"),
        (9.999_999_999_999_94e13, "99999999999999"),
        (9.999_999_999_999_96e13, "1e+14"),
        (123_456_789_012_345.0, "1.2345678901234e+14"),
        // Exact halfway cases round to an even final significant digit.
        (123_456_789_012_355.0, "1.2345678901236e+14"),
        (1e15, "1e+15"),
        (9_007_199_254_740_992.0, "9.007199254741e+15"),
        (9_223_372_036_854_775_808.0, "9.2233720368548e+18"),
        (1e100, "1e+100"),
        (1e300, "1e+300"),
        (0.1, "0.1"),
        (0.1 + 0.2, "0.3"),
        (1.0 / 3.0, "0.33333333333333"),
        (-123.456, "-123.456"),
        (123.456_789_012_345_67, "123.45678901235"),
        (1e-4, "0.0001"),
        (1e-5, "1e-05"),
        (1.5e-5, "1.5e-05"),
        (9.999_999_999_999_94e-5, "9.9999999999999e-05"),
        (9.999_999_999_999_96e-5, "0.0001"),
        (-9.999_999_999_999_96e-5, "-0.0001"),
        (5e-324, "4.9406564584125e-324"),
        (f64::MIN_POSITIVE, "2.2250738585072e-308"),
        (f64::MAX, "1.7976931348623e+308"),
        (1.234_567_890_123_4e-200, "1.2345678901234e-200"),
        (1.23e20, "1.23e+20"),
        (std::f64::consts::PI, "3.1415926535898"),
        (f64::INFINITY, "inf"),
        (f64::NEG_INFINITY, "-inf"),
        (f64::NAN, "nan"),
        (-f64::NAN, "nan"),
    ];
    for &(number, expected) in cases {
        assert_eq!(render(number), expected, "rendering of {number:e}");
    }
}

#[test]
fn string_functions_see_the_g14_spelling() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            if string.len(1/3) ~= 16 then error("len(1/3) " .. string.len(1/3)) end
            if ("x" .. 1e15) ~= "x1e+15" then error("concat " .. ("x" .. 1e15)) end
            local t = {}
            t[tostring(0.1 + 0.2)] = true
            if not t["0.3"] then error("string key built from a number") end
            if tostring(0/0) ~= "nan" then error("nan " .. tostring(0/0)) end
            if tostring(-(0/0)) ~= "nan" then error("negated nan") end
            "#,
        )
        .expect("the script parses");
    state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(0))
        .expect("number coercions use the %.14g spelling");
}
