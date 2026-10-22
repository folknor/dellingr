//! Lua 5.4-compatible `string.format`.

use super::exact_integer_value;
use crate::error::{ArgDetail, ArgError, Error, ErrorKind, LibraryError};
use crate::limits::{MAX_FORMAT_FIELD_DIGITS, MAX_FORMAT_LEN};
use crate::numeral::{exact_i64, parse_lua_numeral};
use crate::{LuaType, Result, State};

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
    Float(FloatKind),
    String,
    Quoted,
    Pointer,
}

#[derive(Clone, Copy)]
enum FloatKind {
    ScientificLower,
    ScientificUpper,
    Fixed,
    GeneralLower,
    GeneralUpper,
    HexFloatLower,
    HexFloatUpper,
}

pub(crate) fn format(state: &mut State) -> Result<u8> {
    if state.get_top() == 0 {
        return Err(format_arg_type_error(state, 1, LuaType::String, None));
    }
    state.check_type_in(1, &[LuaType::String, LuaType::Number], "format")?;
    let format_bytes = if state.typ(1) == LuaType::Number {
        state.bytes_with_default_string_coercion(1)?
    } else {
        state.to_bytes(1)?.to_vec()
    };
    state.consume_cost(format_bytes.len().max(1) as u64)?;
    let argument_count = state.get_top();
    let mut output = Vec::new();
    let mut argument = 1usize;
    let mut cursor = 0usize;

    while cursor < format_bytes.len() {
        if format_bytes[cursor] != b'%' {
            append_output(state, &mut output, &format_bytes[cursor..=cursor])?;
            cursor += 1;
            continue;
        }
        if format_bytes.get(cursor + 1) == Some(&b'%') {
            let (directive, next) = parse_directive(state, &format_bytes, cursor)?;
            let formatted = format_argument(state, 0, &directive)?;
            append_output(state, &mut output, &formatted)?;
            cursor = next;
            continue;
        }

        argument += 1;
        if argument > argument_count {
            return Err(format_arg_error(state, argument, ArgDetail::NoValue));
        }

        let (directive, next) = parse_directive(state, &format_bytes, cursor)?;
        validate_directive(state, &directive)?;
        let formatted = format_argument(state, argument, &directive)?;
        append_output(state, &mut output, &formatted)?;
        cursor = next;
    }

    state.set_top(0)?;
    state.push_bytes(output)?;
    Ok(1)
}

fn append_output(state: &mut State, output: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let next = crate::limits::checked_string_growth(output.len(), bytes.len())?;
    state.consume_cost(bytes.len() as u64)?;
    output.reserve(next - output.len());
    output.extend_from_slice(bytes);
    Ok(())
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
    let conversion = classify_conversion(conversion_byte)
        .ok_or_else(|| invalid_conversion_error(state, &raw))?;
    if matches!(conversion, Conversion::Percent) && raw != b"%%" {
        return Err(invalid_conversion_error(state, &raw));
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
        Err(state.library_error(LibraryError::FormatDirectiveTooLong))
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
        // %u is deprecated, not removed: 5.2 and 5.4 both still accept it and
        // print the integer. Rejecting it here would diverge from both.
        b'u' => Conversion::UnsignedDecimal,
        b'o' => Conversion::Octal,
        b'x' => Conversion::HexLower,
        b'X' => Conversion::HexUpper,
        b'e' => Conversion::Float(FloatKind::ScientificLower),
        b'E' => Conversion::Float(FloatKind::ScientificUpper),
        // No `F`: reference lstrlib rejects it too (5.4: "invalid conversion
        // '%F' to 'format'"; 5.2: "invalid option"), so it falls to the
        // invalid-conversion arm below on purpose.
        b'f' => Conversion::Float(FloatKind::Fixed),
        b'g' => Conversion::Float(FloatKind::GeneralLower),
        b'G' => Conversion::Float(FloatKind::GeneralUpper),
        b'a' => Conversion::Float(FloatKind::HexFloatLower),
        b'A' => Conversion::Float(FloatKind::HexFloatUpper),
        b's' => Conversion::String,
        b'q' => Conversion::Quoted,
        // %p is 5.4-only; 5.2 rejects it with "invalid option". Accepting it
        // follows this module's 5.4 contract, so the 5.2 mismatch is expected.
        b'p' => Conversion::Pointer,
        _ => return None,
    };
    Some(conversion)
}

