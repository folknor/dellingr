//! String-library byte arguments read where they lie.
//!
//! The pattern functions (`find`, `match`, `gsub`, and `gmatch` once its
//! iterator runs) read their subject and pattern without copying them: a
//! string argument is read in place in its stack slot, which roots it for the
//! whole call (a `gsub` replacement call runs above that slot and cannot
//! change it). A read borrows the State only for its own span, through a
//! split of the State's fields that leaves the cost meter and interrupt flag
//! usable while the bytes are borrowed, so the matcher and the plain search
//! charge per unit as they scan. Anything that must outlive a read (a capture
//! string, `gsub` output) is copied out of the range it needs, never the
//! whole argument.
//!
//! A number argument is rendered once into an owned buffer instead of being
//! interned: its bytes are needed only during the call, and interning them
//! would add a heap allocation (and with it a collection point) that the call
//! never had. Only `gmatch` interns a number argument, because its iterator
//! keeps the subject and pattern beyond the call.

use std::ops::Range;
use std::sync::atomic::AtomicBool;

use super::object::GcHeap;
use super::{CostMeter, Result, State, Val};
use crate::cost_model::Charge;
use crate::error::InternalFault;

/// A string-library argument accepted as a string or a number.
pub(crate) enum StringArg {
    /// A string, read in place at this callback-relative stack index.
    Slot(isize),
    /// A number's rendering, owned for the duration of the call.
    Rendered(Vec<u8>),
}

impl StringArg {
    /// Whether this argument is a string in its stack slot (so a pattern
    /// read there has an interned key in the compiled-pattern cache).
    pub(crate) fn slot(&self) -> Option<isize> {
        match self {
            Self::Slot(idx) => Some(*idx),
            Self::Rendered(_) => None,
        }
    }
}

impl State {
    /// The argument at `idx`, which the caller has already type-gated as a
    /// string or a number. A string stays in its slot; a number is rendered.
    pub(crate) fn string_arg(&mut self, idx: isize) -> Result<StringArg> {
        if self.typ(idx) == super::LuaType::String {
            Ok(StringArg::Slot(idx))
        } else {
            Ok(StringArg::Rendered(
                self.bytes_with_default_string_coercion(idx)?,
            ))
        }
    }

    /// The argument's bytes, borrowed for as long as the State is not
    /// mutated.
    pub(crate) fn string_arg_bytes<'a>(&'a self, arg: &'a StringArg) -> Result<&'a [u8]> {
        match arg {
            StringArg::Slot(idx) => self.to_bytes(*idx),
            StringArg::Rendered(bytes) => Ok(bytes),
        }
    }

    /// Run `operation` over the bytes of two arguments (a subject and a
    /// plain needle) with a meter for `charge` and the interrupt flag. The
    /// borrow ends when `operation` returns.
    pub(crate) fn with_string_args<T>(
        &mut self,
        first: &StringArg,
        second: &StringArg,
        charge: Charge,
        operation: impl FnOnce(&[u8], &[u8], CostMeter<'_>, &AtomicBool) -> T,
    ) -> Result<T> {
        let first_slot = first
            .slot()
            .map(|idx| self.expect_string_at(idx))
            .transpose()?;
        let second_slot = second
            .slot()
            .map(|idx| self.expect_string_at(idx))
            .transpose()?;
        let State {
            stack,
            heap,
            cost,
            interrupt,
            ..
        } = self;
        Ok(operation(
            resolved_bytes(first, first_slot, stack, heap),
            resolved_bytes(second, second_slot, stack, heap),
            cost.charge_meter(charge),
            interrupt.flag(),
        ))
    }

    /// `with_string_args` for one argument: the subject a matcher runs over.
    pub(crate) fn with_string_arg<T>(
        &mut self,
        arg: &StringArg,
        charge: Charge,
        operation: impl FnOnce(&[u8], CostMeter<'_>, &AtomicBool) -> T,
    ) -> Result<T> {
        let slot = arg
            .slot()
            .map(|idx| self.expect_string_at(idx))
            .transpose()?;
        let State {
            stack,
            heap,
            cost,
            interrupt,
            ..
        } = self;
        Ok(operation(
            resolved_bytes(arg, slot, stack, heap),
            cost.charge_meter(charge),
            interrupt.flag(),
        ))
    }

    /// Push `range` of the argument as a new Lua string, copying only the
    /// range.
    pub(crate) fn push_string_arg_range(
        &mut self,
        arg: &StringArg,
        range: Range<usize>,
    ) -> Result<()> {
        match arg {
            StringArg::Slot(idx) => self.push_bytes_from_stack_range(*idx, range),
            StringArg::Rendered(bytes) => {
                let bytes = bytes.get(range).ok_or_else(|| {
                    super::Error::internal_fault(InternalFault::CaptureRangeOutsideString)
                })?;
                self.push_bytes(bytes)
            }
        }
    }

    /// Store the argument into the stack as a Lua string: a string slot's
    /// value as it is, a rendering interned.
    pub(crate) fn push_string_arg(&mut self, arg: &StringArg) -> Result<()> {
        match arg {
            StringArg::Slot(idx) => self.push_value(*idx),
            StringArg::Rendered(bytes) => self.push_bytes(bytes),
        }
    }
}

/// The bytes of an argument whose slot (if it has one) `with_string_args`
/// resolved and type-checked.
fn resolved_bytes<'a>(
    arg: &'a StringArg,
    slot: Option<usize>,
    stack: &'a [Val],
    heap: &'a GcHeap,
) -> &'a [u8] {
    match (arg, slot) {
        (StringArg::Rendered(bytes), _) => bytes,
        (StringArg::Slot(_), Some(slot)) => stack[slot]
            .as_string(heap)
            .expect("expect_string_at validated the slot's string type"),
        (StringArg::Slot(_), None) => {
            unreachable!("with_string_args resolves every slot argument to a stack index")
        }
    }
}
