//! `sin`, ported from CORE-MATH `src/binary64/sin/sin.c` (Paul Zimmermann,
//! Tom Hubrecht and Alexei Sibidanov). The proof of the fast paths is
//! upstream's sin.pdf; the hard-to-round cases come from Lefèvre, Ly and
//! Zimmermann, "Computing hard-to-round cases of sin, cos, tan in double
//! precision", ARITH 2026.

macro_rules! kernel {
    ($backend:ident, $fma:path, $test_cfg:meta, $inline:meta, $(#[$feature:meta])*) => {
        use $fma as fma;
        use crate::crmath::tables::sin::{C2U, PC, PS, S1U, S2U, T, U1, U2};
        use crate::crmath::{bits, fabs, fasttwosum, from_bits, roundeven_finite, u128_lh};

        /// Rounds `(-1)^s * r / 2^128` to binary64, for nonzero `r` outside the
        /// subnormal range (`r >> 64` is nonzero for every reduced argument, since
        /// no binary64 lies closer than 2^-60.888 to a multiple of pi/2).
        #[$inline]
        $(#[$feature])*
        fn u128_tod(r: u128, s: usize) -> f64 {
            let sh = u64::from(((r >> 64) as u64).leading_zeros());
            let h = (r >> (75 - sh)) as u64; // upper 53 nonzero bits
            let rbit = ((r >> (74 - sh)) & 1) as usize; // round bit
            const SGN: [f64; 2] = [h!("0x1p-53"), h!("-0x1p-53")];
            let v = from_bits(bits(SGN[s]).wrapping_sub(sh << 52)); // scale by 2^-sh
            const LOW: [f64; 2] = [h!("0x1.8p-2"), h!("0x1.8p-1")];
            let a = h as f64 * v;
            let b = LOW[rbit] * v;
            a + b
        }

        /// High 128 bits of the 256-bit product, ignoring the low-by-low term.
        #[$inline]
        $(#[$feature])*
        fn mh_uu(a: u128, b: u128) -> u128 {
            let ah = (a >> 64) as u64;
            let al = a as u64;
            let bh = (b >> 64) as u64;
            let bl = b as u64;
            let ahbh = (ah as u128) * (bh as u128);
            let ahbl = (ah as u128) * (bl as u128);
            let albh = (al as u128) * (bh as u128);
            ahbh.wrapping_add((ahbl >> 64) + (albh >> 64))
        }

        /// sin(2 pi r) in fixed point for 0 <= r < 2^-14.
        #[$inline]
        $(#[$feature])*
        fn eval_ps(u: u128, u2: u128, u2h: u128, u4: u128) -> u128 {
            let sh = PS[2].wrapping_sub(PS[3].wrapping_mul(u2h));
            let sh = mh_uu(sh, u4);
            let s = mh_uu(PS[1], u2);
            let s = PS[0].wrapping_sub(s);
            let s = s.wrapping_add(sh);
            mh_uu(s, u)
        }

        /// cos(2 pi r) in fixed point for 0 <= r < 2^-14.
        #[$inline]
        $(#[$feature])*
        fn eval_pc(u2: u128, u2h: u128, u4: u128) -> u128 {
            let sh = PC[3].wrapping_sub(u2h.wrapping_mul(PC[4]));
            let sh = PC[2].wrapping_sub(mh_uu(sh, u2));
            let s = mh_uu(PC[1], u2);
            let s = PC[0].wrapping_sub(s);
            s.wrapping_add(mh_uu(sh, u4))
        }

        /// Argument reduction for |x| >= 2^31: `(k, r)` with
        /// x/(2 pi) mod 1 = k/2^15 + r + s, 0 <= r < 2^-15, 0 <= s < 2^-67.988.
        #[$inline]
        $(#[$feature])*
        fn reduce_large(x: f64) -> (u64, f64) {
            let u = bits(x);
            let e = ((u >> 52) & 0x7ff) as i32; // 1054 <= e <= 2046
            let m = (1u64 << 52) | (u & 0xfffffffffffff);
            let i = ((e - 1011) / 64) as usize;
            let f = ((e - 1011) & 0x3f) as u32;
            let (v0, v1) = if f == 0 {
                (T[i], T[i + 1])
            } else {
                (
                    (T[i] << f) | (T[i + 1] >> (64 - f)),
                    (T[i + 1] << f) | (T[i + 2] >> (64 - f)),
                )
            };
            let mut w = (v1 as u128) | ((v0 as u128) << 64);
            w = (m as u128).wrapping_mul(w);
            // Round r to nearest; the magic constant is 2^112 + 2^59 + 2^52.
            let magic: u128 = (1u128 << 112) + 0x810000000000000;
            w = w.wrapping_add(magic);
            let t = ((w << 15) >> 75) as u64; // next 53 bits after the first 15
            let r = t as f64 * h!("0x1p-68") - h!("0x1p-16");
            ((w >> 113) as u64, r)
        }

        #[$inline]
        $(#[$feature])*
        fn fastsum(xh: f64, xl: f64, yh: f64, yl: f64) -> (f64, f64) {
            let (sh, sl) = fasttwosum(xh, yh);
            (sh, (xl + yl) + sl)
        }

        #[$inline]
        $(#[$feature])*
        fn muldd(xh: f64, xl: f64, ch: f64, cl: f64) -> (f64, f64) {
            let ahhh = xh * ch;
            (ahhh, (xh * cl + xl * ch) + fma(xh, ch, -ahhh))
        }

        const PIH: f64 = h!("-0x1.921fb54442d18p-13");
        const PIL: f64 = h!("-0x1.1a62633145c07p-67");

        /// Argument reduction for the accurate path: `(k, r, neg)` with
        /// x/(2 pi) mod 1 = k/2^13 + r/2^128 + eps, |r/2^128| <= 2^-14,
        /// 0 <= eps < 2^-127.999, and `neg` the sign of r.
        $(#[$feature])*
        fn reduce_large_acc(x: f64) -> (u64, u128, bool) {
            let u = bits(x);
            let e = ((u >> 52) & 0x7ff) as i32;
            let m = (1u64 << 52) | (u & 0xfffffffffffff);
            let i = -1 + (e - 947) / 64;
            let f = ((e - 1011) & 0x3f) as u32;
            let tt = |j: i32| T[j as usize];
            let mut v0 = if i >= 0 { tt(i) } else { 0 };
            let mut v1 = tt(i + 1);
            let mut v2 = tt(i + 2);
            if f != 0 {
                v0 = (v0 << f) | (v1 >> (64 - f));
                v1 = (v1 << f) | (v2 >> (64 - f));
                v2 = (v2 << f) | (tt(i + 3) >> (64 - f));
            }
            let mut w = (v1 as u128) | ((v0 as u128) << 64);
            w = (m as u128).wrapping_mul(w);
            let v = (m as u128) * (v2 as u128);
            w = w.wrapping_add(v >> 64);
            let mut k = (w >> (128 - 13)) as u64;
            let mask: u128 = u128_lh(0xffffffffffffffff, 0x7ffffffffffff);
            w &= mask; // ignore the leading 13 bits
            // Round k to nearest so that |r/2^128| < 2^-14.
            let neg = (w >> 114) != 0;
            if neg {
                k = (k + 1) & ((1u64 << 13) - 1);
                w = mask.wrapping_add(1).wrapping_sub(w);
            }
            (k, w, neg)
        }

        /// Accurate path for |x| >= 2^-16 (all-integer, 128-bit fixed point).
        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn sin_large_accurate(x: f64) -> f64 {
            let (k, r, neg) = reduce_large_acc(x);
            // twopi / 2^128 approximates 2 pi / 2^3.
            let twopi: u128 = u128_lh(0xc4c6628b80dc1cd1, 0xc90fdaa22168c234);
            let r = mh_uu(twopi, r << 3); // r becomes 2 pi r
            let u2 = mh_uu(r, r);
            let u4 = mh_uu(u2, u2);
            let u2h = u2 >> 64;
            let mut sbit = usize::from(x <= 0.0);
            sbit ^= (k >> 12) as usize;
            let i1 = ((k >> 6) & 0x3f) as usize;
            let i2 = (k & 0x3f) as usize;
            // |sin| of the table angle, using sin(pi - x) = sin(x).
            let s1 = if i1 <= 32 { S1U[i1] } else { S1U[64 - i1] };
            // cos(x) = sin(x + pi/2), so |C1u[i1]| = |S1u[(i1 + 32) mod 64]|.
            let c1 = if i1 <= 32 { S1U[32 - i1] } else { S1U[i1 - 32] };
            let mut s1u = mh_uu(s1, C2U[i2]);
            let t = mh_uu(c1, S2U[i2]);
            s1u = if i1 < 32 {
                s1u.wrapping_add(t)
            } else {
                s1u.wrapping_sub(t)
            };
            let mut c1u = mh_uu(c1, C2U[i2]);
            let t = mh_uu(s1, S2U[i2]);
            c1u = if i1 < 32 {
                c1u.wrapping_sub(t)
            } else {
                c1u.wrapping_add(t)
            };
            let sr = eval_ps(r, u2, u2h, u4);
            let cr = eval_pc(u2, u2h, u4);
            s1u = mh_uu(s1u, cr);
            c1u = mh_uu(c1u, sr);
            if (i1 < 32) ^ neg {
                s1u = s1u.wrapping_add(c1u);
            } else if s1u < c1u {
                s1u = c1u - s1u;
                sbit ^= 1;
            } else {
                s1u -= c1u;
            }
            u128_tod(s1u, sbit)
        }

        /// Accurate path for |x| < 2^-16.
        #[$inline]
        $(#[$feature])*
        fn sin_small_accurate(x: f64) -> f64 {
            const C: [f64; 3] = [
                h!("-0x1.5555555555555p-3"),
                h!("-0x1.55554b00de7e8p-57"),
                h!("0x1.111111110848p-7"),
            ];
            let x2h = x * x;
            let x2l = fma(x, x, -x2h);
            let mut h = C[2] * x2h;
            h += C[1];
            let (h, l) = fasttwosum(C[0], h);
            let (h, l) = muldd(h, l, x2h, x2l);
            let (h, l) = muldd(h, l, x, 0.0);
            let (h, t) = fasttwosum(x, h);
            let l = l + t;
            h + l
        }

        /// The table step shared by the two fast paths: `(Sh, Sl, Ch)` for the
        /// reduced index `j`.
        #[$inline]
        $(#[$feature])*
        fn table_sum(i1: usize, i2: usize) -> (f64, f64, f64) {
            let (s1h, s1l) = muldd(U1[i1][0], U1[i1][1], U2[i2][2], U2[i2][3]);
            let (s2h, s2l) = muldd(U2[i2][0], U2[i2][1], U1[i1][2], U1[i1][3]);
            let (sh, sl) = fastsum(s1h, s1l, s2h, s2l);
            let ch = U1[i1][2] * U2[i2][2] - U1[i1][0] * U2[i2][0];
            (sh, sl, ch)
        }

        const SGN: [f64; 2] = [1.0, -1.0];

        /// Fast path for 0x1.7137449123ef6p-26 < |x| < 2^31.
        #[$inline]
        $(#[$feature])*
        fn sin_moderate(x: f64, sbit: usize) -> f64 {
            let ax = fabs(x);
            let invpi = h!("0x1.45f306dc9c883p+12");
            let k = roundeven_finite(invpi * ax);
            let rh = fma(k, PIH, ax); // exact
            let rl = k * PIL;
            let r = rh + rl;
            let r2 = r * r;
            let j = k as i64;
            let sbit = sbit ^ ((j >> 14) & 1) as usize; // odd multiple of pi?
            let i1 = ((j >> 7) & 0x7f) as usize;
            let i2 = (j & 0x7f) as usize;
            let (big_sh, big_sl, big_ch) = table_sum(i1, i2);
            let sh = r * (1.0 - h!("0x1.55555553068fp-3") * r2);
            let ch = r2 * (-0.5 + h!("0x1.55555553bfd3p-5") * r2);
            let fh = big_sh;
            let fl = big_sl + big_sh * ch + big_ch * sh;
            let eps = h!("0x1.dep-64");
            let fh = SGN[sbit] * fh;
            let fl = SGN[sbit] * fl;
            let lb = fh + (fl - eps);
            let ub = fh + (fl + eps);
            if ub == lb {
                return lb;
            }
            if fabs(x) < h!("0x1p-16") {
                return sin_small_accurate(x);
            }
            sin_large_accurate(x)
        }

        /// Fast path for |x| >= 2^31.
        #[inline(never)]
        $(#[$feature])*
        fn sin_large(x: f64) -> f64 {
            let ax = fabs(x);
            let (j, r) = reduce_large(ax);
            let sbit = usize::from(x <= 0.0);
            let r2 = r * r;
            let sbit = sbit ^ (j >> 14) as usize;
            let i1 = ((j >> 7) & 0x7f) as usize;
            let i2 = (j & 0x7f) as usize;
            let (big_sh, big_sl, big_ch) = table_sum(i1, i2);
            let sh = r * (h!("0x1.921fb54442d18p2") - h!("0x1.4abbcdb6b26d1p5") * r2);
            let ch = r2 * (h!("-0x1.3bd3cc9be45dep4") + h!("0x1.03c1eee483083p6") * r2);
            let fh = big_sh;
            let fl = big_sl + big_sh * ch + big_ch * sh;
            let fh = SGN[sbit] * fh;
            let fl = SGN[sbit] * fl;
            let eps = h!("0x1.01p-63");
            let lb = fh + (fl - eps);
            let ub = fh + (fl + eps);
            if lb == ub {
                return lb;
            }
            sin_large_accurate(x)
        }

        /// Correctly rounded sine.
        $(#[$feature])*
        pub(crate) fn sin(x: f64) -> f64 {
            let u = bits(x);
            let e = ((u >> 52) & 0x7ff) as i32;
            if e < 0x3ff - 26 {
                // |x| < 2^-26: sin(x) rounds to x - x^3/6, computed as x - 2^-54 x.
                if u << 1 == 0 {
                    return x;
                }
                return fma(x, h!("-0x1p-54"), x);
            }
            if e < 0x3ff + 31 {
                return sin_moderate(x, (u >> 63) as usize);
            }
            if e == 0x7ff {
                return f64::NAN; // NaN or infinity
            }
            sin_large(x)
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

pub(crate) fn sin(x: f64) -> f64 {
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
        return unsafe { hardware::sin(x) };
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        return hardware::sin(x);
    }
    #[cfg(test)]
    crate::crmath::tests::record_kernel_backend(false);
    software::sin(x)
}
