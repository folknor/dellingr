//! Typed reads of a table argument's own fields, for host functions.
//!
//! [`State::raw_field`] reads `t[name]` for the table `t` at an argument
//! index without metamethods, converts it to a Rust type and, on failure,
//! returns an argument error that names the field. It is meant for plain
//! option tables such as `game.spawn{player = 1, x = 2}`.

use super::lua_val::Val;
use super::{LuaType, Result, State};
use crate::error::{ArgDetail, ArgError, ErrorKind};
use sealed::{FieldFailure, FieldValue};

// Public items in a private module: nameable only inside this crate, which
// is what seals the trait.
mod sealed {
    use super::super::LuaType;
    use super::super::lua_val::Val;
    use super::super::object::GcHeap;

    /// Why a field value could not be converted.
    #[derive(Debug)]
    pub enum FieldFailure {
        /// The value has the wrong Lua type.
        Type(LuaType),
        /// A number is not a finite integer.
        NoInteger,
        /// An integer is outside the target type's range.
        OutOfRange,
        /// A string is not valid UTF-8.
        InvalidUtf8,
    }

    /// The value of a field, with the heap needed to read strings.
    pub struct FieldValue<'a> {
        pub(in super::super) val: Val,
        pub(in super::super) heap: &'a GcHeap,
    }

    /// The conversion behind [`super::RawField`]. Sealed: a field read never
    /// runs Lua, touches the stack or charges cost, which an outside
    /// implementation could not promise.
    ///
    /// `'s` is the borrow of the State the read holds, so a borrowed
    /// conversion (`&str`, `&[u8]`) can return the string's own bytes.
    pub trait Convert<'s>: Sized {
        fn convert(value: FieldValue<'s>) -> Result<Self, FieldFailure>;
    }
}

/// A Rust type [`State::raw_field`] can read a field into.
///
/// Implemented for `f64`, `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`,
/// `u64`, `bool`, `String`, `Vec<u8>`, the borrowed `&str` and `&[u8]`, and
/// for `Option<T>` whenever `T` implements `RawField`. A borrowed read holds
/// the State's shared borrow for as long as the result lives and copies
/// nothing. The trait is sealed.
//
// The integer set is a choice for option-table fields, not an oversight.
// `i128`/`u128` would be correct (every integral f64 below 2^127 or 2^128
// converts exactly) but no embedder reads one; add them when one does.
// `isize`/`usize` are left out because their accepted range would vary with
// the host target, which a deterministic VM should not expose to scripts.
pub trait RawField<'s>: sealed::Convert<'s> {}

fn number(value: &FieldValue<'_>) -> std::result::Result<f64, FieldFailure> {
    value
        .val
        .as_num()
        .ok_or(FieldFailure::Type(LuaType::Number))
}

/// A finite integral number, or `NoInteger`. `-0.0` is zero.
fn integral(value: &FieldValue<'_>) -> std::result::Result<f64, FieldFailure> {
    let n = number(value)?;
    // For a finite n, truncation leaves the bits unchanged exactly when n is
    // integral (-0.0 included).
    if n.is_finite() && n.trunc().to_bits() == n.to_bits() {
        Ok(n)
    } else {
        Err(FieldFailure::NoInteger)
    }
}

impl sealed::Convert<'_> for f64 {
    fn convert(value: FieldValue<'_>) -> std::result::Result<Self, FieldFailure> {
        // A NaN leaves as the canonical NaN, as from `State::to_number`.
        number(&value).map(crate::numeral::canonical_nan)
    }
}
impl RawField<'_> for f64 {}

// Every bound of a type up to 32 bits is an exact f64, so an inclusive
// comparison is exact.
macro_rules! small_integer {
    ($($ty:ty),+) => {$(
        impl sealed::Convert<'_> for $ty {
            fn convert(value: FieldValue<'_>) -> std::result::Result<Self, FieldFailure> {
                let n = integral(&value)?;
                if n < f64::from(<$ty>::MIN) || n > f64::from(<$ty>::MAX) {
                    return Err(FieldFailure::OutOfRange);
                }
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "n is integral and inside the type's range, checked above"
                )]
                Ok(n as $ty)
            }
        }
        impl RawField<'_> for $ty {}
    )+};
}

