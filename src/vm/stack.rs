//! Stack manipulation operations for the Lua VM.
//!
//! This module contains methods for pushing, popping, and manipulating
//! values on the VM stack.

use std::cmp::Ordering;

use super::lua_val::{RustFunc, Val};
use super::{ErrorKind, MAX_STACK_SIZE, Result, State};
use crate::error::InternalFault;

impl State {
    /// Returns the index of the top element in the stack. Because indices start
    /// at 1, this result is equal to the number of elements in the stack (and
    /// so 0 means an empty stack).
    pub fn get_top(&self) -> usize {
        self.stack.len() - self.in_flight.stack_bottom
    }

    /// Accepts any acceptable index, or 0, and sets the stack top to this index.
    /// If the new top is larger than the old one, then the new elements are
    /// filled with `nil`. If `index` is 0, then all visible stack elements are
    /// removed. Negative indices are relative to the current top: `-1` keeps the
    /// top unchanged, `-2` pops one value, and so on.
    ///
    /// Returns [`ErrorKind::InvalidStackIndex`] for an index below the stack
    /// bottom, or [`ErrorKind::StackOverflow`] if growth exceeds the stack cap.
    pub fn set_top(&mut self, i: isize) -> Result<()> {
        self.check_poisoned()?;
        let old_top = self.get_top();
        let new_top = match i.cmp(&0) {
            // Compute in signed space. `old_top + i` may be negative while
            // `old_top + i + 1` is still a valid top - set_top(-3) with two
            // values means "empty" - so an unsigned intermediate would reject
            // legal calls.
            Ordering::Less => {
                let top = isize::try_from(old_top)
                    .map_err(|_| self.error(ErrorKind::InvalidStackIndex { index: i }))?;
                top.checked_add(i)
                    .and_then(|target| target.checked_add(1))
                    .filter(|target| *target >= 0)
                    .map(|target| target as usize)
                    .ok_or_else(|| self.error(ErrorKind::InvalidStackIndex { index: i }))?
            }
            Ordering::Equal => 0,
            Ordering::Greater => usize::try_from(i)
                .map_err(|_| self.error(ErrorKind::InvalidStackIndex { index: i }))?,
        };

        match new_top.cmp(&old_top) {
            Ordering::Less => self.pop_n((old_top - new_top) as isize)?,
            Ordering::Equal => (),
            Ordering::Greater => {
                self.check_stack_space(new_top - old_top)?;
                for _ in old_top..new_top {
                    self.push_unchecked(Val::Nil);
                }
            }
        }
        Ok(())
    }

    /// Pops `n` elements from the stack.
    ///
    /// Returns [`ErrorKind::InvalidStackIndex`] when `n` is negative or exceeds
    /// the visible stack size.
    pub fn pop(&mut self, n: isize) -> Result<()> {
        self.check_poisoned()?;
        self.pop_n(n)
    }

    /// [`State::pop`] without the poison check, for VM-internal callers.
    pub(super) fn pop_n(&mut self, n: isize) -> Result<()> {
        let count = usize::try_from(n)
            .map_err(|_| self.error(ErrorKind::InvalidStackIndex { index: n }))?;
        if count > self.get_top() {
            return Err(self.error(ErrorKind::InvalidStackIndex { index: n }));
        }
        for _ in 0..count {
            self.pop_val();
        }
        Ok(())
    }

    /// Pop a value from the stack (internal helper).
    /// Panics if stack is empty - this indicates a VM bug, not a user error.
    /// Bytecode admission proves operand availability; public stack operations
    /// preflight their inputs. Dispatch does not recheck static stack effects.
    #[inline(always)]
    pub(super) fn pop_val(&mut self) -> Val {
        self.stack.pop().expect("VM bug: pop from empty stack")
    }

    /// Pushes a `nil` value onto the stack.
    pub fn push_nil(&mut self) -> Result<()> {
        self.check_poisoned()?;
        self.check_stack_slot()?;
        self.push_unchecked(Val::Nil);
        Ok(())
    }

    /// Pushes a number with value `n` onto the stack.
    pub fn push_number(&mut self, n: f64) -> Result<()> {
        self.check_poisoned()?;
        self.check_stack_slot()?;
        self.push_unchecked(Val::Num(n));
        Ok(())
    }

    /// Pushes a boolean onto the stack.
    pub fn push_boolean(&mut self, b: bool) -> Result<()> {
        self.check_poisoned()?;
        self.check_stack_slot()?;
        self.push_unchecked(Val::Bool(b));
        Ok(())
    }

    /// Pushes the given UTF-8 string onto the stack.
    pub fn push_string(&mut self, s: impl AsRef<str>) -> Result<()> {
        self.check_poisoned()?;
        self.check_stack_slot()?;
        let val = self.alloc_string(s.as_ref().as_bytes())?;
        self.push_unchecked(val);
        Ok(())
    }

