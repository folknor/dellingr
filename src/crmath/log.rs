//! `log` (natural logarithm), ported from CORE-MATH
//! `src/binary64/log/log.c` (Paul Zimmermann and Tom Hubrecht).

macro_rules! kernel {
    ($backend:ident, $fma:path, $test_cfg:meta, $inline:meta, $(#[$feature:meta])*) => {
        use $fma as fma;
        use crate::crmath::dint::Dint;
        use crate::crmath::dint::v1::{LOG2, M_ONE, add_dint, dint_fromd, dint_tod, mul_dint, mul_dint_2};
        use crate::crmath::tables::log::{INVERSE, INVERSE_2, LOG_INV, LOG_INV_2, P, P_2};
        use crate::crmath::{bits, fasttwosum, from_bits};

        /// Double-double approximation of log(2^e * x) for 1 <= x < 2, with absolute
        /// error below 0x1.b6p-69.
        #[$inline]
        $(#[$feature])*
        fn log_fast(e: i32, v: u64) -> (f64, f64) {
            let m = 0x10000000000000 + (v & 0xfffffffffffff);
            // If x > sqrt(2), halve it to avoid cancellation.
            let c = usize::from(m >= 0x16a09e667f3bcd);
            let e = e + c as i32;
            const CY: [f64; 2] = [1.0, 0.5];
            const CM: [u32; 2] = [43, 44];
            let i = (m >> CM[c]) as usize;
            let y = from_bits(v) * CY[c];
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
            (h, l)
        }

        /// Degree-12 polynomial for the accurate path.
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
        fn log_2(x: &Dint) -> Dint {
            let mut x = *x;
            let mut e = x.ex;
            // Lookup index.
            let mut i = (x.hi >> 55) as u16;
            if x.hi > 0xb504f333f9de6484 {
                e += 1;
                i >>= 1;
            }
            x.ex -= e;
            let z = mul_dint(&x, &INVERSE_2[usize::from(i) - 128]);
            let z = add_dint(&M_ONE, &z);
            // E * log(2)
            let r = mul_dint_2(e, &LOG2);
            let p = p_2(&z);
            let p = add_dint(&LOG_INV_2[usize::from(i) - 128], &p);
            add_dint(&p, &r)
        }

        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn log_accurate(x: f64) -> f64 {
            let xd = dint_fromd(x);
            let y = log_2(&xd);
            dint_tod(&y)
        }

        /// Correctly rounded natural logarithm.
        $(#[$feature])*
        pub(crate) fn ln(x: f64) -> f64 {
            let mut v = bits(x);
            // C computes this in `int`: the biased exponent field minus the bias,
            // with the sign bit landing above it for negative inputs.
            let mut e = (v >> 52) as i32 - 0x3ff;
            if e >= 0x400 || e == -0x3ff {
                // x <= 0, NaN, infinity or subnormal.
                let minus_infinity = from_bits(0xfffu64 << 52);
                if e == 0x400 || (e == 0xc00 && x != minus_infinity) {
                    return x + x; // +inf or NaN
                }
                if x <= 0.0 {
                    // log(x < 0) is NaN, log(+-0) is -inf.
                    if x < 0.0 {
                        return f64::NAN;
                    }
                    return f64::NEG_INFINITY;
                }
                // Subnormal.
                v = bits(from_bits(v) * h!("0x1p52"));
                e = (v >> 52) as i32 - 0x3ff - 52;
            }
            // Normalise v to [1, 2).
            v = (0x3ffu64 << 52) | (v & 0xfffffffffffff);
            if v == 0x3ff0000000000000 && e == 0 {
                return 0.0; // x = 1
            }
            let (h, l) = log_fast(e, v);
            let err = h!("0x1.b6p-69");
            let left = h + (l - err);
            let right = h + (l + err);
            if left == right {
                return left;
            }
            // The fast path fails with probability about 2^-11.5.
            log_accurate(x)
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

pub(crate) fn ln(x: f64) -> f64 {
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
        return unsafe { hardware::ln(x) };
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        return hardware::ln(x);
    }
    #[cfg(test)]
    crate::crmath::tests::record_kernel_backend(false);
    software::ln(x)
}
