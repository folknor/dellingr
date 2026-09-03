//! Lua numeral recognition and numeric semantics shared by runtime-facing
//! conversions and the compile-time constant folder.

/// Lua 5.4 floored modulo, including its special floating-point cases.
///
/// The parser's constant folder must produce the bit-exact result the VM
/// would compute at runtime, so both call this one definition.
pub(crate) fn lua_modulo(a: f64, b: f64) -> f64 {
    let mut m = a % b;
    if (m > 0.0 && b < 0.0) || (m < 0.0 && b > 0.0) {
        m += b;
    }
    m
}

/// Returns an integer-valued finite `f64` when it lies inside the range whose
/// endpoints can be checked exactly in an `f64`.
pub(crate) fn exact_i64(number: f64) -> Option<i64> {
    let minimum = i64::MIN as f64;
    let maximum = f64::from_bits((i64::MAX as f64).to_bits() - 1);
    let outside_i64 = number.total_cmp(&minimum).is_lt() || number.total_cmp(&maximum).is_gt();
    if number.is_finite() && !outside_i64 && number.trunc().to_bits() == number.to_bits() {
        Some(number as i64)
    } else {
        None
    }
}

/// Converts a complete Lua numeral, allowing only Lua's ASCII whitespace.
pub(crate) fn parse_lua_numeral(input: &[u8]) -> Option<f64> {
    let bytes = trim_lua_whitespace(input);
    let (negative, body) = match bytes {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        _ => (false, bytes),
    };
    let value = if body.starts_with(b"0x") || body.starts_with(b"0X") {
        parse_hex(&body[2..])?
    } else {
        parse_decimal(body)?
    };
    Some(if negative { -value } else { value })
}

fn trim_lua_whitespace(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(|byte| is_lua_whitespace(*byte)) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(|byte| is_lua_whitespace(*byte)) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

pub(crate) const fn is_lua_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn parse_decimal(bytes: &[u8]) -> Option<f64> {
    let (mantissa, exponent) = split_exponent(bytes, b'e', b'E')?;
    let digits = validate_mantissa(mantissa, |byte| byte.is_ascii_digit())?;
    if digits == 0 {
        return None;
    }
    if let Some(exponent) = exponent {
        validate_signed_digits(exponent)?;
    }
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => return None,
    };
    let parsed = text.parse::<f64>();
    if parsed.is_err() {
        return None;
    }
    Some(parsed.unwrap_or(f64::NAN))
}

fn parse_hex(bytes: &[u8]) -> Option<f64> {
    let (mantissa, exponent) = split_exponent(bytes, b'p', b'P')?;
    let digits = validate_mantissa(mantissa, |byte| byte.is_ascii_hexdigit())?;
    if digits == 0 {
        return None;
    }

    // Any mantissa correction is bounded by four bits per byte. Once an
    // exponent exceeds this limit, no mantissa of this length can move the
    // result back from definite overflow or underflow.
    let mantissa_len = i64::try_from(mantissa.len()).unwrap_or(i64::MAX);
    let exponent_limit = mantissa_len.saturating_mul(4).saturating_add(2048);

    let exponent = match exponent {
        Some(exponent) => parse_signed_i64_saturating(exponent, exponent_limit)?,
        None => 0,
    };

    let mut prefix = 0_u64;
    let mut significant_digits = 0_i64;
    let mut fractional_digits = 0_i64;
    let mut dropped_digits = 0_i64;
    let mut dropped_nonzero = false;
    let mut fractional = false;

    for &byte in mantissa {
        if byte == b'.' {
            fractional = true;
            continue;
        }
        let digit = hex_value(byte)?;
        if fractional {
            fractional_digits = fractional_digits.saturating_add(1);
        }
        // Leading zeroes affect the radix-point correction, but not the
        // significant-digit budget.
        if significant_digits == 0 && digit == 0 {
            continue;
        }
        if significant_digits < MAX_SIGNIFICANT_HEX_DIGITS {
            prefix = prefix.saturating_mul(16).saturating_add(u64::from(digit));
            significant_digits = significant_digits.saturating_add(1);
        } else {
            dropped_digits = dropped_digits.saturating_add(1);
            dropped_nonzero |= digit != 0;
        }
    }

    // Zero must bypass all scaling so it can never become 0 * inf.
    if significant_digits == 0 {
        return Some(0.0);
    }

    // Exact value = (prefix + discarded_fraction)
    //   * 2^(exponent + 4*(dropped_digits - fractional_digits))
    let digit_adjustment = dropped_digits
        .saturating_sub(fractional_digits)
        .saturating_mul(4);
    let scale = exponent.saturating_add(digit_adjustment);

    Some(finish_hex(prefix, dropped_nonzero, scale))
}