    /// Pushes the State's interned key for a metamethod event. Unlike
    /// `push_string` it never allocates, so it cannot collect.
    pub(crate) fn push_meta_name(&mut self, name: super::MetaName) -> Result<()> {
        self.check_stack_slot()?;
        self.push_unchecked(self.meta_names.get(name));
        Ok(())
    }

    /// Pushes the given raw Lua string bytes onto the stack.
    pub fn push_bytes(&mut self, bytes: impl AsRef<[u8]>) -> Result<()> {
        self.check_poisoned()?;
        self.check_stack_slot()?;
        let val = self.alloc_string(bytes.as_ref())?;
        self.push_unchecked(val);
        Ok(())
    }

    /// Copies a byte range from a string already on the stack into a new Lua
    /// string. The source stack value remains rooted if allocation collects.
    pub(crate) fn push_bytes_from_stack_range(
        &mut self,
        idx: isize,
        range: std::ops::Range<usize>,
    ) -> Result<()> {
        let bytes = self.to_bytes(idx)?;
        // The copy is not redundant: `bytes` borrows `self.heap`, and pushing
        // needs `&mut self`. Copying out first ends the borrow, and it also
        // keeps the source rooted on the stack if the allocation collects.
        let mut copied = Vec::new();
        match bytes.get(range) {
            Some(slice) => copied.extend_from_slice(slice),
            None => {
                return Err(super::Error::internal_fault(
                    InternalFault::CaptureRangeOutsideString,
                ));
            }
        }
        self.push_bytes(copied)
    }

    /// Pushes a Rust function onto the stack.
    pub fn push_rust_fn(&mut self, f: RustFunc) -> Result<()> {
        self.check_poisoned()?;
        self.check_stack_slot()?;
        self.push_unchecked(Val::RustFn(f));
        Ok(())
    }

    /// Register and push a Rust function with a stable save/load id.
    #[cfg(feature = "snapshot")]
    pub fn push_named_rust_fn(&mut self, id: &str, f: RustFunc) -> Result<()> {
        self.check_poisoned()?;
        self.check_stack_slot()?;
        self.register_rust_fn(id, f)
            .map_err(Self::registration_error)?;
        self.push_unchecked(Val::RustFn(f));
        Ok(())
    }

    /// Pushes a copy of the element at the given index onto the stack.
    pub fn push_value(&mut self, i: isize) -> Result<()> {
        self.check_poisoned()?;
        let val = self.at_index(i)?;
        self.check_stack_slot()?;
        self.push_unchecked(val);
        Ok(())
    }

    /// Moves the top element into the given valid index, shifting up the
    /// elements above this index to open space.
    /// Returns [`ErrorKind::InvalidStackIndex`] when the index is outside the
    /// visible stack.
    #[deny(clippy::panic, clippy::manual_assert)]
    pub fn insert(&mut self, index: isize) -> Result<()> {
        self.check_poisoned()?;
        let idx = self.convert_idx(index)?;
        let slice = &mut self.stack[idx..];
        slice.rotate_right(1);
        Ok(())
    }

    /// Removes the element at the given valid index, shifting down the elements
    /// above this index to fill the gap.
    /// Returns [`ErrorKind::InvalidStackIndex`] when the index is outside the
    /// visible stack.
    #[deny(clippy::panic, clippy::manual_assert)]
    pub fn remove(&mut self, i: isize) -> Result<()> {
        self.check_poisoned()?;
        let idx = self.convert_idx(i)?;
        self.stack.remove(idx);
        Ok(())
    }

    /// Removes the `count` lowest visible values in one move, shifting the rest
    /// down. Equivalent to `count` calls of `remove(1)` at the cost of one.
    // The error index is diagnostic only; a count past isize::MAX saturates.
    #[allow(clippy::disallowed_methods)]
    pub(crate) fn remove_bottom(&mut self, count: usize) -> Result<()> {
        if count > self.get_top() {
            return Err(self.error(ErrorKind::InvalidStackIndex {
                index: isize::try_from(count).unwrap_or(isize::MAX),
            }));
        }
        let bottom = self.in_flight.stack_bottom;
        self.stack.drain(bottom..bottom + count);
        Ok(())
    }

    /// Pops a value from the stack, then replaces the value at the given index
    /// with that value. If the index points to the top element (e.g., -1),
    /// this is equivalent to a simple pop since we'd be replacing the popped
    /// value with itself. Returns [`ErrorKind::InvalidStackIndex`] when the
    /// index is outside the visible stack.
    #[deny(clippy::panic, clippy::manual_assert)]
    pub fn replace(&mut self, i: isize) -> Result<()> {
        self.check_poisoned()?;
        let idx = self.convert_idx(i)?;
        // A successful conversion requires a visible stack value above
        // stack_bottom, so this pop cannot empty the backing stack. Failure is
        // an internal VM invariant violation.
        let val = self
            .stack
            .pop()
            .expect("VM bug: pop from empty stack in replace()");
        // If idx pointed to the top element (which we just popped), skip the assignment
        // This handles replace(-1) correctly without panicking
        if idx < self.stack.len() {
            self.stack[idx] = val;
        }
        Ok(())
    }

