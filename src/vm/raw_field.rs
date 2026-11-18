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
use std::sync::atomic::AtomicUsize;

// Public items in a private module: nameable only inside this crate, which
// is what seals the trait. `State::invoke` reuses the conversions for its
// results (`vm/invoke.rs`).
pub(super) mod sealed {
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

/// A field name for [`State::raw_field`] that remembers where it was last
/// found, for a callback that reads the same fields of consistently laid-out
/// tables over and over.
///
/// ```
/// use dellingr::FieldName;
///
/// static X: FieldName = FieldName::new("x");
/// # fn read(state: &dellingr::State) -> dellingr::Result<f64> {
/// let x: f64 = state.raw_field(1, &X)?;
/// # Ok(x)
/// # }
/// ```
///
/// It reads exactly what the plain name reads. The hint is a slot index
/// checked on every read: a hit needs the table's live key at that index to
/// spell the name, so the hint holds no VM value and is sound across
/// States, threads, snapshot loads, collections and tables of any layout.
/// A stale or foreign hint costs one failed check before the ordinary
/// lookup, which then refreshes it. One descriptor shared by sites that
/// read differently laid-out tables keeps missing; give each decode site
/// its own.
//
// The hint is a relaxed atomic rather than a `Cell` so a descriptor can be
// a plain `static`: it publishes one speculative integer that every read
// validates, so no ordering is needed. A hint that holds an interned key
// instead of an index was refused: the key would have to be rooted, which
// changes later string identity.
pub struct FieldName {
    name: &'static str,
    hint: AtomicUsize,
}

// Shows the name only: the hint is whatever slot the last read through the
// descriptor found, possibly in another State, and must not leak into
// anything a script or replay can see.
impl std::fmt::Debug for FieldName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldName")
            .field("name", &self.name)
            .finish()
    }
}

impl FieldName {
    /// A descriptor for field `name`, with no hint yet.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            hint: AtomicUsize::new(usize::MAX),
        }
    }

    /// The field's name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

/// The field name [`State::raw_field`] takes: a `&str`, or a
/// [`FieldName`] that remembers the field's slot. Sealed.
pub trait FieldKey: sealed_key::Key + Copy {}

mod sealed_key {
    use super::super::lua_val::Val;
    use super::super::object::GcHeap;
    use super::super::table::Table;
    use super::FieldName;
    use super::sealed::FieldValue;
    use std::sync::atomic::Ordering;

    /// A table to look a field up in, with the heap that holds its keys.
    #[derive(Clone, Copy)]
    pub struct Lookup<'a> {
        pub(in super::super) heap: &'a GcHeap,
        pub(in super::super) table: &'a Table,
    }

    pub trait Key {
        fn name(&self) -> &str;
        /// The table's own value for this name, nil when absent.
        fn lookup<'a>(&self, at: Lookup<'a>) -> FieldValue<'a>;
    }

    impl Key for &str {
        fn name(&self) -> &str {
            self
        }

        // Inline here and not on the descriptor's lookup: measured both
        // ways on `host_records` (reference/performance.md, "Host field
        // descriptors").
        #[inline]
        fn lookup<'a>(&self, Lookup { heap, table }: Lookup<'a>) -> FieldValue<'a> {
            // A key no live string spells cannot be in any table: reachable
            // keys keep their interned strings alive.
            let val = heap
                .find_string(self.as_bytes())
                .map_or(Val::Nil, |key| table.get(&Val::Str(key)));
            FieldValue { val, heap }
        }
    }

    impl Key for &FieldName {
        fn name(&self) -> &str {
            self.name
        }

        fn lookup<'a>(&self, Lookup { heap, table }: Lookup<'a>) -> FieldValue<'a> {
            let name = self.name.as_bytes();
            // A string key is interned by content and a table holds at most
            // one slot for it, so a live slot whose key spells the name is
            // the one the lookup would find. An array slot's key is a number
            // and never matches; `get_index` filters vacant and nil slots.
            if let Some((key, value)) = table.get_index(self.hint.load(Ordering::Relaxed))
                && key.as_string(heap) == Some(name)
            {
                return FieldValue { val: value, heap };
            }
            let val = match heap
                .find_string(name)
                .and_then(|key| table.get_with_index(&Val::Str(key)))
            {
                Some((index, value)) => {
                    self.hint.store(index, Ordering::Relaxed);
                    value
                }
                None => Val::Nil,
            };
            FieldValue { val, heap }
        }
    }
}

impl FieldKey for &str {}
impl FieldKey for &FieldName {}

mod sealed_fields {
    use super::super::{Result, State};
    use super::sealed_key::Lookup;

    /// Reads a tuple of fields of one resolved table, in tuple order.
    pub trait ReadFields<'s, K>: Sized {
        fn read(state: &'s State, table_arg: isize, at: Lookup<'s>, keys: K) -> Result<Self>;
    }
}

/// The tuple [`State::raw_fields`] reads into: up to eight [`RawField`]
/// types, named by a tuple of the same number of [`FieldKey`]s. Sealed.
pub trait RawFields<'s, K>: sealed_fields::ReadFields<'s, K> {}

