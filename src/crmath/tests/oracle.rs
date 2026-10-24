//! A certified reference for the kernels: IEEE special values by the C99
//! Annex F rules, everything else by an interval Ziv procedure over `libbeef`
//! primitives in directed rounding.
//!
//! For an input, compute at precision p a lower bound `lo` and an upper bound
//! `hi` of the exact result, each from primitives rounded toward the side that
//! keeps the bound valid. If `lo` and `hi` round to the same binary64 value,
//! the exact result (which lies between them) rounds to it too. Otherwise
//! double p, up to a cap; reaching the cap is reported, never guessed. `pow`
//! can be exactly a rounding midpoint, which no enclosure can resolve, so it
//! gets an exact rational check of the candidate midpoint.
//!
//! The trust base is `libbeef`'s directed-mode correct rounding of single
//! primitives and its exact arithmetic. Its `pow` is deliberately avoided: a
//! shortcut for power-of-two bases truncates the exponent (4^0.25 gives 1).

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

    /// The port under test. `atan2` takes `(y, x)` in that order.
    pub(crate) fn port(self, x: f64, y: f64) -> f64 {
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

/// Round an exact big-float value to binary64, nearest even, through libbf's
/// rounding to the binary64 format (its direct `to_f64` mishandles the
/// bottom of the subnormal range).
fn to_f64(value: &BigFloat) -> f64 {
    value
        .round(BigFormat::BINARY64)
        .to_f64(Rounding::NearestEven)
}

/// floor(pi * 2^1700) in hexadecimal.
const PI_TIMES_2_1700_HEX: &str = concat!(
    "3243f6a8885a308d313198a2e03707344a4093822299f31d0082efa98ec4e6c89452821e638d0137",
    "7be5466cf34e90c6cc0ac29b7c97c50dd3f84d5b5b54709179216d5d98979fb1bd1310ba698dfb5a",
    "c2ffd72dbd01adfb7b8e1afed6a267e96ba7c9045f12c7f9924a19947b3916cf70801f2e2858efc1",
    "6636920d871574e69a458fea3f4933d7e0d95748f728eb658718bcd5882154aee7b54a41dc25a59b",
    "59c30d5392af26013c5d1b023286085f0ca417918b8db38ef8e79dcb0603a180e6c9e0e8bb01e8a3",
    "ed71577c1bd314b2778af2fda5",
);

const DOWN: Rounding = Rounding::TowardNegative;
const UP: Rounding = Rounding::TowardPositive;

/// libbeef's pi primitive returns zero from 688 bits upward. This cap bounds
/// only the oracle's explicit Ziv loop; libbeef primitives can escalate
/// internally beyond it. Even the externally reduced trig path still calls
/// pi-dependent primitives, so the oracle stops at the last safe outer step.
fn check_precision(f: Func, p: u64) -> Result<(), String> {
    let cap = match f {
        Func::Sin | Func::Cos | Func::Tan | Func::Asin | Func::Acos | Func::Atan | Func::Atan2 => {
            512
        }
        Func::Exp | Func::Ln | Func::Log10 | Func::Pow => 4096,
    };
    if p > cap {
        return Err(format!(
            "{} oracle precision {p} exceeds its safe cap of {cap} bits",
            f.name()
        ));
    }
    Ok(())
}

#[test]
fn pi_dependent_oracles_refuse_unsafe_precision() {
    for f in Func::ALL {
        assert!(check_precision(f, 512).is_ok());
        let pi_dependent = matches!(
            f,
            Func::Sin | Func::Cos | Func::Tan | Func::Asin | Func::Acos | Func::Atan | Func::Atan2
        );
        assert_eq!(check_precision(f, 1024).is_err(), pi_dependent);
        if pi_dependent {
            assert!(certify_at_precision(f, 0.5, 0.75, 1024).is_err());
        }
    }
}

/// Lower and upper bounds of the exact result at precision `p`.
fn enclose(f: Func, x: f64, y: f64, p: u64) -> (BigFloat, BigFloat) {
    let bx = BigFloat::from_f64(x);
    let direct = |g: &dyn Fn(&BigFloat, BigFormat) -> BigFloat| {
        (g(&bx, big_format(p, DOWN)), g(&bx, big_format(p, UP)))
    };
    match f {
        Func::Sin | Func::Cos | Func::Tan if x.abs() > 1e3 => trig_reduced(f, x, p),
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
            // |x|^y = exp(y * log|x|); exp is increasing.
            let ax = BigFloat::from_f64(x.abs());
            let l_lo = ax.log(big_format(p, DOWN));
            let l_hi = ax.log(big_format(p, UP));
            let by = BigFloat::from_f64(y);
            let (z_lo, z_hi) = if y >= 0.0 {
                (by.mul(&l_lo, big_format(p, DOWN)), by.mul(&l_hi, big_format(p, UP)))
            } else {
                (by.mul(&l_hi, big_format(p, DOWN)), by.mul(&l_lo, big_format(p, UP)))
            };
            // As for exp: far outside +-1e4 the bound is an overflow or an
            // underflow, beyond the big-float exponent range.
            let limit = BigFloat::from_i64(10_000);
            let exp_bound = |z: &BigFloat, rounding| {
                if z.cmp_num(&limit) == Some(std::cmp::Ordering::Greater) {
                    BigFloat::from_f64(f64::INFINITY)
                } else if z.cmp_num(&limit.neg()) == Some(std::cmp::Ordering::Less) {
                    BigFloat::from_f64(0.0)
                } else {
                    z.exp(big_format(p, rounding))
                }
            };
            let lo = exp_bound(&z_lo, DOWN);
            let hi = exp_bound(&z_hi, UP);
            if x < 0.0 && is_odd_integer(y) {
                (hi.neg(), lo.neg())
            } else {
                (lo, hi)
            }
        }
    }
}

