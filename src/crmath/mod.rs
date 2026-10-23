//! Deterministic, correctly rounded binary64 elementary functions.
//!
//! Every transcendental result a script can observe (`math.sin` and friends,
//! and the `^` operator) is computed here, never by the platform `libm` or by
//! compiler-builtins. The kernels are Rust ports of the CORE-MATH binary64
//! implementations (<https://core-math.gitlabpages.inria.fr/>, MIT licence,
//! see `LICENSE-CORE-MATH` beside this file), specialised to
//! round-to-nearest-even with the `errno`, exception-flag and directed-rounding
//! code removed.
//!
//! Two properties, in order of importance:
//!
//! 1. **Bit-identical results on every supported target.** Each kernel is a
//!    fixed sequence of operations whose rounded results IEEE 754 specifies
//!    exactly: binary64 `+ - * /` and `sqrt`, integer and bit manipulation,
//!    and fused multiply-add through `fma`. Rust never contracts `a * b + c`
//!    into an FMA and never reassociates, so the sequence the source spells is
//!    the sequence that runs. `fma` is either the hardware instruction or an
//!    in-crate software implementation; both are the correctly rounded IEEE
//!    operation, so they agree bit for bit. This property does not depend on
//!    the kernels being correctly rounded.
//! 2. **Correct rounding.** CORE-MATH certifies the rounding of each result.
//!    For `pow` and `atan2` the final accurate phase can, in principle, fail to
//!    certify (no binary64 input is known to reach that branch); the port then
//!    returns the round-to-nearest of its fixed approximation, which is still
//!    deterministic but may differ from the correctly rounded value. A future
//!    correction of such a case would be replay-visible.
//!
//! The supported-target contract: binary64 arithmetic with gradual underflow
//! and the default floating-point environment (round to nearest, no
//! flush-to-zero), which is also what Rust itself assumes. x87-only targets
//! (i586 and older without SSE2) evaluate with excess range and are out of
//! scope. NaN sign and payload bits are not part of the contract: they never
//! affect a non-NaN result, and the VM never lets a script observe them.

// Exact floating-point comparisons are the algorithms here: rounding tests
// compare two bounds for bit equality and special-case dispatch matches exact
// constants.
#![allow(clippy::float_cmp)]

/// A hexadecimal float literal, evaluated at compile time by [`hf`].
macro_rules! h {
    ($literal:literal) => {
        const { $crate::crmath::hf($literal) }
    };
}

mod acos;
mod asin;
mod atan;
mod atan2;
mod cos;
mod dint;
mod exp;
mod log;
mod log10;
mod pow;
mod qint;
mod sin;
mod tables;
mod tan;
mod trig_dint;

#[cfg(test)]
mod tests;

pub(crate) use acos::acos;
pub(crate) use asin::asin;
pub(crate) use atan::atan;
pub(crate) use atan2::atan2;
pub(crate) use cos::cos;
pub(crate) use exp::exp;
pub(crate) use log::ln;
pub(crate) use log10::log10;
pub(crate) use pow::pow;
pub(crate) use sin::sin;
pub(crate) use tan::tan;

