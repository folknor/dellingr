//! Precision-directed decimal rendering of numbers: the `%.14g` spelling
//! behind `tostring`, `..` and every default string coercion, and the
//! `%e`/`%g` conversions of `string.format`.
//!
//! One exact scientific conversion yields the rounded significant digits and
//! the post-rounding decimal exponent; both notations are laid out from
//! those digits. For `%g` this is exact, not an approximation of a second
//! fixed-notation conversion: without a carry, fixed notation with `P-1-X`
//! fractional places rounds at the same decimal position, with the same
//! ties-to-even parity, as the scientific conversion did. When scientific
//! rounding carries into the next decade (`9.99..` to `1.00..e+1`), the value
//! lies within half a fine-grid step of the power of ten, so rounding at the
//! one-coarser fixed position also gives exactly that power: the digits are
//! `1` followed by zeros either way, and no coarser-grid tie is possible.
//!
//! The conversion itself is Rust's float formatting, which is correctly
//! rounded from the binary value with ties to even (like glibc's), so the
//! digits are host-independent. A shortest-round-trip formatter would not
//! do: `%.14g` asks for a precision, not for the shortest spelling.

use std::fmt::Write as _;

/// The longest `%.14g` spelling of a finite number,
/// `-1.2345678901234e-308`, plus slack.
const NUMBER_TEXT_CAPACITY: usize = 24;

/// An upper bound on the length of any number's default spelling.
pub(crate) const MAX_NUMBER_TEXT: usize = NUMBER_TEXT_CAPACITY;

/// Significant digits a conversion can ask for: `%.99e` needs 100 (the
/// format precision is at most two decimal digits, `MAX_FORMAT_FIELD_DIGITS`).
const MAX_SIGNIFICANT: usize = 100;

/// A number's default spelling, held inline: no allocation to render one.
#[derive(Clone, Copy)]
pub(crate) struct NumberText {
    bytes: [u8; NUMBER_TEXT_CAPACITY],
    len: u8,
}

impl NumberText {
    fn new() -> Self {
        Self {
            bytes: [0; NUMBER_TEXT_CAPACITY],
            len: 0,
        }
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    pub(crate) fn as_str(&self) -> &str {
        std::str::from_utf8(self.as_bytes()).expect("number spellings are ASCII")
    }
}

/// Where rendered bytes go: inline text or a growing byte buffer.
trait Sink {
    fn put(&mut self, bytes: &[u8]);
    fn put_zeros(&mut self, count: usize);
}

impl Sink for NumberText {
    fn put(&mut self, bytes: &[u8]) {
        let start = usize::from(self.len);
        let end = start + bytes.len();
        self.bytes[start..end].copy_from_slice(bytes);
        self.len = u8::try_from(end).expect("a number spelling fits its inline capacity");
    }

    fn put_zeros(&mut self, count: usize) {
        let start = usize::from(self.len);
        let end = start + count;
        self.bytes[start..end].fill(b'0');
        self.len = u8::try_from(end).expect("a number spelling fits its inline capacity");
    }
}

impl std::fmt::Write for NumberText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if usize::from(self.len) + text.len() > NUMBER_TEXT_CAPACITY {
            return Err(std::fmt::Error);
        }
        self.put(text.as_bytes());
        Ok(())
    }
}

impl Sink for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }

    fn put_zeros(&mut self, count: usize) {
        self.resize(self.len() + count, b'0');
    }
}

