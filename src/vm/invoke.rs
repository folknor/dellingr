//! Typed host calls into Lua: [`State::invoke`].
//!
//! `invoke` owns the whole transfer of one host call: it writes the callee
//! and the arguments straight into the call window, runs the call, converts
//! the results and leaves the stack as it found it on every ordinary exit.
//! The same call spelled with the stack API (`push_number` per argument,
//! `call_anchor`, `to_number` per result, `pop`) passes one usability check
//! per operation, each of which reads the floating-point control register on
//! x86-64, and inserts the callee below the arguments; and a host that
//! returns early on a failed conversion leaves the results on its stack.
//!
//! One entry check covers the whole call. No host code runs between it and
//! the call: arguments and results are converted only by the sealed
//! implementations here, never by host trait impls. Every host callback the
//! call reaches is checked at its own return (`invoke_rust_fn`), so the
//! results are read under an environment that was checked after the last
//! host code ran.
//!
//! The arguments and results are a deliberately small, sealed set. String
//! arguments would intern, which can collect and fail partway; a host with
//! one uses the stack API. Results are converted like
//! [`State::raw_field`]'s fields, but only into owned types: the results are
//! dropped before `invoke` returns, so nothing may borrow them.
//
// Two larger shapes were weighed and not built. Separate callee,
// argument-span and result-destination operands for the call machinery buy
// nothing here: the callee pushed below its arguments already is a Lua
// callee's final call window, and a Rust callee keeps the single remove the
// stack cap requires (see `begin_call`). Decoding from the returned result
// span before the activation is torn down, instead of after
// `collect_returns`, would put the generic decoder inside
// `run_invocation`, which is live across every native re-entry, to save a
// slide of the returned values.

use super::anchor::Anchor;
use super::lua_val::Val;
use super::raw_field::sealed::{Convert, FieldFailure, FieldValue};
use super::{Result, State};
use crate::error::{ArgDetail, Error, ErrorKind};
use crate::instr::{ArgCount, RetCount};

mod sealed {
    use super::super::State;

    /// Writes one argument into the call window. The caller has already
    /// reserved its slot.
    pub trait EncodeArg {
        fn push(self, state: &mut State);
    }

    /// Writes the argument list into the call window. The caller has
    /// already reserved `COUNT` slots.
    pub trait EncodeArgs {
        const COUNT: u8;
        fn push(self, state: &mut State);
    }

    /// Converts the result at absolute stack index `slot`, the `position`th
    /// result (one-based).
    pub trait DecodeResult: Sized {
        fn decode(state: &State, slot: usize, position: u8) -> crate::Result<Self>;
    }

    /// Converts the `COUNT` results starting at absolute stack index `base`.
    pub trait DecodeResults: Sized {
        const COUNT: u8;
        fn decode(state: &State, base: usize) -> crate::Result<Self>;
    }
}

/// A value [`State::invoke`] can pass as an argument: `f64`, `bool`, `i32`
/// or `u32` (integers convert exactly to a Lua number). The trait is sealed.
pub trait InvokeArg: sealed::EncodeArg {}

/// The argument list of [`State::invoke`]: `()` or a tuple of up to eight
/// [`InvokeArg`] values. The trait is sealed.
pub trait InvokeArgs: sealed::EncodeArgs {}

/// A Rust type [`State::invoke`] can convert a result into: `f64`, `i8`,
/// `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`, `bool`, `String`,
/// `Vec<u8>`, or `Option` of one of those. The conversions are
/// [`State::raw_field`]'s. The trait is sealed.
pub trait InvokeResult: sealed::DecodeResult {}

/// The results of [`State::invoke`]: `()` or a tuple of up to eight
/// [`InvokeResult`] types. The trait is sealed.
pub trait InvokeResults: sealed::DecodeResults {}

impl sealed::EncodeArg for f64 {
    fn push(self, state: &mut State) {
        state.push_unchecked(Val::Num(self));
    }
}
impl InvokeArg for f64 {}

impl sealed::EncodeArg for bool {
    fn push(self, state: &mut State) {
        state.push_unchecked(Val::Bool(self));
    }
}
impl InvokeArg for bool {}

impl sealed::EncodeArg for i32 {
    fn push(self, state: &mut State) {
        state.push_unchecked(Val::Num(f64::from(self)));
    }
}
impl InvokeArg for i32 {}

impl sealed::EncodeArg for u32 {
    fn push(self, state: &mut State) {
        state.push_unchecked(Val::Num(f64::from(self)));
    }
}
impl InvokeArg for u32 {}

fn decode_with<T: for<'s> Convert<'s>>(state: &State, slot: usize, position: u8) -> Result<T> {
    let val = state.stack[slot];
    // The received type is classified only on failure, as `read_field` does.
    // Not a saving: every converter refuses an object result, so the heap
    // read an object's type needs happens anyway, and a result that converts
    // pays only a tag match.
    T::convert(FieldValue {
        val,
        heap: &state.heap,
    })
    .map_err(|failure| {
        let (detail, expected) = match failure {
            FieldFailure::Type(expected) => (ArgDetail::TypeMismatch, expected),
            FieldFailure::NoInteger => (ArgDetail::NoInteger, super::LuaType::Number),
            FieldFailure::OutOfRange => (ArgDetail::ValueOutOfRange, super::LuaType::Number),
            FieldFailure::InvalidUtf8 => (ArgDetail::InvalidUtf8, super::LuaType::String),
        };
        Error::without_location(ErrorKind::ResultConversion {
            index: position,
            detail,
            expected,
            received: val.typ(&state.heap),
        })
    })
}

