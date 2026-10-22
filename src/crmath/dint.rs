//! CORE-MATH's `dint64_t`: a 128-bit significand `hi:lo` with a binary
//! exponent and a sign, representing `(-1)^sgn * (hi:lo / 2^127) * 2^ex`
//! (normalised values have the top bit of `hi` set).
//!
//! Upstream carries several copies of the arithmetic on this type whose
//! rounding behaviour differs; each port uses the copy its source used, so
//! the operations live in per-source modules ([`v1`] for `log`/`log10`,
//! [`v2`] for `pow`, `cos` and `tan`). Unsigned arithmetic wraps exactly as C
//! unsigned arithmetic does.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Dint {
    pub(crate) hi: u64,
    pub(crate) lo: u64,
    pub(crate) ex: i64,
    pub(crate) sgn: u64,
}

impl Dint {
    #[inline(always)]
    pub(crate) const fn r(&self) -> u128 {
        ((self.hi as u128) << 64) | self.lo as u128
    }

    #[inline(always)]
    pub(crate) fn set_r(&mut self, r: u128) {
        self.hi = (r >> 64) as u64;
        self.lo = r as u64;
    }
}

#[inline(always)]
fn cmp_i64(a: i64, b: i64) -> i32 {
    i32::from(a > b) - i32::from(a < b)
}

#[inline(always)]
fn cmp_u64(a: u64, b: u64) -> i32 {
    i32::from(a > b) - i32::from(a < b)
}

#[inline(always)]
fn cmp_u128(a: u128, b: u128) -> i32 {
    i32::from(a > b) - i32::from(a < b)
}

/// `(biased exponent - bias_offset, significand with hidden bit)` of `x`.
#[inline(always)]
fn fast_extract(x: f64, bias: i64) -> (i64, u64) {
    let u = x.to_bits();
    let e = ((u >> 52) & 0x7ff) as i64;
    let m = (u & (!0u64 >> 12)) + if e != 0 { 1u64 << 52 } else { 0 };
    (e - bias, m)
}

/// The arithmetic of `log/dint.h` and `log10/dint.h`.
pub(crate) mod v1 {
    use super::{Dint, cmp_i64, cmp_u64, fast_extract};

    pub(crate) const M_ONE: Dint = Dint {
        hi: 0x8000000000000000,
        lo: 0x0,
        ex: 0,
        sgn: 0x1,
    };
    /// log(2) with absolute error below 2^-129.97.
    pub(crate) const LOG2: Dint = Dint {
        hi: 0xb17217f7d1cf79ab,
        lo: 0xc9e3b39803f2f6af,
        ex: -1,
        sgn: 0x0,
    };
    /// 1/log(10) with absolute error below 2^-131.72.
    pub(crate) const ONE_OVER_LOG10: Dint = Dint {
        hi: 0xde5bd8a937287195,
        lo: 0x355baaafad33dc32,
        ex: -2,
        sgn: 0x0,
    };
    pub(crate) const ZERO: Dint = Dint {
        hi: 0x0,
        lo: 0x0,
        ex: 0,
        sgn: 0x0,
    };

    /// `addu_128`: wrapping sum and whether it carried.
    #[inline(always)]
    fn addu_128(a: u128, b: u128) -> (u128, bool) {
        let al = a as u64;
        let ah = (a >> 64) as u64;
        let bl = b as u64;
        let bh = (b >> 64) as u64;
        let rl = al.wrapping_add(bl);
        let rh = ah.wrapping_add(bh).wrapping_add(u64::from(rl < al));
        let carry = if rh == ah { rl < al } else { rh < ah };
        (((rh as u128) << 64) | rl as u128, carry)
    }

    /// `subu_128`: wrapping difference.
    #[inline(always)]
    fn subu_128(a: u128, b: u128) -> u128 {
        let c = b.wrapping_neg();
        let al = a as u64;
        let ah = (a >> 64) as u64;
        let cl = c as u64;
        let ch = (c >> 64) as u64;
        let rl = al.wrapping_add(cl);
        let rh = ah.wrapping_add(ch).wrapping_add(u64::from(rl < al));
        ((rh as u128) << 64) | rl as u128
    }

    #[inline(always)]
    fn cmp_dint(a: &Dint, b: &Dint) -> i32 {
        let c = cmp_i64(a.ex, b.ex);
        if c != 0 {
            return c;
        }
        let c = cmp_u64(a.hi, b.hi);
        if c != 0 {
            return c;
        }
        cmp_u64(a.lo, b.lo)
    }

