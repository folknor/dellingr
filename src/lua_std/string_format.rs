//! Lua 5.4-compatible `string.format`.

use crate::error::{Error, ErrorKind};
use crate::numeral::{exact_i64, parse_lua_numeral};
use crate::{LuaType, Result, State};

const MAX_FORMAT_LEN: usize = 22;

#[derive(Clone, Copy, Default)]
struct FormatFlags {
    left: bool,
    plus: bool,
    space: bool,
    alternate: bool,
    zero: bool,
}

struct Directive {
    raw: Vec<u8>,
    flags: FormatFlags,
    width: Option<u8>,
    precision: Option<u8>,
    conversion: Conversion,
    has_modifiers: bool,
    width_digits: usize,
    precision_digits: usize,
}

#[derive(Clone, Copy)]
enum Conversion {
    Percent,
    Char,
    SignedDecimal,
    UnsignedDecimal,
    Octal,
    HexLower,
    HexUpper,
    ScientificLower,
    ScientificUpper,
    Fixed,
    GeneralLower,
    GeneralUpper,
    HexFloatLower,
    HexFloatUpper,
    String,
    Quoted,
    Pointer,
}

pub(crate) fn format(state: &mut State) -> Result<u8> {
    let format_bytes = match state.typ(1) {
        LuaType::Number => state.bytes_coerce(1)?,
        LuaType::String => state.to_bytes(1)?.to_vec(),
        received => {
            let got = if state.get_top() == 0 {
                "no value"
            } else {
                received.as_str()
            };
            return Err(format_arg_error(
                state,
                1,
                &format!("string expected, got {got}"),
            ));
        }
    };
    let argument_count = state.get_top();
    let mut output = Vec::new();
    let mut argument = 1usize;
    let mut cursor = 0usize;

    while cursor < format_bytes.len() {
        if format_bytes[cursor] != b'%' {
            output.push(format_bytes[cursor]);
            cursor += 1;
            continue;
        }
        if format_bytes.get(cursor + 1) == Some(&b'%') {
            let (directive, next) = parse_directive(state, &format_bytes, cursor)?;
            output.extend_from_slice(&format_argument(state, 0, &directive)?);
            cursor = next;
            continue;
        }

        argument += 1;
        if argument > argument_count {
            return Err(format_arg_error(state, argument, "no value"));
        }

        let (directive, next) = parse_directive(state, &format_bytes, cursor)?;
        validate_directive(state, &directive)?;
        let formatted = format_argument(state, argument, &directive)?;
        output.extend_from_slice(&formatted);
        cursor = next;
    }

    state.set_top(0)?;
    state.push_bytes(output);
    Ok(1)
}

fn parse_directive(state: &State, bytes: &[u8], start: usize) -> Result<(Directive, usize)> {
    let mut cursor = start + 1;
    let mut flags = FormatFlags::default();
    while let Some(byte) = bytes.get(cursor) {
        let recognized = match byte {
            b'-' => {
                flags.left = true;
                true
            }
            b'+' => {
                flags.plus = true;
                true
            }
            b' ' => {
                flags.space = true;
                true
            }
            b'#' => {
                flags.alternate = true;
                true
            }
            b'0' => {
                flags.zero = true;
                true
            }
            _ => false,
        };
        if !recognized {
            break;
        }
        cursor += 1;
        check_directive_length(state, start, cursor)?;
    }

    let width_start = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
        check_directive_length(state, start, cursor)?;
    }
    let width_digits = cursor - width_start;
    let width = parse_decimal_field(&bytes[width_start..cursor]);

    let mut precision = None;
    let mut precision_digits = 0usize;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        check_directive_length(state, start, cursor)?;
        let precision_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
            check_directive_length(state, start, cursor)?;
        }
        precision_digits = cursor - precision_start;
        precision = Some(parse_decimal_field(&bytes[precision_start..cursor]).unwrap_or(0));
    }

    let conversion_byte = bytes.get(cursor).copied().unwrap_or(b'%');
    if cursor < bytes.len() {
        cursor += 1;
    }
    check_directive_length(state, start, cursor)?;
    let raw = bytes[start..cursor].to_vec();
    let conversion = classify_conversion(conversion_byte).ok_or_else(|| {
        state.error(ErrorKind::RuntimeError(format!(
            "invalid conversion '{}' to 'format'",
            String::from_utf8_lossy(&raw)
        )))
    })?;
    if matches!(conversion, Conversion::Percent) && raw != b"%%" {
        return Err(state.error(ErrorKind::RuntimeError(format!(
            "invalid conversion '{}' to 'format'",
            String::from_utf8_lossy(&raw)
        ))));
    }
    let has_modifiers = raw.len() > 2;

    Ok((
        Directive {
            raw,
            flags,
            width,
            precision,
            conversion,
            has_modifiers,
            width_digits,
            precision_digits,
        },
        cursor,
    ))
}

