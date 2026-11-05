//! Integer conversion that clamps to the target type's range.
//!
//! `TryFrom` between primitive integers fails only when the value lies outside
//! the target's range, so its error carries nothing a caller could handle
//! beyond "too small" or "too large". Every saturating conversion in the crate
//! goes through [`saturating_cast`], so that rule is written once instead of as
//! `unwrap_or(MAX)` (banned on `Result` in clippy.toml because it discards
//! errors that do mean something), identity `map_or`s or hand-written matches.

/// The primitive integer types. Sealed: implemented only by the macro below,
/// so a `TryFrom` failure between two of them always means out of range.
pub(crate) trait PrimitiveInt: Copy + PartialOrd + private::Sealed {
    const MIN: Self;
    const MAX: Self;
    const ZERO: Self;
}

mod private {
    pub(crate) trait Sealed {}
}

macro_rules! primitive_int {
    ($($t:ty),* $(,)?) => {$(
        impl private::Sealed for $t {}
        impl PrimitiveInt for $t {
            const MIN: Self = <$t>::MIN;
            const MAX: Self = <$t>::MAX;
            const ZERO: Self = 0;
        }
    )*};
}

primitive_int!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);

/// Convert `value` to `T`, clamping to `T::MIN` or `T::MAX` when it is out of
/// range: a negative value below the range becomes `T::MIN`, anything above it
/// `T::MAX`.
#[inline]
pub(crate) fn saturating_cast<T, S>(value: S) -> T
where
    S: PrimitiveInt,
    T: PrimitiveInt + TryFrom<S>,
{
    match T::try_from(value) {
        Ok(converted) => converted,
        Err(_) if value < S::ZERO => T::MIN,
        Err(_) => T::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::saturating_cast;

    #[test]
    fn clamps_both_ends_and_keeps_values_in_range() {
        assert_eq!(saturating_cast::<usize, i64>(-5), 0);
        assert_eq!(saturating_cast::<usize, i64>(7), 7);
        assert_eq!(saturating_cast::<u32, i64>(i64::MAX), u32::MAX);
        assert_eq!(saturating_cast::<u32, i64>(-1), 0);
        assert_eq!(saturating_cast::<isize, usize>(usize::MAX), isize::MAX);
        assert_eq!(saturating_cast::<i64, usize>(3), 3);
        assert_eq!(saturating_cast::<i8, i64>(-1000), i8::MIN);
        assert_eq!(saturating_cast::<u16, u64>(u64::MAX), u16::MAX);
    }
}
