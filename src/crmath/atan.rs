//! `atan`, ported from CORE-MATH `src/binary64/atan/atan.c` (Alexei
//! Sibidanov).

macro_rules! kernel {
    ($backend:ident, $fma:path, $test_cfg:meta, $inline:meta, $(#[$feature:meta])*) => {
        use $fma as fma;
        use crate::crmath::tables::atan::{A, C, DB};
        use crate::crmath::{bits, copysign, fabs, fasttwosum, from_bits};

        #[$inline]
        $(#[$feature])*
        fn adddd(xh: f64, xl: f64, ch: f64, cl: f64) -> (f64, f64) {
            let s = xh + ch;
            let d = s - xh;
            (s, ((ch - d) + (xh + (d - s))) + (xl + cl))
        }

        #[$inline]
        $(#[$feature])*
        fn muldd_acc(xh: f64, xl: f64, ch: f64, cl: f64) -> (f64, f64) {
            let ahlh = ch * xl;
            let alhh = cl * xh;
            let ahhh = ch * xh;
            let mut ahhl = fma(ch, xh, -ahhh);
            ahhl += alhh + ahlh;
            fasttwosum(ahhh, ahhl)
        }

        $(#[$feature])*
        fn polydd(xh: f64, xl: f64, c: &[[f64; 2]], l: f64) -> (f64, f64) {
            let mut i = c.len() - 1;
            let mut ch = c[i][0] + l;
            let mut cl = (c[i][0] - ch) + l + c[i][1];
            while i > 0 {
                i -= 1;
                (ch, cl) = muldd_acc(xh, xl, ch, cl);
                let th = ch + c[i][0];
                let tl = (c[i][0] - th) + ch;
                ch = th;
                cl += tl + c[i][1];
            }
            (ch, cl)
        }

        /// Accurate path for 0x1p-27 <= |x|, where `a` is the fast-path
        /// approximation of atan(x) (so 0x1p-27 <= |a| <= pi/2).
        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn atan_refine2(x: f64, a: f64) -> f64 {
            const CH: [[f64; 2]; 3] = [
                [h!("-0x1.5555555555555p-2"), h!("-0x1.5555555555555p-56")],
                [h!("0x1.999999999999ap-3"), h!("-0x1.999999999bcb8p-57")],
                [h!("-0x1.2492492492492p-3"), h!("-0x1.249242093c016p-57")],
            ];
            const CL: [f64; 4] = [
                h!("0x1.c71c71c71c71cp-4"),
                h!("-0x1.745d1745d1265p-4"),
                h!("0x1.3b13b115bcbc4p-4"),
                h!("-0x1.1107c41ad3253p-4"),
            ];
            let phi = bits(fabs(a) * h!("0x1.45f306dc9c883p6") + 256.5);
            // 256.5 < phi < 384.5
            let i = ((phi >> (52 - 8)) & 0xff) as usize; // 0 <= i <= 128
            let (h, hl);
            if i == 128 {
                h = -1.0 / x;
                hl = fma(h, x, 1.0) * h;
            } else {
                let ta = copysign(A[i][0], x);
                let zta = x * ta;
                let ztal = fma(x, ta, -zta);
                let zmta = x - ta;
                let v = 1.0 + zta;
                let d = 1.0 - v;
                let ev = (d + zta) - ((d + v) - 1.0) + ztal;
                let r = 1.0 / v;
                let rl = (fma(r, -v, 1.0) - ev * r) * r;
                h = r * zmta;
                hl = fma(r, zmta, -h) + rl * zmta;
            }
            let (h2, h2l) = muldd_acc(h, hl, h, hl);
            let h4 = h2 * h2;
            let (h3, h3l) = muldd_acc(h, hl, h2, h2l);
            let fl = h2 * ((CL[0] + h2 * CL[1]) + h4 * (CL[2] + h2 * CL[3]));
            let (f, fl) = polydd(h2, h2l, &CH, fl);
            let (f, fl) = muldd_acc(h3, h3l, f, fl);
            let (ah, al, at);
            if i == 0 {
                ah = h;
                al = f;
                at = fl;
            } else {
                let mut df = 0.0;
                if i < 128 {
                    df = copysign(1.0, x) * A[i][1];
                }
                let id = copysign(i as f64, x);
                ah = h!("0x1.921fb54442dp-7") * id;
                let al0 = h!("0x1.8469898cc518p-55") * id;
                let at0 = h!("-0x1.fc8f8cbb5bf8p-104") * id;
                let (al1, at1) = adddd(al0, at0, df, 0.0);
                let (al2, at2) = adddd(al1, at1, h, hl);
                let (al3, at3) = adddd(al2, at2, f, fl);
                al = al3;
                at = at3;
            }
            let (v0, v2) = fasttwosum(ah, al);
            let (mut v1, v2) = fasttwosum(v2, at);
            let ax = fabs(x);
            let t0 = bits(v0);
            let mut t1 = bits(v1);
            // C evaluates the exponent difference in uint64_t, wrapping when v1's
            // exponent is the larger.
            let exp_gap = ((t0 >> 52) & 0x7ff).wrapping_sub((t1 >> 52) & 0x7ff);
            if (t1.wrapping_add(1) & (!0u64 >> 12)) <= 2 || exp_gap > 103 {
                for entry in &DB {
                    if ax == entry[0] {
                        return copysign(entry[1], x) + copysign(1.0, x) * entry[2];
                    }
                }
                if t1 & (!0u64 >> 12) == 0 {
                    let w = bits(v2);
                    if (w ^ t1) >> 63 != 0 {
                        t1 = t1.wrapping_sub(1);
                    } else {
                        t1 = t1.wrapping_add(1);
                    }
                    v1 = from_bits(t1);
                }
            }
            v1 + v0
        }

        /// Correctly rounded arctangent.
        $(#[$feature])*
        pub(crate) fn atan(x: f64) -> f64 {
            const CH: [f64; 4] = [
                h!("0x1p+0"),
                h!("-0x1.555555555552bp-2"),
                h!("0x1.9999999069c2p-3"),
                h!("-0x1.248d2c8444ac6p-3"),
            ];
            let t = bits(x);
            let at = t & (!0u64 >> 1); // |x|
            let mut i = (at >> 51) as i64 - 2030; // -2030 <= i <= 2065
            if at < 0x3f7b21c475e6362a {
                // |x| < 0x1.b21c475e6362ap-8
                if at == 0 {
                    return x; // atan(+-0) = +-0
                }
                const CH2: [f64; 4] = [
                    h!("-0x1.5555555555555p-2"),
                    h!("0x1.99999999998c1p-3"),
                    h!("-0x1.249249176aecp-3"),
                    h!("0x1.c711fd121ae8p-4"),
                ];
                if at < 0x3e40000000000000 {
                    // |x| < 2^-27
                    return fma(h!("-0x1p-54"), x, x);
                }
                let x2 = x * x;
                let x3 = x * x2;
                let x4 = x2 * x2;
                let f = x3 * ((CH2[0] + x2 * CH2[1]) + x4 * (CH2[2] + x2 * CH2[3]));
                let epsp = f * h!("0x1.6p-50");
                let epsm = f * h!("0x1.6p-51");
                let ub = (f + epsp) + x;
                let lb = (f - epsm) + x;
                if ub == lb {
                    return ub;
                }
                return atan_refine2(x, ub);
            }
            let (h, ah, mut al);
            if at > 0x4062ded8e34a9035 {
                // |x| > 0x1.2ded8e34a9035p+7
                ah = copysign(h!("0x1.921fb54442d18p+0"), x);
                al = copysign(h!("0x1.1a62633145c07p-54"), x);
                if at >= 0x434d02967c31cdb5 {
                    // |x| >= 0x1.d02967c31cdb5p+53
                    if at > 0x7ffu64 << 52 {
                        return x + x; // NaN
                    }
                    return ah + al;
                }
                h = -1.0 / x;
            } else {
                // 0x1.b21c475e6362ap-8 <= |x| <= 0x1.2ded8e34a9035p+7, so 1 <= i <= 30.
                let u = t & (!0u64 >> 13);
                let ut = u >> (51 - 16);
                let ut2 = (ut * ut) >> 16;
                let ci = &C[i as usize];
                // A crude piecewise quadratic estimate of the table index (C
                // unsigned arithmetic).
                i = (((ci[0] as u64) << 16)
                    .wrapping_add(ut.wrapping_mul(ci[1] as u64))
                    .wrapping_sub(ut2.wrapping_mul(ci[2] as u64))
                    >> (16 + 9)) as i64;
                let iu = i as usize;
                let ta = copysign(1.0, x) * A[iu][0];
                let id = copysign(1.0, x) * i as f64;
                al = copysign(1.0, x) * A[iu][1] + h!("0x1.8469898cc517p-55") * id;
                h = (x - ta) / (1.0 + x * ta);
                ah = h!("0x1.921fb54442dp-7") * id;
            }
            let h2 = h * h;
            let h4 = h2 * h2;
            let f = (CH[0] + h2 * CH[1]) + h4 * (CH[2] + h2 * CH[3]);
            al = fma(h, f, al);
            let e = h * h!("0x3.fp-52");
            let ub = (al + e) + ah;
            let lb = (al - e) + ah;
            if ub == lb {
                return ub;
            }
            atan_refine2(x, ub)
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

pub(crate) fn atan(x: f64) -> f64 {
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
        return unsafe { hardware::atan(x) };
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        return hardware::atan(x);
    }
    #[cfg(test)]
    crate::crmath::tests::record_kernel_backend(false);
    software::atan(x)
}