    /// Copies the element at `from` into the valid index `to`, replacing the
    /// value at that position. Equivalent to Lua's `lua_copy`.
    pub fn copy_val(&mut self, from: isize, to: isize) -> Result<()> {
        self.check_poisoned()?;
        let val = self.at_index(from)?;
        let to = self.convert_idx(to)?;
        self.stack[to] = val;
        Ok(())
    }

    /// Returns whether the value at the given index is not `false` or `nil`.
    ///
    /// Follows Lua's acceptable-index rule: an index outside the current
    /// frame reads as `nil`, so this returns false. Standard library
    /// functions rely on that for optional trailing arguments. Use
    /// [`State::try_to_boolean`] to have an invalid index reported instead.
    pub fn to_boolean(&self, idx: isize) -> bool {
        matches!(self.try_to_boolean(idx), Ok(true))
    }

    /// Like [`State::to_boolean`], but returns
    /// [`ErrorKind::InvalidStackIndex`] when the index is invalid.
    pub fn try_to_boolean(&self, idx: isize) -> Result<bool> {
        self.check_poisoned()?;
        self.at_index(idx).map(|v| v.truthy())
    }

    /// Attempts to convert the value at the given index to a number.
    pub fn to_number(&self, idx: isize) -> Result<f64> {
        self.check_poisoned()?;
        let i = self.convert_idx(idx)?;
        let val = &self.stack[i];
        val.as_num()
            .ok_or_else(|| self.type_error(super::TypeError::Arithmetic(val.typ(&self.heap))))
    }

    /// Converts the value at the given index to a UTF-8 string.
    /// Lua strings with invalid UTF-8 bytes are converted lossily; a result
    /// that would exceed `MAX_STRING_BYTES` is rejected before conversion.
    pub fn to_string(&mut self, idx: isize) -> Result<String> {
        self.check_poisoned()?;
        let raw = self.bytes_with_default_string_coercion(idx)?;
        Self::lossy_utf8_checked(&raw)
    }

    /// Byte length of `raw` after lossy UTF-8 conversion: each invalid UTF-8
    /// chunk becomes one 3-byte U+FFFD. Computed without materializing the
    /// converted text so callers can check it against the string cap first.
    pub(crate) fn lossy_utf8_len(raw: &[u8]) -> usize {
        raw.utf8_chunks()
            .map(|chunk| {
                chunk.valid().len()
                    + if chunk.invalid().is_empty() {
                        0
                    } else {
                        char::REPLACEMENT_CHARACTER.len_utf8()
                    }
            })
            .sum()
    }

    /// Lossy UTF-8 conversion preflighted against `MAX_STRING_BYTES`: invalid
    /// bytes expand up to threefold, so a capped Lua string can otherwise
    /// produce an over-cap Rust `String`.
    pub(crate) fn lossy_utf8_checked(raw: &[u8]) -> Result<String> {
        crate::limits::checked_string_growth(0, Self::lossy_utf8_len(raw))?;
        Ok(String::from_utf8_lossy(raw).into_owned())
    }

    /// Returns exact Lua string bytes at the given index.
    pub fn to_bytes(&self, idx: isize) -> Result<&[u8]> {
        self.check_poisoned()?;
        let i = self.expect_string_at(idx)?;
        Ok(self.stack[i]
            .as_string(&self.heap)
            .expect("expect_string_at validated the value's string type"))
    }

    /// Validate a string while preserving the caller's relative argument index.
    pub(super) fn expect_string_at(&self, idx: isize) -> Result<usize> {
        self.expect_string_at_inner(idx, None)
    }

    fn expect_string_at_inner(&self, idx: isize, function_name: Option<&str>) -> Result<usize> {
        let i = self.convert_idx(idx)?;
        let val = &self.stack[i];
        if val.as_string_ptr().is_some() {
            return Ok(i);
        }
        // Iterator values use the same argument-type contract as to_bytes,
        // including when a snapshot supplies a wrong type. Reserve internal
        // errors for missing cache entries and failed-compilation ordering;
        // neither changes the argument index or the expected value type.
        Err(
            self.error(ErrorKind::ArgError(crate::error::ArgError::type_mismatch(
                idx,
                function_name,
                super::LuaType::String,
                Some(val.typ(&self.heap)),
            ))),
        )
    }