/// Render a number the way reference Lua 5.2's `tostring` does: C `%.14g`
/// (`LUA_NUMBER_FMT`). Every coercion goes through here - `tostring`,
/// `print`, `..`, `table.concat`, `%s`, string functions on numbers - so the
/// spelling is replay-visible and must be identical on every host.
///
/// Val has no integer/float subtype, so the 5.2 rule (one `%.14g` for every
/// number) is the one dellingr can follow exactly: integral values print
/// without a decimal point (`3`, which 5.4 also prints for integers), large
/// ones switch to exponent form (`1e+15`, `9.007199254741e+15`), and negative
/// zero prints `-0`. Lua 5.4 splits by subtype (`3.0`/`-0.0` for floats, `3`/
/// `0` for integers); either 5.4 spelling would match neither reference for
/// the other half, while 5.2's always matches 5.2.
///
/// NaN always prints `nan`, never `-nan`: a hardware NaN's sign differs by
/// architecture (x86-64's default NaN is negative, aarch64's positive), so
/// printing it would make output host-dependent. `string.format` drops the
/// sign at the same point for the same reason. Reference C Lua prints `-nan`
/// for `0/0` on x86-64 glibc.
pub(crate) fn number_to_text(number: f64) -> NumberText {
    let mut text = NumberText::new();
    if number.is_nan() {
        text.put(b"nan");
        return text;
    }
    if number.is_infinite() {
        text.put(if number > 0.0 { b"inf" } else { b"-inf" });
        return text;
    }
    // Fast exact path: an integral value of at most 14 digits is exactly its
    // `%.14g` spelling, and integer formatting avoids the float-to-decimal
    // conversion entirely. This covers loop counters, indices and most
    // numbers that reach concat and print.
    if number.abs() < 1e14 && number.fract().to_bits() << 1 == 0 {
        if number.to_bits() == (-0.0_f64).to_bits() {
            text.put(b"-0");
            return text;
        }
        let integer = number as i64; // Exact: integral and below 1e14 in magnitude.
        write!(text, "{integer}").expect("a 14-digit integer fits its inline capacity");
        return text;
    }
    general_g14(number)
}

/// `%.14g` of a finite number off the integral fast path. Outlined so the
/// conversion's scratch stays off the frames of its many callers.
#[inline(never)]
fn general_g14(number: f64) -> NumberText {
    let mut text = NumberText::new();
    if number.is_sign_negative() {
        text.put(b"-");
    }
    write_general(&mut text, number.abs(), 14, false);
    text
}

/// A finite nonnegative magnitude rounded to `count` significant digits:
/// the digits (ASCII, exactly `count` of them) and the decimal exponent
/// after rounding. Zero has exponent 0.
struct Decimal {
    digits: [u8; MAX_SIGNIFICANT],
    count: usize,
    exponent: i32,
}

impl Decimal {
    fn digits(&self) -> &[u8] {
        &self.digits[..self.count]
    }
}

/// Scratch for one scientific conversion: at most `MAX_SIGNIFICANT` digits,
/// a point and `e-324`.
struct Scratch {
    bytes: [u8; MAX_SIGNIFICANT + 8],
    len: usize,
}

impl std::fmt::Write for Scratch {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let end = self.len + text.len();
        let slot = self.bytes.get_mut(self.len..end).ok_or(std::fmt::Error)?;
        slot.copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

#[inline(never)]
fn decimal(magnitude: f64, count: usize) -> Decimal {
    debug_assert!((1..=MAX_SIGNIFICANT).contains(&count));
    debug_assert!(magnitude.is_finite() && magnitude.is_sign_positive());
    let mut scratch = Scratch {
        bytes: [0; MAX_SIGNIFICANT + 8],
        len: 0,
    };
    let fraction = count - 1;
    write!(scratch, "{magnitude:.fraction$e}")
        .expect("a bounded-precision scientific conversion fits its scratch");
    let text = &scratch.bytes[..scratch.len];
    let marker = text
        .iter()
        .position(|byte| *byte == b'e')
        .expect("Rust scientific formatting always contains an exponent");
    let mut decimal = Decimal {
        digits: [b'0'; MAX_SIGNIFICANT],
        count: 0,
        exponent: 0,
    };
    // bounded: at most MAX_SIGNIFICANT digits and a point.
    for &byte in &text[..marker] {
        if byte != b'.' {
            decimal.digits[decimal.count] = byte;
            decimal.count += 1;
        }
    }
    debug_assert_eq!(decimal.count, count);
    let (negative, magnitude_digits) = match text[marker + 1..].split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, &text[marker + 1..]),
    };
    // bounded: an f64 decimal exponent has at most three digits.
    let exponent = magnitude_digits
        .iter()
        .fold(0i32, |value, digit| value * 10 + i32::from(digit - b'0'));
    decimal.exponent = if negative { -exponent } else { exponent };
    decimal
}