/// Parses a C99 hexadecimal floating-point literal at compile time.
///
/// Rust has no hex float literals, and CORE-MATH's tables are written in
/// them. Keeping the original spelling makes every constant checkable against
/// the upstream source by eye. The literal must be exactly representable as a
/// binary64 value (normal or subnormal); anything else is a compile error,
/// which catches a mistyped digit that would otherwise round silently.
pub(crate) const fn hf(literal: &str) -> f64 {
    let bytes = literal.as_bytes();
    let mut i = 0;
    let mut negative = false;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        negative = bytes[i] == b'-';
        i += 1;
    }
    assert!(
        i + 1 < bytes.len() && bytes[i] == b'0' && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X'),
        "hex float literal must start with 0x"
    );
    i += 2;
    let mut mantissa: u128 = 0;
    let mut exponent: i64 = 0;
    let mut seen_point = false;
    let mut digits = 0;
    while i < bytes.len() && bytes[i] != b'p' && bytes[i] != b'P' {
        let c = bytes[i];
        if c == b'.' {
            assert!(!seen_point, "two radix points in hex float literal");
            seen_point = true;
        } else {
            let value = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => panic!("invalid hex digit in hex float literal"),
            };
            assert!(
                mantissa >> 120 == 0,
                "hex float literal has too many digits"
            );
            mantissa = (mantissa << 4) | value as u128;
            digits += 1;
            if seen_point {
                exponent -= 4;
            }
        }
        i += 1;
    }
    assert!(digits > 0, "hex float literal has no digits");
    assert!(i < bytes.len(), "hex float literal needs a binary exponent");
    i += 1;
    let mut exp_negative = false;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        exp_negative = bytes[i] == b'-';
        i += 1;
    }
    assert!(i < bytes.len(), "hex float literal has an empty exponent");
    let mut binary_exponent: i64 = 0;
    while i < bytes.len() {
        let c = bytes[i];
        assert!(
            c.is_ascii_digit(),
            "invalid exponent digit in hex float literal"
        );
        binary_exponent = binary_exponent * 10 + (c - b'0') as i64;
        assert!(binary_exponent < 100_000, "hex float exponent out of range");
        i += 1;
    }
    exponent += if exp_negative {
        -binary_exponent
    } else {
        binary_exponent
    };
    let sign = if negative { 1u64 << 63 } else { 0 };
    if mantissa == 0 {
        return f64::from_bits(sign);
    }
    // value = mantissa * 2^exponent with an odd mantissa after this loop.
    while mantissa & 1 == 0 {
        mantissa >>= 1;
        exponent += 1;
    }
    let width = 128 - mantissa.leading_zeros() as i64;
    assert!(
        width <= 53,
        "hex float literal is not exactly representable"
    );
    let top = exponent + width - 1;
    assert!(top <= 1023, "hex float literal overflows binary64");
    let bits = if top >= -1022 {
        let fraction = (mantissa << (52 - (width - 1))) as u64 & ((1u64 << 52) - 1);
        (((top + 1023) as u64) << 52) | fraction
    } else {
        assert!(
            exponent >= -1074,
            "hex float literal is below the subnormal grid"
        );
        (mantissa << (exponent + 1074)) as u64
    };
    f64::from_bits(sign | bits)
}

/// Builds a `u128` from its low and high 64-bit halves, in CORE-MATH's
/// `U128(l, h)` argument order.
pub(crate) const fn u128_lh(low: u64, high: u64) -> u128 {
    ((high as u128) << 64) | low as u128
}

// Select once at kernel entry. The instantiated helpers share the selected
// target feature, so hardware FMA calls can inline within the whole kernel.
#[cfg(any(
    target_arch = "x86",
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_feature = "neon")
))]
#[inline]
fn kernel_hardware_available() -> bool {
    #[cfg(test)]
    match tests::forced_fma() {
        tests::ForcedFma::Software => return false,
        tests::ForcedFma::Hardware => {
            assert!(hardware_fma(1.0, 1.0, 1.0).is_some());
            return true;
        }
        tests::ForcedFma::Default => {}
    }
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        cfg!(target_feature = "fma") || std::arch::is_x86_feature_detected!("fma")
    }
    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    {
        true
    }
}

/// Fused multiply-add: `a * b + c` with a single rounding.
///
/// Kernels bind their FMA directly in each backend instead of calling this
/// standalone dispatcher; keeping it here also tests independent FMA calls.
///
/// On x86 the `vfmadd` instruction when the CPU has it (known at compile time
/// when the build targets FMA, otherwise detected at run time), on aarch64 the
/// always-present `fmadd`, and everywhere else [`soft_fma`]. All of them
/// return the same bits for every input (NaN payloads aside), so the choice
/// never shows in results. `f64::mul_add` is avoided wherever LLVM could lower
/// it to a library call: that call reaches whichever `fma` symbol the link
/// resolves, and compiler-builtins' generic one is not correctly rounded
/// (rust-lang/compiler-builtins issue 1353).
#[cfg(test)]
#[inline(always)]
pub(crate) fn fma(a: f64, b: f64, c: f64) -> f64 {
    match tests::forced_fma() {
        tests::ForcedFma::Software => return soft_fma(a, b, c),
        tests::ForcedFma::Hardware => return hardware_fma_checked(a, b, c),
        tests::ForcedFma::Default => {}
    }
    fma_dispatch(a, b, c)
}

#[cfg(all(test, any(target_arch = "x86", target_arch = "x86_64")))]
#[inline(always)]
fn fma_dispatch(a: f64, b: f64, c: f64) -> f64 {
    // With the feature enabled at compile time the detection folds away and
    // the intrinsic inlines; otherwise std caches the CPUID result.
    if cfg!(target_feature = "fma") || std::arch::is_x86_feature_detected!("fma") {
        // SAFETY: the CPU supports the `fma` target feature, checked above.
        unsafe { x86_fma(a, b, c) }
    } else {
        soft_fma(a, b, c)
    }
}

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
#[inline(always)]
fn fma_dispatch(a: f64, b: f64, c: f64) -> f64 {
    // Every aarch64 target with floating point has `fmadd`, so this lowers to
    // the instruction and never to a library call.
    #[allow(clippy::disallowed_methods)]
    a.mul_add(b, c)
}

