//! Tests for the deterministic math kernels.
//!
//! Three layers:
//!
//! - `fma`: the software FMA against the hardware instruction, bit for bit.
//! - `golden`: every kernel against `tests/fixtures/crmath_golden.txt`, under
//!   each FMA path this host can run. The expected bits there were certified
//!   by the oracle below; the same file must pass on every target, which is
//!   the cross-platform replay contract in executable form. Checkout checks
//!   and long oracle sweeps require the explicit `crmath-sweeps` feature.
//!   Regeneration lives in the `regenerate_crmath_golden` example, outside
//!   the test harness, and requires an output path.
//! - `oracle`: an interval Ziv procedure over `libbeef` in directed rounding,
//!   and the reader for CORE-MATH's worst-case files.

use std::cell::Cell;

mod fma;
mod golden;
mod oracle;
mod rng;

pub(crate) use oracle::parse_hex_float;
pub(crate) use rng::TestRng;

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

/// Runs `f` with every kernel entry on this thread selecting the `mode`
/// backend, and every standalone [`super::fma`] call routed to it.
pub(crate) fn with_forced_fma<T>(mode: ForcedFma, f: impl FnOnce() -> T) -> T {
    let previous = FORCED_FMA.with(|cell| cell.replace(mode));
    struct Restore(ForcedFma);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCED_FMA.with(|cell| cell.set(self.0));
        }
    }
    let _restore = Restore(previous);
    f()
}

#[test]
fn forced_fma_restores_after_nested_unwind() {
    let previous = forced_fma();
    with_forced_fma(ForcedFma::Software, || {
        let caught = std::panic::catch_unwind(|| {
            with_forced_fma(ForcedFma::Default, || {
                panic!("deliberate unwind while overriding the FMA backend");
            });
        });
        assert!(caught.is_err());
        assert_eq!(forced_fma(), ForcedFma::Software);
    });
    assert_eq!(forced_fma(), previous);
}

/// The distinct FMA backends this host can exercise.
/// Production selection is tested separately.
pub(crate) fn fma_modes() -> Vec<ForcedFma> {
    let mut modes = vec![ForcedFma::Software];
    if super::hardware_fma(1.0, 1.0, 1.0).is_some() {
        modes.push(ForcedFma::Hardware);
    }
    modes
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

// Count entries, rather than individual FMA operations, so a forced-mode test
// detects a kernel accidentally bypassing whole-kernel backend selection.
thread_local! {
    static KERNEL_BACKENDS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

pub(crate) fn record_kernel_backend(hardware: bool) {
    KERNEL_BACKENDS.with(|cell| {
        let (soft, hard) = cell.get();
        cell.set((soft + usize::from(!hardware), hard + usize::from(hardware)));
    });
}

#[test]
fn kernel_dispatch_honors_forced_fma() {
    let hardware = super::hardware_fma(1.0, 1.0, 1.0).is_some();
    for mode in std::iter::once(ForcedFma::Default).chain(fma_modes()) {
        KERNEL_BACKENDS.with(|cell| cell.set((0, 0)));
        with_forced_fma(mode, || {
            let unary: [fn(f64) -> f64; 9] = [
                super::acos,
                super::asin,
                super::atan,
                super::cos,
                super::exp,
                super::ln,
                super::log10,
                super::sin,
                super::tan,
            ];
            for kernel in unary {
                assert!(kernel(0.5).is_finite());
            }
            assert!(super::atan2(0.5, 0.75).is_finite());
            assert!(super::pow(0.5, 0.75).is_finite());
        });
        let uses_hardware = match mode {
            ForcedFma::Default => hardware,
            ForcedFma::Software => false,
            ForcedFma::Hardware => true,
        };
        let expected = if uses_hardware { (0, 11) } else { (11, 0) };
        KERNEL_BACKENDS.with(|cell| assert_eq!(cell.get(), expected, "{mode:?}"));
    }
}
