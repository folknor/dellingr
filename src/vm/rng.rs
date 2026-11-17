use std::sync::atomic::AtomicBool;

/// Small deterministic RNG with explicit serializable state.
///
/// This uses SplitMix64. It is not cryptographic; it exists so `math.random`
/// has a deterministic stream whose complete state is owned by this crate.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct VmRng {
    state: u64,
}

impl VmRng {
    /// Seed used for a new state's deterministic random stream.
    pub(crate) const DEFAULT_SEED: u64 = 0;

    // Initial seeds and restored stream positions are distinct inputs that
    // both initialize the same SplitMix64 state word.
    pub(super) const fn seed_from_u64(seed: u64) -> Self {
        Self { state: seed }
    }

    #[cfg(feature = "snapshot")]
    pub(crate) const fn state(&self) -> u64 {
        self.state
    }

    #[cfg(feature = "snapshot")]
    pub(crate) const fn from_state(state: u64) -> Self {
        Self { state }
    }

    fn next_u64(&mut self) -> u64 {
        Self::step(&mut self.state)
    }

    /// One SplitMix64 step on a state word, so a caller can advance a copy
    /// and commit it only once the draw is kept.
    fn step(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    pub(crate) fn random_f64(&mut self) -> f64 {
        const SCALE: f64 = 1.0 / (crate::numeral::MAX_EXACT_INTEGER as f64);
        ((self.next_u64() >> 11) as f64) * SCALE
    }

    /// Uniform integer in the inclusive range `[start, end]`.
    ///
    /// Degenerate or inverted ranges collapse to `start` rather than wrapping a
    /// negative span into a huge modulus.
    ///
    /// Modulo bias is removed by rejection: a raw draw below
    /// `2^64 mod span` is discarded and the generator advances again, so the
    /// accepted draws cover an exact multiple of `span`. An accepted draw maps
    /// to `draw % span`, exactly as the earlier biased version did, so the
    /// result sequence for a seed changed only at the (probability
    /// `< span / 2^64`) draws that are now rejected. Any rejection is still a
    /// replay-visible change to the stream relative to builds without it.
    ///
    /// Termination is certain: the state walks all 2^64 words (odd
    /// increment) and the output mix is a bijection, so one period yields
    /// every u64 once and at most `threshold` (under 2^63) draws in a row are
    /// rejected. That bound is finite but not small, and the draws are not
    /// independent, so no shorter worst case is claimed: a script can pick
    /// the largest threshold (`math.random(math.mininteger, 0)`) and a host
    /// seed or snapshot any start. Each retry is therefore an interrupt
    /// safepoint. The first draw is covered by the callback-entry poll; after
    /// a rejection `interrupt` is polled before the next draw, and `None`
    /// means a set flag was observed there. Draws advance a copy of the
    /// state, committed only when one is accepted, so an interrupted call
    /// leaves the stream where it was and uninterrupted streams are
    /// unchanged. A degenerate or full-width range never retries and never
    /// polls.
    pub(crate) fn random_range_i64(
        &mut self,
        start: i64,
        end: i64,
        interrupt: &AtomicBool,
    ) -> Option<i64> {
        if end <= start {
            return Some(start);
        }
        let span = (i128::from(end) - i128::from(start) + 1) as u128;
        let draw = if span > u128::from(u64::MAX) {
            // span == 2^64: every u64 is a valid offset, no bias possible.
            u128::from(self.next_u64())
        } else {
            let span = span as u64;
            // 2^64 mod span, computed without overflowing u64.
            let threshold = span.wrapping_neg() % span;
            let mut state = self.state;
            loop {
                let raw = Self::step(&mut state);
                if raw >= threshold {
                    self.state = state;
                    break u128::from(raw % span);
                }
                if crate::vm::interrupt_requested(interrupt) {
                    return None;
                }
            }
        };
        Some((i128::from(start) + draw as i128) as i64)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::VmRng;

    #[test]
    fn splitmix64_stream_is_pinned() {
        // The RNG is part of the deterministic save/replay contract. Pin the
        // exact SplitMix64 stream for seed 0 so a change to the generator can
        // never slip through silently.
        let clear = AtomicBool::new(false);
        let mut rng = VmRng::seed_from_u64(VmRng::DEFAULT_SEED);
        let seq = [
            rng.next_u64(),
            rng.next_u64(),
            rng.next_u64(),
            rng.next_u64(),
        ];
        assert_eq!(
            seq,
            [
                0xE220A8397B1DCDAF,
                0x6E789E6AA1B965F4,
                0x06C45D188009454F,
                0xF88BB8A8724C81EC,
            ]
        );

        // These pinned draws all exceed the threshold (2^64 mod 6 = 4), so
        // they map to the same offsets as the earlier `% span` implementation.
        let mut ranged = VmRng::seed_from_u64(VmRng::DEFAULT_SEED);
        for &raw in &seq {
            assert_eq!(
                ranged.random_range_i64(1, 6, &clear),
                Some(1 + (raw % 6) as i64)
            );
        }

        // Rejection: span = 0xF900... (> 2^63) has threshold 2^64 - span =
        // 0x0700..., which rejects the third raw draw (0x06C4...) and uses
        // the fourth (0xF88B..., below span, so it maps to itself).
        let span: u64 = 0xF900_0000_0000_0000;
        let mut rejecting = VmRng::seed_from_u64(VmRng::DEFAULT_SEED);
        rejecting.next_u64();
        rejecting.next_u64();
        let end = (i128::from(i64::MIN) + i128::from(span) - 1) as i64;
        assert_eq!(
            rejecting.random_range_i64(i64::MIN, end, &clear),
            Some((i128::from(i64::MIN) + i128::from(seq[3])) as i64)
        );
        assert_eq!(rejecting, {
            let mut after = VmRng::seed_from_u64(VmRng::DEFAULT_SEED);
            for _ in 0..4 {
                after.next_u64();
            }
            after
        });

        // The widest span a Lua script can request stays in range.
        let mut wide = VmRng::seed_from_u64(VmRng::DEFAULT_SEED);
        let lo = i64::MIN;
        let hi = 9_223_372_036_854_774_784; // largest f64 below 2^63
        for _ in 0..64 {
            let v = wide
                .random_range_i64(lo, hi, &clear)
                .expect("a clear flag never interrupts");
            assert!((lo..=hi).contains(&v));
        }
        let mut full = VmRng::seed_from_u64(VmRng::DEFAULT_SEED);
        assert_eq!(
            full.random_range_i64(i64::MIN, i64::MAX, &clear),
            Some((i128::from(i64::MIN) + i128::from(seq[0])) as i64)
        );

        // Same seed reproduces the same stream.
        let mut again = VmRng::seed_from_u64(VmRng::DEFAULT_SEED);
        for &expected in &seq {
            assert_eq!(again.next_u64(), expected);
        }
    }

    #[test]
    fn a_rejection_polls_and_an_interrupted_draw_leaves_the_stream_unmoved() {
        // Seed 0's third raw draw (0x06C4...) is below the threshold 0x0700...
        // of span 0xF900..., so the call rejects once and then accepts the
        // fourth draw (0xF88B...).
        let span: u64 = 0xF900_0000_0000_0000;
        let end = (i128::from(i64::MIN) + i128::from(span) - 1) as i64;
        let accepted = (i128::from(i64::MIN) + 0xF88B_B8A8_724C_81EC_i128) as i64;
        let positioned = || {
            let mut rng = VmRng::seed_from_u64(VmRng::DEFAULT_SEED);
            rng.next_u64();
            rng.next_u64();
            rng
        };
        let set = AtomicBool::new(true);

        // Interrupted at the retry: no result, no state change.
        let mut rng = positioned();
        assert_eq!(rng.random_range_i64(i64::MIN, end, &set), None);
        assert_eq!(rng, positioned());

        // After the host resets, the retried call draws exactly what an
        // uninterrupted one would.
        let clear = AtomicBool::new(false);
        assert_eq!(rng.random_range_i64(i64::MIN, end, &clear), Some(accepted));
        let mut after = VmRng::seed_from_u64(VmRng::DEFAULT_SEED);
        for _ in 0..4 {
            after.next_u64();
        }
        assert_eq!(rng, after);

        // An accepted first draw never reaches the poll: the callback-entry
        // safepoint covers it.
        let mut first = VmRng::seed_from_u64(VmRng::DEFAULT_SEED);
        assert_eq!(
            first.random_range_i64(1, 6, &set),
            Some(1 + (0xE220_A839_7B1D_CDAF_u64 % 6) as i64)
        );
    }
}
