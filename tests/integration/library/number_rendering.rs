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

// The independent oracle below is the two-conversion `%g` the VM used before
// it laid both notations out from one scientific conversion: probe the
// rounded exponent with a scientific conversion, then convert again in the
// chosen notation and strip zeros from the text. It shares Rust's converter
// with the VM, so the literal rows in `number_format_g14` and the corpus's
// comparison with reference Lua stay the check on the converter itself.

fn oracle_scientific(magnitude: f64, precision: usize, alternate: bool) -> String {
    let text = format!("{magnitude:.precision$e}");
    let (mantissa, exponent) = text.split_once('e').expect("scientific text");
    let exponent: i32 = exponent.parse().expect("decimal exponent");
    let point = if alternate && precision == 0 { "." } else { "" };
    format!("{mantissa}{point}e{exponent:+03}")
}

fn oracle_strip_zeros(text: &str) -> String {
    let (body, exponent) = match text.find('e') {
        Some(at) => text.split_at(at),
        None => (text, ""),
    };
    let body = if body.contains('.') {
        body.trim_end_matches('0').trim_end_matches('.')
    } else {
        body
    };
    format!("{body}{exponent}")
}

fn oracle_general(magnitude: f64, precision: usize, alternate: bool) -> String {
    let significant = precision.max(1);
    let probe = format!("{magnitude:.prec$e}", prec = significant - 1);
    let exponent: i32 = probe
        .split_once('e')
        .expect("scientific text")
        .1
        .parse()
        .expect("decimal exponent");
    let significant_i32 = i32::try_from(significant).expect("small precision");
    let text = if exponent < -4 || exponent >= significant_i32 {
        oracle_scientific(magnitude, significant - 1, alternate)
    } else {
        let fraction = usize::try_from(significant_i32 - exponent - 1).expect("nonnegative");
        let mut text = format!("{magnitude:.fraction$}");
        if alternate && fraction == 0 {
            text.push('.');
        }
        text
    };
    if alternate {
        text
    } else {
        oracle_strip_zeros(&text)
    }
}

fn signed(number: f64, body: String) -> String {
    if number.is_sign_negative() {
        format!("-{body}")
    } else {
        body
    }
}

/// Finite values that exercise the layout's decisions: decade boundaries and
/// their neighbouring floats (rounding carries across both notation
/// thresholds), exact binary ties, zeros, subnormals and fixed-seed samples.
fn layout_probe_values() -> Vec<f64> {
    let mut values = vec![
        0.0,
        -0.0,
        0.5,
        1.25,
        1.75,
        2.5,
        3.5,
        0.125,
        9.5,
        99.5,
        0.000_099_999_5,
        123_456_789_012_355.0,
        5e-324,
        f64::MIN_POSITIVE,
        f64::MAX,
        -f64::MAX,
        1.0 / 3.0,
        -2.0 / 3.0,
        -1e-5,
        -1e14,
    ];
    // Every decade around the notation thresholds, a stride elsewhere: the
    // sweep runs every precision for each value, so it is kept small.
    let decades = (-310..-20)
        .step_by(7)
        .chain(-20..=30)
        .chain((31..=309).step_by(7));
    for exponent in decades {
        let decade: f64 = format!("1e{exponent}").parse().expect("decimal literal");
        if !decade.is_finite() || decade == 0.0 {
            continue;
        }
        let bits = decade.to_bits();
        for offset in [-1_i64, 0, 1] {
            values.push(f64::from_bits(bits.wrapping_add_signed(offset)));
        }
        let nines: f64 = format!("9.99999999999999e{exponent}")
            .parse()
            .expect("decimal literal");
        if nines.is_finite() {
            values.push(nines);
        }
    }
    let mut bits = 0x3c6e_f372_fe94_f82b_u64;
    for _ in 0..64 {
        bits ^= bits << 13;
        bits ^= bits >> 7;
        bits ^= bits << 17;
        let sample = f64::from_bits(bits);
        if sample.is_finite() {
            values.push(sample);
        }
    }
    values
}

#[test]
fn float_conversions_match_the_two_conversion_oracle() {
    let mut state = State::new();
    crate::support::run(
        &mut state,
        "function render_all(x)
            local out = {tostring(x)}
            for p = 0, 99 do
                out[#out + 1] = string.format('%.' .. p .. 'g', x)
                out[#out + 1] = string.format('%#.' .. p .. 'g', x)
                out[#out + 1] = string.format('%.' .. p .. 'e', x)
                out[#out + 1] = string.format('%#.' .. p .. 'E', x)
            end
            return table.concat(out, '|')
        end",
    );
    for number in layout_probe_values() {
        let magnitude = number.abs();
        let mut expected = vec![if number.fract() == 0.0 && magnitude < 1e14 {
            signed(number, format!("{magnitude}"))
        } else {
            signed(number, oracle_general(magnitude, 14, false))
        }];
        for precision in 0..=99 {
            expected.push(signed(number, oracle_general(magnitude, precision, false)));
            expected.push(signed(number, oracle_general(magnitude, precision, true)));
            expected.push(signed(
                number,
                oracle_scientific(magnitude, precision, false),
            ));
            expected.push(signed(
                number,
                oracle_scientific(magnitude, precision, true).to_ascii_uppercase(),
            ));
        }
        state
            .get_global("render_all")
            .expect("the test defines render_all");
        state
            .push_number(number)
            .expect("the test stack has room for a number");
        state
            .call(ArgCount::Fixed(1), RetCount::Fixed(1))
            .expect("every finite number renders");
        let rendered = state.to_string(-1).expect("render_all returns a string");
        let rendered: Vec<&str> = rendered.split('|').collect();
        assert_eq!(rendered.len(), expected.len());
        for (index, (got, want)) in rendered.iter().zip(&expected).enumerate() {
            assert_eq!(
                got,
                want,
                "rendering {index} of bits {:016x}",
                number.to_bits()
            );
        }
        state.set_top(0).expect("the test stack can be cleared");
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
