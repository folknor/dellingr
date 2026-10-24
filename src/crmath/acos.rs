//! `acos`, ported from CORE-MATH `src/binary64/acos/acos.c` (Alexei
//! Sibidanov). Upstream reports an exhaustive search on
//! [-1, -2^-4] and [2^-4, 1], with and without FMA contraction.

macro_rules! kernel {
    ($backend:ident, $fma:path, $test_cfg:meta, $inline:meta, $(#[$feature:meta])*) => {
        use $fma as fma;
        use crate::crmath::asin::$backend::{REFINE_C, REFINE_CT, fastsum, muldd, polydd};
        use crate::crmath::tables::asin::{ACOS_CC, ACOS_S};
        use crate::crmath::{bits, copysign, fabs, fasttwosum, from_bits, roundeven_finite};

        /// TwoSum (Handbook of Floating-Point Arithmetic, Algorithm 4.4).
        #[$inline]
        $(#[$feature])*
        fn twosum(a: f64, b: f64) -> (f64, f64) {
            let s = a + b;
            let a_prime = s - b;
            let b_prime = s - a_prime;
            let delta_a = a - a_prime;
            let delta_b = b - b_prime;
            (s, delta_a + delta_b)
        }

        #[$inline]
        $(#[$feature])*
        fn sum(xh: f64, xl: f64, ch: f64, cl: f64) -> (f64, f64) {
            let (sh, sl) = twosum(xh, ch);
            (sh, (xl + cl) + sl)
        }

        /// Correctly rounded arccosine.
        $(#[$feature])*
        pub(crate) fn acos(x: f64) -> f64 {
            let ix = bits(x);
            let ax = ix << 1;
            let (t, z, zl, jd, f0h, f0l, eps);
            if ax > 0x7fc0000000000000 {
                // |x| > 0.5
                const OFF: [[f64; 2]; 2] = [
                    [0.0, 0.0],
                    [h!("0x1.921fb54442d18p+1"), h!("0x1.1a62633145c07p-53")],
                ];
                let k = (ix >> 63) as usize;
                f0h = OFF[k][0];
                f0l = OFF[k][1];
                if ax >= 0x7fe0000000000000 {
                    // |x| >= 1
                    if ax == 0x7fe0000000000000 {
                        return f0h + f0l; // |x| = 1
                    }
                    if ax > 0xffe0000000000000 {
                        return x + x; // NaN
                    }
                    return f64::NAN; // |x| > 1
                }
                // acos(x) = 2 asin(sqrt((1 - x)/2)) for x > 0.5, and
                // pi - 2 asin(sqrt((1 + x)/2)) for x < -0.5.
                let t0 = 2.0 - 2.0 * fabs(x);
                jd = roundeven_finite(t0 * h!("0x1p5"));
                z = copysign(t0.sqrt(), x);
                zl = fma(z, z, -t0) * ((-0.5 / t0) * z);
                t = 0.25 * t0 - jd * h!("0x1p-7");
                eps = fabs(z * t) * h!("0x1.8cp-52") + h!("0x1p-105");
            } else {
                // |x| <= 0.5
                f0h = h!("0x1.921fb54442d18p+0");
                f0l = h!("0x1.1a62633145c07p-54");
                if ax <= 0x7e00000000000000 {
                    // |x| < 2^-15
                    let c = h!("-0x1.5555555555555p-3");
                    // No spurious underflow for |x| <= 0x1.cb3b3869747f4p-55, where
                    // the rounding test always succeeds.
                    let v = if ax <= 0x791967670d2e8fe8 {
                        0.0
                    } else {
                        (x * x) * (c * x)
                    };
                    let (h, w) = fasttwosum(f0h, -x);
                    let l = v + (w + f0l);
                    let eps1 = h!("0x1.34p-79");
                    let lb = h + (l - eps1);
                    let ub = h + (l + eps1);
                    if lb != ub {
                        return acos_refine(x, lb);
                    }
                    return lb;
                }
                // acos(x) = pi/2 - asin(x), sharing asin's tables.
                let t0 = x * x;
                jd = roundeven_finite(t0 * h!("0x1p7"));
                t = fma(x, x, h!("-0x1p-7") * jd);
                z = -x;
                zl = 0.0;
                // eps < 0 for x > 0, but the rounding test is still correct.
                eps = (z * t) * h!("0x1.81p-52");
            }
            let j = jd as usize;
            let c = &ACOS_CC[j];
            let t2 = t * t;
            let d = t * ((c[2] + t * c[3]) + t2 * ((c[4] + t * c[5]) + t2 * (c[6] + t * c[7])));
            let (fh, fl) = muldd(z, zl, c[0], c[1] + d);
            let (fh, fl) = fastsum(f0h, f0l, fh, fl);
            let lb = fh + (fl - eps);
            let ub = fh + (fl + eps);
            if lb != ub {
                return acos_refine(x, lb);
            }
            lb
        }

        /// `phi` is the fast-path approximation of acos(x).
        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn acos_refine(x: f64, phi: f64) -> f64 {
            // With x = cos(phi), sin(phi) = sqrt(1 - x^2).
            let s2 = x * x;
            let dx2 = fma(x, x, -s2);
            let (c2h, c2l) = fasttwosum(1.0, -s2);
            let c2l = c2l - dx2;
            let (c2h, c2l) = fasttwosum(c2h, c2l);
            let ch = c2h.sqrt();
            let cl = (c2l - fma(ch, ch, -c2h)) * (0.5 / ch);
            // jf = round(|phi - pi/2| * 64/pi), held in a C int upstream.
            let jf = roundeven_finite(fabs(phi - h!("0x1.921fb54442d18p+0")) * h!("0x1.45f306dc9c883p+4"))
                as i32;
            let j = jf as usize; // 0 <= jf <= 32
            let big_ch = ACOS_S[32 - j][1];
            let big_cl = ACOS_S[32 - j][0];
            let big_sh = ACOS_S[j][1];
            let big_sl = ACOS_S[j][0];
            let ax = fabs(x);
            let dsh = ax - big_sh;
            let dsl = -big_sl;
            let dch = ch - big_ch;
            let dcl = cl - big_cl;
            let magic = h!("0x1.8p-4");
            let sc = fma(big_sh, dch, magic) - magic;
            let d_sc = fma(big_sh, dch, -sc);
            let cs = fma(big_ch, dsh, magic) - magic;
            let d_cs = fma(big_ch, dsh, -cs);
            let v = cs - sc; // exact
            let dv = (big_ch * dsl + big_cl * dsh) - (big_sh * dcl + big_sl * dch) - (d_sc - d_cs);
            let (v, dv) = fasttwosum(v, dv);
            let sgn = copysign(1.0, x);
            let jt = 32.0 - f64::from(jf) * sgn; // 0 <= jt <= 64
            let (v2, dv2) = muldd(v, dv, v, dv);
            let v = v * -sgn;
            let dv = dv * -sgn;
            let ct = &REFINE_CT;
            let fl = v2 * (ct[0] + v2 * (ct[1] + v2 * ct[2]));
            let (fh, fl) = polydd(v2, dv2, &REFINE_C, fl);
            let (fh, fl) = muldd(v, dv, fh, fl);
            // ph + pl + ps approximates jt * pi/64; ph and pl are exact.
            let ph = jt * h!("0x1.921fb54442dp-5");
            let pl = h!("0x1.8469898cc518p-53") * jt;
            let ps = h!("-0x1.fc8f8cbb5bf6cp-102") * jt;
            let (pl, ps) = sum(fh, fl, pl, ps);
            let (ph, pl) = fasttwosum(ph, pl);
            let (pl, ps) = fasttwosum(pl, ps);
            let (ph, pl) = fasttwosum(ph, pl);
            let (mut pl, ps) = fasttwosum(pl, ps);
            let mut t = bits(pl);
            let mut e = ((t >> 52) & 0x7ff) as i64 - 1023;
            e = 52 - (107 + e);
            e = e.clamp(0, 52);
            let m = (1u64 << 52) - (1u64 << e);
            let e = if e == 0 { 64 } else { e };
            if t.wrapping_add(1u64 << (e - 1)) & m == 0 {
                if x == h!("0x1.ffffffffffdc0p-1") {
                    return h!("0x1.8000000000024p-22") + h!("0x1p-76");
                }
                if x == h!("0x1.53ea6c7255e88p-4") {
                    return h!("0x1.7cdacb6bbe707p+0") + h!("0x1p-54");
                }
                if x == h!("0x1.fd737be914578p-11") {
                    return h!("0x1.91e006d41d8d8p+0") + h!("0x1.8p-53");
                }
                if x == h!("0x1.fffffffffff70p-1") {
                    return h!("0x1.8000000000009p-23") + h!("0x1p-77");
                }
                if x == h!("0x1.390e6939cd1a6p-5") {
                    return h!("0x1.8856a5d3296a4p+0") - h!("0x1p-109");
                }
                let w = bits(ps);
                if (w ^ t) >> 63 != 0 {
                    t = t.wrapping_sub(1);
                } else {
                    t = t.wrapping_add(1);
                }
                pl = from_bits(t);
            }
            ph + pl
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

pub(crate) fn acos(x: f64) -> f64 {
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
        return unsafe { hardware::acos(x) };
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        return hardware::acos(x);
    }
    #[cfg(test)]
    crate::crmath::tests::record_kernel_backend(false);
    software::acos(x)
}
