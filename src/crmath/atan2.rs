//! `atan2`, ported from CORE-MATH `src/binary64/atan2/atan2.c` and `tint.h`
//! (Paul Zimmermann and Alexei Sibidanov, with help from Silviu-Ioan Filip
//! for the rational approximation).
//!
//! The accurate path works on 192-bit `tint_t` values. Upstream aborts when
//! its final error bound straddles a rounding boundary; no input is known to
//! get there, and this port rounds the 192-bit approximation instead, which
//! keeps the result deterministic (see the module doc of `crmath`).

/// `(-1)^sgn * (h/2^64 + m/2^128 + l/2^192) * 2^ex`, with `h`'s top bit set
/// unless the value is zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tint {
    pub(crate) h: u64,
    pub(crate) m: u64,
    pub(crate) l: u64,
    pub(crate) ex: i64,
    pub(crate) sgn: u64,
}

impl Tint {
    /// The upper 128 bits `h:m`.
    #[inline(always)]
    fn hi(&self) -> u128 {
        ((self.h as u128) << 64) | self.m as u128
    }

    #[inline(always)]
    fn set_hi(&mut self, v: u128) {
        self.h = (v >> 64) as u64;
        self.m = v as u64;
    }
}

macro_rules! kernel {
    ($backend:ident, $fma:path, $test_cfg:meta, $inline:meta, $(#[$feature:meta])*) => {
        use $fma as fma;
        use crate::crmath::tables::atan2::{F2, O, P, Q, T2};
        use crate::crmath::{bits, copysign, fabs, fasttwosum, from_bits};

        use super::Tint;

        const ZERO: Tint = Tint {
            h: 0,
            m: 0,
            l: 0,
            ex: -1076,
            sgn: 0,
        };
        const ONE: Tint = Tint {
            h: 0x8000000000000000,
            m: 0,
            l: 0,
            ex: 1,
            sgn: 0,
        };
        /// pi with error below 2^-196.96.
        const PI: Tint = Tint {
            h: 0xc90fdaa22168c234,
            m: 0xc4c6628b80dc1cd1,
            l: 0x29024e088a67cc74,
            ex: 2,
            sgn: 0,
        };
        /// pi/2 with error below 2^-197.96.
        const PI2: Tint = Tint {
            h: 0xc90fdaa22168c234,
            m: 0xc4c6628b80dc1cd1,
            l: 0x29024e088a67cc74,
            ex: 1,
            sgn: 0,
        };

        const PI_H: f64 = h!("0x1.921fb54442d18p+1");
        const PI_L: f64 = h!("0x1.1a62633145c07p-53");
        const PI_OVER2_H: f64 = h!("0x1.921fb54442d18p+0");
        const PI_OVER2_L: f64 = h!("0x1.1a62633145c07p-54");

        /// Product with error below 10 ulps.
        $(#[$feature])*
        fn mul_tint(a: &Tint, b: &Tint) -> Tint {
            let mut r = Tint {
                h: 0,
                m: 0,
                l: 0,
                ex: a.ex + b.ex,
                sgn: a.sgn ^ b.sgn,
            };
            let (ah, am, al) = (a.h as u128, a.m as u128, a.l as u128);
            let (bh, bm, bl) = (b.h as u128, b.m as u128, b.l as u128);
            let rh = ah * bh;
            let rm1 = ah * bm;
            let rm2 = am * bh;
            let rl1 = ah * bl;
            let rl2 = am * bm;
            let rl3 = al * bh;
            r.h = (rh >> 64) as u64;
            r.m = rh as u64;
            // Accumulate rm1.
            r.l = rm1 as u64;
            let h = (rm1 >> 64) as u64;
            r.m = r.m.wrapping_add(h);
            r.h = r.h.wrapping_add(u64::from(r.m < h));
            // Accumulate rm2.
            let l = rm2 as u64;
            let h = (rm2 >> 64) as u64;
            r.l = r.l.wrapping_add(l);
            let mut cm = u64::from(r.l < l);
            r.m = r.m.wrapping_add(h);
            r.h = r.h.wrapping_add(u64::from(r.m < h));
            // Accumulate rl1 + rl2 + rl3.
            let rl = (rl1 >> 64) + (rl2 >> 64) + (rl3 >> 64);
            let l = rl as u64;
            cm = cm.wrapping_add((rl >> 64) as u64);
            r.l = r.l.wrapping_add(l);
            cm = cm.wrapping_add(u64::from(r.l < l));
            // Accumulate cm.
            r.m = r.m.wrapping_add(cm);
            r.h = r.h.wrapping_add(u64::from(r.m < cm));
            if r.h >> 63 == 0 {
                r.h = (r.h << 1) | (r.m >> 63);
                r.m = (r.m << 1) | (r.l >> 63);
                r.l <<= 1;
                r.ex -= 1;
            }
            r
        }

        #[$inline]
        $(#[$feature])*
        fn tint_zero_p(a: &Tint) -> bool {
            a.h == 0
        }

        #[$inline]
        $(#[$feature])*
        fn cmp3<T: PartialOrd + Copy>(a: T, b: T) -> i32 {
            i32::from(a > b) - i32::from(a < b)
        }

        /// Compare absolute values.
        $(#[$feature])*
        fn cmp_tint_abs(a: &Tint, b: &Tint) -> i32 {
            if tint_zero_p(a) {
                return if tint_zero_p(b) { 0 } else { -1 };
            }
            if tint_zero_p(b) {
                return 1;
            }
            let c = cmp3(a.ex, b.ex);
            if c != 0 {
                return c;
            }
            let c = cmp3(a.hi(), b.hi());
            if c != 0 {
                return c;
            }
            cmp3(a.l, b.l)
        }

        /// The significand of `b` shifted right by `k` bits, as `(hi, l)`.
        $(#[$feature])*
        fn rshift(b: &Tint, k: u64) -> (u128, u64) {
            let bh = b.hi();
            if k == 0 {
                (bh, b.l)
            } else if k < 64 {
                (bh >> k, ((bh << (64 - k)) as u64) | (b.l >> k))
            } else if k == 64 {
                (bh >> k, bh as u64)
            } else if k < 128 {
                (bh >> k, (bh >> (k - 64)) as u64)
            } else if k < 192 {
                (0, (bh >> (k - 64)) as u64)
            } else {
                (0, 0)
            }
        }

        /// The significand `(hi, l)` shifted left by `k` bits.
        $(#[$feature])*
        fn lshift(bh: u128, bl: u64, k: u64) -> (u128, u64) {
            if k == 0 {
                (bh, bl)
            } else if k < 64 {
                ((bh << k) | (bl >> (64 - k)) as u128, bl << k)
            } else if k == 64 {
                ((bh << k) | bl as u128, 0)
            } else if k < 128 {
                ((bh << k) | ((bl as u128) << (k - 64)), 0)
            } else if k < 192 {
                ((bl as u128) << (k - 64), 0)
            } else {
                (0, 0)
            }
        }

        /// Leading zeros of the 192-bit significand `(hi, l)`.
        #[$inline]
        $(#[$feature])*
        fn clz192(th: u128, tl: u64) -> u64 {
            let thh = (th >> 64) as u64;
            if thh != 0 {
                u64::from(thh.leading_zeros())
            } else if th != 0 {
                64 + u64::from((th as u64).leading_zeros())
            } else {
                128 + u64::from(tl.leading_zeros())
            }
        }

        /// Sum with error below 2 ulps.
        $(#[$feature])*
        fn add_tint(a: &Tint, b: &Tint) -> Tint {
            let (a, b) = match cmp_tint_abs(a, b) {
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
            // From now on |a| > |b|, thus a.ex >= b.ex.
            let sh = (a.ex as u64).wrapping_sub(b.ex as u64);
            let (t_h, t_l) = rshift(b, sh);
            let mut r = Tint {
                h: 0,
                m: 0,
                l: 0,
                ex: 0,
                sgn: a.sgn,
            };
            if a.sgn ^ b.sgn != 0 {
                // Subtraction.
                let tl = a.l.wrapping_sub(t_l);
                let th = a.hi().wrapping_sub(t_h).wrapping_sub(u128::from(tl > a.l));
                let ex = clz192(th, tl);
                if ex <= 1 || sh == 0 {
                    let (rh, rl) = lshift(th, tl, ex);
                    r.set_hi(rh);
                    r.l = rl;
                    r.ex = a.ex - ex as i64;
                } else {
                    // ex >= 2 and sh >= 1 (only when sh = 1): shift both operands
                    // left before subtracting, which makes the subtraction exact.
                    let (bh, bl) = lshift(b.hi(), b.l, ex - sh);
                    let (ah, al) = lshift(a.hi(), a.l, ex);
                    let tl = al.wrapping_sub(bl);
                    let th = ah.wrapping_sub(bh).wrapping_sub(u128::from(tl > al));
                    let ex1 = clz192(th, tl);
                    let (rh, rl) = lshift(th, tl, ex1);
                    r.set_hi(rh);
                    r.l = rl;
                    r.ex = a.ex - (ex + ex1) as i64;
                }
            } else {
                // Addition.
                let ah = a.hi();
                let al = a.l;
                let mut rl = al.wrapping_add(t_l);
                let cl = u64::from(rl < al);
                let mut rh = ah.wrapping_add(t_h);
                let mut ch = u64::from(rh < ah);
                rh = rh.wrapping_add(cl as u128);
                ch += u64::from(rh < cl as u128);
                if ch != 0 {
                    r.ex = a.ex + 1;
                    rl = ((rh << 63) as u64) | (rl >> 1);
                    rh = ((ch as u128) << 127) | (rh >> 1);
                } else {
                    r.ex = a.ex;
                }
                r.set_hi(rh);
                r.l = rl;
            }
            r
        }

        /// A nonzero finite `x` as a [`Tint`], exact.
        $(#[$feature])*
        fn tint_fromd(x: f64) -> Tint {
            let u = bits(x);
            let sgn = u >> 63;
            let ax = u & 0x7fffffffffffffff;
            let e = (ax >> 52) as i64;
            if e != 0 {
                Tint {
                    h: (1u64 << 63) | (ax << 11),
                    m: 0,
                    l: 0,
                    ex: e - 0x3fe,
                    sgn,
                }
            } else {
                let e = i64::from(ax.leading_zeros());
                Tint {
                    h: ax << e,
                    m: 0,
                    l: 0,
                    ex: -0x3f2 - e,
                    sgn,
                }
            }
        }

        #[$inline]
        $(#[$feature])*
        fn as_ldexp(x: f64, i: i64) -> f64 {
            from_bits(bits(x).wrapping_add((i as u64) << 52))
        }

        /// Round `a` to binary64, nearest even. `exact` says `a` holds a binary64
        /// value exactly (upstream's `err = 0`).
        $(#[$feature])*
        fn tint_tod(a: &Tint, exact: bool) -> f64 {
            if a.ex >= 1025 {
                // overflow: |a| >= 2^1024
                return if a.sgn != 0 {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                };
            }
            if a.ex <= -1074 {
                // underflow: |a| < 2^-1074
                let unit = if a.sgn != 0 {
                    h!("-0x1p-1074")
                } else {
                    h!("0x1p-1074")
                };
                if a.ex < -1074 {
                    return unit * 0.5;
                }
                // 2^-1075 <= |a| < 2^-1074
                let mid = a.h == 1u64 << 63 && a.m == 0 && a.l == 0;
                return unit * if mid { 0.5 } else { 0.75 };
            }
            let mut hh = a.h;
            let mut mm = a.m;
            let mut ll = a.l;
            let mut ex = a.ex;
            let mut low = hh & 0x7ff; // low 11 bits of h
            if ex <= -1022 {
                // Subnormal.
                let sh = (-1021 - ex) as u32; // 1 <= sh <= 52
                ll = (mm << (64 - sh)) | (ll >> sh) | u64::from(ll > 0);
                mm = (hh << (64 - sh)) | (mm >> sh);
                hh >>= sh;
                low = hh & 0x7ff;
                ex += i64::from(sh);
            }
            let mut h = (hh >> 11) as f64; // significant bits from h
            let l = if exact {
                0.0
            } else if low < 0x400 {
                0.25 // round to zero
            } else if low > 0x400 {
                0.75 // round away
            } else if mm == 0 && ll == 0 {
                0.5 // exact midpoint
            } else {
                0.75
            };
            let s = if a.sgn != 0 { -1.0 } else { 1.0 };
            h = fma(l, s, s * h);
            h *= h!("0x1p-52");
            // -1021 <= ex <= 1024, so 2^(ex-1) neither underflows nor overflows.
            h * as_ldexp(1.0, ex - 1)
        }

        /// 1/A, absolute error below 2^-103.9 for 1 <= r <= 2.
        $(#[$feature])*
        fn inv_tint(a: &Tint) -> Tint {
            let mut ad = tint_tod(a, true); // exact
            let subnormal = fabs(ad) < h!("0x1p-1022");
            if subnormal {
                ad *= h!("0x1p53");
            }
            let mut r = tint_fromd(1.0 / ad); // accurate to about 53 bits
            if subnormal {
                r.ex += 53;
            }
            // Newton's iteration r <- r + r (1 - a r).
            let mut q = mul_tint(a, &r);
            q.sgn = 1 - q.sgn;
            let q = add_tint(&ONE, &q);
            let q = mul_tint(&r, &q);
            add_tint(&r, &q)
        }

        /// b/a with relative error below 2^-185.53.
        $(#[$feature])*
        fn div_tint(b: &Tint, a: &Tint) -> Tint {
            let y = inv_tint(a);
            let r = mul_tint(&y, b);
            // Karp-Markstein: r1 = r0 + y (b - a r0).
            let mut z = mul_tint(a, &r);
            z.sgn = 1 - z.sgn;
            let z = add_tint(b, &z);
            let z = mul_tint(&y, &z);
            add_tint(&r, &z)
        }

        $(#[$feature])*
        fn div_tint_d(b: f64, a: f64) -> Tint {
            div_tint(&tint_fromd(b), &tint_fromd(a))
        }

        /// Accurate path: a [29,29] rational approximation of atan on [0, 1].
        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn atan2_accurate(y: f64, x: f64) -> f64 {
            // First the case t = y/x small and exact with x > 0, where atan(t)
            // rounds to t perturbed towards zero.
            let t = y / x;
            if t == 0.0 {
                if x > 0.0 {
                    return t;
                }
                return if y > 0.0 { PI_H + PI_L } else { -PI_H - PI_L };
            }
            let corr = fma(t, x, -y);
            if corr == 0.0 && x > 0.0 && fabs(t) <= h!("0x1.d12ed0af1a27fp-27") {
                // If |y| >= 2^-969, t*x - y cannot underflow.
                if fabs(y) >= h!("0x1p-915") {
                    return fma(t, h!("-0x1p-54"), t);
                }
                // Scale by 2^105 so that t*x - y does not underflow.
                let corr = fma(t * h!("0x1p105"), x, -y * h!("0x1p105"));
                if corr == 0.0 {
                    return fma(t, h!("-0x1p-54"), t);
                }
            }
            let inv = fabs(y) > fabs(x);
            let mut z = if inv {
                div_tint_d(x, y)
            } else {
                div_tint_d(y, x)
            };
            // For |y/x| < 2^-96 and x > 0, atan(y/x) rounds like y/x nudged towards
            // zero (subtract 2 from the low limb).
            if !inv && x > 0.0 && z.ex <= -96 {
                z.l = z.l.wrapping_sub(2);
                z.m = z.m.wrapping_sub(u64::from(z.l < 2));
                z.h = z.h.wrapping_sub(u64::from(z.m < 1));
                return tint_tod(&z, false);
            }
            // The rational approximation is for z > 0.
            let sz = z.sgn;
            z.sgn = 0;
            let mut p = P[29];
            let mut q = Q[29];
            for i in (0..29).rev() {
                p = mul_tint(&p, &z);
                q = mul_tint(&q, &z);
                p = add_tint(&p, &P[i]);
                q = add_tint(&q, &Q[i]);
            }
            p = mul_tint(&p, &z);
            let mut z = div_tint(&p, &q);
            let mut err = 662u64; // error bound for inv = 0 and x > 0
            z.sgn = sz;
            // z approximates atan(y/x) for inv = 0 and atan(x/y) for inv = 1, with
            // -pi/4 < z < pi/4.
            if inv {
                if z.sgn == 0 {
                    // pi/2 - atan(x/y)
                    z.sgn = 1;
                    z = add_tint(&PI2, &z);
                } else {
                    // -pi/2 - atan(x/y)
                    z = add_tint(&PI2, &z);
                    z.sgn = 1;
                }
                err = 524;
            }
            // For negative x, move to the opposite quadrant.
            if x < 0.0 {
                if z.sgn == 0 {
                    // first quadrant to third: subtract pi
                    z.sgn = 1;
                    z = add_tint(&PI, &z);
                    z.sgn = 1;
                } else {
                    // fourth quadrant to second: add pi
                    z = add_tint(&PI, &z);
                }
                err = 266;
            }
            // `err` bounds the error in ulps of the low limb. Upstream checks
            // whether that bound straddles a rounding boundary and aborts if so; no
            // input is known to get there. The port skips the check and rounds the
            // approximation, which is what upstream does whenever the check passes.
            let _ = err;
            tint_tod(&z, false)
        }

        #[$inline]
        $(#[$feature])*
        fn fastsum(xh: f64, xl: f64, yh: f64, yl: f64) -> (f64, f64) {
            let (sh, sl) = fasttwosum(xh, yh);
            (sh, (xl + yl) + sl)
        }

        /// Zeros, infinities and NaNs.
        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn atan2_special(y0: f64, x0: f64) -> f64 {
            let iy = bits(y0);
            let ix = bits(x0);
            let aiy = iy << 1;
            let aix = ix << 1;
            if aiy >= 0x7ffu64 << 53 || aix >= 0x7ffu64 << 53 {
                // NaN or infinity
                if aiy > 0x7ffu64 << 53 || aix > 0x7ffu64 << 53 {
                    return y0 + x0;
                }
                if aiy == 0x7ffu64 << 53 && aix == 0x7ffu64 << 53 {
                    // atan2(+-inf, -inf) = +-3pi/4, atan2(+-inf, +inf) = +-pi/4
                    const FINF: [[f64; 2]; 2] = [
                        [h!("0x1p-55"), h!("0x1.921fb54442d18p-1")],
                        [h!("0x1p-54"), h!("0x1.2d97c7f3321d2p+1")],
                    ];
                    let k = (ix >> 63) as usize;
                    return copysign(FINF[k][1], y0) + copysign(FINF[k][0], y0);
                }
                if aix == 0x7ffu64 << 53 {
                    if x0 < 0.0 {
                        return copysign(PI_H, y0) + copysign(PI_L, y0);
                    }
                    return copysign(0.0, y0);
                }
                // y = +-inf: +-pi/2 for finite x
                return copysign(PI_OVER2_H, y0) + copysign(PI_OVER2_L, y0);
            }
            if aiy == 0 || aix == 0 {
                if aiy == 0 && aix == 0 {
                    if ix == 0 {
                        return y0; // atan2(+-0, +0) = +-0
                    }
                    // atan2(+-0, -0) = +-pi
                    return if iy == 0 { PI_H + PI_L } else { -PI_H - PI_L };
                }
                if aiy == 0 {
                    if x0 > 0.0 {
                        return y0 / x0; // +-0
                    }
                    return if iy >> 63 == 0 {
                        PI_H + PI_L
                    } else {
                        -PI_H - PI_L
                    };
                }
                // Only x is zero.
                return if y0 > 0.0 {
                    PI_OVER2_H + PI_OVER2_L
                } else {
                    -PI_OVER2_H - PI_OVER2_L
                };
            }
            0.0
        }

        /// Correctly rounded two-argument arctangent atan2(y0, x0).
        $(#[$feature])*
        pub(crate) fn atan2(y0: f64, x0: f64) -> f64 {
            const MASK: u64 = 0x7fffffffffffffff;
            let iy = bits(y0);
            let ix = bits(x0);
            let aiy = iy & MASK;
            if aiy == 0 || aiy >= 0x7ffu64 << 52 {
                return atan2_special(y0, x0);
            }
            let aix = ix & MASK;
            if aix == 0 || aix >= 0x7ffu64 << 52 {
                return atan2_special(y0, x0);
            }
            let ax = fabs(x0);
            let ay = fabs(y0);
            let mut x = if ax > ay { ax } else { ay };
            let mut y = if ax > ay { ay } else { ax };
            let sy = iy >> 63;
            let sx = ix >> 63;
            let gt = u64::from(aix < aiy);
            let dxy = aix.wrapping_sub(aiy) ^ gt.wrapping_neg();
            if dxy >= 53u64 << 52 {
                return atan2_accurate(y0, x0);
            }
            const ASGN: [f64; 2] = [0.0, -0.0];
            let sgn = ASGN[(gt ^ sx ^ sy) as usize];
            let sgn1 = copysign(1.0, sgn);
            let kw = ((sx << 2) | (sy << 1) | gt) as usize;
            let jj = bits(y / x + (2.0 + 1.0 / 128.0));
            let jt = ((jj >> (52 - 7)) & 127) as usize;
            let mut fh = F2[jt][1] * sgn1;
            let mut fl = F2[jt][0] * sgn1;
            fh += O[kw][0];
            fl += O[kw][1];
            if x < h!("0x1p-920") {
                x *= h!("0x1p920");
                y *= h!("0x1p920");
            }
            if x > h!("0x1p1022") && jt != 0 {
                x *= 0.5;
                y *= 0.5;
            }
            let t0 = T2[jt];
            let zn = fma(-t0, x, y);
            let zd = fma(t0, y, x);
            let mut z = zn / zd;
            const B: [f64; 3] = [
                h!("-0x1.55555555554d2p-2"),
                h!("0x1.999999860e1cap-3"),
                h!("-0x1.248ad469844a1p-3"),
            ];
            let mut z2 = z * z;
            z *= sgn1;
            let dz = (z * z2) * (B[0] + z2 * (B[1] + z2 * B[2]));
            let mut eps = fabs(z) * h!("0x1.051p-51") + h!("0x1p-90");
            let (rh, zz) = fasttwosum(fh, z);
            let rl = (fl + dz) + zz;
            let mut lb = rh + (rl - eps);
            let mut ub = rh + (rl + eps);
            if lb != ub {
                let mut dh = y * t0;
                let mut dl = fma(y, t0, -dh);
                let e;
                (dh, e) = fasttwosum(x, dh);
                let rdh = 1.0 / dh;
                dl += e;
                let mut nh = x * t0;
                let mut nl = fma(x, t0, -nh);
                let dt = y - nh;
                let y1 = dt + nh;
                if y1 == y {
                    (nh, nl) = fasttwosum(dt, -nl);
                } else {
                    (nh, nl) = fasttwosum(dt, (y - y1) - nl);
                }
                let mut zh = nh * rdh;
                z2 = zh * zh;
                let mut zl = rdh * (fma(dh, -zh, nh) + (nl - (nh * rdh) * dl));
                const B2: [f64; 4] = [
                    h!("-0x1.5555555555555p-2"),
                    h!("0x1.999999999755ep-3"),
                    h!("-0x1.24924883596f8p-3"),
                    h!("0x1.c6f7d73531bc2p-4"),
                ];
                zl += zh * z2 * ((B2[0] + z2 * B2[1]) + (z2 * z2) * (B2[2] + z2 * B2[3]));
                zh *= sgn1;
                zl *= sgn1;
                eps = h!("0x1.4p-50") * (fabs(zh) * z2 + h!("0x1p-51"));
                (fh, fl) = fastsum(fh, fl, zh, zl);
                lb = fh + (fl - eps);
                ub = fh + (fl + eps);
                if lb != ub {
                    return atan2_accurate(y0, x0);
                }
            }
            ub
        }
    };
}

pub(crate) mod software {
    kernel!(software, crate::crmath::soft_fma, test, inline(always),);
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub(crate) mod hardware {
    kernel!(hardware, crate::crmath::x86_fma, any(), inline, #[target_feature(enable = "fma")]);
}

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
pub(crate) mod hardware {
    kernel!(hardware, crate::crmath::fma_dispatch, any(), inline(always),);
}

pub(crate) fn atan2(y0: f64, x0: f64) -> f64 {
    #[cfg(any(
        target_arch = "x86",
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_feature = "neon")
    ))]
    if crate::crmath::kernel_hardware_available() {
        #[cfg(test)]
        crate::crmath::tests::record_kernel_backend(true);
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        // SAFETY: kernel_hardware_available checked x86 FMA support before
        // entering this branch. Forced hardware tests also check support.
        return unsafe { hardware::atan2(y0, x0) };
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        return hardware::atan2(y0, x0);
    }
    #[cfg(test)]
    crate::crmath::tests::record_kernel_backend(false);
    software::atan2(y0, x0)
}
