//! Numeric formatting cases cover NaN sign through `string.format`, `%q` on
//! integral numbers, and `tonumber(s, base)` whitespace.

use crate::support;

#[test]
fn nan_formats_unsigned_whatever_its_sign_bit() {
    // Literal `0/0` (NaN results are not folded), variable-based `0/0`,
    // `sqrt(-1)` and an explicitly negated NaN all print the same on every host.
    assert_eq!(
        support::eval_bytes(
            r#"local z = 0
            local n = z / z
            return string.format("%f|%e|%g|%a|%E|%G|%+f|% f",
                0/0, n, math.sqrt(-1), -n, -(0/0), n, n, n)"#
        ),
        b"nan|nan|nan|nan|NAN|NAN|+nan| nan"
    );
    assert_eq!(
        support::eval_bytes(r#"return string.format("[%6f]|[%-6f]", 0/0, -(0/0))"#),
        b"[   nan]|[nan   ]"
    );
}

#[test]
fn quote_prints_integral_numbers_in_decimal() {
    assert_eq!(
        support::eval_bytes(r#"return string.format("%q|%q|%q|%q|%q", 1, 0, -42, 1e15, 2^53)"#),
        b"1|0|-42|1000000000000000|9007199254740992"
    );
    assert_eq!(
        support::eval_bytes(r#"return string.format("%q|%q", -2^63, 2^63)"#),
        b"-9223372036854775808|0x1p+63"
    );
    // Non-integral values and -0.0 keep the hex form.
    assert_eq!(
        support::eval_bytes(r#"return string.format("%q|%q|%q", 1.5, -0.0, 1e300)"#),
        b"0x1.8p+0|-0x0p+0|0x1.7e43c8800759cp+996"
    );
}

#[test]
fn tonumber_with_base_trims_vertical_tab() {
    assert_eq!(
        support::eval_bytes(
            r#"return tostring(tonumber("10\v", 16)) .. "|" .. tostring(tonumber("\v\f\t\r\n 10 \v", 16))
                .. "|" .. tostring(tonumber("\v", 16)) .. "|" .. tostring(tonumber("10\v"))"#
        ),
        b"16|16|nil|10"
    );
}
