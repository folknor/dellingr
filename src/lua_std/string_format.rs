//! Lua 5.4-compatible `string.format`.

use std::fmt::Write as _;

use super::exact_integer_value;
use super::string::append_bytes;
use crate::cost_model::Charge;
use crate::error::{ArgDetail, ArgError, Error, ErrorKind, LibraryError};
use crate::limits::{MAX_FORMAT_FIELD_DIGITS, MAX_FORMAT_LEN};
use crate::number_text::Sink;
use crate::numeral::{exact_i64, parse_lua_numeral};
use crate::vm::TostringRendering;
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
    /// The directive's bytes in the template, kept for error payloads.
    raw: std::ops::Range<usize>,
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
    // A string template is read in place from slot 1, which stays rooted and
    // unchanged for the whole call: a `%s` argument's `__tostring` runs above
    // this frame's arguments. The slice is reacquired after every call that
    // takes `&mut State`, so only offsets live across a re-entry. A numeric
    // template is coerced once into an owned buffer.
    let coerced_template = if state.typ(1) == LuaType::Number {
        Some(state.bytes_with_default_string_coercion(1)?)
    } else {
        None
    };
    let template_len = template(state, coerced_template.as_deref())?.len();
    let argument_count = state.get_top();
    let mut output = state.take_result_buffer();
    let mut argument = 1usize;
    // Charges (docs/budget.md, "String costs"): 1 per directive, admitted
    // after the directive parses and validates and before its argument is
    // rendered (so before any `__tostring`), and 1 for a call with no
    // directive, charged once it has succeeded. Literal text, `%%` escapes
    // and every byte read or produced are free. The pending literal run
    // `literal_start..cursor` is flushed before every `%` polls and at the
    // end. A `%%` escape is literal text: after its poll the run restarts at
    // its second `%`, and it never becomes a directive.
    let mut literal_start = 0usize;
    let mut cursor = 0usize;

    // bounded: one pass over the capped format string's bytes. Each `%`
    // polls before its directive is parsed and its argument rendered.
    while cursor < template_len {
        let bytes = template(state, coerced_template.as_deref())?;
        let Some(offset) = memchr::memchr(b'%', &bytes[cursor..]) else {
            break;
        };
        cursor += offset;
        append_literal(&mut output, &bytes[literal_start..cursor], template_len)?;
        state.poll_interrupt()?;
        if bytes.get(cursor + 1) == Some(&b'%') {
            literal_start = cursor + 1;
            cursor += 2;
            continue;
        }

        argument += 1;
        if argument > argument_count {
            return Err(format_arg_error(state, argument, ArgDetail::NoValue));
        }

        let (directive, next) = parse_directive(state, bytes, cursor)?;
        validate_directive(state, bytes, &directive)?;
        state.charge(Charge::StringFormat, 1)?;
        crate::native_call!(
            "format->format_argument",
            format_argument(state, argument, &directive, &mut output)
        )?;
        literal_start = next;
        cursor = next;
    }
    let bytes = template(state, coerced_template.as_deref())?;
    append_literal(&mut output, &bytes[literal_start..], template_len)?;
    if argument == 1 {
        state.charge(Charge::StringFormat, 1)?;
    }

    // A template with no `%` is not returned as slot 1 itself: the result
    // is interned after `set_top(0)`, so the allocation policy may collect
    // with the template and the arguments unrooted, and a dynamic template
    // can be swept and re-interned with a new `%p` identity. Returning
    // slot 1 would keep it alive through that collection, a replay-visible
    // change of what survives.
    state.set_top(0)?;
    state.push_result_buffer(output)?;
    Ok(1)
}

fn template<'a>(state: &'a State, coerced: Option<&'a [u8]>) -> Result<&'a [u8]> {
    match coerced {
        Some(bytes) => Ok(bytes),
        None => state.to_bytes(1),
    }
}

/// Append a literal run. The first non-empty run sizes the output for the
/// template's length, bounded by `FORMAT_CAPACITY_HINT`, so a typical result
/// grows once or not at all. The hint is taken only after the run passed its
/// cap check: a call that fails before producing output, or a deep recursion
/// through `__tostring` on a large shared template, holds no large
/// reservation. Capacity never affects which error is raised.
fn append_literal(out: &mut Vec<u8>, run: &[u8], template_len: usize) -> Result<()> {
    if out.capacity() == 0 && !run.is_empty() {
        crate::limits::checked_string_growth(0, run.len())?;
        out.reserve(run.len().max(template_len.min(FORMAT_CAPACITY_HINT)));
    }
    append_bytes(out, run)
}

