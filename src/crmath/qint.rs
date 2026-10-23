//! CORE-MATH's `qint64_t` from `pow/qint.h`: a 256-bit significand in four
//! limbs `hh:hl:lh:ll` with a binary exponent and a sign, representing
//! `(-1)^sgn * (hh:hl:lh:ll / 2^255) * 2^ex`. Used by the third (256-bit)
//! phase of `pow`. Unsigned arithmetic wraps as C unsigned arithmetic does.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Qint {
    pub(crate) hh: u64,
    pub(crate) hl: u64,
    pub(crate) lh: u64,
    pub(crate) ll: u64,
    pub(crate) ex: i64,
    pub(crate) sgn: u64,
}

impl Qint {
    #[inline(always)]
    fn rh(&self) -> u128 {
        ((self.hh as u128) << 64) | self.hl as u128
    }

    #[inline(always)]
    fn rl(&self) -> u128 {
        ((self.lh as u128) << 64) | self.ll as u128
    }

    #[inline(always)]
    fn with(rh: u128, rl: u128, ex: i64, sgn: u64) -> Self {
        Self {
            hh: (rh >> 64) as u64,
            hl: rh as u64,
            lh: (rl >> 64) as u64,
            ll: rl as u64,
            ex,
            sgn,
        }
    }
}

pub(crate) const M_ONE_Q: Qint = Qint {
    hh: 0x8000000000000000,
    hl: 0x0,
    lh: 0x0,
    ll: 0x0,
    ex: 0,
    sgn: 0x1,
};
/// log(2) with absolute error below 2^-256.14.
pub(crate) const LOG2_Q: Qint = Qint {
    hh: 0xb17217f7d1cf79ab,
    hl: 0xc9e3b39803f2f6af,
    lh: 0x40f343267298b62d,
    ll: 0x8a0d175b8baafa2b,
    ex: -1,
    sgn: 0x0,
};
/// 2^12/log(2) with absolute error below 2^-52.96.
pub(crate) const LOG2_INV_Q: Qint = Qint {
    hh: 0xb8aa3b295c17f0bc,
    hl: 0x0,
    lh: 0x0,
    ll: 0x0,
    ex: 12,
    sgn: 0x0,
};
const ZERO_Q: Qint = Qint {
    hh: 0x0,
    hl: 0x0,
    lh: 0x0,
    ll: 0x0,
    ex: 0,
    sgn: 0x0,
};

#[inline(always)]
fn cmp_i64(a: i64, b: i64) -> i32 {
    i32::from(a > b) - i32::from(a < b)
}

#[inline(always)]
fn cmp_u128(a: u128, b: u128) -> i32 {
    i32::from(a > b) - i32::from(a < b)
}

/// `addu128`: wrapping sum and carry.
#[inline(always)]
fn addu128(a: u128, b: u128) -> (u128, u128) {
    let r = a.wrapping_add(b);
    (r, u128::from(r < a))
}

/// `subu128`: wrapping difference and borrow.
#[inline(always)]
fn subu128(a: u128, b: u128) -> (u128, bool) {
    let r = a.wrapping_sub(b);
    (r, r > a)
}

/// Leading zeros of the 256-bit value `ch:cl` (`ch:cl` nonzero).
#[inline(always)]
fn clz256(ch: u128, cl: u128) -> u64 {
    let chh = (ch >> 64) as u64;
    let clh = (cl >> 64) as u64;
    if chh != 0 {
        u64::from(chh.leading_zeros())
    } else if ch != 0 {
        64 + u64::from((ch as u64).leading_zeros())
    } else if clh != 0 {
        128 + u64::from(clh.leading_zeros())
    } else {
        192 + u64::from((cl as u64).leading_zeros())
    }
}

/// Compare absolute values.
#[inline(always)]
fn cmp_qint(a: &Qint, b: &Qint) -> i32 {
    let c = cmp_i64(a.ex, b.ex);
    if c != 0 {
        return c;
    }
    let c = cmp_u128(a.rh(), b.rh());
    if c != 0 {
        return c;
    }
    cmp_u128(a.rl(), b.rl())
}

/// Compare absolute values on the upper two limbs.
#[inline(always)]
fn cmp_qint_22(a: &Qint, b: &Qint) -> i32 {
    let c = cmp_i64(a.ex, b.ex);
    if c != 0 { c } else { cmp_u128(a.rh(), b.rh()) }
}

