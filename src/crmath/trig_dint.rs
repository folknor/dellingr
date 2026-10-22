//! Code that CORE-MATH's `cos.c` and `tan.c` share (each carries its own
//! copy upstream; the copies are identical, so the port keeps one): the
//! 128-bit `dint64_t` arithmetic in their convention, where a value is
//! `(-1)^sgn * (hi:lo / 2^128) * 2^ex` (the binary point is before the top
//! bit, unlike `pow`'s), the Payne-Hanek reduction by 1/(2 pi), and the
//! sin/cos polynomial evaluations of both the fast and the accurate path.

use super::dint::Dint;
use super::dint::v2::add_dint;
use super::tables::trig::{PC, PC_FAST, PS, PS_FAST, T};
use super::{bits, fasttwosum, fma, from_bits};

/// 2^-11.
pub(crate) const MAGIC: Dint = Dint {
    hi: 0x8000000000000000,
    lo: 0x0,
    ex: -10,
    sgn: 0x0,
};
/// 1.
pub(crate) const ONE: Dint = Dint {
    hi: 0x8000000000000000,
    lo: 0x0,
    ex: 1,
    sgn: 0x0,
};

/// Product with error below 6 ulps.
pub(crate) fn mul_dint(a: &Dint, b: &Dint) -> Dint {
    let bh = b.hi as u128;
    let bl = b.lo as u128;
    let m1 = (a.hi as u128) * bl;
    let m2 = (a.lo as u128) * bh;
    let mut r = (a.hi as u128) * bh;
    r = r.wrapping_add((m1 >> 64) + (m2 >> 64));
    let ex = (r >> 127) as u64;
    r <<= 1 - ex;
    let mut out = Dint {
        hi: 0,
        lo: 0,
        ex: a.ex + b.ex + ex as i64 - 1,
        sgn: a.sgn ^ b.sgn,
    };
    out.set_r(r);
    out
}

/// Product assuming the low limb of `b` is zero; error below 2 ulps.
pub(crate) fn mul_dint_21(a: &Dint, b: &Dint) -> Dint {
    let bh = b.hi as u128;
    let hi = (a.hi as u128) * bh;
    let lo = (a.lo as u128) * bh;
    let mut r = hi.wrapping_add(lo >> 64);
    let ex = (r >> 127) as u64;
    r <<= 1 - ex;
    let mut out = Dint {
        hi: 0,
        lo: 0,
        ex: a.ex + b.ex + ex as i64 - 1,
        sgn: a.sgn ^ b.sgn,
    };
    out.set_r(r);
    out
}

/// A nonzero `x` as a [`Dint`]: |x| = 2^ex * hi / 2^64, exact.
pub(crate) fn dint_fromd(b: f64) -> Dint {
    let u = bits(b);
    let e = ((u >> 52) & 0x7ff) as i64;
    let hi = (u & (!0u64 >> 12)) + if e != 0 { 1u64 << 52 } else { 0 };
    let ex = e - 0x3fe;
    let t = i64::from(hi.leading_zeros());
    Dint {
        hi: hi << t,
        lo: 0,
        ex: ex - if t > 11 { t - 12 } else { 0 },
        sgn: u64::from(b < 0.0),
    }
}

fn subnormalize_dint(a: &mut Dint) {
    if a.ex > -1023 {
        return;
    }
    let ex = (-(1011 + a.ex)) as u64;
    let mut hi = a.hi >> ex;
    let md = (a.hi >> (ex - 1)) & 0x1;
    let lo = u64::from((a.hi & (!0u64 >> ex)) != 0 || a.lo != 0);
    hi += if lo != 0 { md } else { hi & md };
    a.hi = hi << ex;
    a.lo = 0;
    if a.hi == 0 {
        a.ex += 1;
        a.hi = 1u64 << 63;
    }
}

/// Round to binary64, nearest even.
pub(crate) fn dint_tod(a: &Dint) -> f64 {
    let mut a = *a;
    subnormalize_dint(&mut a);
    let mut r = from_bits((a.hi >> 11) | (0x3ffu64 << 52));
    let mut rd = 0.0;
    if (a.hi >> 10) & 0x1 != 0 {
        rd += h!("0x1p-53");
    }
    if a.hi & 0x3ff != 0 || a.lo != 0 {
        rd += h!("0x1p-54");
    }
    if a.sgn != 0 {
        rd = -rd;
    }
    r = from_bits(bits(r) | (a.sgn << 63));
    r += rd;
    let e;
    if a.ex > -1022 {
        if a.ex > 1024 {
            if a.ex == 1025 {
                r *= 2.0;
                e = h!("0x1p+1023");
            } else {
                r = f64::MAX;
                e = f64::MAX;
            }
        } else {
            e = from_bits(((a.ex.wrapping_add(1022) as u64) & 0x7ff) << 52);
        }
    } else if a.ex < -1073 {
        if a.ex == -1074 {
            r *= 0.5;
            e = h!("0x1p-1074");
        } else {
            r = h!("0x1p-1074");
            e = h!("0x1p-1074");
        }
    } else {
        e = from_bits(1u64 << (a.ex + 1073));
    }
    r * e
}

