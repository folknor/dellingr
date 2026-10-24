//! SplitMix64 inputs shared by verification and the regeneration tool.

pub(crate) struct TestRng(u64);

impl TestRng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform finite bit patterns, so every binade is equally likely.
    pub(crate) fn finite(&mut self) -> f64 {
        loop {
            let x = f64::from_bits(self.next_u64());
            if x.is_finite() {
                return x;
            }
        }
    }

    /// A value in [lo, hi) with uniformly random bits in that range.
    pub(crate) fn in_range(&mut self, lo: f64, hi: f64) -> f64 {
        let u = (self.next_u64() >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0);
        lo + (hi - lo) * u
    }
}
