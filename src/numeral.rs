//! Lua numeral recognition shared by runtime-facing conversions.

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

const fn is_lua_whitespace(byte: u8) -> bool {
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
    let exponent = exponent.map_or(Some(0), parse_signed_i32)?;
    let mut value = 0.0;
    let mut fraction_digits = 0_i32;
    let mut fractional = false;
    for byte in mantissa {
        if *byte == b'.' {
            fractional = true;
        } else {
            value = value * 16.0 + f64::from(hex_value(*byte)?);
            if fractional {
                fraction_digits = fraction_digits.checked_add(1)?;
            }
        }
    }
    Some(value * 2_f64.powi(exponent.checked_sub(fraction_digits.checked_mul(4)?)?))
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

fn parse_signed_i32(bytes: &[u8]) -> Option<i32> {
    validate_signed_digits(bytes)?;
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => return None,
    };
    let parsed = text.parse::<i32>();
    if parsed.is_err() {
        return None;
    }
    Some(parsed.unwrap_or(0))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
