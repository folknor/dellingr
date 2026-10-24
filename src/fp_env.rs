//! Detection of the host thread's floating-point environment.
//!
//! dellingr's results are the same on every host only in the default IEEE 754
//! environment: round to nearest, gradual underflow (no flush-to-zero, no
//! denormals-are-zero) and every exception masked. A host such as a game
//! engine may change that on its threads for speed, and every subnormal or
//! inexact result would then differ silently between hosts. Rust and LLVM
//! assume the default environment too, so changing it is outside the
//! language; dellingr cannot reset it, but it can refuse to run under it.
//!
//! The check reads the control register directly (`stmxcsr` on x86_64,
//! `mrs fpcr` on aarch64) and compares bits. It performs no floating-point
//! arithmetic, so it depends on nothing the environment could change, sets no
//! status flags and diagnoses the exact settings. Other targets have no check
//! (`docs/determinism.md`). The decoders are plain integer code, compiled and
//! tested on every target.

use std::fmt;

/// The raw value of the floating-point control register dellingr read.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatControl {
    /// x86_64 `MXCSR`.
    Mxcsr(u32),
    /// aarch64 `FPCR`.
    Fpcr(u64),
}

/// The rounding direction a control register selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundingDirection {
    /// Round to nearest, ties to even: the only supported direction.
    Nearest,
    /// Round toward positive infinity.
    Upward,
    /// Round toward negative infinity.
    Downward,
    /// Round toward zero.
    TowardZero,
}

/// An unsupported floating-point environment, as dellingr found it on the
/// calling thread. Carried by
/// [`ErrorKind::UnsupportedFloatEnvironment`](crate::error::ErrorKind::UnsupportedFloatEnvironment)
/// and the matching save and load errors.
///
/// The accessors decode the register; [`FloatEnvironment::control`] gives
/// its raw bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatEnvironment {
    control: FloatControl,
}

// MXCSR: bits 0-5 are sticky status flags and are ignored, as are the bits
// above 15 (AMD's misaligned-exception mask at 17 does not touch arithmetic).
const MXCSR_DAZ: u32 = 1 << 6;
const MXCSR_MASKS: u32 = 0x3F << 7;
const MXCSR_ROUNDING_SHIFT: u32 = 13;
const MXCSR_FTZ: u32 = 1 << 15;
const MXCSR_CONTROLS: u32 = 0xFFC0;
const MXCSR_DEFAULT_CONTROLS: u32 = MXCSR_MASKS;

// FPCR: FIZ, AH and NEP (FEAT_AFP), the trap enables, RMode and FZ must all be
// clear. DN only changes NaN bits, which the determinism guarantee excludes,
// and FZ16 and the BFloat16 controls touch formats dellingr never uses.
const FPCR_FIZ: u64 = 1 << 0;
const FPCR_AH: u64 = 1 << 1;
const FPCR_NEP: u64 = 1 << 2;
const FPCR_TRAPS: u64 = (0x1F << 8) | (1 << 15);
const FPCR_ROUNDING_SHIFT: u64 = 22;
const FPCR_FZ: u64 = 1 << 24;
const FPCR_CONTROLS: u64 =
    FPCR_FIZ | FPCR_AH | FPCR_NEP | FPCR_TRAPS | (0b11 << FPCR_ROUNDING_SHIFT) | FPCR_FZ;

impl FloatControl {
    /// Whether this register value selects the supported environment.
    #[inline(always)]
    const fn is_supported(self) -> bool {
        match self {
            Self::Mxcsr(bits) => bits & MXCSR_CONTROLS == MXCSR_DEFAULT_CONTROLS,
            Self::Fpcr(bits) => bits & FPCR_CONTROLS == 0,
        }
    }
}

impl FloatEnvironment {
    /// Decode `control`, whatever it selects. Supported values decode too;
    /// only unsupported ones are ever carried by an error.
    pub(crate) const fn from_control(control: FloatControl) -> Self {
        Self { control }
    }

    /// The raw register value.
    pub fn control(&self) -> FloatControl {
        self.control
    }

    /// Whether subnormal results are flushed to zero (x86_64 `FTZ`, aarch64
    /// `FZ`).
    pub fn flushes_subnormal_results(&self) -> bool {
        match self.control {
            FloatControl::Mxcsr(bits) => bits & MXCSR_FTZ != 0,
            FloatControl::Fpcr(bits) => bits & FPCR_FZ != 0,
        }
    }

    /// Whether subnormal inputs are read as zero (x86_64 `DAZ`; aarch64 `FIZ`,
    /// or `FZ` without alternate handling, which flushes inputs as well as
    /// results).
    pub fn flushes_subnormal_inputs(&self) -> bool {
        match self.control {
            FloatControl::Mxcsr(bits) => bits & MXCSR_DAZ != 0,
            FloatControl::Fpcr(bits) => {
                bits & FPCR_FIZ != 0 || (bits & FPCR_FZ != 0 && bits & FPCR_AH == 0)
            }
        }
    }

