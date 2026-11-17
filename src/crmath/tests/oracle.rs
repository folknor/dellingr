//! A certified reference for the kernels: IEEE special values by the C99
//! Annex F rules, everything else by an interval Ziv procedure over `libbeef`
//! primitives in directed rounding.
//!
//! For an input, compute at precision p a lower bound `lo` and an upper bound
//! `hi` of the exact result, each from primitives rounded toward the side that
//! keeps the bound valid. If `lo` and `hi` round to the same binary64 value,
//! the exact result (which lies between them) rounds to it too. Otherwise
//! double p, up to a cap; reaching the cap is reported, never guessed. Every
//! function but `log10` is one primitive, so an exact result (a `pow` that
//! lands on a rounding midpoint, say) comes back as itself from both
//! directions and rounds once. `log10`, a quotient of two logs, is exact only
//! at powers of ten, where the result is a small integer that both bounds
//! round to.
//!
//! The trust base is `libbeef`'s directed-mode correct rounding of single
//! primitives and its exact arithmetic.

use crate::crmath as crm;
use libbeef::{BigFloat, BigFormat, ExpBits, Precision, Rounding};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Func {
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Exp,
    Ln,
    Log10,
    Pow,
}

impl Func {
    pub(crate) const ALL: [Self; 11] = [
        Self::Sin,
        Self::Cos,
        Self::Tan,
        Self::Asin,
        Self::Acos,
        Self::Atan,
        Self::Atan2,
        Self::Exp,
        Self::Ln,
        Self::Log10,
        Self::Pow,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Sin => "sin",
            Self::Cos => "cos",
            Self::Tan => "tan",
            Self::Asin => "asin",
            Self::Acos => "acos",
            Self::Atan => "atan",
            Self::Atan2 => "atan2",
            Self::Exp => "exp",
            Self::Ln => "log",
            Self::Log10 => "log10",
            Self::Pow => "pow",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.name() == name)
    }

    pub(crate) fn binary(self) -> bool {
        matches!(self, Self::Atan2 | Self::Pow)
    }

    /// The port under test. `atan2` takes `(y, x)` in that order. Panics if
    /// the call rounded an approximation its own error bound could not
    /// certify (where upstream aborts): the result may still match the
    /// oracle, but the input is the first known witness and must be reported.
    /// Outside test builds (the regeneration tool) there is no record.
    pub(crate) fn port(self, x: f64, y: f64) -> f64 {
        #[cfg(test)]
        let _ = super::take_uncertified_rounding();
        let result = self.port_unchecked(x, y);
        #[cfg(test)]
        assert!(
            !super::take_uncertified_rounding(),
            "{} rounded without certification at x={x:e} ({:#018x}), y={y:e} ({:#018x}), \
             FMA mode {:?}",
            self.name(),
            x.to_bits(),
            y.to_bits(),
            super::forced_fma(),
        );
        result
    }

    fn port_unchecked(self, x: f64, y: f64) -> f64 {
        match self {
            Self::Sin => crm::sin(x),
            Self::Cos => crm::cos(x),
            Self::Tan => crm::tan(x),
            Self::Asin => crm::asin(x),
            Self::Acos => crm::acos(x),
            Self::Atan => crm::atan(x),
            Self::Atan2 => crm::atan2(x, y),
            Self::Exp => crm::exp(x),
            Self::Ln => crm::ln(x),
            Self::Log10 => crm::log10(x),
            Self::Pow => crm::pow(x, y),
        }
    }
}

const PI: f64 = crm::hf("0x1.921fb54442d18p+1");
const PI_2: f64 = crm::hf("0x1.921fb54442d18p+0");
const PI_4: f64 = crm::hf("0x1.921fb54442d18p-1");
const PI_3_4: f64 = crm::hf("0x1.2d97c7f3321d2p+1");

fn is_integer(y: f64) -> bool {
    y.is_finite() && y.trunc() == y
}

fn is_odd_integer(y: f64) -> bool {
    is_integer(y) && y.abs() < 9_007_199_254_740_992.0 && (y as i64) % 2 != 0
}