/// Enclosure of sin, cos or tan for large |x|, with the argument reduction
/// done here: libbeef's own reduction does not converge for arguments near
/// the top of the binary64 range. For an integer k, r = |x| - 2 pi k lies in
/// [r_lo, r_hi] computed from directed roundings of 2 pi at a precision
/// covering x's exponent; sin and cos are 1-Lipschitz, so evaluating at r_lo
/// and widening by r_hi - r_lo encloses them, and tan is the quotient of
/// those enclosures.
fn trig_reduced(f: Func, x: f64, p: u64) -> (BigFloat, BigFloat) {
    let ax = BigFloat::from_f64(x.abs());
    let precision = p + 1024 + 128;
    // libbeef's own pi comes back as 0 (and its atan at high precision with
    // it), so pi enters as a constant: floor(pi * 2^1700), computed with
    // Machin's formula in integer arithmetic and checked against the known
    // hex expansion; the bounds allow one unit of truncation error.
    let pi_units = BigFloat::parse_integer_radix(PI_TIMES_2_1700_HEX, 16)
        .expect("the pi constant is valid hex");
    let unit = |delta: i64| {
        pi_units
            .add(&BigFloat::from_i64(delta), EXACT)
            .mul_exp2(1 - 1700)
    };
    let two_pi_lo = unit(-1);
    let two_pi_hi = unit(2);
    let k = ax
        .div(&two_pi_hi, big_format(precision, DOWN))
        .rint(Rounding::TowardZero);
    let r_lo = ax.sub(
        &k.mul(&two_pi_hi, big_format(precision, UP)),
        big_format(precision, DOWN),
    );
    let r_hi = ax.sub(
        &k.mul(&two_pi_lo, big_format(precision, DOWN)),
        big_format(precision, UP),
    );
    let width = r_hi.sub(&r_lo, big_format(p, UP));
    let widen = |g: &dyn Fn(&BigFloat, BigFormat) -> BigFloat| {
        (
            g(&r_lo, big_format(p, DOWN)).sub(&width, big_format(p, DOWN)),
            g(&r_lo, big_format(p, UP)).add(&width, big_format(p, UP)),
        )
    };
    let (s_lo, s_hi) = widen(&|a, fm| a.sin(fm));
    let negative = x < 0.0;
    match f {
        Func::Sin => {
            if negative {
                (s_hi.neg(), s_lo.neg())
            } else {
                (s_lo, s_hi)
            }
        }
        Func::Cos => widen(&|a, fm| a.cos(fm)),
        _ => {
            let (c_lo, c_hi) = widen(&|a, fm| a.cos(fm));
            // The cosine enclosure must not contain zero; otherwise report
            // an unbounded enclosure, which fails the test loudly.
            let positive = !c_lo.sign().is_negative() && !c_lo.is_zero();
            let negative_c = c_hi.sign().is_negative() && !c_hi.is_zero();
            if !(positive || negative_c) {
                return (
                    BigFloat::from_f64(f64::NEG_INFINITY),
                    BigFloat::from_f64(f64::INFINITY),
                );
            }
            let candidates = |rounding| {
                [
                    s_lo.div(&c_lo, big_format(p, rounding)),
                    s_lo.div(&c_hi, big_format(p, rounding)),
                    s_hi.div(&c_lo, big_format(p, rounding)),
                    s_hi.div(&c_hi, big_format(p, rounding)),
                ]
            };
            let pick = |values: [BigFloat; 4], want_max: bool| {
                values
                    .into_iter()
                    .reduce(|best, v| {
                        let greater = v.cmp_num(&best) == Some(std::cmp::Ordering::Greater);
                        if greater == want_max { v } else { best }
                    })
                    .expect("four candidates")
            };
            let lo = pick(candidates(DOWN), false);
            let hi = pick(candidates(UP), true);
            if negative {
                (hi.neg(), lo.neg())
            } else {
                (lo, hi)
            }
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

/// Whether `|x|^y` equals `m` exactly, for `y = n / 2^k` with small `k`:
/// checks `m^(2^k) == |x|^n` (or `m^(2^k) * |x|^-n == 1`) in exact
/// arithmetic.
fn pow_is_exactly(x: f64, y: f64, m: &BigFloat) -> bool {
    let mut k = 0u32;
    let mut scaled = y;
    while !is_integer(scaled) {
        scaled *= 2.0;
        k += 1;
        if k > 12 {
            return false;
        }
    }
    if scaled.abs() > 4096.0 {
        return false;
    }
    let n = scaled as i64;
    let power = |base: &BigFloat, mut e: u64| {
        let mut result = BigFloat::from_i64(1);
        let mut b = base.clone();
        while e > 0 {
            if e & 1 == 1 {
                result = result.mul(&b, EXACT);
            }
            b = b.mul(&b, EXACT);
            e >>= 1;
        }
        result
    };
    let mut lhs = m.abs();
    for _ in 0..k {
        lhs = lhs.mul(&lhs, EXACT);
    }
    let ax = BigFloat::from_f64(x.abs());
    let xn = power(&ax, n.unsigned_abs());
    if n >= 0 {
        lhs.cmp_num(&xn) == Some(std::cmp::Ordering::Equal)
    } else {
        lhs.mul(&xn, EXACT).cmp_num(&BigFloat::from_i64(1)) == Some(std::cmp::Ordering::Equal)
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
        check_precision(f, p)?;
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
        if f == Func::Pow && a.is_finite() && b.is_finite() {
            let (small, large) = if a.abs() <= b.abs() { (a, b) } else { (b, a) };
            let adjacent = small.is_sign_negative() == large.is_sign_negative()
                && small.abs().to_bits() + 1 == large.abs().to_bits();
            if adjacent {
                // A candidate midpoint between two adjacent results.
                let m = BigFloat::from_f64(a)
                    .add(&BigFloat::from_f64(b), EXACT)
                    .mul_exp2(-1);
                if pow_is_exactly(x, y, &m) {
                    return Ok(to_f64(&m));
                }
            }
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
fn parse_hex_float(body: &str) -> f64 {
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
