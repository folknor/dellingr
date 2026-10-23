//! `asin`, ported from CORE-MATH `src/binary64/asin/asin.c` (Alexei
//! Sibidanov). Upstream reports an exhaustive search for
//! 2^-4 <= x < 1, with and without FMA contraction.

macro_rules! kernel {
    ($backend:ident, $fma:path, $test_cfg:meta, $inline:meta, $(#[$feature:meta])*) => {
        use $fma as fma;
        use crate::crmath::tables::asin::{ASIN_CC, ASIN_DB, ASIN_S};
        use crate::crmath::{bits, copysign, fabs, fasttwosum, roundeven_finite};

        #[$inline]
        $(#[$feature])*
        pub(crate) fn fastsum(xh: f64, xl: f64, yh: f64, yl: f64) -> (f64, f64) {
            let (sh, sl) = fasttwosum(xh, yh);
            (sh, (xl + yl) + sl)
        }

        #[$inline]
        $(#[$feature])*
        pub(crate) fn muldd(xh: f64, xl: f64, ch: f64, cl: f64) -> (f64, f64) {
            let ahhh = ch * xh;
            (ahhh, (cl * xh + ch * xl) + fma(ch, xh, -ahhh))
        }

        /// Double-double polynomial evaluation with a carried-in low part `l`.
        #[$inline]
        $(#[$feature])*
        pub(crate) fn polydd(xh: f64, xl: f64, c: &[[f64; 2]], l: f64) -> (f64, f64) {
            let mut i = c.len() - 1;
            let (mut ch, l) = fasttwosum(c[i][0], l);
            let mut cl = c[i][1] + l;
            while i > 0 {
                i -= 1;
                (ch, cl) = muldd(xh, xl, ch, cl);
                (ch, cl) = fastsum(c[i][0], c[i][1], ch, cl);
            }
            (ch, cl)
        }

        /// The degree-15 tail shared by the asin and acos refinement paths.
        pub(crate) const REFINE_C: [[f64; 2]; 5] = [
            [h!("0x1p+0"), h!("-0x1.fc2c76456515bp-108")],
            [h!("0x1.5555555555555p-3"), h!("0x1.5555555623513p-57")],
            [h!("0x1.3333333333333p-4"), h!("0x1.9997e3427441bp-59")],
            [h!("0x1.6db6db6db6db7p-5"), h!("-0x1.cb95ff08658e6p-62")],
            [h!("0x1.f1c71c71c6d5bp-6"), h!("0x1.b125bccdcc89ep-60")],
        ];
        pub(crate) const REFINE_CT: [f64; 3] = [
            h!("0x1.6e8ba2ec8cb69p-6"),
            h!("0x1.1c4ea7a15c997p-6"),
            h!("0x1.ca8355d39bb67p-7"),
        ];

        /// Correctly rounded arcsine.
        $(#[$feature])*
        pub(crate) fn asin(x: f64) -> f64 {
            let ix = bits(x);
            let ax = ix << 1;
            let (t, z, zl, jd, f0h, f0l, eps);
            if ax > 0x7fc0000000000000 {
                // |x| > 0.5
                const OFF: [[f64; 2]; 2] = [
                    [h!("0x1.921fb54442d18p+0"), h!("0x1.1a62633145c07p-54")],
                    [h!("-0x1.921fb54442d18p+0"), h!("-0x1.1a62633145c07p-54")],
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
                // asin(x) = pi/2 - 2 asin(sqrt((1 - x)/2)), and the odd extension.
                let t0 = 2.0 - 2.0 * fabs(x);
                jd = roundeven_finite(t0 * h!("0x1p5"));
                z = copysign(t0.sqrt(), -x);
                zl = fma(z, z, -t0) * ((-0.5 / t0) * z);
                t = 0.25 * t0 - jd * h!("0x1p-7");
                eps = fabs(z * t) * h!("0x1.99p-52");
            } else {
                // |x| <= 0.5
                if ax < 0x7cae26e892247dec {
                    // |asin(x) - x| < ulp/2 for |x| < 0x1.7137449123ef6p-26.
                    return fma(h!("0x1p-55"), x, x);
                }
                f0h = 0.0;
                f0l = 0.0;
                let t0 = x * x;
                jd = roundeven_finite(t0 * h!("0x1p7"));
                t = fma(x, x, h!("-0x1p-7") * jd);
                z = x;
                zl = 0.0;
                eps = fabs(z * t) * h!("0x1.77p-52");
            }
            // asin(xh + xl) = (xh + xl) * (cc[j][0] + (cc[j][1] + t * Poly(t)))
            let j = jd as usize;
            let c = &ASIN_CC[j];
            let t2 = t * t;
            let d = t * ((c[2] + t * c[3]) + t2 * ((c[4] + t * c[5]) + t2 * (c[6] + t * c[7])));
            let (fh, fl) = muldd(z, zl, c[0], c[1] + d);
            let (fh, fl) = fastsum(f0h, f0l, fh, fl);
            let lb = fh + (fl - eps);
            let ub = fh + (fl + eps);
            if lb != ub {
                return asin_refine(x, lb);
            }
            lb
        }

        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn asin_refine(x: f64, phi: f64) -> f64 {
            // With x = sin(phi), cos(phi) = sqrt(1 - x^2); rotate the angle close to
            // zero where the Taylor expansion of asin works well.
            let s2 = x * x;
            let dx2 = fma(x, x, -s2);
            let (c2h, c2l) = fasttwosum(1.0, -s2);
            let c2l = c2l - dx2;
            let (c2h, c2l) = fasttwosum(c2h, c2l);
            let ch = c2h.sqrt();
            let cl = (c2l - fma(ch, ch, -c2h)) * (0.5 / ch);
            let jf = roundeven_finite(fabs(phi) * h!("0x1.45f306dc9c883p+4"));
            let j = jf as usize; // 0 <= j <= 32
            let big_ch = ASIN_S[32 - j][1];
            let big_cl = ASIN_S[32 - j][0];
            let big_sh = ASIN_S[j][1];
            let big_sl = ASIN_S[j][0];
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
            let v = cs - sc;
            let dv = (big_ch * dsl + big_cl * dsh) - (big_sh * dcl + big_sl * dch) - (d_sc - d_cs);
            let (v, dv) = fasttwosum(v, dv);
            let sgn = copysign(1.0, x);
            let jt = jf * sgn; // 0 <= |jt| <= 64
            let (v2, dv2) = muldd(v, dv, v, dv);
            let v = v * sgn;
            let dv = dv * sgn;
            let ct = &REFINE_CT;
            let fl = v2 * (ct[0] + v2 * (ct[1] + v2 * ct[2]));
            let (fh, fl) = polydd(v2, dv2, &REFINE_C, fl);
            let (fh, fl) = muldd(v, dv, fh, fl);
            // ph + pl + ps approximates jt * pi/64; ph and pl are exact.
            let ph = jt * h!("0x1.921fb54442dp-5");
            let pl = h!("0x1.8469898cc518p-53") * jt;
            let ps = h!("-0x1.fc8f8cbb5bf6cp-102") * jt;
            let (pl, ps) = fastsum(fh, fl, pl, ps);
            let (ph, pl) = fasttwosum(ph, pl);
            let (pl, ps) = fasttwosum(pl, ps);
            let (ph, pl) = fasttwosum(ph, pl);
            let (pl, _ps) = fasttwosum(pl, ps);
            let th = bits(ph);
            let tl = bits(pl) & (!0u64 >> 1);
            let tn = (th & (0x7ffu64 << 52)).wrapping_sub(53u64 << 52);
            let dn = (tl as i64).wrapping_sub(tn as i64);
            let de = (tn.wrapping_sub(tl) >> 52) as i64;
            let hard = (-2..=0).contains(&dn) || de > 47;
            let res = ph + pl;
            if hard {
                return asin_database(x, res);
            }
            res
        }

        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn asin_database(x: f64, f: f64) -> f64 {
            let ax = fabs(x);
            let mut a = 0;
            let mut b = ASIN_DB.len();
            while a + 1 < b {
                let m = (a + b) / 2;
                if ASIN_DB[m][0] <= ax {
                    a = m;
                } else {
                    b = m;
                }
            }
            if ASIN_DB[a][0] == ax {
                return if x > 0.0 {
                    ASIN_DB[a][1] + ASIN_DB[a][2]
                } else {
                    -ASIN_DB[a][1] - ASIN_DB[a][2]
                };
            }
            f
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

pub(crate) fn asin(x: f64) -> f64 {
    #[cfg(any(
        target_arch = "x86",
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_feature = "neon")
    ))]
    if crate::crmath::kernel_hardware_available() {
        #[cfg(test)]
        crate::crmath::tests::record_kernel_backend(true);
        // SAFETY: dispatch checks the required CPU feature, including forced test modes.
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        return unsafe { hardware::asin(x) };
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        return hardware::asin(x);
    }
    #[cfg(test)]
    crate::crmath::tests::record_kernel_backend(false);
    software::asin(x)
}