fn check_directive_length(state: &State, start: usize, end: usize) -> Result<()> {
    if end - start > MAX_FORMAT_LEN {
        Err(state.error(ErrorKind::RuntimeError(
            "invalid format (too long)".to_string(),
        )))
    } else {
        Ok(())
    }
}

fn parse_decimal_field(bytes: &[u8]) -> Option<u8> {
    if bytes.is_empty() {
        return None;
    }
    Some(bytes.iter().fold(0u8, |value, byte| {
        value.saturating_mul(10).saturating_add(*byte - b'0')
    }))
}

fn classify_conversion(byte: u8) -> Option<Conversion> {
    let conversion = match byte {
        b'%' => Conversion::Percent,
        b'c' => Conversion::Char,
        b'd' | b'i' => Conversion::SignedDecimal,
        b'u' => Conversion::UnsignedDecimal,
        b'o' => Conversion::Octal,
        b'x' => Conversion::HexLower,
        b'X' => Conversion::HexUpper,
        b'e' => Conversion::ScientificLower,
        b'E' => Conversion::ScientificUpper,
        b'f' => Conversion::Fixed,
        b'g' => Conversion::GeneralLower,
        b'G' => Conversion::GeneralUpper,
        b'a' => Conversion::HexFloatLower,
        b'A' => Conversion::HexFloatUpper,
        b's' => Conversion::String,
        b'q' => Conversion::Quoted,
        b'p' => Conversion::Pointer,
        _ => return None,
    };
    Some(conversion)
}

fn validate_directive(state: &State, directive: &Directive) -> Result<()> {
    if matches!(directive.conversion, Conversion::Quoted) && directive.has_modifiers {
        return Err(state.error(ErrorKind::RuntimeError(
            "specifier '%q' cannot have modifiers".to_string(),
        )));
    }

    let flags = directive.flags;
    let flags_valid = match directive.conversion {
        Conversion::ScientificLower
        | Conversion::ScientificUpper
        | Conversion::Fixed
        | Conversion::GeneralLower
        | Conversion::GeneralUpper
        | Conversion::HexFloatLower
        | Conversion::HexFloatUpper
        | Conversion::Quoted
        | Conversion::Percent => true,
        Conversion::SignedDecimal => !flags.alternate,
        Conversion::UnsignedDecimal => !flags.plus && !flags.space && !flags.alternate,
        Conversion::Octal | Conversion::HexLower | Conversion::HexUpper => {
            !flags.plus && !flags.space
        }
        Conversion::Char | Conversion::Pointer | Conversion::String => {
            !flags.plus && !flags.space && !flags.alternate && !flags.zero
        }
    };
    let precision_valid = !matches!(
        directive.conversion,
        Conversion::Percent | Conversion::Char | Conversion::Pointer | Conversion::Quoted
    );
    if !flags_valid
        || (!precision_valid && directive.precision.is_some())
        || directive.width_digits > 2
        || directive.precision_digits > 2
    {
        return Err(invalid_specification(state, &directive.raw));
    }
    Ok(())
}

fn invalid_specification(state: &State, raw: &[u8]) -> Error {
    state.error(ErrorKind::RuntimeError(format!(
        "invalid conversion specification: '{}'",
        String::from_utf8_lossy(raw)
    )))
}