/// The C99 Annex F result for inputs outside the oracle's domain (zeros,
/// infinities, NaNs and domain errors), with every NaN treated as quiet.
/// `None` means the input is ordinary and goes to the Ziv procedure.
pub(crate) fn special(f: Func, x: f64, y: f64) -> Option<f64> {
    let nan = f64::NAN;
    match f {
        Func::Sin | Func::Tan => {
            if x.is_nan() || x.is_infinite() {
                Some(nan)
            } else if x == 0.0 {
                Some(x)
            } else {
                None
            }
        }
        Func::Cos => {
            if x.is_nan() || x.is_infinite() {
                Some(nan)
            } else if x == 0.0 {
                Some(1.0)
            } else {
                None
            }
        }
        Func::Asin => {
            if x.is_nan() || x.abs() > 1.0 {
                Some(nan)
            } else if x == 0.0 {
                Some(x)
            } else {
                None
            }
        }
        Func::Acos => {
            if x.is_nan() || x.abs() > 1.0 {
                Some(nan)
            } else if x == 1.0 {
                Some(0.0)
            } else {
                None
            }
        }
        Func::Atan => {
            if x.is_nan() {
                Some(nan)
            } else if x == 0.0 {
                Some(x)
            } else if x.is_infinite() {
                Some(PI_2.copysign(x))
            } else {
                None
            }
        }
        Func::Atan2 => {
            // f(x = y-coordinate, y = x-coordinate).
            let (yy, xx) = (x, y);
            if yy.is_nan() || xx.is_nan() {
                return Some(nan);
            }
            if yy == 0.0 {
                return Some(if xx > 0.0 || (xx == 0.0 && xx.is_sign_positive()) {
                    yy
                } else {
                    PI.copysign(yy)
                });
            }
            if yy.is_infinite() {
                return Some(if xx == f64::INFINITY {
                    PI_4.copysign(yy)
                } else if xx == f64::NEG_INFINITY {
                    PI_3_4.copysign(yy)
                } else {
                    PI_2.copysign(yy)
                });
            }
            if xx == 0.0 {
                return Some(PI_2.copysign(yy));
            }
            if xx == f64::INFINITY {
                return Some(0.0f64.copysign(yy));
            }
            if xx == f64::NEG_INFINITY {
                return Some(PI.copysign(yy));
            }
            None
        }
        Func::Exp => {
            // Beyond +-1e4 the result is far outside binary64 (it overflows
            // past e^709.79 and underflows to +0 below e^-745.14), and the
            // big-float exponent range cannot hold e^x for huge x.
            if x.is_nan() {
                Some(nan)
            } else if x == 0.0 {
                Some(1.0)
            } else if x > 1e4 {
                Some(f64::INFINITY)
            } else if x < -1e4 {
                Some(0.0)
            } else {
                None
            }
        }
        Func::Ln | Func::Log10 => {
            if x.is_nan() || x < 0.0 {
                Some(nan)
            } else if x == 0.0 {
                Some(f64::NEG_INFINITY)
            } else if x == f64::INFINITY {
                Some(x)
            } else if x == 1.0 {
                Some(0.0)
            } else {
                None
            }
        }
        Func::Pow => pow_special(x, y),
    }
}