    pub(crate) fn add_dint(a: &Dint, b: &Dint) -> Dint {
        if a.hi | a.lo == 0 {
            return *b;
        }
        if b.hi | b.lo == 0 {
            return *a;
        }
        match cmp_dint(a, b) {
            0 => {
                if a.sgn ^ b.sgn != 0 {
                    return ZERO;
                }
                let mut r = *a;
                r.ex += 1;
                return r;
            }
            -1 => return add_dint(b, a),
            _ => {}
        }
        // From now on, |A| > |B|.
        let aa = a.r();
        let mut bb = b.r();
        let mut m_ex = a.ex;
        if a.ex > b.ex {
            let sh = (a.ex - b.ex) as i32;
            // Round to nearest.
            if sh <= 128 {
                bb = bb.wrapping_add(1 & (bb >> (sh - 1)));
            }
            bb = if sh < 128 { bb >> sh } else { 0 };
        }
        let sgn = a.sgn;
        let mut c = if a.sgn ^ b.sgn != 0 {
            subu_128(aa, bb)
        } else {
            let (mut c, carry) = addu_128(aa, bb);
            if carry {
                c = c.wrapping_add(c & 0x1);
                c = (1u128 << 127) | (c >> 1);
                m_ex += 1;
            }
            c
        };
        let ch = (c >> 64) as u64;
        let cl = c as u64;
        let ex: u64 = if ch != 0 {
            u64::from(ch.leading_zeros())
        } else {
            64u64.wrapping_add(if cl != 0 {
                u64::from(cl.leading_zeros())
            } else {
                a.ex as u64
            })
        };
        c = if ex < 128 { c << ex } else { 0 };
        let mut r = Dint {
            hi: 0,
            lo: 0,
            ex: m_ex.wrapping_sub(ex as i64),
            sgn,
        };
        r.set_r(c);
        r
    }

    /// Multiply with 126 bits of accuracy.
    pub(crate) fn mul_dint(a: &Dint, b: &Dint) -> Dint {
        let mut t = (a.hi as u128) * (b.hi as u128);
        let m1 = (a.hi as u128) * (b.lo as u128);
        let m2 = (a.lo as u128) * (b.hi as u128);
        let (m, carry) = addu_128(m1, m2);
        // t.h += carry
        t = t.wrapping_add((u128::from(carry)) << 64);
        t = t.wrapping_add(m >> 64);
        // Ensure that hi starts with a 1.
        let ex = u64::from((t >> 127) == 0);
        if ex != 0 {
            t <<= 1;
        }
        t = t.wrapping_add((m as u64 >> 63) as u128);
        let mut r = Dint {
            hi: 0,
            lo: 0,
            ex: a
                .ex
                .wrapping_add(b.ex)
                .wrapping_sub(ex as i64)
                .wrapping_add(1),
            sgn: a.sgn ^ b.sgn,
        };
        r.set_r(t);
        r
    }

    /// Multiply by a small integer.
    pub(crate) fn mul_dint_2(b: i64, a: &Dint) -> Dint {
        if b == 0 {
            return ZERO;
        }
        let c = b.unsigned_abs();
        let sgn = if b < 0 { u64::from(a.sgn == 0) } else { a.sgn };
        let mut t = (a.hi as u128) * (c as u128);
        let th = (t >> 64) as u64;
        let mut m: i64 = if th != 0 {
            i64::from(th.leading_zeros())
        } else {
            64
        };
        t <<= m;
        // Callers pass |b| < 2^11, so hi * c < 2^75 and m >= 53: the shift by
        // m - 1 below is in range, as upstream assumes.
        let mut l = (a.lo as u128) * (c as u128);
        l = (l << (m - 1)) >> 63;
        let (mut s, carry) = addu_128(l, t);
        if carry {
            s = s.wrapping_add(s & 0x1);
            s = (1u128 << 127) | (s >> 1);
            m -= 1;
        }
        let mut r = Dint {
            hi: 0,
            lo: 0,
            ex: a.ex + 64 - m,
            sgn,
        };
        r.set_r(s);
        r
    }

    /// `x` as a [`Dint`], for normal or subnormal nonzero `x`.
    pub(crate) fn dint_fromd(b: f64) -> Dint {
        let (ex, hi) = fast_extract(b, 0x3ff);
        let t = i64::from(hi.leading_zeros());
        Dint {
            hi: hi << t,
            lo: 0,
            ex: ex - if t > 11 { t - 12 } else { 0 },
            sgn: u64::from(b < 0.0),
        }
    }