fn format_argument(state: &mut State, argument: usize, directive: &Directive) -> Result<Vec<u8>> {
    let idx = argument as isize;
    match directive.conversion {
        Conversion::Percent => Ok(b"%".to_vec()),
        Conversion::Char => {
            let integer = integer_argument(state, idx, argument)?;
            let byte = integer.rem_euclid(256) as u8;
            Ok(pad_bytes(&[byte], directive.width, directive.flags.left))
        }
        Conversion::SignedDecimal => {
            let integer = integer_argument(state, idx, argument)?;
            Ok(format_signed_integer(integer, directive))
        }
        Conversion::UnsignedDecimal => {
            let integer = integer_argument(state, idx, argument)?;
            Ok(format_unsigned_integer(
                integer as u64,
                10,
                false,
                directive,
            ))
        }
        Conversion::Octal => {
            let integer = integer_argument(state, idx, argument)?;
            Ok(format_unsigned_integer(integer as u64, 8, false, directive))
        }
        Conversion::HexLower => {
            let integer = integer_argument(state, idx, argument)?;
            Ok(format_unsigned_integer(
                integer as u64,
                16,
                false,
                directive,
            ))
        }
        Conversion::HexUpper => {
            let integer = integer_argument(state, idx, argument)?;
            Ok(format_unsigned_integer(integer as u64, 16, true, directive))
        }
        Conversion::ScientificLower
        | Conversion::ScientificUpper
        | Conversion::Fixed
        | Conversion::GeneralLower
        | Conversion::GeneralUpper
        | Conversion::HexFloatLower
        | Conversion::HexFloatUpper => {
            let number = number_argument(state, idx, argument)?;
            Ok(format_float(number, directive))
        }
        Conversion::String => {
            let mut bytes = if state.typ(idx) == LuaType::String {
                state.to_bytes(idx)?.to_vec()
            } else {
                state.bytes_with_tostring_meta(idx)?
            };
            if directive.has_modifiers && bytes.contains(&0) {
                return Err(format_arg_error(state, argument, "string contains zeros"));
            }
            if let Some(precision) = directive.precision {
                bytes.truncate(usize::from(precision).min(bytes.len()));
            }
            Ok(pad_bytes(&bytes, directive.width, directive.flags.left))
        }
        Conversion::Quoted => quote_argument(state, idx, argument),
        Conversion::Pointer => {
            let bytes = match state.typ(idx) {
                LuaType::Nil | LuaType::Boolean | LuaType::Number => b"(null)".to_vec(),
                LuaType::String | LuaType::Table | LuaType::Function => {
                    format!("0x{:x}", state.format_pointer_id(idx)?).into_bytes()
                }
            };
            Ok(pad_bytes(&bytes, directive.width, directive.flags.left))
        }
    }
}

fn number_argument(state: &State, idx: isize, argument: usize) -> Result<f64> {
    match state.typ(idx) {
        LuaType::Number => state.to_number(idx),
        LuaType::String => parse_lua_numeral(state.to_bytes(idx)?).ok_or_else(|| {
            format_arg_error(
                state,
                argument,
                &format!("number expected, got {}", state.typ(idx).as_str()),
            )
        }),
        received => Err(format_arg_error(
            state,
            argument,
            &format!("number expected, got {}", received.as_str()),
        )),
    }
}

fn integer_argument(state: &State, idx: isize, argument: usize) -> Result<i64> {
    let number = number_argument(state, idx, argument)?;
    exact_i64(number)
        .ok_or_else(|| format_arg_error(state, argument, "number has no integer representation"))
}

fn format_arg_error(state: &State, argument: usize, detail: &str) -> Error {
    state.error(ErrorKind::RuntimeError(format!(
        "bad argument #{argument} to 'format' ({detail})"
    )))
}

fn format_signed_integer(value: i64, directive: &Directive) -> Vec<u8> {
    let sign = if value.is_negative() {
        b"-".as_slice()
    } else if directive.flags.plus {
        b"+".as_slice()
    } else if directive.flags.space {
        b" ".as_slice()
    } else {
        b"".as_slice()
    };
    let digits = if directive.precision == Some(0) && value == 0 {
        Vec::new()
    } else {
        value.unsigned_abs().to_string().into_bytes()
    };
    format_integer_parts(sign, b"", digits, directive)
}