fn validate_directive(state: &State, directive: &Directive) -> Result<()> {
    if matches!(directive.conversion, Conversion::Quoted) && directive.has_modifiers {
        return Err(state.library_error(LibraryError::QuotedFormatHasModifiers));
    }

    let flags = directive.flags;
    let flags_valid = match directive.conversion {
        Conversion::Float(_) | Conversion::Quoted | Conversion::Percent => true,
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
        || directive.width_digits > MAX_FORMAT_FIELD_DIGITS
        || directive.precision_digits > MAX_FORMAT_FIELD_DIGITS
    {
        return Err(invalid_specification(state, &directive.raw));
    }
    Ok(())
}

fn invalid_specification(state: &State, raw: &[u8]) -> Error {
    state.library_error(LibraryError::InvalidFormatSpecification {
        directive: raw.to_vec(),
    })
}

fn invalid_conversion_error(state: &State, raw: &[u8]) -> Error {
    state.library_error(LibraryError::InvalidFormatConversion {
        directive: raw.to_vec(),
    })
}

// Lua's unsigned integer conversions preserve the signed value's two's
// complement bit pattern, matching the former `i64 as u64` conversion.
fn unsigned_integer_bits(integer: i64) -> u64 {
    u64::from_ne_bytes(integer.to_ne_bytes())
}

fn format_argument(state: &mut State, argument: usize, directive: &Directive) -> Result<Vec<u8>> {
    let idx =
        isize::try_from(argument).map_err(|_| invalid_specification(state, &directive.raw))?;
    match directive.conversion {
        Conversion::Percent => Ok(b"%".to_vec()),
        Conversion::Char => {
            let integer = integer_argument(state, idx, argument)?;
            let byte = u8::try_from(integer.rem_euclid(256))
                .map_err(|_| invalid_specification(state, &directive.raw))?;
            Ok(pad_bytes(&[byte], directive.width, directive.flags.left))
        }
        Conversion::SignedDecimal => {
            let integer = integer_argument(state, idx, argument)?;
            Ok(format_signed_integer(integer, directive))
        }
        Conversion::UnsignedDecimal => {
            let integer = integer_argument(state, idx, argument)?;
            Ok(format_unsigned_integer(
                unsigned_integer_bits(integer),
                10,
                false,
                directive,
            ))
        }
        Conversion::Octal => {
            let integer = integer_argument(state, idx, argument)?;
            Ok(format_unsigned_integer(
                unsigned_integer_bits(integer),
                8,
                false,
                directive,
            ))
        }
        Conversion::HexLower => {
            let integer = integer_argument(state, idx, argument)?;
            Ok(format_unsigned_integer(
                unsigned_integer_bits(integer),
                16,
                false,
                directive,
            ))
        }
        Conversion::HexUpper => {
            let integer = integer_argument(state, idx, argument)?;
            Ok(format_unsigned_integer(
                unsigned_integer_bits(integer),
                16,
                true,
                directive,
            ))
        }
        Conversion::Float(kind) => {
            let number = number_argument(state, idx, argument)?;
            Ok(format_float(number, directive, kind))
        }
        Conversion::String => {
            // Charged on the full rendering, before precision truncation.
            let mut bytes = state.tostring_bytes_unguarded(idx)?;
            state.consume_cost(bytes.len() as u64)?;
            if directive.has_modifiers && bytes.contains(&0) {
                return Err(format_arg_error(
                    state,
                    argument,
                    ArgDetail::StringContainsZeros,
                ));
            }
            if let Some(precision) = directive.precision {
                bytes.truncate(usize::from(precision).min(bytes.len()));
            }
            Ok(pad_bytes(&bytes, directive.width, directive.flags.left))
        }
        Conversion::Quoted => quote_argument(state, idx, argument),
        Conversion::Pointer => {
            let bytes = match state.typ(idx) {
                // `(null)` is correct, not a stub: 5.4 substitutes it for any
                // argument with no pointer representation. Verified against
                // the reference - `string.format("%p", 42)` prints `(null)`.
                LuaType::Nil | LuaType::Boolean | LuaType::Number => b"(null)".to_vec(),
                LuaType::String | LuaType::Table | LuaType::Function => {
                    match state.format_pointer_id(idx)? {
                        Some(id) => format!("0x{id:x}").into_bytes(),
                        None => b"(null)".to_vec(),
                    }
                }
            };
            Ok(pad_bytes(&bytes, directive.width, directive.flags.left))
        }
    }
}

fn number_argument(state: &mut State, idx: isize, argument: usize) -> Result<f64> {
    match state.typ(idx) {
        LuaType::Number => state.to_number(idx),
        LuaType::String => {
            let bytes = state.to_bytes(idx)?.to_vec();
            state.consume_cost(bytes.len() as u64)?;
            parse_lua_numeral(&bytes).ok_or_else(|| {
                format_arg_type_error(state, argument, LuaType::Number, Some(LuaType::String))
            })
        }
        received => Err(format_arg_type_error(
            state,
            argument,
            LuaType::Number,
            Some(received),
        )),
    }
}

fn integer_argument(state: &mut State, idx: isize, argument: usize) -> Result<i64> {
    let number = number_argument(state, idx, argument)?;
    exact_integer_value(state, number, idx, "format")
}

fn format_arg_error(state: &State, argument: usize, detail: ArgDetail) -> Error {
    let argument =
        isize::try_from(argument).expect("format argument index is bounded by the VM stack cap");
    super::argument_error(state, argument, "format", detail)
}

fn format_arg_type_error(
    state: &State,
    argument: usize,
    expected: LuaType,
    received: Option<LuaType>,
) -> Error {
    let argument =
        isize::try_from(argument).expect("format argument index is bounded by the VM stack cap");
    state.error(ErrorKind::ArgError(ArgError::type_mismatch(
        argument,
        Some("format"),
        expected,
        received,
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

fn format_float(number: f64, directive: &Directive, kind: FloatKind) -> Vec<u8> {
    // NaN is canonicalised here, at output, rather than in the arithmetic.
    // The sign of a hardware-generated NaN is platform-specific (x86-64's
    // default NaN has the sign bit set, aarch64's does not), and NaNs reach
    // the VM from many producers: runtime `/` and `%`, `sqrt`, the
    // `crmath` functions behind `math.*` and `^`, and host `push_number`. Dropping the sign here makes formatted NaNs identical
    // on every host. `tostring` also prints unsigned `nan`, `%q` prints
    // `(0/0)`, NaN never compares equal and cannot be a table key, and
    // snapshots canonicalise its bits. Reference C Lua prints
    // `-nan` for `0/0` on x86-64; dellingr always prints `nan`.
    let negative = number.is_sign_negative() && !number.is_nan();
    let magnitude = number.abs();
    let uppercase = matches!(
        kind,
        FloatKind::ScientificUpper | FloatKind::GeneralUpper | FloatKind::HexFloatUpper
    );
    let precision = usize::from(directive.precision.unwrap_or(6));
    let mut body = match kind {
        FloatKind::ScientificLower | FloatKind::ScientificUpper => {
            format_scientific(magnitude, precision, uppercase, directive.flags.alternate)
        }
        FloatKind::Fixed => format_fixed(magnitude, precision, directive.flags.alternate),
        FloatKind::GeneralLower | FloatKind::GeneralUpper => {
            format_general(magnitude, precision, uppercase, directive.flags.alternate)
        }
        FloatKind::HexFloatLower | FloatKind::HexFloatUpper => format_hex_float(
            magnitude,
            directive.precision,
            uppercase,
            directive.flags.alternate,
        ),
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
    let significant_i32 =
        i32::try_from(significant).expect("format precision is bounded by u8::MAX");
    let probe = format_scientific(number, significant - 1, false, true);
    let marker = probe
        .iter()
        .position(|byte| *byte == b'e')
        .expect("finite scientific formatting contains an exponent");
    let exponent = std::str::from_utf8(&probe[marker + 1..])
        .expect("scientific exponent is ASCII")
        .parse::<i32>()
        .expect("scientific exponent is a decimal integer");

    let mut output = if exponent < -4 || exponent >= significant_i32 {
        format_scientific(number, significant - 1, uppercase, alternate)
    } else {
        let fractional = usize::try_from((significant_i32 - exponent - 1).max(0))
            .expect("fractional precision is nonnegative");
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
    let exponent_bits =
        i32::try_from((bits >> 52) & 0x7ff).expect("the f64 exponent field fits in i32");
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
            let leading = u8::try_from(rounded >> (digits * 4))
                .expect("rounded hexadecimal leading digit fits in u8");
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
            let leading =
                u8::try_from(significand >> 52).expect("hexadecimal leading digit fits in u8");
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
            let leading =
                u8::try_from(significand >> 52).expect("hexadecimal leading digit fits in u8");
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

fn quote_argument(state: &mut State, idx: isize, argument: usize) -> Result<Vec<u8>> {
    match state.typ(idx) {
        LuaType::String => {
            let bytes = state.to_bytes(idx)?.to_vec();
            state.consume_cost(bytes.len() as u64)?;
            quote_string(&bytes)
        }
        LuaType::Number => {
            let number = state.to_number(idx)?;
            let output = if number.is_nan() {
                b"(0/0)".to_vec()
            } else if number == f64::INFINITY {
                b"1e9999".to_vec()
            } else if number == f64::NEG_INFINITY {
                b"-1e9999".to_vec()
            } else if let Some(integer) =
                exact_i64(number).filter(|_| number.to_bits() != (-0.0_f64).to_bits())
            {
                // With no integer subtype, use decimal for all integral
                // values in i64 range. Lua 5.4 uses this form for its integer
                // subtype only. `-0.0` keeps the hex form so the literal
                // round-trips its sign.
                integer.to_string().into_bytes()
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
            ArgDetail::ValueHasNoLiteralForm,
        )),
    }
}

fn quote_string(bytes: &[u8]) -> Result<Vec<u8>> {
    let capacity = crate::limits::checked_string_growth(bytes.len(), 2)?;
    let mut output = Vec::with_capacity(capacity);
    output.push(b'"');
    for (index, byte) in bytes.iter().copied().enumerate() {
        match byte {
            b'"' | b'\\' => {
                crate::limits::checked_string_growth(output.len(), 2)?;
                output.push(b'\\');
                output.push(byte);
            }
            b'\n' => {
                crate::limits::checked_string_growth(output.len(), 2)?;
                output.push(b'\\');
                output.push(b'\n');
            }
            0..=31 | 127 => {
                let next_is_digit = bytes.get(index + 1).is_some_and(u8::is_ascii_digit);
                if next_is_digit {
                    let escaped = format!("{byte:03}");
                    crate::limits::checked_string_growth(output.len(), escaped.len() + 1)?;
                    output.push(b'\\');
                    output.extend_from_slice(escaped.as_bytes());
                } else {
                    let escaped = byte.to_string();
                    crate::limits::checked_string_growth(output.len(), escaped.len() + 1)?;
                    output.push(b'\\');
                    output.extend_from_slice(escaped.as_bytes());
                }
            }
            _ => {
                crate::limits::checked_string_growth(output.len(), 1)?;
                output.push(byte);
            }
        }
    }
    crate::limits::checked_string_growth(output.len(), 1)?;
    output.push(b'"');
    Ok(output)
}
