use dellingr::{ArgCount, RetCount, State};

fn assert_rendering_matches_format(state: &mut State, number: f64) {
    state.get_global("render").expect("the test defines render");
    state
        .push_number(number)
        .expect("the test stack has room for a number");
    state
        .call(ArgCount::Fixed(1), RetCount::Fixed(4))
        .expect("the rendering paths accept every f64 value");
    let expected = state
        .to_bytes(-1)
        .expect("format returns a string")
        .to_vec();
    for index in 1..=3 {
        assert_eq!(
            state.to_bytes(index).expect("render returns strings"),
            expected,
            "Lua rendering path {index}, bits {:016x}",
            number.to_bits()
        );
    }
    state.set_top(0).expect("the test stack can be cleared");

    state
        .push_number(number)
        .expect("the test stack has room for a number");
    state
        .push_string("")
        .expect("the test stack has room for an empty string");
    state
        .concat(2)
        .expect("number concatenation uses tostring's number spelling");
    let concatenated = state
        .to_bytes(-1)
        .expect("concatenation returns a string")
        .to_vec();
    assert_eq!(
        concatenated,
        expected,
        "host concat, bits {:016x}",
        number.to_bits()
    );
    state.set_top(0).expect("the test stack can be cleared");
}

#[test]
fn number_rendering_matches_format_across_f64_classes() {
    let fractions = [0, 1, 1 << 51, (1 << 52) - 1];
    let mut state = State::new();
    crate::support::run(
        &mut state,
        "function render(x) return tostring(x), x .. '', '' .. x, string.format('%.14g', x) end",
    );

    // Exercise both signs, every exponent class, and boundary significands.
    // The all-ones exponent covers infinities and multiple NaN payloads.
    for exponent in 0..=0x7ff_u64 {
        for fraction in fractions {
            for sign in [0, 1 << 63] {
                let bits = sign | (exponent << 52) | fraction;
                assert_rendering_matches_format(&mut state, f64::from_bits(bits));
            }
        }
    }

    // Fixed-seed xorshift samples add significands beyond the boundary sweep.
    let mut bits = 0x6a09_e667_f3bc_c909_u64;
    for _ in 0..4096 {
        bits ^= bits << 13;
        bits ^= bits >> 7;
        bits ^= bits << 17;
        assert_rendering_matches_format(&mut state, f64::from_bits(bits));
    }
}

#[test]
fn tostring_preserves_lua_negative_zero_spelling() {
    let mut state = State::new();
    state
        .push_number(-0.0)
        .expect("the test stack has room for a number");
    assert_eq!(state.to_string(-1).expect("negative zero is numeric"), "-0");
}