fn format_unsigned_integer(
    value: u64,
    radix: u8,
    uppercase: bool,
    directive: &Directive,
) -> Vec<u8> {
    let mut digits = match radix {
        8 => format!("{value:o}").into_bytes(),
        16 if uppercase => format!("{value:X}").into_bytes(),
        16 => format!("{value:x}").into_bytes(),
        _ => value.to_string().into_bytes(),
    };
    if directive.precision == Some(0) && value == 0 {
        digits.clear();
    }
    let prefix: &[u8] = if directive.flags.alternate {
        match radix {
            8 if digits.is_empty()
                || (digits.first() != Some(&b'0')
                    && directive
                        .precision
                        .is_none_or(|precision| usize::from(precision) <= digits.len())) =>
            {
                b"0"
            }
            16 if value != 0 && uppercase => b"0X",
            16 if value != 0 => b"0x",
            _ => b"",
        }
    } else {
        b""
    };
    format_integer_parts(b"", prefix, digits, directive)
}

fn format_integer_parts(
    sign: &[u8],
    prefix: &[u8],
    mut digits: Vec<u8>,
    directive: &Directive,
) -> Vec<u8> {
    if let Some(precision) = directive.precision {
        let required = usize::from(precision).saturating_sub(digits.len());
        if required > 0 {
            let mut padded = vec![b'0'; required];
            padded.extend_from_slice(&digits);
            digits = padded;
        }
    }

    let content_len = sign.len() + prefix.len() + digits.len();
    let padding = directive
        .width
        .map_or(0, usize::from)
        .saturating_sub(content_len);
    let zero_width_padding =
        directive.flags.zero && !directive.flags.left && directive.precision.is_none();
    let mut output = Vec::with_capacity(content_len + padding);
    if !directive.flags.left && !zero_width_padding {
        output.resize(padding, b' ');
    }
    output.extend_from_slice(sign);
    output.extend_from_slice(prefix);
    if zero_width_padding {
        output.resize(output.len() + padding, b'0');
    }
    output.extend_from_slice(&digits);
    if directive.flags.left {
        output.resize(output.len() + padding, b' ');
    }
    output
}

fn format_float(number: f64, directive: &Directive) -> Vec<u8> {
    let negative = number.is_sign_negative();
    let magnitude = number.abs();
    let uppercase = matches!(
        directive.conversion,
        Conversion::ScientificUpper | Conversion::GeneralUpper | Conversion::HexFloatUpper
    );
    let precision = usize::from(directive.precision.unwrap_or(6));
    let mut body = match directive.conversion {
        Conversion::ScientificLower | Conversion::ScientificUpper => {
            format_scientific(magnitude, precision, uppercase, directive.flags.alternate)
        }
        Conversion::Fixed => format_fixed(magnitude, precision, directive.flags.alternate),
        Conversion::GeneralLower | Conversion::GeneralUpper => {
            format_general(magnitude, precision, uppercase, directive.flags.alternate)
        }
        Conversion::HexFloatLower | Conversion::HexFloatUpper => format_hex_float(
            magnitude,
            directive.precision,
            uppercase,
            directive.flags.alternate,
        ),
        _ => unreachable!("format_float called for a non-floating conversion"),
    };
    if uppercase {
        body.make_ascii_uppercase();
    }

    let sign: &[u8] = if negative {
        b"-"
    } else if directive.flags.plus {
        b"+"
    } else if directive.flags.space {
        b" "
    } else {
        b""
    };
    pad_float(sign, &body, directive)
}

fn format_scientific(number: f64, precision: usize, uppercase: bool, alternate: bool) -> Vec<u8> {
    if !number.is_finite() {
        return special_float(number, uppercase);
    }
    let text = format!("{number:.precision$e}");
    let (mantissa, exponent) = text
        .split_once('e')
        .expect("Rust scientific formatting always contains an exponent");
    let mut mantissa = mantissa.to_string();
    if alternate && precision == 0 {
        mantissa.push('.');
    }
    let exponent = exponent
        .parse::<i32>()
        .expect("Rust scientific formatting emits a decimal exponent");
    let marker = if uppercase { 'E' } else { 'e' };
    format!("{mantissa}{marker}{exponent:+03}").into_bytes()
}

