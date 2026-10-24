//! `tan`, ported from CORE-MATH `src/binary64/tan/tan.c` (Paul Zimmermann
//! and Tom Hubrecht).

macro_rules! kernel {
    ($backend:ident, $fma:path, $test_cfg:meta, $inline:meta, $(#[$feature:meta])*) => {
        use $fma as fma;
        use crate::crmath::dint::Dint;
        use crate::crmath::dint::v2::add_dint;
        use crate::crmath::tables::trig::{C, S, SC, T, TINV};
        use crate::crmath::trig_dint::$backend::{
            MAGIC, ONE, a_mul, dint_fromd, dint_tod, eval_pc, eval_pc_fast, eval_ps, eval_ps_fast,
            floor_nonnegative, mul_dint, mul_dint_21, reduce, reduce2, s_mul, set_dd,
        };
        use crate::crmath::{bits, fabs, fasttwosum};

        /// 1/a with relative error below 2^-124.999, for nonzero normalised `a`.
        $(#[$feature])*
        fn inv_dint(a: &Dint) -> Dint {
            let h = a.hi; // 2^63 <= h < 2^64
            let i = ((h >> 55) & 0xff) as usize;
            let mut t = TINV[i];
            let one = 1u128 << 127;
            // Three Newton iterations on a 64-bit inverse.
            let mut e = one.wrapping_sub((h as u128) * (t as u128));
            e = (t as u128).wrapping_mul(e >> 55);
            t = t.wrapping_add((e >> 72) as u64);
            e = one.wrapping_sub((h as u128) * (t as u128));
            e = (t as u128).wrapping_mul(e >> 47);
            t = t.wrapping_add((e >> 80) as u64);
            e = one.wrapping_sub((h as u128) * (t as u128));
            e = (t as u128).wrapping_mul(e >> 31);
            t = t.wrapping_add((e >> 96) as u64);
            let mut r = Dint {
                hi: t,
                lo: 0,
                ex: 1 - a.ex,
                sgn: 1,
            };
            // One more Newton step in 128-bit: r <- r + r * (1 - a * r).
            let q = mul_dint_21(a, &r); // -a*r
            r.sgn = 0;
            let q = add_dint(&ONE, &q); // 1 - a*r
            let q = mul_dint(&r, &q); // r * (1 - a*r)
            add_dint(&r, &q)
        }

        /// b/a with relative error below 2^-123.67.
        $(#[$feature])*
        fn div_dint(b: &Dint, a: &Dint) -> Dint {
            let r = inv_dint(a);
            mul_dint(&r, b)
        }

        /// For 0x1.7137449123ef6p-26 < x < inf: `(i, h, l)` with i/2^11 + h + l
        /// approximating frac(x/(2 pi)) within 2^-104.815.
        #[$inline]
        $(#[$feature])*
        fn reduce_fast(x: f64) -> (u32, f64, f64) {
            let (mut h, l);
            if x <= h!("0x1.921fb54442d17p+2") {
                let (hh, ll) = a_mul(h!("0x1.45f306dc9c883p-3"), x); // exact
                h = hh;
                l = fma(h!("-0x1.6b01ec5417056p-57"), x, ll);
            } else {
                let u = bits(x);
                let mut e = ((u >> 52) & 0x7ff) as i32; // 1025 <= e <= 2046
                let m = (1u64 << 52) | (u & 0xfffffffffffff);
                let mut c = [0u64; 3];
                if e <= 1074 {
                    let v = (m as u128) * (T[2] as u128);
                    let mut w = (m as u128) * (T[1] as u128);
                    c[0] = (w as u64).wrapping_add((v >> 64) as u64);
                    c[1] = ((w >> 64) as u64).wrapping_add(u64::from(c[0] < w as u64));
                    w = (m as u128) * (T[0] as u128);
                    c[1] = c[1].wrapping_add(w as u64);
                    c[2] = ((w >> 64) as u64).wrapping_add(u64::from(c[1] < w as u64));
                    e = 1075 - e; // 1 <= e <= 50
                } else {
                    let i = ((e - 1138 + 63) / 64) as usize; // 0 <= i <= 15
                    let v = (m as u128) * (T[i + 3] as u128);
                    let mut w = (m as u128) * (T[i + 2] as u128);
                    c[0] = (w as u64).wrapping_add((v >> 64) as u64);
                    c[1] = ((w >> 64) as u64).wrapping_add(u64::from(c[0] < w as u64));
                    w = (m as u128) * (T[i + 1] as u128);
                    c[1] = c[1].wrapping_add(w as u64);
                    c[2] = ((w >> 64) as u64).wrapping_add(u64::from(c[1] < w as u64));
                    w = (m as u128) * (T[i] as u128);
                    c[2] = c[2].wrapping_add(w as u64);
                    e = 1139 + ((i as i32) << 6) - e; // 1 <= e <= 64
                }
                if e == 64 {
                    c[0] = c[1];
                    c[1] = c[2];
                } else {
                    c[0] = (c[1] << (64 - e)) | (c[0] >> e);
                    c[1] = (c[2] << (64 - e)) | (c[1] >> e);
                }
                let (hh, ll) = set_dd(c[1], c[0]);
                h = hh;
                l = ll;
            }
            let i = floor_nonnegative(h * h!("0x1p11"));
            h = fma(i, h!("-0x1p-11"), h);
            (i as u32, h, l)
        }

        /// (bh + bl) / (ah + al) with relative error below 2^-96.99
        /// (Karp-Markstein).
        #[$inline]
        $(#[$feature])*
        fn fast_div(bh: f64, bl: f64, ah: f64, al: f64) -> (f64, f64) {
            let y = 1.0 / ah;
            let h = bh * y;
            let eh = fma(ah, -h, bh);
            let el = fma(al, -h, bl);
            (h, y * (eh + el))
        }

        /// Fast path for finite |x| > 0x1.d12ed0af1a27ep-27: `(h, l, err)` with
        /// |h + l - tan(x)| < err.
        #[$inline]
        $(#[$feature])*
        fn tan_fast(x: f64) -> (f64, f64, f64) {
            let mut neg = u32::from(x < 0.0);
            let mut is_tan = 1u32;
            let absx = if neg != 0 { -x } else { x };
            let (mut i, mut h, mut l) = reduce_fast(absx);
            // i >= 2^10: tan(pi + x) = tan(x).
            i &= 0x3ff;
            // i >= 2^9: tan(pi/2 + x) = -cot(x).
            is_tan ^= i >> 9;
            neg ^= i >> 9;
            i &= 0x1ff;
            if i & 0x100 != 0 {
                // pi/4 <= x_red <= pi/2: tan(pi/2 - x) = cot(x); exact subtraction.
                is_tan = u32::from(is_tan == 0);
                i = 0x1ff - i;
                h = h!("0x1p-11") - h;
                l = -l;
            }
            if i == 0 && h < h!("0x1p-37") {
                // Defer to the accurate path.
                return (h, l, 1.0);
            }
            let i = i as usize;
            h -= SC[i][0]; // exact
            (h, l) = fasttwosum(h, l);
            let (uh, ul) = a_mul(h, h);
            let ul = fma(h + h, l, ul);
            let (sh, sl) = eval_ps_fast(h, l, uh, ul);
            let (ch, cl) = eval_pc_fast(uh, ul);
            let (sh0, sl0) = s_mul(SC[i][2], sh, sl);
            let (ch0, cl0) = s_mul(SC[i][1], ch, cl);
            let (h1, l1) = fasttwosum(ch0, sh0);
            let l1 = l1 + (sl0 + cl0);
            let (ch, cl) = s_mul(SC[i][2], ch, cl);
            let (sh, sl) = s_mul(SC[i][1], sh, sl);
            let (h2, l2) = fasttwosum(ch, -sh);
            let l2 = l2 + (cl - sl);
            (h, l) = if is_tan != 0 {
                fast_div(h1, l1, h2, l2)
            } else {
                fast_div(h2, l2, h1, l1)
            };
            const SGN: [f64; 2] = [1.0, -1.0];
            h *= SGN[neg as usize];
            l *= SGN[neg as usize];
            (h, l, h * h!("0x1.1ap-66"))
        }

        /// Accurate path for regular |x| > 0x1.d12ed0af1a27ep-27.
        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn tan_accurate(x: f64) -> f64 {
            let absx = if x > 0.0 { x } else { -x };
            let mut xd = dint_fromd(absx);
            reduce(&mut xd);
            let mut is_tan = true;
            let mut neg = x < 0.0;
            let mut i = reduce2(&mut xd); // exact
            i &= 0x3ff; // tan(x) = tan(x - pi)
            if i & 0x200 != 0 {
                // pi/2 <= x < pi: tan(x) = -1/tan(x - pi/2)
                is_tan = false;
                neg = !neg;
                i &= 0x1ff;
            }
            if i & 0x100 != 0 {
                // pi/4 <= x < pi/2: tan(x) = 1/tan(pi/2 - x)
                is_tan = !is_tan;
                xd.sgn = 1;
                xd = add_dint(&MAGIC, &xd); // X becomes 2^-11 - X
                i = 0x1ff - i;
            }
            let i = i as usize;
            let x2 = mul_dint(&xd, &xd);
            let u = eval_pc(&x2); // cos2pi(X)
            let v = eval_ps(&xd, &x2); // sin2pi(X)
            // sin2pi(R) ~ sin2pi(i/2^11) cos2pi(X) + cos2pi(i/2^11) sin2pi(X)
            let uu = mul_dint(&S[i], &u);
            let vv = mul_dint(&C[i], &v);
            let sin = add_dint(&uu, &vv);
            // cos2pi(R) ~ cos2pi(i/2^11) cos2pi(X) - sin2pi(i/2^11) sin2pi(X)
            let u = mul_dint(&C[i], &u);
            let mut v = mul_dint(&S[i], &v);
            v.sgn = 1 - v.sgn;
            let cos = add_dint(&u, &v);
            let mut u = if is_tan {
                div_dint(&sin, &cos)
            } else {
                div_dint(&cos, &sin)
            };
            // Below 86 ulps of U.lo: check whether the upper 54 bits are determined.
            let err = 86u64;
            let lo0 = u.lo.wrapping_sub(err);
            let hi0 = u.hi.wrapping_sub(u64::from(lo0 > u.lo));
            let lo1 = u.lo.wrapping_add(err);
            let hi1 = u.hi.wrapping_add(u64::from(lo1 < u.lo));
            if (hi0 >> 10) != (hi1 >> 10) {
                const EXCEPTIONS: [[f64; 3]; 2] = [
                    [
                        h!("0x1.dffffffffff1fp-22"),
                        h!("0x1.e000000000151p-22"),
                        h!("0x1.fffffffffffffp-76"),
                    ],
                    [
                        h!("0x1.dfffffffffc7cp-21"),
                        h!("0x1.e000000000546p-21"),
                        h!("-0x1.658bcedb6e1d4p-147"),
                    ],
                ];
                for exception in &EXCEPTIONS {
                    if fabs(x) == exception[0] {
                        return if x > 0.0 {
                            exception[1] + exception[2]
                        } else {
                            -exception[1] - exception[2]
                        };
                    }
                }
            }
            if neg {
                u.sgn = 1 - u.sgn;
            }
            dint_tod(&u)
        }

        /// Correctly rounded tangent.
        $(#[$feature])*
        pub(crate) fn tan(x: f64) -> f64 {
            let u = bits(x);
            let e = ((u >> 52) & 0x7ff) as i32;
            if e == 0x7ff {
                if (u << 1) == 0x7ffu64 << 53 {
                    return f64::NAN; // tan(+-inf)
                }
                return x + x; // NaN
            }
            let ux = u & 0x7fffffffffffffff;
            if ux <= 0x3e4d12ed0af1a27e {
                // |x| <= 0x1.d12ed0af1a27ep-27: tan(x) rounds to x + 2^-54 x.
                if x == 0.0 {
                    return x;
                }
                return fma(x, h!("0x1p-54"), x);
            }
            let (h, l, err) = tan_fast(x);
            let left = h + (l - err);
            let right = h + (l + err);
            if left == right {
                return left;
            }
            tan_accurate(x)
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

pub(crate) fn tan(x: f64) -> f64 {
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
        return unsafe { hardware::tan(x) };
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        return hardware::tan(x);
    }
    #[cfg(test)]
    crate::crmath::tests::record_kernel_backend(false);
    software::tan(x)
}
