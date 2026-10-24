//! `exp`, ported from CORE-MATH `src/binary64/exp/exp.c`
//! (Alexei Sibidanov).

macro_rules! kernel {
    ($backend:ident, $fma:path, $test_cfg:meta, $inline:meta, $(#[$feature:meta])*) => {
        use $fma as fma;
        use crate::crmath::tables::exp::{EXP_DB, T0, T1};
        use crate::crmath::{bits, fasttwosum, from_bits, roundeven_finite};

        #[$inline]
        $(#[$feature])*
        fn fastsum(xh: f64, xl: f64, yh: f64, yl: f64) -> (f64, f64) {
            let (sh, sl) = fasttwosum(xh, yh);
            (sh, (xl + yl) + sl)
        }

        #[$inline]
        $(#[$feature])*
        fn muldd(xh: f64, xl: f64, ch: f64, cl: f64) -> (f64, f64) {
            let ahhh = ch * xh;
            (ahhh, (ch * xl + cl * xh) + fma(ch, xh, -ahhh))
        }

        /// Algorithm 4 of Jeannerod, Joldes, Louvet and Muller (ARITH 2026), with the
        /// last two FMAs replaced by a multiplication and an addition, as upstream.
        #[$inline]
        $(#[$feature])*
        fn fast_fma_dw(ah: f64, al: f64, bh: f64, bl: f64, ch: f64, cl: f64) -> (f64, f64) {
            let dh = fma(ah, bh, ch);
            let t = ch - dh;
            let e = fma(ah, bh, t);
            let f = e + cl;
            let g = ah * bl + f;
            (dh, al * bh + g)
        }

        #[$inline]
        $(#[$feature])*
        fn opolydd(xh: f64, xl: f64, c: &[[f64; 2]]) -> (f64, f64) {
            let mut i = c.len() - 1;
            let mut ch = c[i][0];
            let mut cl = c[i][1];
            while i > 0 {
                i -= 1;
                (ch, cl) = fast_fma_dw(xh, xl, ch, cl, c[i][0], c[i][1]);
            }
            (ch, cl)
        }

        /// `x * 2^i` by adding `i` to the exponent field; the caller guarantees the
        /// result stays normal.
        #[$inline]
        $(#[$feature])*
        fn as_ldexp(x: f64, i: i64) -> f64 {
            from_bits(bits(x).wrapping_add((i as u64) << 52))
        }

        /// Clears the exponent field, landing in the subnormal range.
        #[$inline]
        $(#[$feature])*
        fn as_todenormal(x: f64) -> f64 {
            from_bits(bits(x) & (!0u64 >> 12))
        }

        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn as_exp_database(x: f64, f: f64) -> f64 {
            let mut a = 0;
            let mut b = EXP_DB.len();
            while a + 1 < b {
                let m = (a + b) / 2;
                if x < EXP_DB[m][0] {
                    b = m;
                } else {
                    a = m;
                }
            }
            if x == EXP_DB[a][0] {
                return EXP_DB[a][1] + EXP_DB[a][2];
            }
            f
        }

        /// Split `x * 2^12 / log(2)` into table indices and the binary exponent.
        #[$inline]
        $(#[$feature])*
        fn reduce(x: f64) -> (f64, f64, f64, i64) {
            let s = h!("0x1.71547652b82fep+12");
            let t = roundeven_finite(x * s);
            let jt = t as i64;
            let i0 = ((jt >> 6) & 0x3f) as usize;
            let i1 = (jt & 0x3f) as usize;
            let ie = jt >> 12;
            let (th, tl) = muldd(T0[i0][1], T0[i0][0], T1[i1][1], T1[i1][0]);
            (t, th, tl, ie)
        }

        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn as_exp_accurate(x: f64) -> f64 {
            const CH: [[f64; 2]; 7] = [
                [h!("0x1p+0"), 0.0],
                [h!("0x1p-1"), h!("0x1.712f72ecec2cfp-99")],
                [h!("0x1.5555555555555p-3"), h!("0x1.5555555554d07p-57")],
                [h!("0x1.5555555555555p-5"), h!("0x1.55194d28275dap-59")],
                [h!("0x1.1111111111111p-7"), h!("0x1.12faa0e1c0f7bp-63")],
                [h!("0x1.6c16c16da6973p-10"), h!("-0x1.4ba45ab25d2a3p-64")],
                [h!("0x1.a01a019eb7f31p-13"), h!("-0x1.9091d845ecd36p-67")],
            ];
            let ix = bits(x);
            let (t, th, tl, ie) = reduce(x);
            let l2h = h!("0x1.62e42ffp-13");
            let l2l = h!("0x1.718432a1b0e26p-47");
            let l2ll = h!("0x1.9ff0342542fc3p-102");
            let dx = x - l2h * t;
            let dxl = l2l * t;
            let dxll = l2ll * t + fma(l2l, t, -dxl);
            let (dxh, dxl) = fasttwosum(dx, dxl);
            let dxl = dxl + dxll;
            let (fh, fl) = opolydd(dxh, dxl, &CH);
            let (fh, fl) = muldd(dxh, dxl, fh, fl);
            let (fh, fl) = muldd(fh, fl, th, tl);
            let (fh, mut fl) = fastsum(th, tl, fh, fl);
            if ix > 0xc086232bdd7abcd2 {
                // x < -0x1.6232bdd7abcd2p+9
                let scale = from_bits(((1 - ie) as u64) << 52);
                let (fh, e) = fasttwosum(scale, fh);
                fl += e;
                as_todenormal(fh + fl)
            } else {
                let (mut fh, fl) = fasttwosum(fh, fl);
                // d <= 4 only for the single argument x = -0x1.07e05e6e03b83p-2.
                let d = bits(fl).wrapping_add(4) & (!0u64 >> 12);
                if d <= 4 {
                    fh = as_exp_database(x, fh);
                }
                as_ldexp(fh, ie)
            }
        }

        /// Correctly rounded `e^x`.
        $(#[$feature])*
        pub(crate) fn exp(x: f64) -> f64 {
            let ix = bits(x);
            let aix = ix & (!0u64 >> 1);
            // exp(x) rounds to 1 for |x| <= 0x1p-54.
            if aix <= 0x3c90000000000000 {
                return 1.0 + x;
            }
            if aix >= 0x40862e42fefa39f0 {
                // |x| >= 0x1.62e42fefa39fp+9
                if aix > 0x7ff0000000000000 {
                    return x + x; // NaN
                }
                if aix == 0x7ff0000000000000 {
                    return if ix >> 63 != 0 { 0.0 } else { x };
                }
                if ix >> 63 == 0 {
                    return f64::INFINITY; // overflow
                }
                if aix >= 0x40874910d52d3052 {
                    // x <= -0x1.74910d52d3052p+9: rounds to +0.
                    return 0.0;
                }
            }
            let (t, th, tl, ie) = reduce(x);
            let l2h = h!("0x1.62e42ffp-13");
            let l2l = h!("0x1.718432a1b0e26p-47");
            // Cody-Waite reduction: |t| < 2^23 and l2h fits in 29 bits, so l2h * t is
            // exact.
            let dx = (x - l2h * t) + l2l * t;
            let dx2 = dx * dx;
            const CH: [f64; 4] = [
                h!("0x1p+0"),
                h!("0x1p-1"),
                h!("0x1.55555557e54ffp-3"),
                h!("0x1.55555553a12f4p-5"),
            ];
            let p = (CH[0] + dx * CH[1]) + dx2 * (CH[2] + dx * CH[3]);
            let fh = th;
            let tx = th * dx;
            let fl = tl + tx * p;
            let eps = h!("0x1.27p-62");
            if ix > 0xc086232bdd7abcd2 {
                // Subnormal result: x < -0x1.6232bdd7abcd2p+9.
                let scale = from_bits(((1 - ie) as u64) << 52);
                let (fh, e) = fasttwosum(scale, fh);
                let fl = fl + e;
                let ub = fh + (fl + eps);
                let lb = fh + (fl - eps);
                if ub != lb {
                    return as_exp_accurate(x);
                }
                as_todenormal(lb)
            } else {
                let ub = fh + (fl + eps);
                let lb = fh + (fl - eps);
                if ub != lb {
                    return as_exp_accurate(x);
                }
                as_ldexp(lb, ie)
            }
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

pub(crate) fn exp(x: f64) -> f64 {
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
        return unsafe { hardware::exp(x) };
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        return hardware::exp(x);
    }
    #[cfg(test)]
    crate::crmath::tests::record_kernel_backend(false);
    software::exp(x)
}