fn format_fixed(number: f64, precision: usize, alternate: bool) -> Vec<u8> {
    if !number.is_finite() {
        return special_float(number, false);
    }
    let mut text = format!("{number:.precision$}");
    if alternate && precision == 0 {
        text.push('.');
    }
    text.into_bytes()
}

fn format_general(number: f64, precision: usize, uppercase: bool, alternate: bool) -> Vec<u8> {
    if !number.is_finite() {
        return special_float(number, uppercase);
    }
    let significant = precision.max(1);
    let probe = format_scientific(number, significant - 1, false, true);
    let marker = probe
        .iter()
        .position(|byte| *byte == b'e')
        .expect("finite scientific formatting contains an exponent");
    let exponent = std::str::from_utf8(&probe[marker + 1..])
        .expect("scientific exponent is ASCII")
        .parse::<i32>()
        .expect("scientific exponent is a decimal integer");

    let mut output = if exponent < -4 || exponent >= significant as i32 {
        format_scientific(number, significant - 1, uppercase, alternate)
    } else {
        let fractional = (significant as i32 - exponent - 1).max(0) as usize;
        format_fixed(number, fractional, alternate)
    };
    if !alternate {
        strip_trailing_fraction_zeros(&mut output);
    }
    output
}

fn strip_trailing_fraction_zeros(bytes: &mut Vec<u8>) {
    let exponent = bytes
        .iter()
        .position(|byte| matches!(byte, b'e' | b'E'))
        .unwrap_or(bytes.len());
    let Some(dot) = bytes[..exponent].iter().position(|byte| *byte == b'.') else {
        return;
    };
    let mut end = exponent;
    while end > dot + 1 && bytes[end - 1] == b'0' {
        end -= 1;
    }
    if end == dot + 1 {
        end = dot;
    }
    bytes.drain(end..exponent);
}

fn special_float(number: f64, uppercase: bool) -> Vec<u8> {
    let mut bytes = if number.is_nan() {
        b"nan".to_vec()
    } else {
        b"inf".to_vec()
    };
    if uppercase {
        bytes.make_ascii_uppercase();
    }
    bytes
}

fn format_hex_float(
    number: f64,
    precision: Option<u8>,
    uppercase: bool,
    alternate: bool,
) -> Vec<u8> {
    if !number.is_finite() {
        return special_float(number, uppercase);
    }

    let bits = number.to_bits();
    let exponent_bits = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let exponent = if exponent_bits == 0 {
        if fraction == 0 { 0 } else { -1022 }
    } else {
        exponent_bits - 1023
    };
    let significand = if exponent_bits == 0 {
        u128::from(fraction)
    } else {
        (1u128 << 52) | u128::from(fraction)
    };

    let (leading, fraction_text) = match precision {
        Some(requested) if requested < 13 => {
            let digits = usize::from(requested);
            let dropped_bits = 52 - digits * 4;
            let mut rounded = significand >> dropped_bits;
            if dropped_bits > 0 {
                let remainder_mask = (1u128 << dropped_bits) - 1;
                let remainder = significand & remainder_mask;
                let halfway = 1u128 << (dropped_bits - 1);
                if remainder > halfway || (remainder == halfway && rounded & 1 == 1) {
                    rounded += 1;
                }
            }
            let leading = (rounded >> (digits * 4)) as u8;
            let fraction_mask = if digits == 0 {
                0
            } else {
                (1u128 << (digits * 4)) - 1
            };
            let fraction = rounded & fraction_mask;
            let text = if digits == 0 {
                String::new()
            } else if uppercase {
                format!("{fraction:0digits$X}")
            } else {
                format!("{fraction:0digits$x}")
            };
            (leading, text)
        }
        Some(requested) => {
            let digits = usize::from(requested);
            let leading = (significand >> 52) as u8;
            let fraction = significand & ((1u128 << 52) - 1);
            let mut text = if uppercase {
                format!("{fraction:013X}")
            } else {
                format!("{fraction:013x}")
            };
            text.extend(std::iter::repeat_n('0', digits - 13));
            (leading, text)
        }
        None => {
            let leading = (significand >> 52) as u8;
            let fraction = significand & ((1u128 << 52) - 1);
            let mut text = if uppercase {
                format!("{fraction:013X}")
            } else {
                format!("{fraction:013x}")
            };
            while text.ends_with('0') {
                text.pop();
            }
            (leading, text)
        }
    };

    let prefix = if uppercase { "0X" } else { "0x" };
    let marker = if uppercase { 'P' } else { 'p' };
    let dot = if fraction_text.is_empty() && !alternate {
        ""
    } else {
        "."
    };
    format!("{prefix}{leading:x}{dot}{fraction_text}{marker}{exponent:+}").into_bytes()
}