fn pow_special(x: f64, y: f64) -> Option<f64> {
    let nan = f64::NAN;
    if y == 0.0 || x == 1.0 {
        return Some(1.0);
    }
    if x.is_nan() || y.is_nan() {
        return Some(nan);
    }
    if x == 0.0 {
        let odd = is_odd_integer(y);
        return Some(if y < 0.0 {
            if odd {
                f64::INFINITY.copysign(x)
            } else {
                f64::INFINITY
            }
        } else if odd {
            x
        } else {
            0.0
        });
    }
    if y.is_infinite() {
        let ax = x.abs();
        return Some(if ax == 1.0 {
            1.0
        } else if (ax < 1.0) == (y < 0.0) {
            f64::INFINITY
        } else {
            0.0
        });
    }
    if x == f64::NEG_INFINITY {
        let odd = is_odd_integer(y);
        return Some(if y < 0.0 {
            if odd { -0.0 } else { 0.0 }
        } else if odd {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    if x == f64::INFINITY {
        return Some(if y < 0.0 { 0.0 } else { f64::INFINITY });
    }
    if x < 0.0 && !is_integer(y) {
        return Some(nan);
    }
    None
}

fn big_format(precision: u64, rounding: Rounding) -> BigFormat {
    BigFormat {
        precision: Precision::Bits(precision),
        rounding,
        exp_bits: ExpBits::Max,
        subnormal: false,
        radix_point_precision: false,
    }
}

const EXACT: BigFormat = BigFormat {
    precision: Precision::Infinite,
    rounding: Rounding::NearestEven,
    exp_bits: ExpBits::Max,
    subnormal: false,
    radix_point_precision: false,
};

/// Round an exact big-float value to binary64, nearest even.
fn to_f64(value: &BigFloat) -> f64 {
    value.to_f64(Rounding::NearestEven)
}

const DOWN: Rounding = Rounding::TowardNegative;
const UP: Rounding = Rounding::TowardPositive;

/// Lower and upper bounds of the exact result at precision `p`.
fn enclose(f: Func, x: f64, y: f64, p: u64) -> (BigFloat, BigFloat) {
    let bx = BigFloat::from_f64(x);
    let direct = |g: &dyn Fn(&BigFloat, BigFormat) -> BigFloat| {
        (g(&bx, big_format(p, DOWN)), g(&bx, big_format(p, UP)))
    };
    match f {
        Func::Sin => direct(&|a, fm| a.sin(fm)),
        Func::Cos => direct(&|a, fm| a.cos(fm)),
        Func::Tan => direct(&|a, fm| a.tan(fm)),
        Func::Asin => direct(&|a, fm| a.asin(fm)),
        Func::Acos => direct(&|a, fm| a.acos(fm)),
        Func::Atan => direct(&|a, fm| a.atan(fm)),
        Func::Exp => direct(&|a, fm| a.exp(fm)),
        Func::Ln => direct(&|a, fm| a.log(fm)),
        Func::Atan2 => {
            let bxx = BigFloat::from_f64(y);
            (
                bx.atan2(&bxx, big_format(p, DOWN)),
                bx.atan2(&bxx, big_format(p, UP)),
            )
        }
        Func::Log10 => {
            // log10(x) = log(x) / log(10) with log(10) > 0.
            let l_lo = bx.log(big_format(p, DOWN));
            let l_hi = bx.log(big_format(p, UP));
            let ten = BigFloat::from_i64(10);
            let t_lo = ten.log(big_format(p, DOWN));
            let t_hi = ten.log(big_format(p, UP));
            let lo_den = if l_lo.sign().is_negative() {
                &t_lo
            } else {
                &t_hi
            };
            let hi_den = if l_hi.sign().is_negative() {
                &t_hi
            } else {
                &t_lo
            };
            (
                l_lo.div(lo_den, big_format(p, DOWN)),
                l_hi.div(hi_den, big_format(p, UP)),
            )
        }
        Func::Pow => {
            let by = BigFloat::from_f64(y);
            (
                bx.pow(&by, big_format(p, DOWN)),
                bx.pow(&by, big_format(p, UP)),
            )
        }
    }
}

/// The oracle against independently known correctly rounded values, so a
/// broken oracle cannot pass the sweeps vacuously.
#[test]
fn oracle_known_values() {
    let cases: [(Func, f64, f64, u64); 10] = [
        (Func::Sin, 1.0, 0.0, 0x3feaed548f090cee),
        (Func::Sin, 1e22, 0.0, 0xbfeb453ab76bf397),
        (Func::Cos, 1e22, 0.0, 0x3fe0be2cef01c8f4),
        (Func::Exp, 1.0, 0.0, 0x4005bf0a8b145769),
        (Func::Ln, 2.0, 0.0, 0x3fe62e42fefa39ef),
        (Func::Log10, 1000.0, 0.0, 0x4008000000000000),
        (Func::Pow, 2.0, 0.5, 0x3ff6a09e667f3bcd),
        (Func::Pow, 4.0, 0.25, 0x3ff6a09e667f3bcd),
        (Func::Atan, 1.0, 0.0, 0x3fe921fb54442d18),
        (Func::Atan2, 1.0, -1.0, 0x4002d97c7f3321d2),
    ];
    for (f, x, y, expected) in cases {
        let got = certify(f, x, y).unwrap_or_else(|message| panic!("{message}"));
        assert_eq!(got.to_bits(), expected, "{} {x} {y}", f.name());
    }
}

/// The correctly rounded result, or a description of why it could not be
/// certified.
pub(crate) fn certify(f: Func, x: f64, y: f64) -> Result<f64, String> {
    certify_at_precision(f, x, y, 128)
}

fn certify_at_precision(f: Func, x: f64, y: f64, mut p: u64) -> Result<f64, String> {
    if let Some(r) = special(f, x, y) {
        return Ok(r);
    }
    loop {
        let (lo, hi) = enclose(f, x, y, p);
        let a = to_f64(&lo);
        let b = to_f64(&hi);
        if a.is_nan() || b.is_nan() {
            return Err(format!(
                "{} oracle produced NaN bounds for x={x:e} ({:#x}) y={y:e} ({:#x})",
                f.name(),
                x.to_bits(),
                y.to_bits()
            ));
        }
        if a.to_bits() == b.to_bits() {
            return Ok(a);
        }
        if p >= 4096 {
            return Err(format!(
                "{} inconclusive at {p} bits for x={x:e} ({:#x}) y={y:e} ({:#x}): {a:e} vs {b:e}",
                f.name(),
                x.to_bits(),
                y.to_bits()
            ));
        }
        p *= 2;
    }
}

/// Parses one comma-separated field of CORE-MATH's worst-case files to the
/// value CORE-MATH's own checkers get from `sscanf("%la")`, for the shapes
/// those files use; it is not a general `%la` implementation. The first
/// whitespace-delimited word is the number and anything after it is an
/// annotation (sin.wc has lines like `0x1.4f1d73be27a31p+1 44`), and a hex
/// literal with more digits than binary64 holds is rounded to nearest even
/// (pow.wc has subnormal bases such as `0x1.08fa30093e7d8p-1074`). Returns
/// `None` only for an empty field; a shape it does not support panics, since
/// skipping it would silently drop a worst case.
pub(crate) fn parse_wc_token(token: &str) -> Option<f64> {
    let token = token.split_whitespace().next()?;
    let (negative, body) = match token.as_bytes().first() {
        Some(b'-') => (true, &token[1..]),
        Some(b'+') => (false, &token[1..]),
        _ => (false, token),
    };
    let value = match body {
        "nan" | "snan" => f64::NAN,
        "inf" => f64::INFINITY,
        _ if body.starts_with("0x") || body.starts_with("0X") => {
            if body.contains(['p', 'P']) {
                parse_hex_float(body)
            } else {
                u64::from_str_radix(&body[2..], 16).expect("worst-case integers are hex") as f64
            }
        }
        _ => body.parse::<f64>().expect("worst-case tokens are numbers"),
    };
    Some(if negative { -value } else { value })
}

/// A run-time hexadecimal float parser: `0x<hex>[.<hex>]p<decimal>`, rounded
/// to the nearest binary64, ties to even, as C's `%la` does. Panics on a
/// malformed literal.
pub(crate) fn parse_hex_float(body: &str) -> f64 {
    let (mantissa, exponent) = body[2..]
        .split_once(['p', 'P'])
        .expect("hex float literals have a binary exponent");
    let exponent: i64 = exponent.parse().expect("hex float exponents are decimal");
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{int_part}{frac_part}");
    let value = u128::from_str_radix(&digits, 16).expect("hex float digits are hex");
    let shift = exponent - 4 * frac_part.len() as i64;
    let exact = BigFloat::from_u64((value >> 64) as u64)
        .mul_exp2(64)
        .add(&BigFloat::from_u64(value as u64), EXACT)
        .mul_exp2(shift);
    to_f64(&exact)
}

#[test]
fn worst_case_tokens_read_like_c_la() {
    let bits = |token: &str| parse_wc_token(token).map(f64::to_bits);
    assert_eq!(bits("0x1.4f1d73be27a31p+1 44"), Some(0x4004_f1d7_3be2_7a31));
    assert_eq!(bits(" -0x1.8p-1 "), Some((-0.75f64).to_bits()));
    assert_eq!(bits("+inf"), Some(f64::INFINITY.to_bits()));
    assert_eq!(bits(""), None);
    assert_eq!(bits("   "), None);
    // Inexact subnormals round to nearest, ties to even.
    assert_eq!(bits("0x1.08fa30093e7d8p-1074"), Some(1));
    assert_eq!(bits("0x1.4p-1074"), Some(1));
    assert_eq!(bits("0x1.8p-1074"), Some(2));
    assert_eq!(bits("0x1.cp-1074"), Some(2));
    assert_eq!(bits("0x1p-1075"), Some(0));
    assert_eq!(bits("0x1.0000000000001p-1075"), Some(1));
    assert_eq!(bits("-0x1p-1076"), Some((-0.0f64).to_bits()));
    // Normal range: 53 significant bits, tie to even.
    assert_eq!(bits("0x1.00000000000008p+0"), Some(1.0f64.to_bits()));
    assert_eq!(bits("0x1.00000000000018p+0"), Some(0x3ff0_0000_0000_0002));
    assert_eq!(
        bits("0x1.fffffffffffff8p+1023"),
        Some(f64::INFINITY.to_bits())
    );
}