// One `Option` layer only: a nested `Option` could never decode to
// `Some(None)`, and an open-ended family of result types would make the
// native frame of `invoke` unbounded in the types a host can name.
macro_rules! owned_result {
    ($($ty:ty),+) => {$(
        impl sealed::DecodeResult for $ty {
            fn decode(state: &State, slot: usize, position: u8) -> Result<Self> {
                decode_with(state, slot, position)
            }
        }
        impl InvokeResult for $ty {}
        impl sealed::DecodeResult for Option<$ty> {
            fn decode(state: &State, slot: usize, position: u8) -> Result<Self> {
                decode_with(state, slot, position)
            }
        }
        impl InvokeResult for Option<$ty> {}
    )+};
}

owned_result!(
    f64,
    i8,
    i16,
    i32,
    i64,
    u8,
    u16,
    u32,
    u64,
    bool,
    String,
    Vec<u8>
);

impl sealed::EncodeArgs for () {
    const COUNT: u8 = 0;
    fn push(self, _state: &mut State) {}
}
impl InvokeArgs for () {}

impl sealed::DecodeResults for () {
    const COUNT: u8 = 0;
    fn decode(_state: &State, _base: usize) -> Result<Self> {
        Ok(())
    }
}
impl InvokeResults for () {}

macro_rules! tuples {
    ($count:expr; $($name:ident $offset:expr),+) => {
        impl<$($name: InvokeArg),+> sealed::EncodeArgs for ($($name,)+) {
            const COUNT: u8 = $count;
            #[expect(non_snake_case, reason = "the bindings reuse the type parameter names")]
            fn push(self, state: &mut State) {
                let ($($name,)+) = self;
                $($name.push(state);)+
            }
        }
        impl<$($name: InvokeArg),+> InvokeArgs for ($($name,)+) {}

        impl<$($name: InvokeResult),+> sealed::DecodeResults for ($($name,)+) {
            const COUNT: u8 = $count;
            fn decode(state: &State, base: usize) -> Result<Self> {
                Ok(($($name::decode(state, base + $offset, $offset + 1)?,)+))
            }
        }
        impl<$($name: InvokeResult),+> InvokeResults for ($($name,)+) {}
    };
}

tuples!(1; A 0);
tuples!(2; A 0, B 1);
tuples!(3; A 0, B 1, C 2);
tuples!(4; A 0, B 1, C 2, D 3);
tuples!(5; A 0, B 1, C 2, D 3, E 4);
tuples!(6; A 0, B 1, C 2, D 3, E 4, F 5);
tuples!(7; A 0, B 1, C 2, D 3, E 4, F 5, G 6);
tuples!(8; A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);

impl State {
    /// Calls the anchored function with `args` and converts its results to
    /// `R`, leaving the stack as it was.
    ///
    /// ```
    /// use dellingr::{Program, State};
    ///
    /// let mut state = State::new();
    /// let program = Program::compile(
    ///     "function step(id, dt) return id * 10 + dt, id > 1 end",
    ///     None,
    /// )?;
    /// state.load(&program)?;
    /// state.call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(0))?;
    /// state.get_global("step")?;
    /// let step = state.anchor()?;
    ///
    /// for id in 1..=2_u32 {
    ///     let (position, moved): (f64, bool) = state.invoke(&step, (id, 0.5))?;
    ///     assert_eq!((position, moved), (f64::from(id) * 10.0 + 0.5, id > 1));
    /// }
    /// assert_eq!(state.get_top(), 0);
    /// # Ok::<(), dellingr::error::Error>(())
    /// ```
    ///
    /// The call behaves like [`State::call_anchor`] with
    /// `ArgCount::Fixed(n)` for `n` arguments and `RetCount::Fixed(m)` for
    /// the `m` results `R` names: missing results are `nil` (read as `None`
    /// by an `Option`), extra ones are dropped, and the cost, errors and
    /// `on_error` reports of the call are the same. Results convert as
    /// [`State::raw_field`] converts fields: `bool` takes only a Lua boolean,
    /// numbers and strings do not convert into each other, an integer type
    /// needs a finite integral number in its range, `String` needs valid
    /// UTF-8, and a NaN leaves as the canonical NaN.
    ///
    /// # Errors
    ///
    /// Before anything runs: a poisoned State or an unsupported
    /// floating-point environment, [`ErrorKind::InvalidAnchor`] for another
    /// State's handle, and `StackOverflow` without room for the callee and
    /// its arguments. None of these is reported to `on_error`. Any error of
    /// the call itself, with the stack restored to its height at entry.
    /// [`ErrorKind::ResultConversion`] when a result does not convert; the
    /// call completed, its effects stand, the results are dropped, and it is
    /// not reported to `on_error`.
    pub fn invoke<A: InvokeArgs, R: InvokeResults>(&mut self, f: &Anchor, args: A) -> Result<R> {
        self.guarded(|state| {
            let base = state.stack.len();
            let Some(callee) = state.registry.get(f) else {
                return Err(state.error(ErrorKind::InvalidAnchor));
            };
            state.check_stack_space(1 + usize::from(A::COUNT))?;
            // The callee goes first and the arguments above it, which is the
            // final call window for a Lua callee; nothing is inserted below
            // them afterwards.
            state.push_unchecked(callee);
            args.push(state);
            // A dispatched failure truncates the stack back to the callee's
            // slot, which is `base`.
            crate::native_call!(
                "invoke->call_unguarded",
                state.call_unguarded(ArgCount::Fixed(A::COUNT), RetCount::Fixed(R::COUNT))
            )?;
            let results = R::decode(state, base);
            state.stack.truncate(base);
            results
        })
    }
}