small_integer!(i8, i16, i32, u8, u16, u32);

// 2^63 and 2^64 are exact f64 values; the type maxima are not (they round
// up to these), so the upper bounds are exclusive.
const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;
const TWO_POW_64: f64 = 18_446_744_073_709_551_616.0;

impl sealed::Convert<'_> for i64 {
    fn convert(value: FieldValue<'_>) -> std::result::Result<Self, FieldFailure> {
        let n = integral(&value)?;
        if !(-TWO_POW_63..TWO_POW_63).contains(&n) {
            return Err(FieldFailure::OutOfRange);
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "n is integral and inside the i64 range, checked above"
        )]
        Ok(n as i64)
    }
}
impl RawField<'_> for i64 {}

impl sealed::Convert<'_> for u64 {
    fn convert(value: FieldValue<'_>) -> std::result::Result<Self, FieldFailure> {
        let n = integral(&value)?;
        if !(0.0..TWO_POW_64).contains(&n) {
            return Err(FieldFailure::OutOfRange);
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "n is integral and inside the u64 range, checked above"
        )]
        Ok(n as u64)
    }
}
impl RawField<'_> for u64 {}

impl sealed::Convert<'_> for bool {
    fn convert(value: FieldValue<'_>) -> std::result::Result<Self, FieldFailure> {
        match value.val {
            Val::Bool(b) => Ok(b),
            _ => Err(FieldFailure::Type(LuaType::Boolean)),
        }
    }
}
impl RawField<'_> for bool {}

fn bytes<'a>(value: &FieldValue<'a>) -> std::result::Result<&'a [u8], FieldFailure> {
    value
        .val
        .as_string(value.heap)
        .ok_or(FieldFailure::Type(LuaType::String))
}

impl sealed::Convert<'_> for Vec<u8> {
    fn convert(value: FieldValue<'_>) -> std::result::Result<Self, FieldFailure> {
        bytes(&value).map(<[u8]>::to_vec)
    }
}
impl RawField<'_> for Vec<u8> {}

impl<'s> sealed::Convert<'s> for &'s [u8] {
    fn convert(value: FieldValue<'s>) -> std::result::Result<Self, FieldFailure> {
        bytes(&value)
    }
}
impl<'s> RawField<'s> for &'s [u8] {}

impl<'s> sealed::Convert<'s> for &'s str {
    fn convert(value: FieldValue<'s>) -> std::result::Result<Self, FieldFailure> {
        str::from_utf8(bytes(&value)?).map_err(|_| FieldFailure::InvalidUtf8)
    }
}
impl<'s> RawField<'s> for &'s str {}

impl sealed::Convert<'_> for String {
    fn convert(value: FieldValue<'_>) -> std::result::Result<Self, FieldFailure> {
        let raw = bytes(&value)?;
        str::from_utf8(raw)
            .map(str::to_owned)
            .map_err(|_| FieldFailure::InvalidUtf8)
    }
}
impl RawField<'_> for String {}

impl<'s, T: RawField<'s>> sealed::Convert<'s> for Option<T> {
    fn convert(value: FieldValue<'s>) -> std::result::Result<Self, FieldFailure> {
        if matches!(value.val, Val::Nil) {
            Ok(None)
        } else {
            T::convert(value).map(Some)
        }
    }
}
impl<'s, T: RawField<'s>> RawField<'s> for Option<T> {}