    /// Round to binary64 for results in the normal range. Upstream relies on
    /// the bits after the round bit never all being zero (the hardest input
    /// has 64 identical bits after it), so a sticky 2^-54 is always added.
    pub(crate) fn dint_tod(a: &Dint) -> f64 {
        let mut r = f64::from_bits((a.hi >> 11) | (0x3ffu64 << 52));
        let mut rd = h!("0x1p-54");
        if (a.hi >> 10) & 0x1 != 0 {
            rd += h!("0x1p-53");
        }
        r = f64::from_bits(r.to_bits() | (a.sgn << 63));
        r += if a.sgn == 0 { rd } else { -rd };
        let e = f64::from_bits(((a.ex.wrapping_add(1023) as u64) & 0x7ff) << 52);
        r * e
    }
}

/// The arithmetic of `pow/dint.h`, whose operations `cos.c` and `tan.c`
/// also copy.
pub(crate) mod v2 {
    use super::{Dint, cmp_i64, cmp_u64, cmp_u128, fast_extract};

    pub(crate) const M_ONE: Dint = Dint {
        hi: 0x8000000000000000,
        lo: 0x0,
        ex: 0,
        sgn: 0x1,
    };
    /// log(2) to nearest, absolute error below 2^-129.97.
    pub(crate) const LOG2: Dint = Dint {
        hi: 0xb17217f7d1cf79ab,
        lo: 0xc9e3b39803f2f6af,
        ex: -1,
        sgn: 0x0,
    };
    /// 2^12/log(2), absolute error below 2^-52.96.
    pub(crate) const LOG2_INV: Dint = Dint {
        hi: 0xb8aa3b295c17f0bc,
        lo: 0x0,
        ex: 12,
        sgn: 0x0,
    };
    /// Zero, with an exponent that makes `dint_tod(ZERO)` return 0.
    pub(crate) const ZERO: Dint = Dint {
        hi: 0x0,
        lo: 0x0,
        ex: -1076,
        sgn: 0x0,
    };

    #[inline(always)]
    pub(crate) fn dint_zero_p(a: &Dint) -> bool {
        a.hi == 0
    }

    /// Compare absolute values: -1, 0 or +1.
    #[inline(always)]
    pub(crate) fn cmp_dint_abs(a: &Dint, b: &Dint) -> i32 {
        if dint_zero_p(a) {
            return if dint_zero_p(b) { 0 } else { -1 };
        }
        if dint_zero_p(b) {
            return 1;
        }
        let c1 = cmp_i64(a.ex, b.ex);
        if c1 != 0 { c1 } else { cmp_u128(a.r(), b.r()) }
    }

    #[inline(always)]
    fn cmp_dint_11(a: &Dint, b: &Dint) -> i32 {
        let c1 = cmp_i64(a.ex, b.ex);
        if c1 != 0 { c1 } else { cmp_u64(a.hi, b.hi) }
    }

    /// Sum with error below 2 ulps; exact in the Sterbenz case.
    pub(crate) fn add_dint(a: &Dint, b: &Dint) -> Dint {
        if a.hi | a.lo == 0 {
            return *b;
        }
        let (a, b) = match cmp_dint_abs(a, b) {
            0 => {
                if a.sgn ^ b.sgn != 0 {
                    return ZERO;
                }
                let mut r = *a;
                r.ex += 1;
                return r;
            }
            -1 => (b, a),
            _ => (a, b),
        };
        // From now on |A| > |B|, thus a.ex >= b.ex.
        let aa = a.r();
        let mut bb = b.r();
        let k = (a.ex as u64).wrapping_sub(b.ex as u64);
        if k > 0 {
            bb = if k < 128 { bb >> k } else { 0 };
        }
        let sgn = a.sgn;
        let mut ex_r = a.ex;
        let c = if a.sgn ^ b.sgn != 0 {
            let mut c = aa.wrapping_sub(bb);
            let ch = (c >> 64) as u64;
            // C cannot be zero here (|A| = |B| was excluded); C's clz is
            // taken on its low 64 bits when the high half is zero, as the C
            // source's implicit conversion does.
            let mut ex = if ch != 0 {
                u64::from(ch.leading_zeros())
            } else {
                64 + u64::from((c as u64).leading_zeros())
            };
            if ex > 0 {
                c = if k == 1 {
                    // Sterbenz case.
                    (aa << ex).wrapping_sub(b.r() << (ex - 1))
                } else {
                    (aa << ex).wrapping_sub(bb << ex)
                };
                ex_r = ex_r.wrapping_sub(ex as i64);
                ex = u64::from(((c >> 64) as u64).leading_zeros());
            }
            c <<= ex;
            ex_r = ex_r.wrapping_sub(ex as i64);
            c
        } else {
            let mut c = aa.wrapping_add(bb);
            if c < aa {
                c = (1u128 << 127) | (c >> 1);
                ex_r += 1;
            }
            c
        };
        let mut r = Dint {
            hi: 0,
            lo: 0,
            ex: ex_r,
            sgn,
        };
        r.set_r(c);
        r
    }