#[cfg(all(
    test,
    not(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        target_arch = "x86",
        target_arch = "x86_64"
    ))
))]
#[inline(always)]
fn fma_dispatch(a: f64, b: f64, c: f64) -> f64 {
    soft_fma(a, b, c)
}

/// The x86 `vfmadd` instruction.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "fma")]
#[inline]
fn x86_fma(a: f64, b: f64, c: f64) -> f64 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::{_mm_cvtsd_f64, _mm_fmadd_sd, _mm_set_sd};
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::{_mm_cvtsd_f64, _mm_fmadd_sd, _mm_set_sd};
    _mm_cvtsd_f64(_mm_fmadd_sd(_mm_set_sd(a), _mm_set_sd(b), _mm_set_sd(c)))
}

/// The hardware instruction for tests that force that path, or `None` when
/// this host cannot run it.
#[cfg(test)]
pub(crate) fn hardware_fma(a: f64, b: f64, c: f64) -> Option<f64> {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::arch::is_x86_feature_detected!("fma") {
            // SAFETY: the CPU supports the `fma` target feature, checked above.
            Some(unsafe { x86_fma(a, b, c) })
        } else {
            None
        }
    }
    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    {
        Some(fma_dispatch(a, b, c))
    }
    #[cfg(not(any(
        all(target_arch = "aarch64", target_feature = "neon"),
        target_arch = "x86",
        target_arch = "x86_64"
    )))]
    {
        let _ = (a, b, c);
        None
    }
}

#[cfg(test)]
fn hardware_fma_checked(a: f64, b: f64, c: f64) -> f64 {
    hardware_fma(a, b, c).expect("tests force the hardware FMA only on hosts that have it")
}