/// Upper bound on the output capacity `append_literal` reserves up front:
/// the largest buffer the State keeps for reuse, so a short result's
/// hinted buffer is recycled.
const FORMAT_CAPACITY_HINT: usize = crate::vm::RESULT_BUFFER_CAPACITY;

/// Admit a piece of `len` bytes into `out`: the aggregate cap check every
/// directive makes after its rendering's own errors and before its bytes.
fn reserve_piece(out: &mut Vec<u8>, len: usize) -> Result<()> {
    crate::limits::checked_string_growth(out.len(), len)?;
    out.reserve(len);
    Ok(())
}

fn parse_directive(state: &State, bytes: &[u8], start: usize) -> Result<(Directive, usize)> {
    let mut cursor = start + 1;
    let mut flags = FormatFlags::default();
    // bounded: a directive is at most MAX_FORMAT_LEN bytes
    // (`check_directive_length`).
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
    // bounded: a directive is at most MAX_FORMAT_LEN bytes.
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
        // bounded: a directive is at most MAX_FORMAT_LEN bytes.
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
    let raw = start..cursor;
    let conversion = classify_conversion(conversion_byte)
        .ok_or_else(|| invalid_conversion_error(state, &bytes[raw.clone()]))?;
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
    // No `%`: `format` consumes a `%%` escape as literal text before any
    // directive is parsed, so a `%` conversion here carries flags, width or
    // precision (`%5%`) or ends the string, and is invalid like any other
    // unknown conversion.
    let conversion = match byte {
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

fn validate_directive(state: &State, template: &[u8], directive: &Directive) -> Result<()> {
    if matches!(directive.conversion, Conversion::Quoted) && directive.has_modifiers {
        return Err(state.library_error(LibraryError::QuotedFormatHasModifiers));
    }

    let flags = directive.flags;
    let flags_valid = match directive.conversion {
        Conversion::Float(_) | Conversion::Quoted => true,
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
        Conversion::Char | Conversion::Pointer | Conversion::Quoted
    );
    if !flags_valid
        || (!precision_valid && directive.precision.is_some())
        || directive.width_digits > MAX_FORMAT_FIELD_DIGITS
        || directive.precision_digits > MAX_FORMAT_FIELD_DIGITS
    {
        return Err(invalid_specification(
            state,
            &template[directive.raw.clone()],
        ));
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

fn argument_index(argument: usize) -> isize {
    isize::try_from(argument).expect("format argument index is bounded by the VM stack cap")
}

/// `%c` emits its integer modulo 256.
fn char_byte(integer: i64) -> u8 {
    u8::try_from(integer.rem_euclid(256)).expect("a value reduced modulo 256 fits in a byte")
}

/// Render one directive's argument straight into `out`. Each conversion
/// raises its own errors (argument type, `%s` zeros, `%q` quoting size,
/// `%p` identity) before the aggregate cap check in `reserve_piece`, as when
/// every directive rendered into a buffer of its own.
fn format_argument(
    state: &mut State,
    argument: usize,
    directive: &Directive,
    out: &mut Vec<u8>,
) -> Result<()> {
    let idx = argument_index(argument);
    match directive.conversion {
        Conversion::Char => {
            let integer = integer_argument(state, idx, argument)?;
            let byte = char_byte(integer);
            pad_bytes(out, &[byte], directive.width, directive.flags.left)
        }
        Conversion::SignedDecimal => {
            let integer = integer_argument(state, idx, argument)?;
            format_signed_integer(out, integer, directive)
        }
        Conversion::UnsignedDecimal => {
            let integer = integer_argument(state, idx, argument)?;
            format_unsigned_integer(out, unsigned_integer_bits(integer), 10, false, directive)
        }
        Conversion::Octal => {
            let integer = integer_argument(state, idx, argument)?;
            format_unsigned_integer(out, unsigned_integer_bits(integer), 8, false, directive)
        }
        Conversion::HexLower => {
            let integer = integer_argument(state, idx, argument)?;
            format_unsigned_integer(out, unsigned_integer_bits(integer), 16, false, directive)
        }
        Conversion::HexUpper => {
            let integer = integer_argument(state, idx, argument)?;
            format_unsigned_integer(out, unsigned_integer_bits(integer), 16, true, directive)
        }
        Conversion::Float(kind) => {
            let number = number_argument(state, idx, argument)?;
            format_float(out, number, directive, kind)
        }
        Conversion::String => {
            // The rendering and the bytes kept are free; the directive's one
            // unit was charged before rendering.
            let rendering = crate::native_call!(
                "format_argument->tostring_rendering",
                state.tostring_rendering(idx)
            )?;
            match rendering {
                TostringRendering::Itself => {
                    format_string_rendering(state, out, state.to_bytes(idx)?, directive, argument)
                }
                TostringRendering::OnTop => {
                    // The handler's result is popped whatever the append
                    // reports, then the saved result propagates.
                    let formatted = format_string_rendering(
                        state,
                        out,
                        state.to_bytes(-1)?,
                        directive,
                        argument,
                    );
                    state.pop(1)?;
                    formatted
                }
                TostringRendering::Number(text) => {
                    format_string_rendering(state, out, text.as_bytes(), directive, argument)
                }
                TostringRendering::Bytes(bytes) => {
                    format_string_rendering(state, out, &bytes, directive, argument)
                }
            }
        }
        Conversion::Quoted => quote_argument(state, idx, argument, out),
        Conversion::Pointer => {
            // The identity is resolved before the size check: resolving can
            // assign an id or fail on an exhausted id space.
            let id = match state.typ(idx) {
                // `(null)` is correct, not a stub: 5.4 substitutes it for any
                // argument with no pointer representation. Verified against
                // the reference - `string.format("%p", 42)` prints `(null)`.
                LuaType::Nil | LuaType::Boolean | LuaType::Number => None,
                LuaType::String | LuaType::Table | LuaType::Function => {
                    state.format_pointer_id(idx)?
                }
            };
            format_pointer(out, id, directive)
        }
    }
}

#[inline(never)]
fn format_pointer(out: &mut Vec<u8>, id: Option<u64>, directive: &Directive) -> Result<()> {
    match id {
        Some(id) => {
            let mut scratch = [0u8; 18];
            scratch[..2].copy_from_slice(b"0x");
            let digits = render_digits(&mut scratch[2..], id, 16, false);
            let len = 2 + digits;
            scratch.copy_within(18 - digits.., 2);
            pad_bytes(out, &scratch[..len], directive.width, directive.flags.left)
        }
        None => pad_bytes(out, b"(null)", directive.width, directive.flags.left),
    }
}

fn format_string_rendering(
    state: &State,
    out: &mut Vec<u8>,
    bytes: &[u8],
    directive: &Directive,
    argument: usize,
) -> Result<()> {
    if directive.has_modifiers && memchr::memchr(0, bytes).is_some() {
        return Err(format_arg_error(
            state,
            argument,
            ArgDetail::StringContainsZeros,
        ));
    }
    let selected_len = directive.precision.map_or(bytes.len(), |precision| {
        usize::from(precision).min(bytes.len())
    });
    pad_bytes(
        out,
        &bytes[..selected_len],
        directive.width,
        directive.flags.left,
    )
}

fn number_argument(state: &mut State, idx: isize, argument: usize) -> Result<f64> {
    match state.typ(idx) {
        LuaType::Number => state.to_number(idx),
        LuaType::String => {
            let number = parse_lua_numeral(state.to_bytes(idx)?);
            number.ok_or_else(|| {
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

/// Write `value`'s digits in `radix` right-aligned at the end of `scratch`
/// and return how many were written. `scratch` must hold every digit of
/// `value` in `radix` (22 for any `u64` in octal).
fn render_digits(scratch: &mut [u8], mut value: u64, radix: u64, uppercase: bool) -> usize {
    let alphabet: &[u8; 16] = if uppercase {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut end = scratch.len();
    // bounded: at most 22 digits for a u64 in radix 8 or more.
    loop {
        end -= 1;
        scratch[end] = alphabet[usize::try_from(value % radix).expect("a digit fits in usize")];
        value /= radix;
        if value == 0 {
            break;
        }
    }
    scratch.len() - end
}

#[inline(never)]
fn format_signed_integer(out: &mut Vec<u8>, value: i64, directive: &Directive) -> Result<()> {
    let sign = if value.is_negative() {
        b"-".as_slice()
    } else if directive.flags.plus {
        b"+".as_slice()
    } else if directive.flags.space {
        b" ".as_slice()
    } else {
        b"".as_slice()
    };
    let mut scratch = [0u8; 22];
    let count = if directive.precision == Some(0) && value == 0 {
        0
    } else {
        render_digits(&mut scratch, value.unsigned_abs(), 10, false)
    };
    format_integer_parts(out, sign, b"", &scratch[22 - count..], directive)
}

#[inline(never)]
fn format_unsigned_integer(
    out: &mut Vec<u8>,
    value: u64,
    radix: u64,
    uppercase: bool,
    directive: &Directive,
) -> Result<()> {
    let mut scratch = [0u8; 22];
    let count = if directive.precision == Some(0) && value == 0 {
        0
    } else {
        render_digits(&mut scratch, value, radix, uppercase)
    };
    let digits = &scratch[22 - count..];
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
    format_integer_parts(out, b"", prefix, digits, directive)
}

fn format_integer_parts(
    out: &mut Vec<u8>,
    sign: &[u8],
    prefix: &[u8],
    digits: &[u8],
    directive: &Directive,
) -> Result<()> {
    let precision_zeros = directive.precision.map_or(0, |precision| {
        usize::from(precision).saturating_sub(digits.len())
    });
    let content_len = sign.len() + prefix.len() + precision_zeros + digits.len();
    let padding = directive
        .width
        .map_or(0, usize::from)
        .saturating_sub(content_len);
    let zero_width_padding =
        directive.flags.zero && !directive.flags.left && directive.precision.is_none();
    reserve_piece(out, content_len + padding)?;
    if !directive.flags.left && !zero_width_padding {
        out.resize(out.len() + padding, b' ');
    }
    out.extend_from_slice(sign);
    out.extend_from_slice(prefix);
    if zero_width_padding {
        out.resize(out.len() + padding, b'0');
    }
    out.resize(out.len() + precision_zeros, b'0');
    out.extend_from_slice(digits);
    if directive.flags.left {
        out.resize(out.len() + padding, b' ');
    }
    Ok(())
}

/// Capacity of a [`FloatBody`]: the longest body is `%.99f` of the largest
/// finite binary64, 309 integral digits, a point and 99 fractional digits.
const FLOAT_BODY_CAPACITY: usize = 512;

/// A float directive's body (no sign, no padding), or a `%q` number,
/// rendered on the stack: no allocation per directive.
struct FloatBody {
    bytes: [u8; FLOAT_BODY_CAPACITY],
    len: usize,
}

impl FloatBody {
    fn new() -> Self {
        Self {
            bytes: [0; FLOAT_BODY_CAPACITY],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn make_ascii_uppercase(&mut self) {
        self.bytes[..self.len].make_ascii_uppercase();
    }
}

impl Sink for FloatBody {
    fn put(&mut self, bytes: &[u8]) {
        let end = self.len + bytes.len();
        self.bytes[self.len..end].copy_from_slice(bytes);
        self.len = end;
    }

    fn put_zeros(&mut self, count: usize) {
        let end = self.len + count;
        self.bytes[self.len..end].fill(b'0');
        self.len = end;
    }
}

impl std::fmt::Write for FloatBody {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let end = self.len + text.len();
        let slot = self.bytes.get_mut(self.len..end).ok_or(std::fmt::Error)?;
        slot.copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

// Outlined: the body's stack buffer and the decimal conversion's scratch
// stay off `format_argument`, whose frame is live across a `%s` argument's
// `__tostring` call. Nothing here calls back into Lua.
#[inline(never)]
fn format_float(
    out: &mut Vec<u8>,
    number: f64,
    directive: &Directive,
    kind: FloatKind,
) -> Result<()> {
    // NaN is canonicalised here, at output, rather than in the arithmetic.
    // The sign of a hardware-generated NaN is platform-specific (x86-64's
    // default NaN has the sign bit set, aarch64's does not), and NaNs reach
    // the VM from many producers: runtime `/` and `%`, `sqrt`, the
    // `crmath` functions behind `math.*` and `^`, and host `push_number`.
    // Dropping the sign here makes formatted NaNs identical on every host. `tostring` also prints unsigned `nan`, `%q` prints
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
    let alternate = directive.flags.alternate;
    // Every body is rendered lowercase; an uppercase conversion folds the
    // whole body (digits, `0x`, `p`, `e`, `inf`, `nan`) once at the end.
    let mut body = FloatBody::new();
    if !magnitude.is_finite() {
        special_float(&mut body, magnitude);
    } else {
        match kind {
            FloatKind::ScientificLower | FloatKind::ScientificUpper => {
                crate::number_text::scientific(&mut body, magnitude, precision, alternate);
            }
            FloatKind::Fixed => format_fixed(&mut body, magnitude, precision, alternate),
            FloatKind::GeneralLower | FloatKind::GeneralUpper => {
                crate::number_text::general(&mut body, magnitude, precision, alternate);
            }
            FloatKind::HexFloatLower | FloatKind::HexFloatUpper => {
                format_hex_float(&mut body, magnitude, directive.precision, alternate);
            }
        }
    }
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
    pad_float(out, sign, body.as_bytes(), directive)
}

/// `%f` body of a finite nonnegative magnitude.
fn format_fixed(body: &mut FloatBody, magnitude: f64, precision: usize, alternate: bool) {
    write!(body, "{magnitude:.precision$}")
        .expect("a bounded-precision fixed conversion fits a float body");
    if alternate && precision == 0 {
        body.put(b".");
    }
}

fn special_float(body: &mut FloatBody, magnitude: f64) {
    body.put(if magnitude.is_nan() { b"nan" } else { b"inf" });
}

/// `%a` body of a finite nonnegative magnitude, lowercase.
fn format_hex_float(body: &mut FloatBody, number: f64, precision: Option<u8>, alternate: bool) {
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

    // The fraction as `digits` hex digits of `value`, then `zeros` zeros.
    let (leading, value, digits, zeros) = match precision {
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
            (leading, rounded & fraction_mask, digits, 0)
        }
        Some(requested) => {
            let leading =
                u8::try_from(significand >> 52).expect("hexadecimal leading digit fits in u8");
            let fraction = significand & ((1u128 << 52) - 1);
            (leading, fraction, 13, usize::from(requested) - 13)
        }
        None => {
            let leading =
                u8::try_from(significand >> 52).expect("hexadecimal leading digit fits in u8");
            let mut fraction = significand & ((1u128 << 52) - 1);
            let mut digits = 13;
            // bounded: at most the 13 hexadecimal fraction digits.
            while digits > 0 && fraction & 0xf == 0 {
                fraction >>= 4;
                digits -= 1;
            }
            (leading, fraction, digits, 0)
        }
    };

    write!(body, "0x{leading:x}").expect("a hex float fits a float body");
    if digits > 0 || alternate {
        body.put(b".");
    }
    if digits > 0 {
        write!(body, "{value:0digits$x}").expect("a hex float fits a float body");
    }
    body.put_zeros(zeros);
    write!(body, "p{exponent:+}").expect("a hex float fits a float body");
}

fn pad_float(out: &mut Vec<u8>, sign: &[u8], body: &[u8], directive: &Directive) -> Result<()> {
    let content_len = sign.len() + body.len();
    let padding = directive
        .width
        .map_or(0, usize::from)
        .saturating_sub(content_len);
    reserve_piece(out, content_len + padding)?;
    let zero_padding = directive.flags.zero
        && !directive.flags.left
        && !matches!(body, b"inf" | b"INF" | b"nan" | b"NAN");
    if !directive.flags.left && !zero_padding {
        out.resize(out.len() + padding, b' ');
    }
    out.extend_from_slice(sign);
    if zero_padding {
        if body.starts_with(b"0x") || body.starts_with(b"0X") {
            out.extend_from_slice(&body[..2]);
            out.resize(out.len() + padding, b'0');
            out.extend_from_slice(&body[2..]);
            return Ok(());
        }
        out.resize(out.len() + padding, b'0');
    }
    out.extend_from_slice(body);
    if directive.flags.left {
        out.resize(out.len() + padding, b' ');
    }
    Ok(())
}

fn pad_bytes(out: &mut Vec<u8>, bytes: &[u8], width: Option<u8>, left: bool) -> Result<()> {
    let padding = width.map_or(0, usize::from).saturating_sub(bytes.len());
    reserve_piece(out, bytes.len() + padding)?;
    if !left {
        out.resize(out.len() + padding, b' ');
    }
    out.extend_from_slice(bytes);
    if left {
        out.resize(out.len() + padding, b' ');
    }
    Ok(())
}

fn quote_argument(state: &mut State, idx: isize, argument: usize, out: &mut Vec<u8>) -> Result<()> {
    let bytes: &[u8] = match state.typ(idx) {
        LuaType::String => return quote_string(out, state.to_bytes(idx)?),
        LuaType::Number => return quote_number(out, state.to_number(idx)?),
        LuaType::Boolean => {
            if state.to_boolean(idx) {
                b"true"
            } else {
                b"false"
            }
        }
        LuaType::Nil => b"nil",
        LuaType::Table | LuaType::Function => {
            return Err(format_arg_error(
                state,
                argument,
                ArgDetail::ValueHasNoLiteralForm,
            ));
        }
    };
    append_bytes(out, bytes)
}

/// `%q` of a number, rendered on the stack. Outlined like `format_float`,
/// so its buffer stays off `format_argument`'s frame.
#[inline(never)]
fn quote_number(out: &mut Vec<u8>, number: f64) -> Result<()> {
    append_bytes(out, quoted_number(number).as_bytes())
}

/// The literal `%q` writes for a number.
fn quoted_number(number: f64) -> FloatBody {
    let mut text = FloatBody::new();
    if number.is_nan() {
        text.put(b"(0/0)");
    } else if number == f64::INFINITY {
        text.put(b"1e9999");
    } else if number == f64::NEG_INFINITY {
        text.put(b"-1e9999");
    } else if let Some(integer) =
        exact_i64(number).filter(|_| number.to_bits() != (-0.0_f64).to_bits())
    {
        // With no integer subtype, use decimal for all integral values in
        // i64 range. Lua 5.4 uses this form for its integer subtype only.
        // `-0.0` keeps the hex form so the literal round-trips its sign.
        write!(text, "{integer}").expect("an i64 fits a float body");
    } else {
        if number.is_sign_negative() {
            text.put(b"-");
        }
        format_hex_float(&mut text, number.abs(), None, false);
    }
    text
}

/// Quote `bytes` onto `out`. The quoting's own size checks measure the
/// quoted piece alone, from `start`, exactly as when it was built in a
/// buffer of its own; the aggregate cap check follows once it is complete.
#[inline(never)]
fn quote_string(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let start = out.len();
    let capacity = crate::limits::checked_string_growth(bytes.len(), 2)?;
    out.reserve(capacity);
    out.push(b'"');
    // bounded: one pass over the argument string's bytes.
    for (index, byte) in bytes.iter().copied().enumerate() {
        let piece = out.len() - start;
        match byte {
            b'"' | b'\\' => {
                crate::limits::checked_string_growth(piece, 2)?;
                out.push(b'\\');
                out.push(byte);
            }
            b'\n' => {
                crate::limits::checked_string_growth(piece, 2)?;
                out.push(b'\\');
                out.push(b'\n');
            }
            0..=31 | 127 => {
                let next_is_digit = bytes.get(index + 1).is_some_and(u8::is_ascii_digit);
                let mut scratch = [b'0'; 3];
                let count = render_digits(&mut scratch, u64::from(byte), 10, false);
                let escaped = if next_is_digit {
                    &scratch[..]
                } else {
                    &scratch[3 - count..]
                };
                crate::limits::checked_string_growth(piece, escaped.len() + 1)?;
                out.push(b'\\');
                out.extend_from_slice(escaped);
            }
            _ => {
                crate::limits::checked_string_growth(piece, 1)?;
                out.push(byte);
            }
        }
    }
    crate::limits::checked_string_growth(out.len() - start, 1)?;
    out.push(b'"');
    crate::limits::checked_string_growth(start, out.len() - start)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FloatBody, format_hex_float};
    use crate::number_text::Sink;
    use crate::numeral::parse_lua_numeral;

    #[test]
    fn percent_scan_matches_a_bytewise_scan() {
        // Every length across two words plus a tail, with `%` at each
        // position, alone and after a byte just above and below it, and next
        // to high bytes that an inexact zero-byte test misreads.
        let fillers = [b'a', b'$', b'&', 0x00, 0x80, 0xa5, 0xff];
        for len in 0..=20 {
            for filler in fillers {
                let mut bytes = vec![filler; len];
                if bytes.iter().position(|byte| *byte == b'%') != memchr::memchr(b'%', &bytes) {
                    panic!("no-match scan differs for len {len}, filler {filler:#x}");
                }
                for at in 0..len {
                    bytes[at] = b'%';
                    for later in at + 1..len {
                        bytes[later] = b'%';
                        let expected = bytes.iter().position(|byte| *byte == b'%');
                        if expected != memchr::memchr(b'%', &bytes) {
                            panic!("scan differs for len {len}, filler {filler:#x}, at {at}");
                        }
                        bytes[later] = filler;
                    }
                    if Some(at) != memchr::memchr(b'%', &bytes) {
                        panic!("scan misses len {len}, filler {filler:#x}, at {at}");
                    }
                    bytes[at] = filler;
                }
            }
        }
    }

    fn rounded_hex_bits(number: f64, precision: u8) -> u64 {
        let bits = number.to_bits();
        let sign = bits & (1 << 63);
        let exponent_bits = (bits >> 52) & 0x7ff;
        let fraction = bits & ((1 << 52) - 1);
        let (exponent, significand) = if exponent_bits == 0 {
            (-1022, fraction)
        } else {
            (
                i32::try_from(exponent_bits).expect("the f64 exponent field fits in i32") - 1023,
                (1 << 52) | fraction,
            )
        };
        let dropped_bits = 52 - u32::from(precision) * 4;
        let mut rounded = significand >> dropped_bits;
        if dropped_bits > 0 {
            let remainder = significand & ((1 << dropped_bits) - 1);
            let halfway = 1 << (dropped_bits - 1);
            if remainder > halfway || (remainder == halfway && rounded & 1 == 1) {
                rounded += 1;
            }
        }

        if exponent_bits == 0 {
            return sign | (rounded << dropped_bits);
        }

        let bit_count = u64::BITS - rounded.leading_zeros();
        let top_exponent = exponent - 52
            + i32::try_from(dropped_bits).expect("the dropped-bit count fits in i32")
            + i32::try_from(bit_count - 1).expect("the rounded significand bit count fits in i32");
        if top_exponent > 1023 {
            return sign | f64::INFINITY.to_bits();
        }

        let normalized = rounded << (53 - bit_count);
        let exponent_field = u64::try_from(top_exponent + 1023)
            .expect("a finite normal result has a nonnegative exponent field");
        sign | (exponent_field << 52) | (normalized & ((1 << 52) - 1))
    }

    #[test]
    fn hex_float_precision_rounds_across_f64_classes() {
        let fractions = [0, 1, 1 << 51, (1 << 52) - 1];

        for exponent in 0..=0x7ff_u64 {
            for fraction in fractions {
                for sign in [0, 1 << 63] {
                    let number = f64::from_bits(sign | (exponent << 52) | fraction);
                    if !number.is_finite() {
                        continue;
                    }

                    for precision in 0..=13 {
                        let mut body = FloatBody::new();
                        if number.is_sign_negative() {
                            body.put(b"-");
                        }
                        format_hex_float(&mut body, number.abs(), Some(precision), false);
                        let formatted = body.as_bytes();
                        let text =
                            std::str::from_utf8(formatted).expect("hex float output is ASCII");
                        let parsed = parse_lua_numeral(formatted)
                            .expect("formatted hex floats are valid Lua numerals");
                        let rounded_bits = rounded_hex_bits(number, precision);
                        assert_eq!(
                            parsed.to_bits(),
                            rounded_bits,
                            "formatted {text} from bits {:016x} at precision {precision}: parser {parsed:?} ({:016x}), rounded result {rounded_bits:016x}",
                            number.to_bits(),
                            parsed.to_bits()
                        );
                    }
                }
            }
        }
    }
}