    /// The selected rounding direction. The encodings differ: x86 `RC` 01 is
    /// downward, aarch64 `RMode` 01 is upward.
    pub fn rounding(&self) -> RoundingDirection {
        match self.control {
            FloatControl::Mxcsr(bits) => match (bits >> MXCSR_ROUNDING_SHIFT) & 0b11 {
                0b00 => RoundingDirection::Nearest,
                0b01 => RoundingDirection::Downward,
                0b10 => RoundingDirection::Upward,
                _ => RoundingDirection::TowardZero,
            },
            FloatControl::Fpcr(bits) => match (bits >> FPCR_ROUNDING_SHIFT) & 0b11 {
                0b00 => RoundingDirection::Nearest,
                0b01 => RoundingDirection::Upward,
                0b10 => RoundingDirection::Downward,
                _ => RoundingDirection::TowardZero,
            },
        }
    }

    /// Whether any floating-point exception traps instead of being masked
    /// (an x86_64 mask bit clear, an aarch64 trap enable set).
    pub fn traps_enabled(&self) -> bool {
        match self.control {
            FloatControl::Mxcsr(bits) => bits & MXCSR_MASKS != MXCSR_MASKS,
            FloatControl::Fpcr(bits) => bits & FPCR_TRAPS != 0,
        }
    }

    /// Whether aarch64 alternate floating-point handling (`AH`) is on. Always
    /// false on x86_64.
    pub fn alternate_handling(&self) -> bool {
        matches!(self.control, FloatControl::Fpcr(bits) if bits & FPCR_AH != 0)
    }

    /// Whether aarch64 `NEP` is set, which makes scalar instructions write
    /// the upper vector lanes; the procedure-call standard requires it clear.
    /// Always false on x86_64.
    pub fn alters_vector_lanes(&self) -> bool {
        matches!(self.control, FloatControl::Fpcr(bits) if bits & FPCR_NEP != 0)
    }
}

impl fmt::Display for FloatEnvironment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.control {
            FloatControl::Mxcsr(bits) => write!(f, "MXCSR {bits:#06x}")?,
            FloatControl::Fpcr(bits) => write!(f, "FPCR {bits:#010x}")?,
        }
        let rounding = match self.rounding() {
            RoundingDirection::Nearest => None,
            RoundingDirection::Upward => Some("rounding upward"),
            RoundingDirection::Downward => Some("rounding downward"),
            RoundingDirection::TowardZero => Some("rounding toward zero"),
        };
        let settings = [
            (
                self.flushes_subnormal_results(),
                "subnormal results flushed",
            ),
            (self.flushes_subnormal_inputs(), "subnormal inputs flushed"),
            (rounding.is_some(), rounding.unwrap_or_default()),
            (self.traps_enabled(), "exception traps enabled"),
            (self.alternate_handling(), "alternate handling"),
            (self.alters_vector_lanes(), "NEP set"),
        ];
        let mut separator = ": ";
        for (_, name) in settings.iter().filter(|(present, _)| *present) {
            f.write_str(separator)?;
            f.write_str(name)?;
            separator = ", ";
        }
        Ok(())
    }
}

/// Whether the calling thread's environment is unsupported. Always false on
/// targets without a check.
///
/// One register read, a mask and a compare, returning only a flag: the
/// checks are inlined into every public entry point and into the callback
/// return in `invoke_rust_fn`, which is live across a native re-entry, so the
/// fast path must not add a wider temporary to those frames. The failing
/// path reads the register again through [`fault`].
#[inline(always)]
pub(crate) fn unsupported() -> bool {
    read_control().is_some_and(|control| !control.is_supported())
}

/// The calling thread's environment, if it is not the supported one. `None`
/// on targets without a check.
#[inline(always)]
pub(crate) fn fault() -> Option<FloatEnvironment> {
    match read_control() {
        Some(control) if !control.is_supported() => Some(FloatEnvironment::from_control(control)),
        _ => None,
    }
}

