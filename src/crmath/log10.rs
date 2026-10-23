//! `log10`, ported from CORE-MATH `src/binary64/log10/log10.c`
//! (Paul Zimmermann and Tom Hubrecht).

macro_rules! kernel {
    ($backend:ident, $fma:path, $test_cfg:meta, $inline:meta, $(#[$feature:meta])*) => {
        use $fma as fma;
        use crate::crmath::dint::Dint;
        use crate::crmath::dint::v1::{
            LOG2, M_ONE, ONE_OVER_LOG10, add_dint, dint_fromd, dint_tod, mul_dint, mul_dint_2,
        };
        use crate::crmath::tables::log10::{INVERSE, INVERSE_2, LOG_INV, LOG_INV_2, P, P_2, POW10};
        use crate::crmath::{bits, fasttwosum, from_bits};

        /// `(ah + al) * (bh + bl)` without the `al * bl` term.
        #[$inline]
        $(#[$feature])*
        fn d_mul(ah: f64, al: f64, bh: f64, bl: f64) -> (f64, f64) {
            let hi = ah * bh;
            let s = fma(ah, bh, -hi); // exact
            let t = fma(al, bh, s);
            (hi, fma(ah, bl, t))
        }

        /// Double-double approximation of log10(2^e * x) for 1 <= x < 2, with
        /// absolute error below 2^-68.98.
        #[$inline]
        $(#[$feature])*
        fn log10_fast(e: i32, v: u64) -> (f64, f64) {
            let m = v & 0xfffffffffffff;
            // If x > sqrt(2), halve it to avoid cancellation.
            let c = u64::from(m >= 0x6a09e667f3bcd);
            // Normalise to [1/sqrt(2), sqrt(2)).
            let y = from_bits(((0x3ffu64 - c) << 52) + m);
            let e = e + c as i32;
            let em = 0x10000000000000 + m;
            let i = (em >> (43 + c)) as usize;
            const OFFSET: usize = 362;
            let r = INVERSE[i - OFFSET];
            let l1 = LOG_INV[i - OFFSET][0];
            let l2 = LOG_INV[i - OFFSET][1];
            let z = fma(r, y, -1.0); // exact
            let z2 = z * z;
            let p45 = fma(P[5], z, P[4]);
            let p23 = fma(P[3], z, P[2]);
            let mut ph = fma(p45, z2, p23);
            ph = fma(ph, z, P[1]);
            ph *= z2;
            let log2_h = h!("0x1.62e42fefa38p-1");
            let log2_l = h!("0x1.ef35793c7673p-45");
            let ee = f64::from(e);
            let (h, l) = fasttwosum(fma(ee, log2_h, l1), z);
            let l = ph + (l + l2);
            let l = fma(ee, log2_l, l);
            // Divide by log(10) by multiplying by an approximation of 1/log(10).
            d_mul(
                h,
                l,
                h!("0x1.bcb7b1526e50ep-2"),
                h!("0x1.95355baaafad3p-57"),
            )
        }

        $(#[$feature])*
        fn p_2(z: &Dint) -> Dint {
            let mut r = P_2[0];
            for coefficient in &P_2[1..] {
                r = mul_dint(z, &r);
                r = add_dint(coefficient, &r);
            }
            mul_dint(z, &r)
        }

        $(#[$feature])*
        fn accurate_log(x: &Dint) -> Dint {
            let mut x = *x;
            let mut e = x.ex;
            let mut i = (x.hi >> 55) as u16;
            if x.hi > 0xb504f333f9de6484 {
                e += 1;
                i >>= 1;
            }
            x.ex -= e;
            let z = mul_dint(&x, &INVERSE_2[usize::from(i) - 128]);
            let z = add_dint(&M_ONE, &z);
            let r = mul_dint_2(e, &LOG2);
            let p = p_2(&z);
            let p = add_dint(&LOG_INV_2[usize::from(i) - 128], &p);
            add_dint(&p, &r)
        }

        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn log10_accurate(x: f64) -> f64 {
            let xd = dint_fromd(x);
            let y = accurate_log(&xd);
            let y = mul_dint(&ONE_OVER_LOG10, &y);
            dint_tod(&y)
        }

        /// Correctly rounded base-10 logarithm.
        $(#[$feature])*
        pub(crate) fn log10(x: f64) -> f64 {
            let mut v = bits(x);
            let mut e = (v >> 52) as i32 - 0x3ff;
            if e >= 0x400 || e == -0x3ff {
                // x <= 0, NaN, infinity or subnormal.
                let minus_infinity = from_bits(0xfffu64 << 52);
                if e == 0x400 || (e == 0xc00 && x != minus_infinity) {
                    return x + x; // +inf or NaN
                }
                if x <= 0.0 {
                    if x < 0.0 {
                        return f64::NAN;
                    }
                    return f64::NEG_INFINITY;
                }
                // Subnormal.
                v = bits(from_bits(v) * h!("0x1p52"));
                e = (v >> 52) as i32 - 0x3ff - 52;
            }
            // x = 10^n for 0 <= n <= 22 gives n exactly. The index is computed in
            // wrapping 32-bit unsigned arithmetic, as upstream.
            let n = 0x267cfa2u32.wrapping_mul((e as u32).wrapping_add(1)) >> 27;
            if v == bits(POW10[n as usize]) {
                return f64::from(n);
            }
            let (h, l) = log10_fast(e, v);
            let err = h!("0x1.04p-69");
            let left = h + (l - err);
            let right = h + (l + err);
            if left == right {
                return left;
            }
            log10_accurate(x)
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

pub(crate) fn log10(x: f64) -> f64 {
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
        return unsafe { hardware::log10(x) };
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        return hardware::log10(x);
    }
    #[cfg(test)]
    crate::crmath::tests::record_kernel_backend(false);
    software::log10(x)
}
