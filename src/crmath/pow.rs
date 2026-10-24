//! `pow` (and so the `^` operator), ported from CORE-MATH
//! `src/binary64/pow/pow.c`, `pow.h`, `dint.h` and `qint.h` (Tom Hubrecht and
//! Paul Zimmermann; "Towards a correctly-rounded and fast power function in
//! binary64 arithmetic", ARITH 2023).
//!
//! Three phases: a double-double approximation with a rounding test, a
//! 128-bit one with exact and midpoint detection, and a 256-bit one. Upstream
//! aborts if the third phase cannot certify the rounding; no input is known to
//! get there, and this port instead rounds the 256-bit approximation, which
//! keeps the result deterministic (see the module doc of `crmath`).
//!
//! Deliberate departures from upstream: every NaN is treated as quiet, so
//! `pow(NaN, +-0)` and `pow(1, NaN)` are 1 whatever the NaN's bits (upstream
//! distinguishes signalling NaNs, which would make NaN bits observable);
//! round-to-nearest only; no `errno` or exception flags.

macro_rules! kernel {
    ($backend:ident, $fma:path, $test_cfg:meta, $inline:meta, $(#[$feature:meta])*) => {
        use $fma as fma;
        use crate::crmath::dint::Dint;
        use crate::crmath::dint::v2::{
            LOG2, LOG2_INV, M_ONE, add_dint, add_dint_11, cmp_dint_abs, dint_fromd, mul_dint, mul_dint_11,
            mul_dint_21, mul_dint_int64,
        };
        use crate::crmath::qint::{
            LOG2_INV_Q, LOG2_Q, M_ONE_Q, Qint, add_qint, add_qint_22, mul_qint, mul_qint_2, mul_qint_11,
            mul_qint_21, mul_qint_22, mul_qint_31, mul_qint_33, mul_qint_41, qint_fromd, qint_tod,
            qint_toi,
        };
        use crate::crmath::tables::pow::{
            INVERSE, INVERSE_2_1, INVERSE_2_2, LOG_INV, LOG_INV_2_1, LOG_INV_2_2, P_1, P_2, Q_1, Q_2, T1,
            T1_2, T2, T2_2,
        };
        use crate::crmath::tables::pow_qint::{
            INVERSE_3_1, INVERSE_3_2, LOG_INV_3_1, LOG_INV_3_2, P_3, Q_3, T1_3, T2_3,
        };
        use crate::crmath::{bits, fabs, fasttwosum, from_bits, roundeven_finite};

        /************************** double-double helpers ****************************/

        /// `a + (bh + bl)`, assuming |a| >= |bh|.
        #[$inline]
        $(#[$feature])*
        fn fast_sum(a: f64, bh: f64, bl: f64) -> (f64, f64) {
            let (hi, lo) = fasttwosum(a, bh);
            (hi, lo + bl)
        }

        /// Exact product `a * b = hi + lo`.
        #[$inline]
        $(#[$feature])*
        fn a_mul(a: f64, b: f64) -> (f64, f64) {
            let hi = a * b;
            (hi, fma(a, b, -hi))
        }

        /// `a * (bh + bl)`.
        #[$inline]
        $(#[$feature])*
        fn s_mul(a: f64, bh: f64, bl: f64) -> (f64, f64) {
            let (hi, s) = a_mul(a, bh);
            (hi, fma(a, bl, s))
        }

        /// `(ah + al) * (bh + bl)` without the `al * bl` term.
        #[$inline]
        $(#[$feature])*
        fn d_mul(ah: f64, al: f64, bh: f64, bl: f64) -> (f64, f64) {
            let (hi, s) = a_mul(ah, bh);
            let t = fma(al, bh, s);
            (hi, fma(ah, bl, t))
        }

        #[$inline]
        $(#[$feature])*
        fn is_int(x: f64) -> bool {
            x == roundeven_finite(x)
        }

        /// `(e, m)` with `m` odd and `x = 2^e * m`, for nonzero finite `x`.
        #[$inline]
        $(#[$feature])*
        fn extract(x: f64) -> (i64, u64) {
            let u = bits(x);
            let mut e = ((u >> 52) & 0x7ff) as i64;
            let mut m = (u & (!0u64 >> 12)) + if e != 0 { 1u64 << 52 } else { 0 };
            let t = m.trailing_zeros();
            m >>= t;
            e = e + i64::from(t) - (0x433 - i64::from(e == 0));
            (e, m)
        }

        /// Round-half-away-from-zero for a nonnegative value below 2^52.
        #[$inline]
        $(#[$feature])*
        fn round_nonnegative(x: f64) -> f64 {
            let t = (x as u64) as f64;
            if x - t >= 0.5 { t + 1.0 } else { t }
        }

        /// `x * 2^e` in two steps, so that intermediate scalings stay finite.
        #[$inline]
        $(#[$feature])*
        fn pow2(x: f64, e: i64) -> f64 {
            let mut x = x;
            if e & 0x1 != 0 {
                x *= 2.0;
            }
            let e2 = from_bits((((e >> 1) + 0x3ff) as u64 & 0x7ff) << 52);
            (x * e2) * e2
        }

        /// Round to 54 bits, in the shortcut form used by `exact_pow`.
        #[$inline]
        $(#[$feature])*
        fn round_54(x: &Dint) -> (i64, i64) {
            (x.ex - 53, ((x.hi >> 10) + ((x.hi >> 9) & 0x1)) as i64)
        }

        /// Truncate to an integer (towards zero).
        #[$inline]
        $(#[$feature])*
        fn dint_toi(a: &Dint) -> i64 {
            if a.ex < 0 {
                return 0;
            }
            let r = (a.hi >> (63 - a.ex)) as i64;
            if a.sgn != 0 { r.wrapping_neg() } else { r }
        }

        /// Round `a` to binary64 when it lies in the subnormal range.
        $(#[$feature])*
        fn dint_tod_subnormal(a: &Dint) -> f64 {
            let ex = (-(1011 + a.ex)) as u64; // ex >= 12
            if ex >= 64 {
                // All bits disappear: |a| < 2^-1074.
                let rb = a.hi >> 63; // only used when ex = 64
                let sb = (a.hi << 1) | a.lo;
                let ret = if ex > 64 || rb == 0 || sb == 0 {
                    0.0
                } else {
                    h!("0x1p-1074")
                };
                return if a.sgn != 0 { -ret } else { ret };
            }
            let mut hi = a.hi >> ex;
            let rb = (a.hi >> (ex - 1)) & 0x1; // round bit
            let sb = u64::from((a.hi << (65 - ex)) != 0 || a.lo != 0); // sticky bit
            hi += if sb != 0 { rb } else { hi & rb };
            // hi <= 2^52 holds the low bits of the result; a carry into 2^52 is the
            // smallest normal number, which is what the encoding gives.
            from_bits(hi | (a.sgn << 63))
        }

        /// Round to binary64, nearest even.
        $(#[$feature])*
        fn dint_tod(a: &Dint) -> f64 {
            if a.ex < -1022 {
                return dint_tod_subnormal(a);
            }
            let mut r = from_bits((a.hi >> 11) | (0x3ffu64 << 52));
            let mut rd = 0.0;
            if (a.hi >> 10) & 0x1 != 0 {
                rd += h!("0x1p-53");
            }
            if a.hi & 0x3ff != 0 || a.lo != 0 {
                rd += h!("0x1p-54");
            }
            if a.sgn != 0 {
                rd = -rd;
            }
            r = from_bits(bits(r) | (a.sgn << 63));
            r += rd;
            let e;
            if a.ex > -1023 {
                if a.ex > 1023 {
                    if a.ex == 1024 {
                        r *= 2.0;
                        e = h!("0x1p+1023");
                    } else {
                        r = if a.sgn != 0 { -f64::MAX } else { f64::MAX };
                        e = f64::MAX;
                    }
                } else {
                    e = from_bits(((a.ex.wrapping_add(1023) as u64) & 0x7ff) << 52);
                }
            } else if a.ex < -1074 {
                if a.ex == -1075 {
                    r *= 0.5;
                    e = h!("0x1p-1074");
                } else {
                    r = h!("0x1p-1074");
                    e = h!("0x1p-1074");
                }
            } else {
                e = from_bits(1u64 << (a.ex + 1074));
            }
            r * e
        }

        /************************ approximations of exp(z) ***************************/

        /// exp(z) for |z| < 2^-12.905, relative error below 2^-64.902632.
        #[$inline]
        $(#[$feature])*
        fn q_1(z: f64) -> (f64, f64) {
            let mut q = fma(Q_1[4], z, Q_1[3]);
            q = fma(q, z, Q_1[2]);
            let h0 = fma(q, z, Q_1[1]);
            let (h1, l1) = a_mul(z, h0);
            fast_sum(Q_1[0], h1, l1)
        }

        /// exp(y) for |y| < 0.00016923, relative error below 2^-122.29.
        $(#[$feature])*
        fn q_2(y: &Dint) -> Dint {
            let mut r = mul_dint_11(y, &Q_2[0]);
            r = add_dint_11(&Q_2[1], &r);
            r = mul_dint_11(y, &r);
            r = add_dint_11(&Q_2[2], &r);
            r = mul_dint_11(y, &r);
            r = add_dint(&Q_2[3], &r);
            r = mul_dint(y, &r);
            r = add_dint(&Q_2[4], &r);
            r = mul_dint(y, &r);
            r = add_dint(&Q_2[5], &r);
            r = mul_dint(y, &r);
            r = add_dint(&Q_2[6], &r);
            r = mul_dint(y, &r);
            add_dint(&Q_2[7], &r)
        }

        /// exp(y) for |y| < 0.00016923, error below 2^-241.11.
        $(#[$feature])*
        fn q_3(y: &Qint) -> Qint {
            let mut r = mul_qint_11(y, &Q_3[0]);
            r = add_qint_22(&Q_3[1], &r);
            for coefficient in &Q_3[2..7] {
                r = mul_qint_22(y, &r);
                r = add_qint_22(coefficient, &r);
            }
            for coefficient in &Q_3[7..12] {
                r = mul_qint_33(y, &r);
                r = add_qint(coefficient, &r);
            }
            for coefficient in &Q_3[12..15] {
                r = mul_qint(y, &r);
                r = add_qint(coefficient, &r);
            }
            r
        }

        /********************** approximations of log(1+z) ***************************/

        /// log(1+z) - z for |z| <= 33*2^-13, see Lemma 2 of the reference.
        #[$inline]
        $(#[$feature])*
        fn p_1(z: f64) -> (f64, f64) {
            let (wh, wl) = a_mul(z, z);
            let t = fma(P_1[5], z, P_1[4]);
            let mut u = fma(P_1[3], z, P_1[2]);
            let mut v = fma(P_1[1], z, P_1[0]);
            u = fma(t, wh, u);
            v = fma(u, wh, v);
            u = v * wh;
            (-0.5 * wh, fma(u, z, -0.5 * wl))
        }

        /// log(1+z) for |z| <= 2^-13 with a zero low limb.
        $(#[$feature])*
        fn p_2(z: &Dint) -> Dint {
            let mut r = mul_dint_11(z, &P_2[0]);
            r = add_dint_11(&P_2[1], &r);
            r = mul_dint_11(z, &r);
            r = add_dint_11(&P_2[2], &r);
            r = mul_dint_11(z, &r);
            r = add_dint_11(&P_2[3], &r);
            r = mul_dint_11(z, &r);
            r = add_dint(&P_2[4], &r);
            r = mul_dint_21(&r, z);
            r = add_dint(&P_2[5], &r);
            r = mul_dint_21(&r, z);
            r = add_dint(&P_2[6], &r);
            r = mul_dint_21(&r, z);
            r = add_dint(&P_2[7], &r);
            r = mul_dint_21(&r, z);
            r = add_dint(&P_2[8], &r);
            mul_dint_21(&r, z)
        }

        /// log(1+z) for |z| <= 2^-13 with only the upper limb nonzero.
        $(#[$feature])*
        fn p_3(z: &Qint) -> Qint {
            let mut r = mul_qint_11(&P_3[0], z);
            r = add_qint_22(&P_3[1], &r);
            for coefficient in &P_3[2..4] {
                r = mul_qint_11(&r, z);
                r = add_qint_22(coefficient, &r);
            }
            for coefficient in &P_3[4..8] {
                r = mul_qint_21(&r, z);
                r = add_qint_22(coefficient, &r);
            }
            for coefficient in &P_3[8..14] {
                r = mul_qint_31(&r, z);
                r = add_qint(coefficient, &r);
            }
            for coefficient in &P_3[14..18] {
                r = mul_qint_41(&r, z);
                r = add_qint(coefficient, &r);
            }
            mul_qint_41(&r, z)
        }

        /// Double-double log(x) for 2^-1074 <= x <= DBL_MAX (Lemma 4 of the
        /// reference). The flag reports cancellation near x = 1, where the error
        /// bound is the larger one.
        #[$inline]
        $(#[$feature])*
        fn log_1(x: f64) -> (f64, f64, bool) {
            let u = bits(x);
            let mut m = u & (!0u64 >> 12);
            let mut e = ((u >> 52) & 0x7ff) as i64;
            let t_bits;
            if e != 0 {
                t_bits = m | (0x3ffu64 << 52);
                m += 1u64 << 52;
                e -= 0x3ff;
            } else {
                // Subnormal x.
                let k = m.leading_zeros() - 11;
                e = -0x3fe - i64::from(k);
                m <<= k;
                t_bits = m | (0x3ffu64 << 52);
            }
            // |x| = 2^e * t = 2^(e-52) * m with 1 <= t < 2 and 2^52 <= m < 2^53.
            let mut t = from_bits(t_bits);
            // If m > sqrt(2), halve it so that 1/sqrt(2) < t < sqrt(2).
            let c = usize::from(m >= 0x16a09e667f3bcd);
            const CY: [f64; 2] = [1.0, 0.5];
            const CM: [u32; 2] = [44, 45];
            e += c as i64;
            let big_e = e as f64;
            let i = (m >> CM[c]) as usize; // 181 <= i <= 362
            t *= CY[c];
            let r = INVERSE[i - 181];
            let l1 = LOG_INV[i - 181][0];
            let l2 = LOG_INV[i - 181][1];
            let z = fma(r, t, -1.0);
            let th = fma(big_e, h!("0x1.62e42fefa38p-1"), l1);
            let tl = fma(big_e, h!("0x1.ef35793c7673p-45"), l2);
            let (h, l) = fast_sum(th, z, tl);
            let (ph, pl) = p_1(z);
            let (h, l) = fast_sum(h, ph, l + pl);
            if e == 0 && fabs(l) > fabs(h) * h!("0x1p-24") {
                let (h, l) = fasttwosum(h, l);
                return (h, l, true);
            }
            (h, l, false)
        }

        /// log(x), relative error below 2^-122.88.
        $(#[$feature])*
        fn log_2(x: &Dint) -> Dint {
            let mut x = *x;
            let mut e = x.ex;
            let i: u16 = if x.hi > 0xb504f333f9de6484 {
                e += 1;
                (x.hi >> (63 + 1 - 7)) as u16
            } else {
                (x.hi >> (63 - 7)) as u16
            };
            // 90 <= i <= 181
            x.ex -= e;
            let z = mul_dint_11(&x, &INVERSE_2_1[usize::from(i) - 90]); // exact
            let j = (z.hi >> (63 - 13 - z.ex)) as u16;
            let z = mul_dint_11(&z, &INVERSE_2_2[usize::from(j) - 8128]); // exact
            let z = add_dint(&M_ONE, &z); // exact
            let r = mul_dint_int64(&LOG2, e);
            let p = p_2(&z);
            let p = add_dint(&LOG_INV_2_2[usize::from(j) - 8128], &p);
            let p = add_dint(&LOG_INV_2_1[usize::from(i) - 90], &p);
            add_dint(&p, &r)
        }

        /// log(x), relative error below 2^-250.74.
        $(#[$feature])*
        fn log_3(x: &Qint) -> Qint {
            let mut x = *x;
            let mut e = x.ex;
            let i: u16 = if x.hh > 0xb504f333f9de6484 {
                e += 1;
                (x.hh >> (63 + 1 - 7)) as u16
            } else {
                (x.hh >> (63 - 7)) as u16
            };
            x.ex -= e;
            let z = mul_qint(&x, &INVERSE_3_1[usize::from(i) - 90]);
            let j = (z.hh >> (63 - 13 - z.ex)) as u16;
            let z = mul_qint(&z, &INVERSE_3_2[usize::from(j) - 8128]); // exact
            let z = add_qint(&M_ONE_Q, &z);
            let r = mul_qint_2(e, &LOG2_Q);
            let p = p_3(&z);
            let p = add_qint(&LOG_INV_3_2[usize::from(j) - 8128], &p);
            let p = add_qint(&LOG_INV_3_1[usize::from(i) - 90], &p);
            add_qint(&p, &r)
        }

        /// exp(rh + rl) times the sign `s`, for the first phase (Lemma 7 of the
        /// reference). NaN outputs defer to the second phase.
        #[$inline]
        $(#[$feature])*
        fn exp_1(rh: f64, rl: f64, s: f64) -> (f64, f64) {
            let rho0 = h!("-0x1.74910ee4e8a27p+9");
            // Raised from -0x1.577453f1799a6p+9 upstream to avoid a spurious
            // underflow in el.
            let rho1 = h!("-0x1.483b8cca421afp+9");
            let rho2 = h!("0x1.62e42e709a95bp+9");
            let rho3 = h!("0x1.62e4316ea5df9p+9");
            if rh.is_nan() || rh > rho2 {
                if !rh.is_nan() && rh > rho3 {
                    // Certain overflow.
                    return (f64::MAX * s, f64::MAX * s);
                }
                // Possible overflow: defer to the second phase.
                return (f64::NAN, f64::NAN);
            }
            if rh < rho1 {
                if rh < rho0 {
                    return (0.0 * s, h!("0x1p-1074") * (0.5 * s));
                }
                return (f64::NAN, f64::NAN);
            }
            let k = roundeven_finite(rh * h!("0x1.71547652b82fep+12"));
            let zh = fma(h!("0x1.62e42fefa39efp-13"), -k, rh);
            let zl = fma(h!("0x1.abc9e3b39803fp-68"), -k, rl);
            let kk = k as i64; // k is an integer
            let m = (kk >> 12) + 0x3ff;
            let i2 = ((kk >> 6) & 0x3f) as usize;
            let i1 = (kk & 0x3f) as usize;
            let (t1h, t1l) = (T1[i2][0], T1[i2][1]);
            let (t2h, t2l) = (T2[i1][0], T2[i1][1]);
            let (eh, el) = d_mul(t2h, t2l, t1h, t1l);
            let (qh, ql) = q_1(zh + zl);
            let (eh, el) = d_mul(eh, el, qh, ql);
            // 1 < M < 2047 here, since potential underflow and overflow were
            // filtered above.
            let d = from_bits((m as u64) << 52) * s;
            (eh * d, el * d)
        }

        /// exp(x) for |x| < 744.45, relative error below 2^-121.70.
        $(#[$feature])*
        fn exp_2(x: &Dint) -> Dint {
            if x.ex >= 10 {
                // Underflow or overflow.
                let mut r = *x;
                r.ex = if x.sgn == 0x1 { -1076 } else { 1025 };
                r.sgn = 0;
                return r;
            }
            let big_k = mul_dint_11(x, &LOG2_INV);
            let k = dint_toi(&big_k);
            let mut big_k = mul_dint_int64(&LOG2, k);
            big_k.ex -= 12;
            big_k.sgn = u64::from(big_k.sgn == 0);
            let y = add_dint(x, &big_k); // exact (Sterbenz)
            let m = k >> 12;
            let i2 = ((k >> 6) & 0x3f) as usize;
            let i1 = (k & 0x3f) as usize;
            let r = q_2(&y);
            let r = mul_dint(&T1_2[i2], &r);
            let mut r = mul_dint(&T2_2[i1], &r);
            r.ex += m;
            r
        }

        /// exp(x) for |x| < 744.45, relative error below 2^-241.10.
        $(#[$feature])*
        fn exp_3(x: &Qint) -> Qint {
            let big_k = mul_qint_11(x, &LOG2_INV_Q);
            let k = qint_toi(&big_k);
            let mut big_k = mul_qint_2(k, &LOG2_Q);
            big_k.ex -= 12;
            big_k.sgn = u64::from(big_k.sgn == 0);
            let y = add_qint(x, &big_k); // exact (Sterbenz)
            let m = k >> 12;
            let i2 = ((k >> 6) & 0x3f) as usize;
            let i1 = (k & 0x3f) as usize;
            let r = q_3(&y);
            let r = mul_qint(&T1_3[i2], &r);
            let mut r = mul_qint(&T2_3[i1], &r);
            r.ex += m;
            r
        }

        /// Computes x^y exactly and returns it when the result fits in 54 bits,
        /// catching exact and midpoint cases (algorithm detectRoundingBoundaryCase
        /// of Lauter and Lefèvre). `z` approximates x^y with relative error below
        /// 2^-117.
        $(#[$feature])*
        fn exact_pow(x: f64, y: f64, z: &Dint, exact: bool) -> Option<f64> {
            let s: i64 = if z.sgn != 0 { -1 } else { 1 };
            let (e, mut m) = extract(x); // x = 2^E * m with m odd
            if m == 1 {
                // x is a power of 2.
                let g = e as f64 * y;
                // Keep the scaling exponent in range before converting to an integer.
                if g >= 1024.0 {
                    return Some(if z.sgn != 0 {
                        f64::NEG_INFINITY
                    } else {
                        f64::INFINITY
                    });
                }
                if g < -1075.0 {
                    return Some(if z.sgn != 0 { -0.0 } else { 0.0 });
                }
                if is_int(g) {
                    let r = if z.sgn != 0 { -1.0 } else { 1.0 };
                    return Some(pow2(r, g as i64));
                }
                return None;
            }
            if !(0.0..=34.0).contains(&y) {
                return None;
            }
            let (f, n) = extract(y); // y = 2^F * n with n odd
            if n > 34 || f < -5 {
                return None;
            }
            if f < 0 {
                // Case (b): check that E is divisible by 2^-F.
                if (e as u64) & (!0u64 >> (64 + f)) != 0 {
                    return None;
                }
                let g = (e >> -f) * n as i64; // g = E * y
                let (big_g, k) = round_54(z); // z is rounded to k * 2^G
                // The upstream bit-length quirk in `is_exact` only controls this
                // conservative proximity check. The integer/exponent constraints
                // below still gate the candidate; the checked worst-case corpus has
                // exposed no result difference, so keep the quirk as ported.
                if !exact {
                    let cnt = k.leading_zeros();
                    let mut d = Dint {
                        hi: (k as u64) << cnt,
                        lo: 0,
                        ex: big_g + 63 - i64::from(cnt),
                        sgn: 1 - z.sgn,
                    };
                    d = add_dint(z, &d); // exact by Sterbenz
                    d.ex += 116;
                    if cmp_dint_abs(&d, z) >= 0 {
                        return None;
                    }
                }
                if big_g > g {
                    return None;
                }
                // k must be an odd number times 2^(g - G). Upstream shifts by g - G
                // without a range check; at 64 or more no 54-bit k can match.
                let shift = g - big_g;
                if shift >= 64 {
                    return None;
                }
                let shift = shift as u32;
                if (k as u64 & !(!1u64 << shift)) == (1u64 << shift) {
                    let r = ((k >> shift) * s) as f64;
                    return Some(pow2(r, g));
                }
                return None;
            }
            // Case (a): no overflow in n << F since 2^F * n <= 34.
            let mut t = n << f;
            let mut k: i64 = 1;
            // Compute k = m^t, which should fit into 54 bits; upstream gives up when
            // a product overflows (`__builtin_mul_overflow` into int64_t for k and
            // uint64_t for m). An m beyond i64 makes m * k overflow i64 for k >= 1.
            while t != 0 {
                if t & 0x1 != 0 {
                    let Ok(m_signed) = i64::try_from(m) else {
                        return None;
                    };
                    k = k.checked_mul(m_signed)?;
                }
                t >>= 1;
                if t != 0 {
                    m = m.checked_mul(m)?;
                }
            }
            if k >> 54 != 0 {
                return None;
            }
            let r = (k * s) as f64;
            let g = e * (n << f) as i64;
            Some(pow2(r, g))
        }

        /// Whether x^y is exact and exactly representable as a binary64 value.
        $(#[$feature])*
        fn is_exact(x: f64, y: f64) -> bool {
            let v = bits(x);
            let w = bits(y);
            if (v << 1) != 0x7fe0000000000000 && (w << 22) != 0 {
                return false;
            }
            if (v << 1) == 0x7fe0000000000000 {
                return true; // |x| = 1
            }
            // XMAX[y] for 1 <= y <= 33 is the largest odd m with m^y below 2^53.
            const XMAX: [u64; 34] = [
                0,
                0xffffffffffffffff,
                94906265,
                208063,
                9741,
                1551,
                455,
                189,
                97,
                59,
                39,
                27,
                21,
                15,
                13,
                11,
                9,
                7,
                7,
                5,
                5,
                5,
                5,
                4,
                3,
                3,
                3,
                3,
                3,
                3,
                3,
                3,
                3,
                3,
            ];
            if y >= 0.0 && is_int(y) {
                let mut m = v & 0xfffffffffffff;
                let mut e = ((v << 1) >> 53) as i64 - 0x433;
                if e >= -1074 {
                    m |= 0x10000000000000;
                } else {
                    e += 1;
                }
                let t = m.trailing_zeros();
                m >>= t;
                e += i64::from(t);
                if y == 0.0 || y == 1.0 {
                    return true;
                }
                if m == 1 {
                    let ye = y * e as f64;
                    return (-1074.0..1024.0).contains(&ye);
                }
                if !(0.0..=33.0).contains(&y) {
                    return false;
                }
                let y_int = y as i32;
                if m > XMAX[y_int as usize] {
                    return false;
                }
                // Upstream computes m^y here and then uses the bit length of m (not
                // of m^y) below; kept as is.
                let t = 64 - i64::from(m.leading_zeros());
                let ez = e * i64::from(y_int) + t;
                if ez <= -1074 || 1024 < ez {
                    return false;
                }
                return e * i64::from(y_int) >= -1074;
            }
            let mut n = w & 0xfffffffffffff;
            let mut f = ((w << 1) >> 53) as i64 - 0x433;
            if f >= -1074 {
                n |= 0x10000000000000;
            } else {
                f += 1;
            }
            let t = n.trailing_zeros();
            n >>= t;
            f += i64::from(t);
            // |y| = n * 2^f with n odd
            let mut m = v & 0xfffffffffffff;
            let mut e = ((v << 1) >> 53) as i64 - 0x433;
            if e >= -1074 {
                m |= 0x10000000000000;
            } else {
                e += 1;
            }
            let t = m.trailing_zeros();
            m >>= t;
            e += i64::from(t);
            // |x| = m * 2^e with m odd
            if y < 0.0 {
                if m != 1 {
                    return false;
                }
                let ez = if f >= 0 {
                    if f < 12 {
                        (n as i64).wrapping_neg().wrapping_mul(e) << f
                    } else {
                        1024
                    }
                } else {
                    // 2^-f must divide e.
                    let t = i64::from(e.trailing_zeros());
                    if -f > t {
                        return false;
                    }
                    (-e >> (-f)) * n as i64
                };
                return (-1074..1024).contains(&ez);
            }
            // Now y > 0 is not an integer: y = n * 2^f with n odd and f < 0, so x
            // must be an exact 2^-f-th power.
            while f != 0 {
                f += 1;
                if e & 1 != 0 {
                    return false;
                }
                e /= 2;
                let dm = m as f64;
                let s = round_nonnegative(dm.sqrt());
                if s * s != dm {
                    return false;
                }
                m = s as u64;
            }
            if m > 1 {
                if 33 < n {
                    return false;
                }
                if m > XMAX[n as usize] {
                    return false;
                }
            }
            let mut my = m;
            let mut n0 = n;
            while n0 > 1 {
                n0 -= 1;
                my = my.wrapping_mul(m);
            }
            let t = 64 - i64::from(my.leading_zeros());
            let en = e * i64::from(n as i32);
            -1074 <= en && en + t <= 1024
        }

        /// Correctly rounded x^y (see the module doc for the residual case).
        $(#[$feature])*
        pub(crate) fn pow(x: f64, y: f64) -> f64 {
            let mut s = 1.0; // sign of the result
            let x0 = x;
            let mut x = x;
            let ux = bits(x);
            let uy = bits(y);

            if ux >= 0x7ff0000000000000 || uy >= 0x7ff0000000000000 {
                if x.is_nan() {
                    // pow(NaN, +-0) = 1; every NaN is treated as quiet.
                    if y == 0.0 {
                        return 1.0;
                    }
                    return x + x;
                }
                if y.is_nan() {
                    // pow(1, NaN) = 1.
                    if x == 1.0 {
                        return 1.0;
                    }
                    return y + y;
                }
                match ux {
                    0x7ff0000000000000 => {
                        // x = +inf
                        if y == 0.0 {
                            return 1.0;
                        }
                        if y < 0.0 {
                            return 0.0;
                        }
                        if y > 0.0 {
                            return f64::INFINITY;
                        }
                    }
                    0xfff0000000000000 => {
                        // x = -inf
                        if uy == 0x7ff0000000000000 {
                            return y;
                        }
                        if uy == 0xfff0000000000000 {
                            return 0.0;
                        }
                        if is_int(y) && !is_int(y * 0.5) {
                            // y odd integer
                            return if y < 0.0 { -0.0 } else { f64::NEG_INFINITY };
                        }
                        if y < 0.0 {
                            return 0.0;
                        }
                        if y > 0.0 {
                            return f64::INFINITY;
                        }
                    }
                    _ => {}
                }
                match uy {
                    0x7ff0000000000000 => {
                        // y = +inf
                        if x == 0.0 {
                            return 0.0;
                        }
                        if x == -1.0 || x == 1.0 {
                            return 1.0;
                        }
                        if -1.0 < x && x < 1.0 {
                            return 0.0;
                        }
                        if x < -1.0 || 1.0 < x {
                            return f64::INFINITY;
                        }
                    }
                    0xfff0000000000000 => {
                        // y = -inf
                        if x == 0.0 {
                            return f64::INFINITY;
                        }
                        if x == -1.0 || x == 1.0 {
                            return 1.0;
                        }
                        if -1.0 < x && x < 1.0 {
                            return f64::INFINITY;
                        }
                        if x < -1.0 || 1.0 < x {
                            return 0.0;
                        }
                    }
                    _ => {}
                }
            }
            // From now on, x and y are finite.

            if x <= 0.0 {
                if y == 0.0 {
                    return 1.0;
                }
                match ux {
                    0x0 => {
                        // x = +0
                        if is_int(y) && !is_int(y * 0.5) {
                            return if y < 0.0 { f64::INFINITY } else { 0.0 };
                        }
                        if y > 0.0 {
                            return 0.0;
                        }
                        return f64::INFINITY;
                    }
                    0x8000000000000000 => {
                        // x = -0
                        if is_int(y) && !is_int(y * 0.5) {
                            return if y < 0.0 { f64::NEG_INFINITY } else { -0.0 };
                        }
                        if y > 0.0 {
                            return 0.0;
                        }
                        return f64::INFINITY;
                    }
                    _ => {}
                }
                if !is_int(y) {
                    return f64::NAN;
                }
                // The sign is -1 for odd y.
                let y_parity = if fabs(y) >= h!("0x1p53") {
                    0
                } else {
                    (y as i64) & 0x1
                };
                s = if y_parity != 0 { -1.0 } else { 1.0 };
                x = -x;
            }

            // Outside this exponent range, no approximation is needed. For finite
            // positive x, |ln(x)| < 745; the tiny-y product rounds exp to 1.
            // For x != 1, |ln(x)| > 2^-54, so huge y forces overflow or underflow.
            // This also keeps the extended-precision phases inside their domains.
            let ey = (uy >> 52) & 0x7ff;
            if ey < 0x36 || x == 1.0 {
                return s;
            }
            if ey >= 0x7f5 {
                return if (x > 1.0) == (y > 0.0) {
                    s * f64::INFINITY
                } else {
                    s * 0.0
                };
            }

            // Phase 1 (Algorithm phase_1 of the reference).
            let (lh, ll, cancel) = log_1(x);
            let (rh, rl) = s_mul(y, lh, ll);
            let (res_h, res_l) = exp_1(rh, rl, s);
            const ERR: [f64; 2] = [h!("0x1.27p-64"), h!("0x1.57p-58")];
            let err = ERR[usize::from(cancel)];
            let res_min = res_h + fma(err, -res_h, res_l);
            let res_max = res_h + fma(err, res_h, res_l);
            let exact = is_exact(x, y);
            if res_min == res_max {
                return res_max;
            }

            // Easy cases.
            if y == 1.0 {
                return s * x;
            }
            if y == 2.0 {
                return x * x;
            }
            if y == 0.5 {
                return x.sqrt();
            }
            if y == 0.0 {
                return 1.0;
            }

            // Phase 2.
            let mut big_x = dint_fromd(x); // exact
            let big_y = dint_fromd(y); // exact
            big_x.sgn = 0x0;
            let r = log_2(&big_x);
            let r = mul_dint_21(&r, &big_y);
            let mut r = exp_2(&r);
            if r.ex < -1075 {
                // Underflow: rounds to a zero of the right sign.
                return 0.5 * (s * h!("0x1p-1074"));
            }
            if r.ex > 1024 {
                // exp_2's overflow sentinel has no result significand. Do not feed
                // its logarithm-product bits to rounding tests or exact detection.
                return s * f64::INFINITY;
            }
            let rd = if r.ex < -1022 {
                // Subnormal: 2^-1075 <= R < 2^-1022.
                let ex = (-(1022 + r.ex)) as u64; // 1 <= ex <= 53
                let m = (r.lo >> (10 + ex)) | (r.hi << (54 - ex));
                m.wrapping_add(14) > 2 * 14
            } else {
                const ERR_BND_2: u64 = 28;
                let lo = (r.lo >> 10) | (r.hi << 54);
                lo.wrapping_add(ERR_BND_2) > 2 * ERR_BND_2
            };
            r.sgn = u64::from(s == -1.0);
            if rd {
                return dint_tod(&r);
            }
            if let Some(e) = exact_pow(x0, y, &r, exact) {
                return e;
            }

            // Phase 3.
            let mut q_x = qint_fromd(x);
            let q_y = qint_fromd(y);
            q_x.sgn = 0x0;
            let q_r = log_3(&q_x);
            let q_r = mul_qint_41(&q_r, &q_y);
            let mut q_z = exp_3(&q_r);
            const ERR_BND_3: u64 = 60;
            let r1 = (q_z.hh << 54) | (q_z.hl >> 10);
            let r2 = (q_z.hl << 54) | (q_z.lh >> 10);
            let r3 = (q_z.lh << 54) | (q_z.ll >> 10);
            let near_boundary = near_rounding_boundary(r1, r2, r3, ERR_BND_3);
            q_z.sgn = u64::from(s == -1.0);
            if !near_boundary {
                q_z.ll &= !0u64 << 10;
                return qint_tod(&q_z);
            }
            // For |qR| < 2^-56, exp(qR) rounds to 1 in round-to-nearest mode.
            if q_r.ex < -56 {
                return 1.0;
            }
            // Upstream aborts here ("unexpected worst case"). No input is known to
            // reach this point. Rounding the 256-bit approximation keeps the result
            // deterministic; it may differ from the correctly rounded value.
            qint_tod(&q_z)
        }

        /// Whether the three-limb tail lies within `error` units of a boundary.
        $(#[$feature])*
        fn near_rounding_boundary(r1: u64, r2: u64, r3: u64, error: u64) -> bool {
            (r1 == 0 && r2 == 0 && r3 <= error)
                || (r1 == u64::MAX && r2 == u64::MAX && r3 != 0 && r3.wrapping_neg() <= error)
        }

        #[cfg($test_cfg)]
        mod kernel_unit_tests {
            use super::*;
            use crate::crmath::pow::pow;

            #[test]
            fn rounding_boundary_window() {
                for distance in [1u64, 59, 60] {
                    assert!(near_rounding_boundary(
                        u64::MAX,
                        u64::MAX,
                        distance.wrapping_neg(),
                        60
                    ));
                }
                for distance in [61u64, 119, 120, 121] {
                    assert!(!near_rounding_boundary(
                        u64::MAX,
                        u64::MAX,
                        distance.wrapping_neg(),
                        60
                    ));
                }
                assert!(!near_rounding_boundary(u64::MAX, u64::MAX, 0, 60));
                assert!(near_rounding_boundary(0, 0, 60, 60));
                assert!(!near_rounding_boundary(0, 0, 61, 60));
                assert!(!near_rounding_boundary(0, u64::MAX, u64::MAX, 60));
            }

            #[test]
            fn extreme_exponents_and_signed_scaling() {
                for mode in crate::crmath::tests::fma_modes() {
                    crate::crmath::tests::with_forced_fma(mode, || {
                        let huge = h!("0x1p+1014");
                        for x in [0.5, 2.0, h!("0x1.62a88613629b6p+678")] {
                            for y in [huge, -huge] {
                                let expected: f64 = if (x > 1.0) == (y > 0.0) {
                                    f64::INFINITY
                                } else {
                                    0.0
                                };
                                assert_eq!(pow(x, y).to_bits(), expected.to_bits());
                            }
                        }
                        for x in [f64::MAX, h!("0x1p-1074")] {
                            assert_eq!(pow(x, h!("0x1p-970")).to_bits(), 1.0f64.to_bits());
                        }
                        let z = Dint {
                            hi: 1 << 63,
                            lo: 0,
                            ex: 0,
                            sgn: 1,
                        };
                        assert_eq!(
                            dint_tod(&Dint { ex: 1025, ..z }).to_bits(),
                            f64::NEG_INFINITY.to_bits()
                        );
                        let q = Qint {
                            hh: 1 << 63,
                            hl: 0,
                            lh: 0,
                            ll: 0,
                            ex: 1025,
                            sgn: 1,
                        };
                        assert_eq!(qint_tod(&q).to_bits(), f64::NEG_INFINITY.to_bits());
                        assert_eq!(exact_pow(2.0, huge, &z, true), Some(f64::NEG_INFINITY));
                        assert_eq!(
                            exact_pow(2.0, -huge, &z, true)
                                .expect("power of two is exact")
                                .to_bits(),
                            (-0.0f64).to_bits()
                        );
                    });
                }
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

pub(crate) fn pow(x: f64, y: f64) -> f64 {
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
        return unsafe { hardware::pow(x, y) };
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        return hardware::pow(x, y);
    }
    #[cfg(test)]
    crate::crmath::tests::record_kernel_backend(false);
    software::pow(x, y)
}