/// Sum with error below 2 ulps (256-bit); exact in the Sterbenz case.
pub(crate) fn add_qint(a: &Qint, b: &Qint) -> Qint {
    if a.rh() == 0 && a.rl() == 0 {
        return *b;
    }
    if b.rh() == 0 && b.rl() == 0 {
        return *a;
    }
    match cmp_qint(a, b) {
        0 => {
            if a.sgn ^ b.sgn != 0 {
                return ZERO_Q;
            }
            let mut r = *a;
            r.ex += 1;
            return r;
        }
        -1 => return add_qint(b, a),
        _ => {}
    }
    // From now on, |A| > |B|.
    let mut ah = a.rh();
    let mut al = a.rl();
    let mut bh = b.rh();
    let mut bl = b.rl();
    let m_ex = a.ex;
    let k = a.ex - b.ex;
    if k > 0 {
        if k >= 128 {
            bl = if k < 256 { bh >> (k - 128) } else { 0 };
            bh = 0;
        } else {
            bl = (bl >> k) | (bh << (128 - k));
            bh >>= k;
        }
    }
    let sgn = a.sgn;
    let mut ex_r = m_ex;
    let (ch, cl) = if a.sgn ^ b.sgn != 0 {
        // Subtraction.
        let mut ch = ah.wrapping_sub(bh);
        let (mut cl, borrow) = subu128(al, bl);
        if borrow {
            ch = ch.wrapping_sub(1);
        }
        // C is nonzero since |A| > |B|.
        let mut ex = clz256(ch, cl);
        if ex > 0 {
            // Shift A left by ex bits and B left by ex - k bits.
            if ex >= 128 {
                ah = al << (ex - 128);
                al = 0;
            } else {
                ah = (ah << ex) | (al >> (128 - ex));
                al <<= ex;
            }
            let sh = ex as i64 - k;
            bh = b.rh();
            bl = b.rl();
            if sh >= 0 {
                if sh >= 128 {
                    bh = bl << (sh - 128);
                    bl = 0;
                } else if sh > 0 {
                    bh = (bh << sh) | (bl >> (128 - sh));
                    bl <<= sh;
                }
            } else {
                let j = -sh;
                if j >= 128 {
                    bl = bh >> (j - 128);
                    bh = 0;
                } else {
                    bl = (bh << (128 - j)) | (bl >> j);
                    bh >>= j;
                }
            }
            ex_r = ex_r.wrapping_sub(ex as i64);
            ch = ah.wrapping_sub(bh);
            let (new_cl, borrow) = subu128(al, bl);
            cl = new_cl;
            if borrow {
                ch = ch.wrapping_sub(1);
            }
            ex = clz256(ch, cl);
        }
        if ex != 0 {
            ch = (ch << ex) | (cl >> (128 - ex));
            cl <<= ex;
        }
        ex_r = ex_r.wrapping_sub(ex as i64);
        (ch, cl)
    } else {
        // Addition.
        let (mut ch, mut cy) = addu128(ah, bh);
        let (mut cl, carry_low) = addu128(al, bl);
        if carry_low != 0 {
            ch = ch.wrapping_add(1);
            cy += u128::from(ch == 0);
        }
        if cy != 0 {
            cl = (ch << 127) | (cl >> 1);
            ch = (1u128 << 127) | (ch >> 1);
            ex_r += 1;
        }
        (ch, cl)
    };
    Qint::with(ch, cl, ex_r, sgn)
}

