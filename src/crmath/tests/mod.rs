//! Tests for the deterministic math kernels.
//!
//! Three layers:
//!
//! - `fma`: the software FMA against the hardware instruction, bit for bit.
//! - `golden`: every kernel against `tests/fixtures/crmath_golden.txt`, under
//!   each FMA path this host can run. The expected bits there were certified
//!   by the oracle below; the same file must pass on every target, which is
//!   the cross-platform replay contract in executable form. The module's
//!   `#[ignore]`d tests regenerate the golden file, sweep CORE-MATH's
//!   worst-case files from a local checkout, and check random inputs against
//!   the oracle. A name filter that matches the module path (`golden`) runs
//!   the regenerator too, which rewrites the fixture.
//! - `oracle`: an interval Ziv procedure over `libbeef` in directed rounding,
//!   and the reader for CORE-MATH's worst-case files.

use std::cell::Cell;

mod fma;
mod golden;
mod oracle;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ForcedFma {
    Default,
    Software,
    Hardware,
}

thread_local! {
    static FORCED_FMA: Cell<ForcedFma> = const { Cell::new(ForcedFma::Default) };
}

pub(crate) fn forced_fma() -> ForcedFma {
    FORCED_FMA.with(Cell::get)
}

/// Runs `f` with every [`super::fma`] call on this thread routed to `mode`.
pub(crate) fn with_forced_fma<T>(mode: ForcedFma, f: impl FnOnce() -> T) -> T {
    let previous = FORCED_FMA.with(|cell| cell.replace(mode));
    let result = f();
    FORCED_FMA.with(|cell| cell.set(previous));
    result
}

/// The FMA paths this host can exercise.
pub(crate) fn fma_modes() -> Vec<ForcedFma> {
    let mut modes = vec![ForcedFma::Default, ForcedFma::Software];
    if super::hardware_fma(1.0, 1.0, 1.0).is_some() {
        modes.push(ForcedFma::Hardware);
    }
    modes
}

/// SplitMix64, for reproducible test inputs.
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

    /// A finite binary64 value whose bit pattern is uniform over finite
    /// values, so every binade is equally likely.
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

/// Bitwise equality, except that every NaN equals every NaN: NaN sign and
/// payload are outside the determinism contract.
pub(crate) fn same(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
}

#[test]
fn hex_float_literals() {
    use super::hf;
    assert_eq!(hf("0x1p+0").to_bits(), 1.0f64.to_bits());
    assert_eq!(hf("-0x0p+0").to_bits(), (-0.0f64).to_bits());
    assert_eq!(hf("0x1.8p-1").to_bits(), 0.75f64.to_bits());
    assert_eq!(hf("0x3.fp-52").to_bits(), 0x3ccf800000000000);
    assert_eq!(hf("0x1p-1074").to_bits(), 1);
    assert_eq!(hf("0x1.fffffffffffffp+1023").to_bits(), f64::MAX.to_bits());
    assert_eq!(hf("0xd.71bf68b38193p-4").to_bits(), 0x3feae37ed1670326);
    assert_eq!(hf("0x0.0000000000001p-1022").to_bits(), 1);
}

#[test]
fn roundeven_finite_ties_to_even() {
    use super::roundeven_finite;
    for (x, r) in [
        (0.5f64, 0.0f64),
        (1.5, 2.0),
        (2.5, 2.0),
        (-2.5, -2.0),
        (-0.4, -0.0),
        (4_503_599_627_370_495.5, 4_503_599_627_370_496.0),
        (1e300, 1e300),
        (-3.7, -4.0),
    ] {
        assert_eq!(roundeven_finite(x).to_bits(), r.to_bits(), "{x}");
    }
}