    /// Sum of values whose low limbs are zero; error below 2 ulps (64-bit).
    pub(crate) fn add_dint_11(a: &Dint, b: &Dint) -> Dint {
        let destination_lo = b.lo;
        if a.hi == 0 {
            return *b;
        }
        if b.hi == 0 {
            return *a;
        }
        let (a, b) = match cmp_dint_11(a, b) {
            0 => {
                if a.sgn ^ b.sgn != 0 {
                    return ZERO;
                }
                let mut r = *a;
                r.ex += 1;
                return r;
            }
            -1 => (b, a),
            _ => (a, b),
        };
        let aa = a.hi;
        let mut bb = b.hi;
        if a.ex > b.ex {
            let k = (a.ex as u64).wrapping_sub(b.ex as u64);
            bb = if k < 64 { bb >> k } else { 0 };
        }
        let sgn = a.sgn;
        let mut ex_r = a.ex;
        let c: u64 = if a.sgn ^ b.sgn != 0 {
            let mut c = aa.wrapping_sub(bb);
            let mut ex = c.leading_zeros();
            if ex > 0 {
                c = (aa << ex).wrapping_sub(bb << ex);
                ex_r -= i64::from(ex);
                ex = c.leading_zeros();
            }
            c <<= ex;
            ex_r -= i64::from(ex);
            c
        } else {
            let mut c = aa.wrapping_add(bb);
            if c < aa {
                c = (1u64 << 63) | (c >> 1);
                ex_r += 1;
            }
            c
        };
        // Upstream writes only the high limb and leaves the destination's low
        // limb alone. Every upstream call is `add_dint_11(r, &C, r)`, so the
        // destination is the second operand, whose low limb survives (`b`
        // captured before any swap).
        Dint {
            hi: c,
            lo: destination_lo,
            ex: ex_r,
            sgn,
        }
    }

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
            ex: a.ex + b.ex + ex as i64,
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
            ex: a.ex + b.ex + ex as i64,
            sgn: a.sgn ^ b.sgn,
        };
        out.set_r(r);
        out
    }

    /// Exact product assuming both low limbs are zero.
    pub(crate) fn mul_dint_11(a: &Dint, b: &Dint) -> Dint {
        let mut r = (a.hi as u128) * (b.hi as u128);
        let ex = (r >> 127) as u64;
        r <<= 1 - ex;
        let mut out = Dint {
            hi: 0,
            lo: 0,
            ex: a.ex + b.ex + ex as i64,
            sgn: a.sgn ^ b.sgn,
        };
        out.set_r(r);
        out
    }

    /// Product with a small integer, error below 1 ulp.
    pub(crate) fn mul_dint_int64(a: &Dint, b: i64) -> Dint {
        if b == 0 {
            return ZERO;
        }
        let c = b.unsigned_abs();
        let sgn = if b < 0 { u64::from(a.sgn == 0) } else { a.sgn };
        let mut ex = a.ex + 64;
        let mut r = (a.hi as u128) * (c as u128);
        let rh = (r >> 64) as u64;
        // If c = 1 the high limb may be zero.
        let m: u32 = if rh != 0 { rh.leading_zeros() } else { 64 };
        r <<= m;
        ex -= i64::from(m);
        // hi * c < 2^(128 - m) and hi >= 2^63 give c < 2^(65 - m), so this
        // shift cannot overflow (upstream's argument).
        let mut l = (a.lo as u128) * (c as u128);
        l = (l << (m - 1)) >> 63;
        r = r.wrapping_add(l);
        if r < l {
            r = (1u128 << 127) | (r >> 1);
            ex += 1;
        }
        let mut out = Dint {
            hi: 0,
            lo: 0,
            ex,
            sgn,
        };
        out.set_r(r);
        out
    }

    /// A nonzero `x` as a [`Dint`]: |x| = 2^ex * hi / 2^63, exact.
    pub(crate) fn dint_fromd(b: f64) -> Dint {
        let (ex, hi) = fast_extract(b, 0x3ff);
        let t = i64::from(hi.leading_zeros());
        Dint {
            hi: hi << t,
            lo: 0,
            ex: ex - if t > 11 { t - 12 } else { 0 },
            sgn: u64::from(b < 0.0),
        }
    }
}
