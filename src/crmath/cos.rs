//! `cos`, ported from CORE-MATH `src/binary64/cos/cos.c` (Paul Zimmermann
//! and Tom Hubrecht).

macro_rules! kernel {
    ($backend:ident, $fma:path, $test_cfg:meta, $inline:meta, $(#[$feature:meta])*) => {
        use $fma as fma;
        use crate::crmath::dint::v2::add_dint;
        use crate::crmath::tables::trig::{C, S, SC, T};
        use crate::crmath::trig_dint::$backend::{
            MAGIC, a_mul, dint_fromd, dint_tod, eval_pc, eval_pc_fast, eval_ps, eval_ps_fast,
            floor_nonnegative, mul_dint, reduce, reduce2, s_mul, set_dd,
        };
        use crate::crmath::{bits, fabs, fasttwosum, from_bits};

        /// For 0x1.6a09e667f3bccp-27 < x < inf: `(i, h, l, err1)` with
        /// i/2^11 + h + l approximating frac(x/(2 pi)) within absolute error err1.
        #[$inline]
        $(#[$feature])*
        fn reduce_fast(x: f64) -> (u32, f64, f64, f64) {
            let (mut h, l, err1);
            if x <= h!("0x1.921fb54442d17p+2") {
                // x < 2 pi; |CH + CL - 1/(2 pi)| < 2^-110.523.
                let (hh, ll) = a_mul(h!("0x1.45f306dc9c883p-3"), x); // exact
                h = hh;
                l = fma(h!("-0x1.6b01ec5417056p-57"), x, ll);
                err1 = h!("0x1.d9p-105") * h; // error < 2^-104.116 * h
            } else {
                let u = bits(x);
                let mut e = ((u >> 52) & 0x7ff) as i32; // 1025 <= e <= 2046
                let m = (1u64 << 52) | (u & 0xfffffffffffff);
                let mut c = [0u64; 3];
                if e <= 1074 {
                    // 2^2 <= x < 2^52
                    let mut w = (m as u128) * (T[1] as u128);
                    c[0] = w as u64;
                    c[1] = (w >> 64) as u64;
                    w = (m as u128) * (T[0] as u128);
                    c[1] = c[1].wrapping_add(w as u64);
                    c[2] = ((w >> 64) as u64).wrapping_add(u64::from(c[1] < w as u64));
                    e = 1075 - e; // 1 <= e <= 50
                } else {
                    // 2^52 <= x < 2^1024
                    let i = ((e - 1138 + 63) / 64) as usize; // 0 <= i <= 15
                    let mut w = (m as u128) * (T[i + 2] as u128);
                    c[0] = w as u64;
                    c[1] = (w >> 64) as u64;
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
                err1 = h!("0x1.01p-76");
            }
            let i = floor_nonnegative(h * h!("0x1p11"));
            h = fma(i, h!("-0x1p-11"), h);
            (i as u32, h, l, err1)
        }

        /// Fast path for regular x > 0x1.6a09e667f3bccp-27: `(h, l, err)` with
        /// |h + l - cos(x)| < err.
        #[$inline]
        $(#[$feature])*
        fn cos_fast(x: f64) -> (f64, f64, f64) {
            let (mut i, mut h, mut l, err1) = reduce_fast(x);
            let mut neg = 0;
            let mut is_cos = 1;
            // i >= 2^10: pi <= x <= 2 pi, cos(pi + x) = -cos(x).
            neg ^= i >> 10;
            i &= 0x3ff;
            // i >= 2^9: pi/2 <= x <= pi, cos(pi/2 + x) = -sin(x).
            is_cos ^= i >> 9;
            neg ^= i >> 9;
            i &= 0x1ff;
            if i & 0x100 != 0 {
                // pi/4 <= x_red <= pi/2: cos(pi/2 - x) = sin(x). The subtraction is
                // exact (see upstream).
                is_cos = u32::from(is_cos == 0);
                i = 0x1ff - i;
                h = h!("0x1p-11") - h;
                l = -l;
            }
            let i = i as usize;
            // SC[] is evaluated at i/2^11 + SC[i][0]; this subtraction is exact.
            h -= SC[i][0];
            let (uh, ul) = a_mul(h, h);
            let ul = fma(h + h, l, ul);
            let (mut sh, mut sl) = eval_ps_fast(h, l, uh, ul);
            let (mut ch, mut cl) = eval_pc_fast(uh, ul);
            let err;
            if is_cos == 0 {
                (sh, sl) = s_mul(SC[i][2], sh, sl);
                (ch, cl) = s_mul(SC[i][1], ch, cl);
                let (hh, ll) = fasttwosum(ch, sh);
                h = hh;
                l = ll + (sl + cl);
                err = h!("0x1.55p-69");
            } else {
                (ch, cl) = s_mul(SC[i][2], ch, cl);
                (sh, sl) = s_mul(SC[i][1], sh, sl);
                let (hh, ll) = fasttwosum(ch, -sh);
                h = hh;
                l = ll + (cl - sl);
                err = h!("0x1.81p-69");
            }
            const SGN: [f64; 2] = [1.0, -1.0];
            h *= SGN[neg as usize];
            l *= SGN[neg as usize];
            (h, l, err + err1)
        }

        /// Accurate path for regular x > 0x1.6a09e667f3bccp-27.
        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn cos_accurate(x: f64) -> f64 {
            let mut xd = dint_fromd(x);
            reduce(&mut xd);
            let mut neg = false;
            let mut is_cos = true;
            let mut i = reduce2(&mut xd); // exact
            if i & 0x400 != 0 {
                // pi <= x < 2 pi: cos(x) = -cos(x - pi)
                neg = true;
                i &= 0x3ff;
            }
            if i & 0x200 != 0 {
                // pi/2 <= x < pi: cos(x) = -sin(x - pi/2)
                neg = !neg;
                is_cos = false;
                i &= 0x1ff;
            }
            if i & 0x100 != 0 {
                // pi/4 <= x < pi/2: cos(x) = sin(pi/2 - x)
                is_cos = !is_cos;
                xd.sgn = 1;
                xd = add_dint(&MAGIC, &xd); // X becomes 2^-11 - X
                i = 0x1ff - i;
            }
            let i = i as usize;
            let x2 = mul_dint(&xd, &xd);
            let mut u = eval_pc(&x2); // cos2pi(X)
            let mut v = eval_ps(&xd, &x2); // sin2pi(X)
            if is_cos {
                u = mul_dint(&C[i], &u);
                v = mul_dint(&S[i], &v);
                v.sgn = 1 - v.sgn;
            } else {
                u = mul_dint(&S[i], &u);
                v = mul_dint(&C[i], &v);
            }
            let mut u = add_dint(&u, &v);
            // The error is below 41 ulps of U.lo; check whether the upper 54 bits
            // are determined.
            let err = 41u64;
            let lo0 = u.lo.wrapping_sub(err);
            let hi0 = u.hi.wrapping_sub(u64::from(lo0 > u.lo));
            let lo1 = u.lo.wrapping_add(err);
            let hi1 = u.hi.wrapping_add(u64::from(lo1 < u.lo));
            if (hi0 >> 10) != (hi1 >> 10) {
                const EXCEPTIONS: [[f64; 3]; 5] = [
                    [
                        h!("0x1.8000000000009p-23"),
                        h!("0x1.fffffffffff7p-1"),
                        h!("0x1.b56666666666cp-143"),
                    ],
                    [
                        h!("0x1.8000000000024p-22"),
                        h!("0x1.ffffffffffdcp-1"),
                        h!("0x1.b56666666667ep-137"),
                    ],
                    [
                        h!("0x1.800000000009p-21"),
                        h!("0x1.ffffffffff7p-1"),
                        h!("0x1.b5666666666c4p-131"),
                    ],
                    [
                        h!("0x1.20000000000f3p-20"),
                        h!("0x1.fffffffffebcp-1"),
                        h!("0x1.37642666666fdp-127"),
                    ],
                    [
                        h!("0x1.800000000024p-20"),
                        h!("0x1.fffffffffdcp-1"),
                        h!("0x1.b5666666667ddp-125"),
                    ],
                ];
                for exception in &EXCEPTIONS {
                    if fabs(x) == exception[0] {
                        return exception[1] + exception[2];
                    }
                }
                // Otherwise a hard-to-round case; upstream reports all known ones
                // pass.
            }
            if neg {
                u.sgn = 1 - u.sgn;
            }
            dint_tod(&u)
        }

        /// Correctly rounded cosine.
        $(#[$feature])*
        pub(crate) fn cos(x: f64) -> f64 {
            let u = bits(x);
            let e = ((u >> 52) & 0x7ff) as i32;
            if e == 0x7ff {
                // NaN or infinity.
                if (u << 1) == 0x7ffu64 << 53 {
                    return f64::NAN;
                }
                return x + x;
            }
            let ax_bits = u & 0x7fffffffffffffff;
            let ax = from_bits(ax_bits);
            if ax_bits <= 0x3e46a09e667f3bcc {
                // |x| <= 0x1.6a09e667f3bccp-27: cos(x) rounds to 1 - 2^-28 |x|.
                return fma(ax, h!("-0x1p-28"), 1.0);
            }
            let (h, l, err) = cos_fast(ax);
            let left = h + (l - err);
            let right = h + (l + err);
            if left == right {
                return left;
            }
            cos_accurate(ax)
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

pub(crate) fn cos(x: f64) -> f64 {
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
        return unsafe { hardware::cos(x) };
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        return hardware::cos(x);
    }
    #[cfg(test)]
    crate::crmath::tests::record_kernel_backend(false);
    software::cos(x)
}
