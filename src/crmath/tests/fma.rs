//! The software FMA against the hardware instruction and against exact
//! big-float arithmetic.

use super::{TestRng, same};
use crate::crmath::{hardware_fma, soft_fma};
use libbeef::{BigFloat, BigFormat, ExpBits, Precision, Rounding};

/// `RN(a * b + c)` computed exactly with `libbeef` and rounded once.
fn exact_fma(a: f64, b: f64, c: f64) -> f64 {
    if !a.is_finite() || !b.is_finite() || !c.is_finite() {
        // IEEE special-value rules; the software FMA forwards these to the
        // ordinary operations, which the hardware check covers.
        return f64::NAN;
    }
    let exact = BigFormat {
        precision: Precision::Infinite,
        rounding: Rounding::NearestEven,
        exp_bits: ExpBits::Max,
        subnormal: false,
        radix_point_precision: false,
    };
    let product = BigFloat::from_f64(a).mul(&BigFloat::from_f64(b), exact);
    let sum = product.add(&BigFloat::from_f64(c), exact);
    if sum.is_zero() {
        // IEEE: an exact zero sum is +0 in round-to-nearest, except that
        // zero terms combine by the signed-zero rules of addition.
        return if product.is_zero() { a * b + c } else { 0.0 };
    }
    sum.round(BigFormat::BINARY64).to_f64(Rounding::NearestEven)
}

fn check(a: f64, b: f64, c: f64) {
    let soft = soft_fma(a, b, c);
    if let Some(hard) = hardware_fma(a, b, c) {
        assert!(
            same(soft, hard),
            "fma({a:e}, {b:e}, {c:e}): software {:#018x}, hardware {:#018x}",
            soft.to_bits(),
            hard.to_bits()
        );
    }
    if a.is_finite() && b.is_finite() && c.is_finite() {
        let exact = exact_fma(a, b, c);
        assert!(
            same(soft, exact),
            "fma({a:e}, {b:e}, {c:e}): software {:#018x}, exact {:#018x}",
            soft.to_bits(),
            exact.to_bits()
        );
    }
}

fn pow2(e: i32) -> f64 {
    if e >= -1022 {
        f64::from_bits(((e + 1023) as u64) << 52)
    } else {
        f64::from_bits(1u64 << (e + 1074))
    }
}

#[test]
fn special_values() {
    let specials = [
        0.0,
        -0.0,
        1.0,
        -1.0,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        f64::from_bits(1),
        -f64::from_bits(1),
        f64::MAX,
        -f64::MAX,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        3.0,
        0.1,
    ];
    for &a in &specials {
        for &b in &specials {
            for &c in &specials {
                check(a, b, c);
            }
        }
    }
}

#[test]
fn random_operands() {
    let mut rng = TestRng::new(0x5eed_f00d);
    for _ in 0..20_000 {
        check(rng.finite(), rng.finite(), rng.finite());
    }
    // Operands of comparable magnitude, where cancellation and carries live.
    for _ in 0..20_000 {
        let a = rng.in_range(-4.0, 4.0);
        let b = rng.in_range(-4.0, 4.0);
        let c = -(a * b) * rng.in_range(0.5, 2.0);
        check(a, b, c);
        check(a, b, -(a * b));
        check(a, b, (a * b) * pow2((rng.next_u64() % 200) as i32 - 100));
    }
}

#[test]
fn ties_and_near_ties() {
    // x * y = 1 + 2^-3k sits just above a tie of z = 2^53 + 2j (the shape of
    // rust-lang/compiler-builtins issue 1353), scaled across the range.
    let mut rng = TestRng::new(1353);
    for k in 18..=26 {
        let x = 1.0 + pow2(-k);
        let y = 1.0 - pow2(-k) + pow2(-2 * k);
        for _ in 0..400 {
            let j = (rng.next_u64() % 1000) as f64;
            let z = pow2(53) + 2.0 * j;
            let scale = pow2((rng.next_u64() % 1800) as i32 - 900);
            for (sx, sz) in [(1.0, 1.0), (-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0)] {
                check(x * scale, y, z * scale * sx * sz);
                check(sx * x, y * scale, sz * z * scale);
            }
        }
    }
    // Exact products of two 27-bit integers against nearby addends.
    for _ in 0..20_000 {
        let a = ((rng.next_u64() >> 37) | 1) as f64;
        let b = ((rng.next_u64() >> 37) | 1) as f64;
        let p = a * b;
        let c = -p + ((rng.next_u64() % 9) as f64 - 4.0);
        check(a, b, c);
        check(a, b, (rng.next_u64() >> 11) as f64);
    }
}

#[test]
fn subnormal_and_overflow_boundaries() {
    let mut rng = TestRng::new(77);
    for _ in 0..20_000 {
        // Products straddling the subnormal range.
        let ea = (rng.next_u64() % 600) as i32 - 600;
        let eb = -1074 - ea + (rng.next_u64() % 120) as i32 - 60;
        let a = rng.in_range(1.0, 2.0) * pow2(ea);
        let b = rng.in_range(-2.0, 2.0) * pow2(eb.clamp(-1074, 1023));
        let c = rng.in_range(-2.0, 2.0) * pow2(-1074 + (rng.next_u64() % 60) as i32);
        check(a, b, c);
        check(a, b, 0.0);
        check(a, b, -0.0);
        // Products that overflow binary64 alone but not after the addend.
        let a = rng.in_range(1.0, 2.0) * pow2(600);
        let b = rng.in_range(1.0, 2.0) * pow2(424);
        check(a, b, -f64::MAX);
        check(-a, b, f64::MAX);
        check(a, b, f64::MAX);
        check(a, b, f64::NEG_INFINITY);
    }
}