impl State {
    /// Reads field `name` of the table at argument `table_arg` into `T`,
    /// without metamethods: only the table's own fields are seen, so a field
    /// supplied through an `__index` metatable reads as absent. Meant for
    /// plain option tables.
    ///
    /// ```
    /// use dellingr::{ArgCount, Program, RetCount, State};
    ///
    /// fn spawn(state: &mut State) -> dellingr::Result<u8> {
    ///     let player: u32 = state.raw_field(1, "player")?;
    ///     let x: f64 = state.raw_field(1, "x")?;
    ///     let flags: Option<String> = state.raw_field(1, "flags")?;
    ///     assert_eq!((player, x, flags.as_deref()), (1, 2.5, Some("Structure")));
    ///     Ok(0)
    /// }
    ///
    /// let mut state = State::new();
    /// let handle = state.add_rust_fn("spawn", spawn)?;
    /// state.push_rust_fn(handle)?;
    /// state.set_global("spawn")?;
    /// let program = Program::compile(r#"spawn{player = 1, x = 2.5, flags = "Structure"}"#, None)?;
    /// state.load(&program)?;
    /// state.call(ArgCount::Fixed(0), RetCount::Fixed(0))?;
    /// # Ok::<(), dellingr::error::Error>(())
    /// ```
    ///
    /// A missing field and a `nil` field are the same: `Option<T>` reads
    /// them as `None`, any other `T` fails with "got nil". Numbers are not
    /// read as strings nor strings as numbers, and `bool` takes only a Lua
    /// boolean. An integer type requires a finite integral number inside its
    /// range. `String` and `&str` require valid UTF-8; `Vec<u8>` and `&[u8]`
    /// take the exact bytes. `&str` and `&[u8]` borrow the string in place
    /// instead of copying it, so the State stays borrowed while they live.
    //
    // Prepared field descriptors were weighed for repeated reads and not
    // added. A descriptor holding an interned key would have to root it,
    // which changes later string identity; one holding the name's bytes plus
    // a slot-index hint (hit: the key at that index spells the name) is sound
    // across States, snapshot loads and fresh tables, and can live in a
    // `Cell` in user data, but a hit still compares the name and saves only
    // the name hash, one string-pool probe and the table probe per field.
    // Small option tables are inline and scan at most a few handles. Add it
    // when a registered host-traffic workload decoding many fields of
    // consistently laid-out records shows those lookups. Per-field
    // `check_usable` stays either way: host code can change the
    // floating-point environment between reads.
    ///
    /// The read is free, leaves the stack unchanged, and never runs Lua.
    ///
    /// # Errors
    ///
    /// An [`ErrorKind::ArgError`] for the argument: table expected when it is
    /// missing or not a table, otherwise an error whose
    /// [`ArgError::field`] names the field, rendered as
    /// `bad argument #1 to 'spawn' (field 'x': number expected, got string)`.
    /// [`ErrorKind::InvalidStackIndex`] for argument 0, and the usual refusal
    /// of a poisoned State. Argument numbers follow [`State::check_type`].
    pub fn raw_field<'s, T: RawField<'s>>(&'s self, table_arg: isize, name: &str) -> Result<T> {
        self.check_usable()?;
        if table_arg == 0 {
            return Err(self.error(ErrorKind::InvalidStackIndex { index: table_arg }));
        }
        let table = if self.get_top() < table_arg.unsigned_abs() {
            None
        } else {
            Some(self.at_index(table_arg)?)
        };
        let Some(table_ref) = table
            .and_then(|val| val.as_object_ptr())
            .and_then(|ptr| self.heap.as_table_ref(ptr))
        else {
            let received = table.map(|val| val.typ(&self.heap));
            let func_name = self.active_rust_function_name();
            return Err(self.error(ErrorKind::ArgError(ArgError::type_mismatch(
                table_arg,
                func_name.as_deref(),
                LuaType::Table,
                received,
            ))));
        };
        // A key no live string spells cannot be in any table: reachable keys
        // keep their interned strings alive.
        let val = self
            .heap
            .find_string(name.as_bytes())
            .map_or(Val::Nil, |key| table_ref.get(&Val::Str(key)));
        let received = val.typ(&self.heap);
        T::convert(FieldValue {
            val,
            heap: &self.heap,
        })
        .map_err(|failure| {
            let (detail, expected) = match failure {
                FieldFailure::Type(expected) => (ArgDetail::TypeMismatch, Some(expected)),
                FieldFailure::NoInteger => (ArgDetail::NoInteger, Some(LuaType::Number)),
                FieldFailure::OutOfRange => (ArgDetail::ValueOutOfRange, Some(LuaType::Number)),
                FieldFailure::InvalidUtf8 => (ArgDetail::InvalidUtf8, Some(LuaType::String)),
            };
            let func_name = self.active_rust_function_name();
            self.error(ErrorKind::ArgError(ArgError::field_error(
                table_arg,
                func_name.as_deref(),
                name,
                detail,
                expected,
                received,
            )))
        })
    }
}