/// Sum on the upper two limbs only, error below 2 ulps (128-bit).
pub(crate) fn add_qint_22(a: &Qint, b: &Qint) -> Qint {
    if a.rh() == 0 {
        return *b;
    }
    if b.rh() == 0 {
        return *a;
    }
    match cmp_qint_22(a, b) {
        0 => {
            if a.sgn ^ b.sgn != 0 {
                return ZERO_Q;
            }
            let mut r = *a;
            r.ex += 1;
            return r;
        }
        -1 => return add_qint_22(b, a),
        _ => {}
    }
    let mut ah = a.rh();
    let mut bh = b.rh();
    let m_ex = a.ex;
    let k = (a.ex as u64).wrapping_sub(b.ex as u64);
    if k > 0 {
        bh = if k >= 128 { 0 } else { bh >> k };
    }
    let sgn = a.sgn;
    let mut ex_r = m_ex;
    let ch = if a.sgn ^ b.sgn != 0 {
        let mut ch = ah.wrapping_sub(bh);
        let chh = (ch >> 64) as u64;
        let mut ex: u64 = if chh != 0 {
            u64::from(chh.leading_zeros())
        } else {
            64 + u64::from((ch as u64).leading_zeros())
        };
        if ex > 0 {
            ah <<= ex;
            bh = if ex >= k {
                b.rh() << (ex - k)
            } else {
                b.rh() >> (k - ex)
            };
            ex_r = ex_r.wrapping_sub(ex as i64);
            ch = ah.wrapping_sub(bh);
            let chh = (ch >> 64) as u64;
            ex = if chh != 0 {
                u64::from(chh.leading_zeros())
            } else {
                64 + u64::from((ch as u64).leading_zeros())
            };
        }
        ch <<= ex;
        ex_r = ex_r.wrapping_sub(ex as i64);
        ch
    } else {
        let (mut ch, cy) = addu128(ah, bh);
        if cy != 0 {
            ch = (1u128 << 127) | (ch >> 1);
            ex_r += 1;
        }
        ch
    };
    Qint::with(ch, 0, ex_r, sgn)
}

/// Normalise the 256-bit product `t6:t5` (with `t6` its upper half) so the
/// top bit is set, and attach exponent and sign.
#[inline(always)]
fn finish_product(t6: u128, t5: u128, a: &Qint, b: &Qint) -> Qint {
    let ex = u64::from((t6 >> 127) == 0);
    let (rh, rl) = if ex != 0 {
        ((t6 << 1) | (t5 >> 127), t5 << 1)
    } else {
        (t6, t5)
    };
    Qint::with(rh, rl, a.ex + b.ex + 1 - ex as i64, a.sgn ^ b.sgn)
}

#[inline(always)]
fn mul64(a: u64, b: u64) -> u128 {
    (a as u128) * (b as u128)
}

/// Product with error below 14 ulps.
pub(crate) fn mul_qint(a: &Qint, b: &Qint) -> Qint {
    let r33 = mul64(a.hh, b.hh);
    let r32 = mul64(a.hh, b.hl);
    let r23 = mul64(a.hl, b.hh);
    let r31 = mul64(a.hh, b.lh);
    let r13 = mul64(a.lh, b.hh);
    let r22 = mul64(a.hl, b.hl);
    let r30 = mul64(a.hh, b.ll);
    let r03 = mul64(a.ll, b.hh);
    let r21 = mul64(a.hl, b.lh);
    let r12 = mul64(a.lh, b.hl);
    let t3 = (r12 >> 64) + (r21 >> 64) + (r03 >> 64) + (r30 >> 64);
    let (t4, c4a) = addu128(r22, t3);
    let (t4, c4b) = addu128(r13, t4);
    let (t4, c4c) = addu128(r31, t4);
    let c4 = c4a + c4b + c4c;
    let (t5, c5a) = addu128(r23, t4 >> 64);
    let (t5, c5b) = addu128(r32, t5);
    let c5 = c5a + c5b;
    let t6 = r33.wrapping_add((c5 << 64) | (t5 >> 64)).wrapping_add(c4);
    let t5 = (t5 << 64) | (t4 & 0xffffffffffffffff);
    finish_product(t6, t5, a, b)
}

/// Product of the upper three limbs, error below 6 ulps.
pub(crate) fn mul_qint_33(a: &Qint, b: &Qint) -> Qint {
    let r33 = mul64(a.hh, b.hh);
    let r32 = mul64(a.hh, b.hl);
    let r23 = mul64(a.hl, b.hh);
    let r31 = mul64(a.hh, b.lh);
    let r13 = mul64(a.lh, b.hh);
    let r22 = mul64(a.hl, b.hl);
    let r21 = mul64(a.hl, b.lh);
    let r12 = mul64(a.lh, b.hl);
    let t3 = (r12 >> 64) + (r21 >> 64);
    let (t4, c4a) = addu128(r22, t3);
    let (t4, c4b) = addu128(r13, t4);
    let (t4, c4c) = addu128(r31, t4);
    let c4 = c4a + c4b + c4c;
    let (t5, c5a) = addu128(r23, t4 >> 64);
    let (t5, c5b) = addu128(r32, t5);
    let c5 = c5a + c5b;
    let t6 = r33.wrapping_add((c5 << 64) | (t5 >> 64)).wrapping_add(c4);
    let t5 = (t5 << 64) | (t4 & 0xffffffffffffffff);
    finish_product(t6, t5, a, b)
}