/// Correctly rounded fused multiply-add in integer arithmetic.
///
/// Returns exactly what IEEE 754 `fusedMultiplyAdd` returns in
/// round-to-nearest-even, including signed zeros, subnormal results, and
/// products that overflow binary64 on their own but not after adding `c`.
/// NaN payloads may differ from the hardware instruction's. Every kernel has
/// a software instance built on it; aarch64 compiles that instance but
/// selects it only in tests.
pub(crate) fn soft_fma(a: f64, b: f64, c: f64) -> f64 {
    let a_bits = a.to_bits();
    let b_bits = b.to_bits();
    let c_bits = c.to_bits();
    let a_exp = (a_bits >> 52) & 0x7ff;
    let b_exp = (b_bits >> 52) & 0x7ff;
    let c_exp = (c_bits >> 52) & 0x7ff;
    // Infinite or NaN operands, and zero factors: the product is exact (a
    // signed zero, an infinity or NaN) or irrelevant, so ordinary arithmetic
    // gives the IEEE result, including `0 * inf = NaN` and `inf - inf = NaN`.
    if a_exp == 0x7ff || b_exp == 0x7ff || a == 0.0 || b == 0.0 {
        return a * b + c;
    }
    // Finite nonzero factors with an infinite or NaN addend: the exact product
    // is finite, so the addend decides. `a * b + c` would be wrong when the
    // rounded product overflows to an infinity of the other sign.
    if c_exp == 0x7ff {
        return c + c;
    }
    // A zero addend: the result is the product rounded once. Adding the zero
    // afterwards would turn an underflowed `-0` product into `+0`.
    if c == 0.0 {
        return a * b;
    }

    // value = significand * 2^exponent for each finite nonzero operand.
    fn split(bits: u64, biased: u64) -> (u128, i32) {
        let fraction = bits & ((1u64 << 52) - 1);
        if biased == 0 {
            (fraction as u128, -1074)
        } else {
            ((fraction | (1u64 << 52)) as u128, biased as i32 - 1075)
        }
    }
    let (ma, ea) = split(a_bits, a_exp);
    let (mb, eb) = split(b_bits, b_exp);
    let (mc, ec) = split(c_bits, c_exp);
    let product_negative = ((a_bits ^ b_bits) >> 63) != 0;
    let addend_negative = (c_bits >> 63) != 0;

    // Normalise both terms so the leading one sits at bit 125: two bits of
    // headroom for the carry of an addition, and every shift here is exact
    // (the product has at most 106 bits, the addend at most 53).
    let mut p = ma * mb;
    let mut ep = ea + eb;
    let shift = p.leading_zeros() as i32 - 2;
    p <<= shift;
    ep -= shift;
    let mut q = mc;
    let mut eq = ec;
    let shift = q.leading_zeros() as i32 - 2;
    q <<= shift;
    eq -= shift;

    // Align the smaller-exponent term to the larger, folding every bit shifted
    // out into a sticky bit at position 0. Deep cancellation needs the two
    // exponents within one of each other, and such a shift drops only zero
    // bits (both terms have at least 19 trailing zeros), so the sticky bit
    // never meets a cancellation; otherwise it sits at least 70 bits below the
    // rounding position, where it only breaks ties, which is its job.
    fn shift_right_sticky(value: u128, distance: i32) -> u128 {
        if distance == 0 {
            value
        } else if distance >= 128 {
            u128::from(value != 0)
        } else {
            let lost = value & ((1u128 << distance) - 1);
            (value >> distance) | u128::from(lost != 0)
        }
    }
    let (big, big_negative, small, small_negative, exponent) = if ep >= eq {
        (
            p,
            product_negative,
            shift_right_sticky(q, ep - eq),
            addend_negative,
            ep,
        )
    } else {
        (
            q,
            addend_negative,
            shift_right_sticky(p, eq - ep),
            product_negative,
            eq,
        )
    };
    let (sum, negative) = if big_negative == small_negative {
        (big + small, big_negative)
    } else if big >= small {
        (big - small, big_negative)
    } else {
        (small - big, small_negative)
    };
    if sum == 0 {
        // Exact cancellation of nonzero terms is +0 in round-to-nearest.
        return 0.0;
    }

    // Round sum * 2^exponent to 53 significant bits, or to the 2^-1074 grid
    // when the result is subnormal.
    let msb = 127 - sum.leading_zeros() as i32;
    let top = msb + exponent;
    let ulp = if top - 52 > -1074 { top - 52 } else { -1074 };
    let shift = ulp - exponent;
    let significand: u128 = if shift <= 0 {
        sum << (-shift)
    } else if shift >= 128 {
        // Unreachable for finite inputs (the sum's leading bit is at most 126
        // positions above the grid), kept total for safety.
        0
    } else {
        let kept = sum >> shift;
        let rest = sum & ((1u128 << shift) - 1);
        let half = 1u128 << (shift - 1);
        if rest > half || (rest == half && kept & 1 == 1) {
            kept + 1
        } else {
            kept
        }
    };
    let sign = if negative { 1u64 << 63 } else { 0 };
    if significand == 0 {
        return f64::from_bits(sign);
    }
    // For a significand in [2^52, 2^53] at grid exponent `ulp`, the encoding is
    // ((ulp + 1074) << 52) + significand: the hidden bit carries into the
    // exponent field, a rounding carry to 2^53 bumps the exponent, and on the
    // subnormal grid (ulp = -1074) it is the significand itself.
    let encoded = (((ulp + 1074) as u128) << 52) + significand;
    if encoded >= 0x7ff_u128 << 52 {
        return f64::from_bits(sign | (0x7ffu64 << 52));
    }
    f64::from_bits(sign | encoded as u64)
}

/// Round to the nearest integer, ties to even, for finite `x`.
///
/// Values of magnitude 2^52 and above are already integers. Below that,
/// adding and subtracting 2^52 with the sign of `x` rounds in the default
/// mode, which is ties-to-even. Signed zeros survive: `-0.4` gives `-0`.
#[inline(always)]
pub(crate) fn roundeven_finite(x: f64) -> f64 {
    const TWO52: f64 = 4_503_599_627_370_496.0;
    let ax = x.abs();
    if ax >= TWO52 {
        return x;
    }
    let r = (ax + TWO52) - TWO52;
    if x.is_sign_negative() { -r } else { r }
}

/// `x` with the sign bit of `s`.
#[inline(always)]
pub(crate) fn copysign(x: f64, s: f64) -> f64 {
    f64::from_bits((x.to_bits() & !(1u64 << 63)) | (s.to_bits() & (1u64 << 63)))
}

/// `|x|`.
#[inline(always)]
pub(crate) fn fabs(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & !(1u64 << 63))
}

/// Fast2Sum: `(s, e)` with `s = RN(x + y)` and, when the exponent of `x` is
/// at least that of `y`, `s + e = x + y` exactly.
#[inline(always)]
pub(crate) fn fasttwosum(x: f64, y: f64) -> (f64, f64) {
    let s = x + y;
    let z = s - x;
    (s, y - z)
}

/// Reinterprets the bits of an `f64` as `u64`.
#[inline(always)]
pub(crate) fn bits(x: f64) -> u64 {
    x.to_bits()
}

/// Reinterprets a `u64` as an `f64`.
#[inline(always)]
pub(crate) fn from_bits(u: u64) -> f64 {
    f64::from_bits(u)
}
