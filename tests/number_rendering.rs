use dellingr::{ArgCount, RetCount, State};

fn assert_concat_matches_tostring(state: &mut State, number: f64) {
    state
        .get_global("tostring")
        .expect("the base library exposes tostring");
    state
        .push_number(number)
        .expect("the test stack has room for a number");
    state
        .call(ArgCount::Fixed(1), RetCount::Fixed(1))
        .expect("tostring accepts every f64 value");
    let tostring = state
        .to_bytes(-1)
        .expect("tostring returns a string")
        .to_vec();

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
    assert_eq!(concatenated, tostring);
    state.set_top(0).expect("the test stack can be cleared");
}

#[test]
fn concat_number_rendering_matches_tostring_across_f64_classes() {
    let fractions = [0, 1, 1 << 51, (1 << 52) - 1];
    let mut state = State::new();

    // Exercise both signs, every exponent class, and boundary significands.
    // The all-ones exponent covers infinities and multiple NaN payloads.
    for exponent in 0..=0x7ff_u64 {
        for fraction in fractions {
            for sign in [0, 1 << 63] {
                let bits = sign | (exponent << 52) | fraction;
                assert_concat_matches_tostring(&mut state, f64::from_bits(bits));
            }
        }
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