/// Exact product `a * b = hi + lo`.
#[inline(always)]
pub(crate) fn a_mul(a: f64, b: f64) -> (f64, f64) {
    let hi = a * b;
    (hi, fma(a, b, -hi))
}

/// `a * (bh + bl)` with error below ulp(lo).
#[inline(always)]
pub(crate) fn s_mul(a: f64, bh: f64, bl: f64) -> (f64, f64) {
    let (hi, lo) = a_mul(a, bh);
    (hi, fma(a, bl, lo))
}

/// `(ah + al) * (bh + bl)` without the `al * bl` term.
#[inline(always)]
fn d_mul(ah: f64, al: f64, bh: f64, bl: f64) -> (f64, f64) {
    let (hi, s) = a_mul(ah, bh);
    let t = fma(al, bh, s);
    (hi, fma(ah, bl, t))
}

/// sin2pi(xh + xl), with `uh + ul` approximating (xh + xl)^2.
#[inline(always)]
pub(crate) fn eval_ps_fast(xh: f64, xl: f64, uh: f64, ul: f64) -> (f64, f64) {
    let mut h = PS_FAST[4]; // degree 7
    h = fma(h, uh, PS_FAST[3]); // degree 5
    h = fma(h, uh, PS_FAST[2]); // degree 3
    let (h, l) = s_mul(h, uh, ul);
    let (h, t) = fasttwosum(PS_FAST[0], h);
    let l = l + (PS_FAST[1] + t);
    d_mul(h, l, xh, xl)
}

/// cos2pi(xh + xl), with `uh + ul` approximating (xh + xl)^2.
#[inline(always)]
pub(crate) fn eval_pc_fast(uh: f64, ul: f64) -> (f64, f64) {
    let mut h = PC_FAST[4]; // degree 6
    h = fma(h, uh, PC_FAST[3]); // degree 4
    h = fma(h, uh, PC_FAST[2]); // degree 2
    let (h, l) = s_mul(h, uh, ul);
    let (h, t) = fasttwosum(PC_FAST[0], h);
    (h, l + (PC_FAST[1] + t))
}

/// sin2pi(X) for 0 <= X < 2^-11, with X2 approximating X^2.
pub(crate) fn eval_ps(x: &Dint, x2: &Dint) -> Dint {
    let mut y = mul_dint_21(x2, &PS[5]); // degree 11
    y = add_dint(&y, &PS[4]); // degree 9
    y = mul_dint(&y, x2);
    y = add_dint(&y, &PS[3]); // degree 7
    y = mul_dint(&y, x2);
    y = add_dint(&y, &PS[2]); // degree 5
    y = mul_dint(&y, x2);
    y = add_dint(&y, &PS[1]); // degree 3
    y = mul_dint(&y, x2);
    y = add_dint(&y, &PS[0]); // degree 1
    mul_dint(&y, x) // multiply by X
}

/// cos2pi(X) for 0 <= X < 2^-11, with X2 approximating X^2.
pub(crate) fn eval_pc(x2: &Dint) -> Dint {
    let mut y = mul_dint_21(x2, &PC[5]); // degree 10
    y = add_dint(&y, &PC[4]); // degree 8
    y = mul_dint(&y, x2);
    y = add_dint(&y, &PC[3]); // degree 6
    y = mul_dint(&y, x2);
    y = add_dint(&y, &PC[2]); // degree 4
    y = mul_dint(&y, x2);
    y = add_dint(&y, &PC[1]); // degree 2
    y = mul_dint(&y, x2);
    add_dint(&y, &PC[0]) // degree 0
}

/// Shift so the top bit of `hi` is set (unless the value is zero).
fn normalize(x: &mut Dint) {
    if x.hi != 0 {
        let cnt = x.hi.leading_zeros();
        if cnt != 0 {
            x.hi = (x.hi << cnt) | (x.lo >> (64 - cnt));
            x.lo <<= cnt;
        }
        x.ex -= i64::from(cnt);
    } else if x.lo != 0 {
        let cnt = x.lo.leading_zeros();
        x.hi = x.lo << cnt;
        x.lo = 0;
        x.ex -= 64 + i64::from(cnt);
    }
}

