//! `cos`, ported from CORE-MATH `src/binary64/cos/cos.c` (Paul Zimmermann).
//! Upstream adapts it from sin.c and copies sin.c's tables and helpers, so
//! this port takes them from the `sin` kernel of the same backend. The proof
//! of the fast paths is upstream's cos.pdf.
//!
//! The fast phase is CORE-MATH's own, with its proved bound. Two cheaper
//! alternatives were refused: a home-grown, looser "common input" phase (no
//! proof, and a wider bound sends more calls to the accurate path), and
//! fusing `math.sin(a), math.cos(a)` in the compiler (it would have to keep
//! both lookups, both charges, the poll and the failure point). A joint
//! sine and cosine belongs in an explicit library function, if anywhere.

macro_rules! kernel {
    ($backend:ident, $fma:path, $test_cfg:meta, $inline:meta, $(#[$feature:meta])*) => {
        use $fma as fma;
        use crate::crmath::sin::$backend::{
            PIH, PIL, SGN, eval_pc, eval_ps, fastsum, mh_uu, muldd, reduce_large,
            reduce_large_acc, scaled_powers, u128_tod,
        };
        use crate::crmath::tables::sin::{C2U, S1U, S2U, U1, U2};
        use crate::crmath::{bits, fabs, roundeven_finite};

        /// Whether the accurate path's low limb is in the window upstream
        /// checks against its exception list.
        #[$inline]
        $(#[$feature])*
        pub(crate) fn exception_window(c1u: u128) -> bool {
            (c1u as u64).wrapping_add(5) <= 3
        }

        /// Inputs whose cosine the accurate path cannot settle, with the
        /// correctly rounded result as an unevaluated sum.
        #[$inline]
        $(#[$feature])*
        fn exceptions(x: f64, ret: f64) -> f64 {
            let ax = fabs(x);
            if ax == h!("0x1.8000000000009p-23") {
                return h!("0x1.fffffffffff7p-1") + h!("0x1.b56666666666cp-143");
            }
            if ax == h!("0x1.8000000000024p-22") {
                return h!("0x1.ffffffffffdcp-1") + h!("0x1.b56666666667ep-137");
            }
            if ax == h!("0x1.800000000009p-21") {
                return h!("0x1.ffffffffff7p-1") + h!("0x1.b5666666666c4p-131");
            }
            if ax == h!("0x1.20000000000f3p-20") {
                return h!("0x1.fffffffffebcp-1") + h!("0x1.37642666666fdp-127");
            }
            ret
        }

        /// Accurate path for 2^-27 <= x (all-integer, 128-bit fixed point).
        #[cold]
        #[inline(never)]
        $(#[$feature])*
        fn cos_large_accurate(x: f64) -> f64 {
            let (c1u, sbit) = accurate_magnitude(x);
            let ret = u128_tod(c1u, sbit);
            if exception_window(c1u) {
                return exceptions(x, ret);
            }
            ret
        }

        /// The accurate path before rounding: `(m, s)` with cos(x) about
        /// `(-1)^s * m / 2^128`.
        #[$inline]
        $(#[$feature])*
        pub(crate) fn accurate_magnitude(x: f64) -> (u128, usize) {
            let (k, r, neg) = reduce_large_acc(x);
            let (r, u2, u2h, u4) = scaled_powers(r);
            // With rho the signed residual (|r| / 2^128 in magnitude, sign `neg`):
            // cos(x) = (-1)^s [cos(t1 + t2) cos(2 pi rho) - sin(t1 + t2) sin(2 pi rho)]
            // with k = 2^12 s + 2^6 i1 + i2, t1 = pi i1/2^6, t2 = pi i2/2^12.
            let mut sbit = ((k >> 12) & 1) as usize;
            let i1 = ((k >> 6) & 0x3f) as usize;
            let i2 = (k & 0x3f) as usize;
            let s1 = if i1 <= 32 { S1U[i1] } else { S1U[64 - i1] };
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
            c1u = mh_uu(c1u, cr);
            s1u = mh_uu(s1u, sr);
            // c1u is |cos(z) cos(r)| (cos(z) <= 0 for i1 >= 32), s1u is
            // |sin(z) sin(r)| with the sign of r.
            if (i1 >= 32) ^ neg {
                c1u = c1u.wrapping_add(s1u);
            } else if c1u <= s1u {
                c1u = s1u - c1u;
                sbit ^= 1;
            } else {
                c1u -= s1u;
            }
            (c1u, sbit ^ usize::from(i1 >= 32))
        }

        /// Fast path for 2^-27 <= x < 2^31.
        #[$inline]
        $(#[$feature])*
        fn cos_moderate(x: f64) -> f64 {
            let invpi = h!("0x1.45f306dc9c883p+12");
            let k = roundeven_finite(invpi * x);
            let rh = fma(k, PIH, x); // exact
            let rl = k * PIL;
            let r = rh + rl;
            let r2 = r * r;
            let j = k as i64;
            let sbit = ((j >> 14) & 1) as usize; // odd multiple of pi?
            let i1 = ((j >> 7) & 0x7f) as usize;
            let i2 = (j & 0x7f) as usize;
            let (big_ch, big_cl, big_sh) = table_sum(i1, i2);
            let sh = r * (1.0 - h!("0x1.55555553068fp-3") * r2);
            let ch = r2 * (-0.5 + h!("0x1.55555553bfd3p-5") * r2);
            let fh = big_ch;
            let fl = big_cl + big_ch * ch - big_sh * sh;
            let fh = SGN[sbit] * fh;
            let fl = SGN[sbit] * fl;
            let eps = h!("0x1.dep-64");
            let lb = fh + (fl - eps);
            let ub = fh + (fl + eps);
            if ub == lb {
                return lb;
            }
            cos_large_accurate(x)
        }

        /// The table step shared by the two fast paths: `(Ch, Cl, Sh)` for the
        /// reduced index, Ch + Cl approximating cos(t1 + t2) and Sh
        /// sin(t1 + t2).
        #[$inline]
        $(#[$feature])*
        fn table_sum(i1: usize, i2: usize) -> (f64, f64, f64) {
            let (c1h, c1l) = muldd(U1[i1][2], U1[i1][3], U2[i2][2], U2[i2][3]);
            let (c2h, c2l) = muldd(U2[i2][0], U2[i2][1], U1[i1][0], U1[i1][1]);
            let (ch, cl) = fastsum(c1h, c1l, -c2h, -c2l);
            let sh = U1[i1][0] * U2[i2][2] + U1[i1][2] * U2[i2][0];
            (ch, cl, sh)
        }

        /// Fast path for x >= 2^31.
        #[inline(never)]
        $(#[$feature])*
        fn cos_large(x: f64) -> f64 {
            let (j, r) = reduce_large(x);
            let r2 = r * r;
            let sbit = (j >> 14) as usize;
            let i1 = ((j >> 7) & 0x7f) as usize;
            let i2 = (j & 0x7f) as usize;
            let (big_ch, big_cl, big_sh) = table_sum(i1, i2);
            let sh = r * (h!("0x1.921fb54442d18p2") - h!("0x1.4abbcdb6b26d1p5") * r2);
            let ch = r2 * (h!("-0x1.3bd3cc9be45dep4") + h!("0x1.03c1eee483083p6") * r2);
            let fh = big_ch;
            let fl = big_cl + big_ch * ch - big_sh * sh;
            let fh = SGN[sbit] * fh;
            let fl = SGN[sbit] * fl;
            let eps = h!("0x1.01p-63");
            let lb = fh + (fl - eps);
            let ub = fh + (fl + eps);
            if lb == ub {
                return lb;
            }
            cos_large_accurate(x)
        }

        /// Correctly rounded cosine.
        $(#[$feature])*
        pub(crate) fn cos(x: f64) -> f64 {
            let z = fabs(x);
            let e = (bits(z) >> 52) as i32;
            if e < 0x3ff - 27 {
                // |x| < 2^-27: cos(x) rounds to 1.
                return fma(x, -x, 1.0);
            }
            if e < 0x3ff + 31 {
                return cos_moderate(z);
            }
            if e == 0x7ff {
                return f64::NAN; // NaN or infinity
            }
            cos_large(z)
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

#[cfg(test)]
mod tests {
    use super::software;
    use crate::crmath::hf;

    /// The four inputs on upstream's exception list land in the trigger window
    /// on the accurate path, and the public result is correct. Round to
    /// nearest settles them on the fast path, so the result check alone would
    /// not see a broken trigger; the lookup call itself is not observed.
    #[test]
    fn accurate_path_exceptions_hit_the_window() {
        for (x, expected) in [
            ("0x1.8000000000009p-23", "0x1.fffffffffff7p-1"),
            ("0x1.8000000000024p-22", "0x1.ffffffffffdcp-1"),
            ("0x1.800000000009p-21", "0x1.ffffffffff7p-1"),
            ("0x1.20000000000f3p-20", "0x1.fffffffffebcp-1"),
        ] {
            let x = hf(x);
            let (magnitude, _) = software::accurate_magnitude(x);
            if !software::exception_window(magnitude) {
                panic!("cos({x:e}) misses the exception window");
            }
            for x in [x, -x] {
                if software::cos(x).to_bits() != hf(expected).to_bits() {
                    panic!("cos({x:e}) gave {:e}", software::cos(x));
                }
            }
        }
        if software::exception_window(0) || !software::exception_window(u128::from(u64::MAX - 1)) {
            panic!("exception window bounds");
        }
        if !software::exception_window(u128::from(u64::MAX - 4))
            || software::exception_window(u128::from(u64::MAX))
        {
            panic!("exception window edges");
        }
    }
}