/// Product with the upper limb of `b` only, error below 2 ulps.
pub(crate) fn mul_qint_41(a: &Qint, b: &Qint) -> Qint {
    let r33 = mul64(a.hh, b.hh);
    let r23 = mul64(a.hl, b.hh);
    let r13 = mul64(a.lh, b.hh);
    let r03 = mul64(a.ll, b.hh);
    let t3 = r03 >> 64;
    let (t4, c4) = addu128(r13, t3);
    let (t5, c5) = addu128(r23, t4 >> 64);
    let t6 = r33.wrapping_add((c5 << 64) | (t5 >> 64)).wrapping_add(c4);
    let t5 = (t5 << 64) | (t4 & 0xffffffffffffffff);
    finish_product(t6, t5, a, b)
}

/// Exact product of the upper three limbs of `a` by the upper limb of `b`.
pub(crate) fn mul_qint_31(a: &Qint, b: &Qint) -> Qint {
    let r33 = mul64(a.hh, b.hh);
    let r23 = mul64(a.hl, b.hh);
    let r13 = mul64(a.lh, b.hh);
    let t4 = r13;
    let (t5, c5) = addu128(r23, t4 >> 64);
    let t6 = r33.wrapping_add((c5 << 64) | (t5 >> 64));
    let t5 = (t5 << 64) | (t4 & 0xffffffffffffffff);
    finish_product(t6, t5, a, b)
}

/// Exact product of the upper two limbs.
pub(crate) fn mul_qint_22(a: &Qint, b: &Qint) -> Qint {
    let r33 = mul64(a.hh, b.hh);
    let r32 = mul64(a.hh, b.hl);
    let r23 = mul64(a.hl, b.hh);
    let r22 = mul64(a.hl, b.hl);
    let t4 = r22;
    let (t5, c5a) = addu128(r23, t4 >> 64);
    let (t5, c5b) = addu128(r32, t5);
    let c5 = c5a + c5b;
    let t6 = r33.wrapping_add((c5 << 64) | (t5 >> 64));
    let t5 = (t5 << 64) | (t4 & 0xffffffffffffffff);
    finish_product(t6, t5, a, b)
}

/// Exact product of the upper two limbs of `a` by the upper limb of `b`.
pub(crate) fn mul_qint_21(a: &Qint, b: &Qint) -> Qint {
    let r33 = mul64(a.hh, b.hh);
    let r23 = mul64(a.hl, b.hh);
    let t6 = r33.wrapping_add(r23 >> 64);
    let t5 = r23 << 64;
    finish_product(t6, t5, a, b)
}

/// Exact product of the upper limbs.
pub(crate) fn mul_qint_11(a: &Qint, b: &Qint) -> Qint {
    let t6 = mul64(a.hh, b.hh);
    let ex = u64::from((t6 >> 127) == 0);
    Qint::with(t6 << ex, 0, a.ex + b.ex + 1 - ex as i64, a.sgn ^ b.sgn)
}

/// Product with a small integer, error below 2 ulps.
pub(crate) fn mul_qint_2(b: i64, a: &Qint) -> Qint {
    if b == 0 {
        return ZERO_Q;
    }
    let mut c = b.unsigned_abs();
    let sgn = u64::from(b < 0) ^ a.sgn;
    if c == 1 {
        let mut r = *a;
        r.sgn = sgn;
        return r;
    }
    let mut ex = a.ex + 64;
    // Scale c so that 2^63 <= c < 2^64.
    let k = c.leading_zeros();
    c <<= k;
    ex -= i64::from(k);
    let mut t3 = mul64(a.hh, c);
    let t2 = mul64(a.hl, c);
    let t1 = mul64(a.lh, c);
    let t0 = mul64(a.ll, c);
    let t = t0 >> 64;
    let (t1, cy) = addu128(t, t1);
    let t = (cy << 64) | (t1 >> 64);
    let (t2, cy) = addu128(t, t2);
    t3 = t3.wrapping_add((cy << 64) | (t2 >> 64));
    let shift = ((t3 >> 64) as u64).leading_zeros();
    let t2 = (t2 << 64) | (t1 & 0xffffffffffffffff);
    if shift != 0 {
        Qint::with((t3 << 1) | (t2 >> 127), t2 << 1, ex - 1, sgn)
    } else {
        Qint::with(t3, t2, ex, sgn)
    }
}