const MAX_SIGNIFICANT_HEX_DIGITS: i64 = 16;
const F64_PRECISION_BITS: u32 = f64::MANTISSA_DIGITS;
const MIN_NORMAL_EXPONENT: i64 = -1022;
const HALF_MIN_SUBNORMAL_EXPONENT: i64 = -1075;
const MAX_NORMAL_EXPONENT: i64 = 1023;

fn parse_signed_i64_saturating(bytes: &[u8], limit: i64) -> Option<i64> {
    validate_signed_digits(bytes)?;
    let (negative, digits) = match bytes {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        _ => (false, bytes),
    };
    let mut magnitude = 0_i64;
    for &byte in digits {
        magnitude = magnitude
            .saturating_mul(10)
            .saturating_add(i64::from(byte - b'0'))
            .min(limit);
    }
    Some(if negative { -magnitude } else { magnitude })
}

fn finish_hex(prefix: u64, dropped_nonzero: bool, scale: i64) -> f64 {
    let bit_count = u64::BITS - prefix.leading_zeros();
    let top_exponent = scale.saturating_add(i64::from(bit_count.saturating_sub(1)));

    if top_exponent > MAX_NORMAL_EXPONENT {
        return f64::INFINITY;
    }
    if top_exponent < HALF_MIN_SUBNORMAL_EXPONENT {
        return 0.0;
    }
    if top_exponent < MIN_NORMAL_EXPONENT {
        return round_hex_subnormal(prefix, dropped_nonzero, scale);
    }

    let shift = bit_count.saturating_sub(F64_PRECISION_BITS);
    let significand = round_shift_right(prefix, shift, dropped_nonzero);
    let adjusted_scale = scale.saturating_add(i64::from(shift));
    scale_finite_by_power_of_two(significand as f64, adjusted_scale)
}

fn round_shift_right(value: u64, shift: u32, sticky: bool) -> u64 {
    if shift == 0 {
        return value;
    }
    let kept = value >> shift;
    let remainder_mask = (1_u64 << shift) - 1;
    let remainder = value & remainder_mask;
    let halfway = 1_u64 << (shift - 1);
    let round_up = remainder > halfway || (remainder == halfway && (sticky || (kept & 1) != 0));
    kept.saturating_add(u64::from(round_up))
}

fn round_hex_subnormal(prefix: u64, sticky: bool, scale: i64) -> f64 {
    // Round exact_value / 2^-1074 to the nearest integer.
    let shift = (-1074_i64).saturating_sub(scale);
    let units = if shift <= 0 {
        let left = shift.saturating_neg().clamp(0, 63) as u32;
        prefix << left
    } else if shift < 64 {
        round_shift_right(prefix, shift.clamp(1, 63) as u32, sticky)
    } else if shift == 64 {
        let halfway = 1_u64 << 63;
        u64::from(prefix > halfway || (prefix == halfway && sticky))
    } else {
        0
    };
    // Subnormal patterns are an integer number of 2^-1074 units; units==1<<52
    // yields the smallest normal value.
    f64::from_bits(units)
}