macro_rules! field_tuples {
    ($(($($ty:ident $key:ident),+))+) => {$(
        impl<'s, $($ty: RawField<'s>, $key: FieldKey),+>
            sealed_fields::ReadFields<'s, ($($key,)+)> for ($($ty,)+)
        {
            #[expect(non_snake_case, reason = "the bindings reuse the type parameter names")]
            fn read(
                state: &'s State,
                table_arg: isize,
                at: sealed_key::Lookup<'s>,
                ($($key,)+): ($($key,)+),
            ) -> Result<Self> {
                Ok(($(state.read_field::<$ty>(table_arg, at, $key)?,)+))
            }
        }
        impl<'s, $($ty: RawField<'s>, $key: FieldKey),+> RawFields<'s, ($($key,)+)>
            for ($($ty,)+)
        {
        }
    )+};
}

field_tuples! {
    (A KA)
    (A KA, B KB)
    (A KA, B KB, C KC)
    (A KA, B KB, C KC, D KD)
    (A KA, B KB, C KC, D KD, E KE)
    (A KA, B KB, C KC, D KD, E KE, F KF)
    (A KA, B KB, C KC, D KD, E KE, F KF, G KG)
    (A KA, B KB, C KC, D KD, E KE, F KF, G KG, H KH)
}

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
    ///
    /// `name` is a `&str` or a [`FieldName`]; the descriptor skips the name
    /// hash, the string-pool probe and the table probe while its hint holds,
    /// and reads the same value and error either way.
    //
    // Per-field `check_usable` stays: host code can change the
    // floating-point environment between reads, which is also why a record
    // view checked once at creation was refused (host code runs between its
    // reads). `raw_fields` is the once-per-call form. The `host_records`
    // workload (eight fields of persistent map-stored records per callback)
    // measures descriptors against plain names and the batch against
    // separate reads.
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
    pub fn raw_field<'s, T: RawField<'s>>(
        &'s self,
        table_arg: isize,
        name: impl FieldKey,
    ) -> Result<T> {
        self.check_usable()?;
        let table = self.table_arg(table_arg)?;
        self.read_field(table_arg, table, name)
    }

    /// Reads several fields of the table at argument `table_arg` at once,
    /// each exactly as [`State::raw_field`] reads it: `keys` is a tuple of
    /// up to eight names (`&str` or [`FieldName`]) and `T` the tuple of
    /// types they read into.
    ///
    /// ```
    /// use dellingr::{FieldName, State};
    ///
    /// static PLAYER: FieldName = FieldName::new("player");
    /// static X: FieldName = FieldName::new("x");
    ///
    /// fn spawn(state: &mut State) -> dellingr::Result<u8> {
    ///     let (player, x, flags): (u32, f64, Option<&str>) =
    ///         state.raw_fields(1, (&PLAYER, &X, "flags"))?;
    ///     assert_eq!((player, x, flags), (1, 2.5, Some("Structure")));
    ///     Ok(0)
    /// }
    /// # let mut state = State::new();
    /// # let handle = state.add_rust_fn("spawn", spawn)?;
    /// # state.push_rust_fn(handle)?;
    /// # state.set_global("spawn")?;
    /// # let program = dellingr::Program::compile(r#"spawn{player = 1, x = 2.5, flags = "Structure"}"#, None)?;
    /// # state.load(&program)?;
    /// # state.call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(0))?;
    /// # Ok::<(), dellingr::error::Error>(())
    /// ```
    ///
    /// The State is checked and the table resolved once for the whole
    /// tuple, where separate reads pay both per field. That is sound
    /// because no host code runs between the reads: the keys and
    /// conversions are the sealed ones `raw_field` uses (an owned `String`
    /// or `Vec<u8>` allocates, and the allocator is assumed to leave the
    /// floating-point environment alone, as everywhere else).
    ///
    /// With turbofish the key tuple's type is inferred:
    /// `raw_fields::<_, (f64, u32)>(1, ("x", "player"))`.
    ///
    /// # Errors
    ///
    /// The errors of [`State::raw_field`]. Fields are read in tuple order
    /// and the first that fails is the error: the same first error reading
    /// these keys one by one would return, with the State and the
    /// floating-point environment unchanged between them.
    pub fn raw_fields<'s, K, T: RawFields<'s, K>>(
        &'s self,
        table_arg: isize,
        keys: K,
    ) -> Result<T> {
        self.check_usable()?;
        let table = self.table_arg(table_arg)?;
        T::read(self, table_arg, table, keys)
    }

    /// The table at argument `table_arg`, or the argument error `raw_field`
    /// reports for a missing or non-table argument.
    fn table_arg(&self, table_arg: isize) -> Result<sealed_key::Lookup<'_>> {
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
        Ok(sealed_key::Lookup {
            heap: &self.heap,
            table: table_ref,
        })
    }

    /// One field of the resolved table `at`, converted to `T`; the field
    /// error names argument `table_arg`.
    fn read_field<'s, T: RawField<'s>>(
        &'s self,
        table_arg: isize,
        at: sealed_key::Lookup<'s>,
        name: impl FieldKey,
    ) -> Result<T> {
        let value = name.lookup(at);
        let val = value.val;
        T::convert(value).map_err(|failure| {
            let received = val.typ(&self.heap);
            let name = name.name();
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