fn pad_float(sign: &[u8], body: &[u8], directive: &Directive) -> Vec<u8> {
    let content_len = sign.len() + body.len();
    let padding = directive
        .width
        .map_or(0, usize::from)
        .saturating_sub(content_len);
    let mut output = Vec::with_capacity(content_len + padding);
    let zero_padding = directive.flags.zero
        && !directive.flags.left
        && !matches!(body, b"inf" | b"INF" | b"nan" | b"NAN");
    if !directive.flags.left && !zero_padding {
        output.resize(padding, b' ');
    }
    output.extend_from_slice(sign);
    if zero_padding {
        if body.starts_with(b"0x") || body.starts_with(b"0X") {
            output.extend_from_slice(&body[..2]);
            output.resize(output.len() + padding, b'0');
            output.extend_from_slice(&body[2..]);
            return output;
        }
        output.resize(output.len() + padding, b'0');
    }
    output.extend_from_slice(body);
    if directive.flags.left {
        output.resize(output.len() + padding, b' ');
    }
    output
}

fn pad_bytes(bytes: &[u8], width: Option<u8>, left: bool) -> Vec<u8> {
    let padding = width.map_or(0, usize::from).saturating_sub(bytes.len());
    let mut output = Vec::with_capacity(bytes.len() + padding);
    if !left {
        output.resize(padding, b' ');
    }
    output.extend_from_slice(bytes);
    if left {
        output.resize(output.len() + padding, b' ');
    }
    output
}

fn quote_argument(state: &State, idx: isize, argument: usize) -> Result<Vec<u8>> {
    match state.typ(idx) {
        LuaType::String => Ok(quote_string(state.to_bytes(idx)?)),
        LuaType::Number => {
            let number = state.to_number(idx)?;
            let output = if number.is_nan() {
                b"(0/0)".to_vec()
            } else if number == f64::INFINITY {
                b"1e9999".to_vec()
            } else if number == f64::NEG_INFINITY {
                b"-1e9999".to_vec()
            } else {
                let mut bytes = format_hex_float(number.abs(), None, false, false);
                if number.is_sign_negative() {
                    bytes.insert(0, b'-');
                }
                bytes
            };
            Ok(output)
        }
        LuaType::Boolean => Ok(if state.to_boolean(idx) {
            b"true".to_vec()
        } else {
            b"false".to_vec()
        }),
        LuaType::Nil => Ok(b"nil".to_vec()),
        LuaType::Table | LuaType::Function => Err(format_arg_error(
            state,
            argument,
            "value has no literal form",
        )),
    }
}

fn quote_string(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len() + 2);
    output.push(b'"');
    for (index, byte) in bytes.iter().copied().enumerate() {
        match byte {
            b'"' | b'\\' => {
                output.push(b'\\');
                output.push(byte);
            }
            b'\n' => {
                output.push(b'\\');
                output.push(b'\n');
            }
            0..=31 | 127 => {
                output.push(b'\\');
                let next_is_digit = bytes.get(index + 1).is_some_and(u8::is_ascii_digit);
                if next_is_digit {
                    output.extend_from_slice(format!("{byte:03}").as_bytes());
                } else {
                    output.extend_from_slice(byte.to_string().as_bytes());
                }
            }
            _ => output.push(byte),
        }
    }
    output.push(b'"');
    output
}