/// `e+XX`: the marker, an explicit sign and at least two exponent digits.
fn put_exponent(sink: &mut impl Sink, exponent: i32) {
    let magnitude = exponent.unsigned_abs();
    let mut digits = [b'0'; 3];
    digits[0] = b'0' + u8::try_from(magnitude / 100).expect("an f64 exponent has three digits");
    digits[1] = b'0' + u8::try_from(magnitude / 10 % 10).expect("a decimal digit");
    digits[2] = b'0' + u8::try_from(magnitude % 10).expect("a decimal digit");
    sink.put(if exponent < 0 { b"e-" } else { b"e+" });
    sink.put(if magnitude >= 100 {
        &digits
    } else {
        &digits[1..]
    });
}

/// The digits with trailing zeros dropped, unless `keep` (the `#` flag).
fn trimmed(digits: &[u8], keep: bool) -> &[u8] {
    if keep {
        return digits;
    }
    let end = digits
        .iter()
        .rposition(|digit| *digit != b'0')
        .map_or(0, |last| last + 1);
    &digits[..end]
}

/// C `%.<precision>g` of a finite nonnegative magnitude, lowercase. A
/// precision of 0 behaves as 1. With `alternate` (`#`) trailing zeros and
/// the point are kept.
fn write_general(sink: &mut impl Sink, magnitude: f64, precision: usize, alternate: bool) {
    let significant = precision.max(1);
    let decimal = decimal(magnitude, significant);
    let digits = decimal.digits();
    let exponent = decimal.exponent;
    let significant_i32 = i32::try_from(significant).expect("format precision is bounded");
    if exponent < -4 || exponent >= significant_i32 {
        sink.put(&digits[..1]);
        let fraction = trimmed(&digits[1..], alternate);
        if !fraction.is_empty() || alternate {
            sink.put(b".");
        }
        sink.put(fraction);
        put_exponent(sink, exponent);
    } else if let Ok(whole) = usize::try_from(exponent) {
        // Fixed with the point after `exponent + 1` digits; `exponent` is
        // below the significant count, so at least one digit is whole.
        sink.put(&digits[..=whole]);
        let fraction = trimmed(&digits[whole + 1..], alternate);
        if !fraction.is_empty() || alternate {
            sink.put(b".");
        }
        sink.put(fraction);
    } else {
        // Exponent -4..=-1: `0.`, the leading zeros, then the digits. The
        // first digit is nonzero (zero has exponent 0), so trimming never
        // empties the fraction.
        let leading = usize::try_from(-exponent - 1).expect("exponent is negative here");
        sink.put(b"0.");
        sink.put_zeros(leading);
        sink.put(trimmed(digits, alternate));
    }
}

/// `%g`/`%G` body (no sign, no padding) of a finite nonnegative magnitude,
/// lowercase.
pub(crate) fn general(magnitude: f64, precision: usize, alternate: bool) -> Vec<u8> {
    let mut out = Vec::new();
    write_general(&mut out, magnitude, precision, alternate);
    out
}

/// `%e`/`%E` body (no sign, no padding) of a finite nonnegative magnitude,
/// lowercase.
pub(crate) fn scientific(magnitude: f64, precision: usize, alternate: bool) -> Vec<u8> {
    let decimal = decimal(magnitude, precision + 1);
    let digits = decimal.digits();
    let mut out = Vec::with_capacity(precision + 7);
    out.put(&digits[..1]);
    if precision > 0 || alternate {
        out.put(b".");
    }
    out.put(&digits[1..]);
    put_exponent(&mut out, decimal.exponent);
    out
}