fn scale_finite_by_power_of_two(value: f64, scale: i64) -> f64 {
    const FRACTION_MASK: u64 = (1_u64 << 52) - 1;
    const ONE_EXPONENT_BITS: u64 = 1023_u64 << 52;
    let bits = value.to_bits();
    let encoded_exponent = ((bits >> 52) & 0x7ff) as i64;
    let value_exponent = encoded_exponent.saturating_sub(1023);
    let normalized = f64::from_bits((bits & FRACTION_MASK) | ONE_EXPONENT_BITS);
    let exponent = scale.saturating_add(value_exponent);

    if exponent > MAX_NORMAL_EXPONENT {
        return f64::INFINITY;
    }
    if exponent < HALF_MIN_SUBNORMAL_EXPONENT {
        return 0.0;
    }
    if exponent < MIN_NORMAL_EXPONENT {
        let tail_exponent = exponent.saturating_sub(MIN_NORMAL_EXPONENT).clamp(-53, -1) as i32;
        return (normalized * f64::MIN_POSITIVE) * 2_f64.powi(tail_exponent);
    }
    let exponent = exponent.clamp(MIN_NORMAL_EXPONENT, MAX_NORMAL_EXPONENT) as i32;
    normalized * 2_f64.powi(exponent)
}

fn split_exponent(bytes: &[u8], lower: u8, upper: u8) -> Option<(&[u8], Option<&[u8]>)> {
    let index = bytes
        .iter()
        .position(|byte| *byte == lower || *byte == upper);
    match index {
        Some(index)
            if bytes[index + 1..]
                .iter()
                .any(|byte| *byte == lower || *byte == upper) =>
        {
            None
        }
        Some(index) => Some((&bytes[..index], Some(&bytes[index + 1..]))),
        None => Some((bytes, None)),
    }
}

fn validate_mantissa(bytes: &[u8], valid_digit: impl Fn(u8) -> bool) -> Option<usize> {
    let mut dots = 0;
    let mut digits = 0;
    for byte in bytes {
        if *byte == b'.' {
            dots += 1;
            if dots > 1 {
                return None;
            }
        } else if valid_digit(*byte) {
            digits += 1;
        } else {
            return None;
        }
    }
    Some(digits)
}

fn validate_signed_digits(bytes: &[u8]) -> Option<()> {
    let bytes = match bytes {
        [b'+' | b'-', rest @ ..] => rest,
        _ => bytes,
    };
    (!bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit)).then_some(())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_lua_numeral;

    #[test]
    fn parse_hex_extremes() {
        let long_f = format!("0x{}p-1000", "f".repeat(300));
        let long_one = format!("0x1{}p-1000", "0".repeat(299));
        let cases = [
            ("0x1.8p1", Some(3.0_f64.to_bits())),
            ("0xA", Some(10.0_f64.to_bits())),
            ("0x.8", Some(0.5_f64.to_bits())),
            ("0x1p4", Some(16.0_f64.to_bits())),
            ("0x00000000000000000001.8p1", Some(3.0_f64.to_bits())),
            ("0x1p2147483648", Some(f64::INFINITY.to_bits())),
            ("0x1p9999999999", Some(f64::INFINITY.to_bits())),
            ("0x1p-2147483648", Some(0.0_f64.to_bits())),
            ("0x1p-9999999999", Some(0.0_f64.to_bits())),
            ("0x0p9999999999", Some(0.0_f64.to_bits())),
            ("-0x0p9999999999", Some(0x8000_0000_0000_0000)),
            ("0x1p1023", Some(0x7fe0_0000_0000_0000)),
            ("0x1.fffffffffffffp1023", Some(f64::MAX.to_bits())),
            ("0x1p1024", Some(f64::INFINITY.to_bits())),
            ("0x1p-1022", Some(f64::MIN_POSITIVE.to_bits())),
            ("0x1p-1074", Some(0x0000_0000_0000_0001)),
            ("0x1p-1075", Some(0.0_f64.to_bits())),
            ("0x1.0000000000001p-1075", Some(0x0000_0000_0000_0001)),
            ("0x1p+", None),
            ("0x1p--1", None),
        ];

        for (input, expected) in cases {
            assert_eq!(
                parse_lua_numeral(input.as_bytes()).map(f64::to_bits),
                expected
            );
        }
        assert_eq!(
            parse_lua_numeral(long_f.as_bytes()).map(f64::to_bits),
            Some(0x4c70_0000_0000_0000)
        );
        assert_eq!(
            parse_lua_numeral(long_one.as_bytes()).map(f64::to_bits),
            Some(0x4c30_0000_0000_0000)
        );
    }
}