    /// Returns the type of the value at the given acceptable index.
    ///
    /// Follows Lua's acceptable-index rule: an index outside the current
    /// frame reads as `nil`, so this returns [`super::LuaType::Nil`].
    /// Standard library functions rely on that for optional trailing
    /// arguments. Use [`State::try_typ`] to have an invalid index reported.
    /// For objects (tables/functions), the type is looked up in the heap.
    pub fn typ(&self, idx: isize) -> super::LuaType {
        self.at_index(idx)
            .map_or(super::LuaType::Nil, |val| val.typ(&self.heap))
    }

    /// Like [`State::typ`], but returns [`ErrorKind::InvalidStackIndex`]
    /// when the index is invalid.
    pub fn try_typ(&self, idx: isize) -> Result<super::LuaType> {
        self.check_poisoned()?;
        self.at_index(idx).map(|val| val.typ(&self.heap))
    }

    /// Returns true if the values at the two indices are primitively equal.
    /// Does not invoke the `__eq` metamethod.
    ///
    /// Follows Lua's acceptable-index rule as `lua_rawequal` does: if either
    /// index is outside the current frame, the result is false. Use
    /// [`State::try_raw_equal`] to have an invalid index reported.
    pub fn raw_equal(&self, idx1: isize, idx2: isize) -> bool {
        matches!(self.try_raw_equal(idx1, idx2), Ok(true))
    }

    /// Like [`State::raw_equal`], but returns
    /// [`ErrorKind::InvalidStackIndex`] when either index is invalid.
    pub fn try_raw_equal(&self, idx1: isize, idx2: isize) -> Result<bool> {
        self.check_poisoned()?;
        Ok(self.at_index(idx1)? == self.at_index(idx2)?)
    }

    /// Get the value at the given index. Returns error if out of bounds.
    pub(super) fn at_index(&self, idx: isize) -> Result<Val> {
        let i = self.convert_idx(idx)?;
        Ok(self.stack[i])
    }

    /// Given a relative index, convert it to an absolute index to the stack.
    pub(super) fn convert_idx(&self, fake_idx: isize) -> Result<usize> {
        let stack_top = self.stack.len() as isize;
        let stack_bottom = self.in_flight.stack_bottom as isize;
        let stack_len = stack_top - stack_bottom;
        if fake_idx > 0 && fake_idx <= stack_len {
            Ok((fake_idx - 1 + stack_bottom) as usize)
        } else if fake_idx < 0 && fake_idx >= -stack_len {
            Ok((stack_top + fake_idx) as usize)
        } else {
            Err(self.error(ErrorKind::InvalidStackIndex { index: fake_idx }))
        }
    }

    /// Balances a stack after an operation that returns an indefinite number of
    /// results.
    pub(super) fn balance_stack(&mut self, expected: usize, received: usize) -> Result<()> {
        match expected.cmp(&received) {
            Ordering::Greater => {
                self.check_stack_space(expected - received)?;
                for _ in received..expected {
                    self.push_unchecked(Val::Nil);
                }
            }
            Ordering::Less => {
                for _ in expected..received {
                    self.pop_val();
                }
            }
            Ordering::Equal => (),
        }
        Ok(())
    }

    /// Append after a successful capacity check, or when replacing popped values.
    #[inline(always)]
    pub(super) fn push_unchecked(&mut self, val: Val) {
        self.stack.push(val);
    }

    /// Check and append an internal value that has no public typed push API.
    #[inline(always)]
    pub(super) fn push_val(&mut self, val: Val) -> Result<()> {
        self.check_stack_slot()?;
        self.push_unchecked(val);
        Ok(())
    }

    #[inline(always)]
    fn check_stack_slot(&self) -> Result<()> {
        #[cfg(not(feature = "fault-injection"))]
        let injected = false;
        #[cfg(feature = "fault-injection")]
        let injected = self.inject_fault(crate::FaultSite::StackSlot, self.stack.len(), true);
        if injected || self.stack.len() >= MAX_STACK_SIZE {
            return Err(self.error(ErrorKind::StackOverflow {
                size: self.stack.len().saturating_add(1),
                limit: MAX_STACK_SIZE,
            }));
        }
        Ok(())
    }

    /// Check that we have room for `n` more values on the stack.
    /// Returns an error if adding `n` values would exceed the stack limit.
    pub(crate) fn check_stack_space(&self, n: usize) -> Result<()> {
        let new_size = self.stack.len().saturating_add(n);
        #[cfg(not(feature = "fault-injection"))]
        let injected = false;
        #[cfg(feature = "fault-injection")]
        let injected = self.inject_fault(crate::FaultSite::StackSpace, n, true);
        if injected || new_size > MAX_STACK_SIZE {
            return Err(self.error(ErrorKind::StackOverflow {
                size: new_size,
                limit: MAX_STACK_SIZE,
            }));
        }
        Ok(())
    }
}