/// A nonzero `x` as a [`Qint`], exact.
pub(crate) fn qint_fromd(b: f64) -> Qint {
    let u = b.to_bits();
    let e = ((u >> 52) & 0x7ff) as i64;
    let hh = (u & (!0u64 >> 12)) + if e != 0 { 1u64 << 52 } else { 0 };
    let ex = e - 0x3ff;
    let t = i64::from(hh.leading_zeros());
    Qint {
        hh: hh << t,
        hl: 0,
        lh: 0,
        ll: 0,
        ex: ex - if t > 11 { t - 12 } else { 0 },
        sgn: u64::from(b < 0.0),
    }
}

/// Truncate to an integer (towards zero).
pub(crate) fn qint_toi(a: &Qint) -> i64 {
    if a.ex < 0 {
        return 0;
    }
    let r = (a.hh >> (63 - a.ex)) as i64;
    if a.sgn != 0 { r.wrapping_neg() } else { r }
}

/// Round the significand to the subnormal grid when the value is subnormal
/// (round to nearest, ties to even).
fn subnormalize_qint(a: &mut Qint) {
    if a.ex > -1023 {
        return;
    }
    let ex = (-(1011 + a.ex)) as u64;
    let mut hi = a.hh >> ex;
    let md = (a.hh >> (ex - 1)) & 0x1;
    let lo = u64::from((a.hh & (!0u64 >> ex)) != 0 || a.hl != 0 || a.lh != 0 || a.ll != 0);
    hi += if lo != 0 { md } else { hi & md };
    a.hh = hi << ex;
    a.hl = 0;
    a.lh = 0;
    a.ll = 0;
    if a.hh == 0 {
        a.ex += 1;
        a.hh = 1u64 << 63;
    }
}

/// Round to binary64, nearest even.
pub(crate) fn qint_tod(a: &Qint) -> f64 {
    let mut a = *a;
    if a.ex <= -1075 {
        // Below 2^-1074, where upstream's subnormal shift would exceed the
        // limb width: the value is 2^-1075 or less, so it rounds to the
        // smallest subnormal only when strictly above 2^-1075.
        let above_half =
            a.ex == -1075 && (a.hh != 1u64 << 63 || a.hl != 0 || a.lh != 0 || a.ll != 0);
        let magnitude = if above_half { h!("0x1p-1074") } else { 0.0 };
        return if a.sgn != 0 { -magnitude } else { magnitude };
    }
    subnormalize_qint(&mut a);
    let mut r = f64::from_bits((a.hh >> 11) | (0x3ffu64 << 52));
    let mut rd = 0.0;
    if a.hh & 0x400 != 0 {
        rd += h!("0x1p-53");
    }
    if a.hh & 0x3ff != 0 || a.hl != 0 || a.lh != 0 || a.ll != 0 {
        rd += h!("0x1p-54");
    }
    if a.sgn != 0 {
        rd = -rd;
    }
    r = f64::from_bits(r.to_bits() | (a.sgn << 63));
    r += rd;
    let e;
    if a.ex > -1023 {
        if a.ex > 1023 {
            if a.ex == 1024 {
                r *= 2.0;
                e = h!("0x1p+1023");
            } else {
                r = if a.sgn != 0 { -f64::MAX } else { f64::MAX };
                e = f64::MAX;
            }
        } else {
            e = f64::from_bits(((a.ex.wrapping_add(1023) as u64) & 0x7ff) << 52);
        }
    } else if a.ex < -1074 {
        if a.ex == -1075 {
            r *= 0.5;
            e = h!("0x1p-1074");
        } else {
            r = h!("0x1p-1074");
            e = h!("0x1p-1074");
        }
    } else {
        e = f64::from_bits(1u64 << (a.ex + 1074));
    }
    r * e
}