// Out of line: `stmxcsr` stores to memory, so an inlined read gives every
// caller frame a slot for it, including the frames that stay live across a
// native re-entry. The call is a leaf whose frame is gone before anything
// re-enters.
#[cfg(target_arch = "x86_64")]
#[inline(never)]
fn read_control() -> Option<FloatControl> {
    let mut mxcsr: u32 = 0;
    // SAFETY: `stmxcsr` stores MXCSR into the four bytes at the operand
    // address, here a live, aligned local `u32`. It reads no memory, changes
    // no register and no flag, and does not use the stack.
    unsafe {
        std::arch::asm!(
            "stmxcsr dword ptr [{}]",
            in(reg) &raw mut mxcsr,
            options(nostack, preserves_flags),
        );
    }
    Some(FloatControl::Mxcsr(mxcsr))
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn read_control() -> Option<FloatControl> {
    let fpcr: u64;
    // SAFETY: reading FPCR is permitted at EL0, touches no memory and
    // changes no state.
    unsafe {
        std::arch::asm!(
            "mrs {}, fpcr",
            out(reg) fpcr,
            options(nomem, nostack, preserves_flags),
        );
    }
    Some(FloatControl::Fpcr(fpcr))
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline(always)]
fn read_control() -> Option<FloatControl> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mxcsr(bits: u32) -> FloatEnvironment {
        FloatEnvironment::from_control(FloatControl::Mxcsr(bits))
    }

    fn fpcr(bits: u64) -> FloatEnvironment {
        FloatEnvironment::from_control(FloatControl::Fpcr(bits))
    }

    #[test]
    fn test_thread_runs_in_the_supported_environment() {
        assert_eq!(fault(), None);
    }

    #[test]
    fn mxcsr_accepts_the_default_and_ignores_status_flags_and_high_bits() {
        for bits in [0x1F80, 0x1F80 | 0x3F, 0x1F80 | (1 << 17)] {
            assert!(FloatControl::Mxcsr(bits).is_supported(), "{bits:#x}");
        }
    }

    #[test]
    fn mxcsr_decodes_each_rejected_control() {
        let ftz = mxcsr(0x1F80 | MXCSR_FTZ);
        assert!(!ftz.control().is_supported());
        assert!(ftz.flushes_subnormal_results());
        assert!(!ftz.flushes_subnormal_inputs());

        let daz = mxcsr(0x1F80 | MXCSR_DAZ);
        assert!(!daz.control().is_supported());
        assert!(daz.flushes_subnormal_inputs());
        assert!(!daz.flushes_subnormal_results());

        for (rc, direction) in [
            (0b01, RoundingDirection::Downward),
            (0b10, RoundingDirection::Upward),
            (0b11, RoundingDirection::TowardZero),
        ] {
            let env = mxcsr(0x1F80 | (rc << MXCSR_ROUNDING_SHIFT));
            assert!(!env.control().is_supported());
            assert_eq!(env.rounding(), direction);
        }

        for mask in 7..13 {
            let env = mxcsr(0x1F80 & !(1 << mask));
            assert!(!env.control().is_supported(), "mask bit {mask}");
            assert!(env.traps_enabled());
            assert!(!env.flushes_subnormal_results());
            assert_eq!(env.rounding(), RoundingDirection::Nearest);
        }
        assert!(!mxcsr(0x1F80).alternate_handling());
        assert!(!mxcsr(0x1F80).alters_vector_lanes());
    }

    #[test]
    fn fpcr_accepts_the_default_and_ignored_controls() {
        // DN (25), FZ16 (19) and the unrelated high bits.
        for bits in [0, 1 << 25, 1 << 19, 1 << 40] {
            assert!(FloatControl::Fpcr(bits).is_supported(), "{bits:#x}");
        }
    }

    #[test]
    fn fpcr_decodes_each_rejected_control() {
        let fz = fpcr(FPCR_FZ);
        assert!(!fz.control().is_supported());
        assert!(fz.flushes_subnormal_results());
        // Without AH, FZ flushes inputs too.
        assert!(fz.flushes_subnormal_inputs());

        let fz_ah = fpcr(FPCR_FZ | FPCR_AH);
        assert!(fz_ah.flushes_subnormal_results());
        assert!(!fz_ah.flushes_subnormal_inputs());
        assert!(fz_ah.alternate_handling());

        let fiz = fpcr(FPCR_FIZ);
        assert!(!fiz.control().is_supported());
        assert!(fiz.flushes_subnormal_inputs());
        assert!(!fiz.flushes_subnormal_results());

        // AH or NEP alone reads as acceptable on every other accessor.
        for (bits, ah, nep) in [(FPCR_AH, true, false), (FPCR_NEP, false, true)] {
            let env = fpcr(bits);
            assert!(!env.control().is_supported());
            assert_eq!(env.alternate_handling(), ah);
            assert_eq!(env.alters_vector_lanes(), nep);
            assert!(!env.flushes_subnormal_results());
            assert!(!env.traps_enabled());
            assert_eq!(env.rounding(), RoundingDirection::Nearest);
        }

        for (mode, direction) in [
            (0b01, RoundingDirection::Upward),
            (0b10, RoundingDirection::Downward),
            (0b11, RoundingDirection::TowardZero),
        ] {
            let env = fpcr(mode << FPCR_ROUNDING_SHIFT);
            assert!(!env.control().is_supported());
            assert_eq!(env.rounding(), direction);
        }

        for trap in [8, 9, 10, 11, 12, 15] {
            let env = fpcr(1 << trap);
            assert!(!env.control().is_supported(), "trap bit {trap}");
            assert!(env.traps_enabled());
        }
    }

    #[test]
    fn display_names_the_register_and_every_setting() {
        assert_eq!(
            mxcsr(0x1F80 | MXCSR_FTZ | MXCSR_DAZ | (0b11 << MXCSR_ROUNDING_SHIFT)).to_string(),
            "MXCSR 0xffc0: subnormal results flushed, subnormal inputs flushed, \
             rounding toward zero"
        );
        assert_eq!(
            fpcr(FPCR_AH | (1 << 8)).to_string(),
            "FPCR 0x00000102: exception traps enabled, alternate handling"
        );
    }
}