/// X/(2 pi) mod 1 with relative error below 2^-126.67, for normalised X;
/// normalises its result.
pub(crate) fn reduce(x: &mut Dint) {
    let mut e = x.ex as i32;
    if e <= 1 {
        // |X| < 2: multiply by T[0]/2^64 + T[1]/2^128.
        let mut u = (x.hi as u128) * (T[1] as u128);
        let tiny = u as u64;
        x.lo = (u >> 64) as u64;
        u = (x.hi as u128) * (T[0] as u128);
        x.lo = x.lo.wrapping_add(u as u64);
        x.hi = ((u >> 64) as u64).wrapping_add(u64::from(x.lo < u as u64));
        e = x.ex as i32;
        normalize(x);
        e -= x.ex as i32;
        // Put the upper e bits of tiny into lo.
        if e != 0 {
            x.lo |= tiny >> (64 - e);
        }
        return;
    }
    // Now 2 <= e <= 1024.
    let i = if e < 127 {
        0
    } else {
        ((e - 127 + 64 - 1) / 64) as usize
    };
    let mut c = [0u64; 5];
    let mut u = (x.hi as u128) * (T[i + 3] as u128);
    c[0] = u as u64;
    c[1] = (u >> 64) as u64;
    u = (x.hi as u128) * (T[i + 2] as u128);
    c[1] = c[1].wrapping_add(u as u64);
    c[2] = ((u >> 64) as u64).wrapping_add(u64::from(c[1] < u as u64));
    u = (x.hi as u128) * (T[i + 1] as u128);
    c[2] = c[2].wrapping_add(u as u64);
    c[3] = ((u >> 64) as u64).wrapping_add(u64::from(c[2] < u as u64));
    u = (x.hi as u128) * (T[i] as u128);
    c[3] = c[3].wrapping_add(u as u64);
    c[4] = ((u >> 64) as u64).wrapping_add(u64::from(c[3] < u as u64));
    let f = e - 64 * i as i32; // hi*T[i]/2^128 is scaled by 2^f
    let tiny = if f < 64 {
        x.hi = (c[4] << f) | (c[3] >> (64 - f));
        x.lo = (c[3] << f) | (c[2] >> (64 - f));
        (c[2] << f) | (c[1] >> (64 - f))
    } else if f == 64 {
        x.hi = c[3];
        x.lo = c[2];
        c[1]
    } else {
        // 65 <= f <= 127, only when e >= 65: one more term.
        let g = f - 64; // 1 <= g <= 63
        let u = (((x.hi as u128) * (T[i + 4] as u128)) >> 64) as u64;
        c[0] = c[0].wrapping_add(u);
        let carry = c[0] < u;
        c[1] = c[1].wrapping_add(u64::from(carry));
        c[2] = c[2].wrapping_add(u64::from(carry && c[1] == 0));
        c[3] = c[3].wrapping_add(u64::from(carry && c[1] == 0 && c[2] == 0));
        c[4] = c[4].wrapping_add(u64::from(carry && c[1] == 0 && c[2] == 0 && c[3] == 0));
        x.hi = (c[3] << g) | (c[2] >> (64 - g));
        x.lo = (c[2] << g) | (c[1] >> (64 - g));
        (c[1] << g) | (c[0] >> (64 - g))
    };
    x.ex = 0;
    normalize(x);
    // The worst case for 2^25 <= x < 2^1024 is X.ex = -61, so this shift is
    // in range.
    if x.ex < 0 {
        x.lo |= tiny >> (64 + x.ex);
    }
}

/// Split 0 <= X < 1 as i/2^11 + X with 0 <= X < 2^-11 (exact).
pub(crate) fn reduce2(x: &mut Dint) -> u32 {
    if x.ex <= -11 {
        return 0;
    }
    let sh = (64 - 11 - x.ex) as u32;
    let i = (x.hi >> sh) as u32;
    x.hi &= (1u64 << sh) - 1;
    normalize(x);
    i
}

/// `c1/2^64 + c0/2^128` as a double-double (truncated).
pub(crate) fn set_dd(c1: u64, c0: u64) -> (f64, f64) {
    let mut c1 = c1;
    let mut c0 = c0;
    if c1 != 0 {
        let e = u64::from(c1.leading_zeros());
        if e != 0 {
            c1 = (c1 << e) | (c0 >> (64 - e));
            c0 <<= e;
        }
        let f = 0x3fe - e;
        let h = from_bits((f << 52) | ((c1 << 1) >> 12));
        c0 = (c1 << 53) | (c0 >> 11);
        let l = if c0 != 0 {
            let g = u64::from(c0.leading_zeros());
            if g != 0 {
                c0 <<= g;
            }
            from_bits(((f - 53 - g) << 52) | ((c0 << 1) >> 12))
        } else {
            0.0
        };
        (h, l)
    } else if c0 != 0 {
        let e = u64::from(c0.leading_zeros());
        let f = 0x3fe - 64 - e;
        // Shift the most significant bit out (a shift of 64 leaves nothing).
        c0 = if e + 1 < 64 { c0 << (e + 1) } else { 0 };
        let h = from_bits((f << 52) | (c0 >> 12));
        c0 <<= 52;
        let l = if c0 != 0 {
            let g = u64::from(c0.leading_zeros());
            c0 = if g + 1 < 64 { c0 << (g + 1) } else { 0 };
            from_bits(((f - 64 - g) << 52) | (c0 >> 12))
        } else {
            0.0
        };
        (h, l)
    } else {
        (0.0, 0.0)
    }
}

/// floor for 0 <= x < 2^52 (here x < 2^11).
#[inline(always)]
pub(crate) fn floor_nonnegative(x: f64) -> f64 {
    (x as u64) as f64
}
