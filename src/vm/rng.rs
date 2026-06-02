/// Small deterministic RNG with explicit serializable state.
///
/// This uses SplitMix64. It is not cryptographic; it exists so `math.random`
/// has a deterministic stream whose complete state is owned by this crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VmRng {
    state: u64,
}

impl VmRng {
    pub(super) const fn seed_from_u64(seed: u64) -> Self {
        Self { state: seed }
    }

    #[cfg(feature = "snapshot")]
    pub(crate) const fn state(self) -> u64 {
        self.state
    }

    #[cfg(feature = "snapshot")]
    pub(crate) const fn from_state(state: u64) -> Self {
        Self { state }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    pub(crate) fn random_f64(&mut self) -> f64 {
        const SCALE: f64 = 1.0 / ((1u64 << 53) as f64);
        ((self.next_u64() >> 11) as f64) * SCALE
    }

    /// Uniform-ish integer in the inclusive range `[start, end]`.
    ///
    /// Degenerate or inverted ranges collapse to `start` rather than wrapping a
    /// negative span into a huge modulus. The modulo introduces at most a
    /// `span / 2^64` bias, negligible for game-sized ranges.
    pub(crate) fn random_range_i64(&mut self, start: i64, end: i64) -> i64 {
        if end <= start {
            return start;
        }
        let span = (i128::from(end) - i128::from(start) + 1) as u128;
        let draw = u128::from(self.next_u64()) % span;
        (i128::from(start) + draw as i128) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::VmRng;

    #[test]
    fn splitmix64_stream_is_pinned() {
        // The RNG is part of the deterministic save/replay contract. Pin the
        // exact SplitMix64 stream for seed 0 so a change to the generator can
        // never slip through silently.
        let mut rng = VmRng::seed_from_u64(0);
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

        // Same seed reproduces the same stream.
        let mut again = VmRng::seed_from_u64(0);
        for &expected in &seq {
            assert_eq!(again.next_u64(), expected);
        }
    }
}
