//! Stack manipulation operations for the Lua VM.
//!
//! This module contains methods for pushing, popping, and manipulating
//! values on the VM stack.

use std::cmp::Ordering;

use super::lua_val::{RustFunc, Val};
use super::{Error, ErrorKind, Result, State, MAX_STACK_SIZE};

impl State {
    /// Returns the index of the top element in the stack. Because indices start
    /// at 1, this result is equal to the number of elements in the stack (and
    /// so 0 means an empty stack).
    pub fn get_top(&self) -> usize {
        self.stack.len() - self.stack_bottom
    }

    /// Accepts any acceptable index, or 0, and sets the stack top to this index.
    /// If the new top is larger than the old one, then the new elements are filled
    /// with `nil`. If `index` is 0, then all stack elements are removed.
    pub fn set_top(&mut self, i: isize) {
        match i.cmp(&0) {
            Ordering::Less => {
                panic!("negative not supported yet ({})", i);
            }
            Ordering::Equal => {
                self.stack.truncate(self.stack_bottom);
            }
            Ordering::Greater => {
                let i = i as usize;
                let old_top = self.get_top();
                match i.cmp(&old_top) {
                    Ordering::Less => {
                        self.pop((old_top - i) as isize);
                    }
                    Ordering::Equal => (),
                    Ordering::Greater => {
                        for _ in old_top..i {
                            self.push_nil();
                        }
                    }
                }
            }
        }
    }

    /// Pops `n` elements from the stack.
    pub fn pop(&mut self, n: isize) {
        assert!(
            n <= self.get_top() as isize,
            "Tried to pop too many elements ({})",
            n
        );
        for _ in 0..n {
            self.pop_val();
        }
    }

    /// Pop a value from the stack (internal helper).
    #[inline(always)]
    pub(super) fn pop_val(&mut self) -> Val {
        self.stack.pop().unwrap()
    }

    /// Pushes a `nil` value onto the stack.
    pub fn push_nil(&mut self) {
        self.stack.push(Val::Nil);
    }

    /// Pushes a number with value `n` onto the stack.
    pub fn push_number(&mut self, n: f64) {
        self.stack.push(Val::Num(n));
    }

    /// Pushes a boolean onto the stack.
    pub fn push_boolean(&mut self, b: bool) {
        self.stack.push(Val::Bool(b));
    }

    /// Pushes the given string onto the stack.
    pub fn push_string(&mut self, s: String) {
        let val = self.alloc_string(s);
        self.stack.push(val);
    }

    /// Pushes a Rust function onto the stack.
    pub fn push_rust_fn(&mut self, f: RustFunc) {
        self.stack.push(Val::RustFn(f));
    }

    /// Pushes a copy of the element at the given index onto the stack.
    pub fn push_value(&mut self, i: isize) -> Result<()> {
        let val = self.at_index(i)?;
        self.stack.push(val);
        Ok(())
    }

    /// Moves the top element into the given valid index, shifting up the
    /// elements above this index to open space.
    pub fn insert(&mut self, index: isize) -> Result<()> {
        let idx = self.convert_idx(index)?;
        let slice = &mut self.stack[idx..];
        slice.rotate_right(1);
        Ok(())
    }

    /// Removes the element at the given valid index, shifting down the elements
    /// above this index to fill the gap.
    pub fn remove(&mut self, i: isize) -> Result<()> {
        let idx = self.convert_idx(i)?;
        self.stack.remove(idx);
        Ok(())
    }

    /// Pops a value from the stack, then replaces the value at the given index
    /// with that value.
    pub fn replace(&mut self, i: isize) -> Result<()> {
        let idx = self.convert_idx(i)?;
        let val = self.stack.pop().unwrap();
        self.stack[idx] = val;
        Ok(())
    }

    /// Copies the element at `from` into the valid index `to`, replacing the
    /// value at that position. Equivalent to Lua's `lua_copy`.
    pub fn copy_val(&mut self, from: isize, to: isize) -> Result<()> {
        let val = self.at_index(from)?;
        let to = self.convert_idx(to)?;
        self.stack[to] = val;
        Ok(())
    }

    /// Returns whether the value at the given index is not `false` or `nil`.
    /// Returns false if the index is invalid.
    pub fn to_boolean(&self, idx: isize) -> bool {
        match self.at_index(idx) {
            Ok(val) => val.truthy(),
            Err(_) => false,
        }
    }

    /// Attempts to convert the value at the given index to a number.
    pub fn to_number(&self, idx: isize) -> Result<f64> {
        let i = self.convert_idx(idx)?;
        let val = &self.stack[i];
        val.as_num()
            .ok_or_else(|| self.type_error(super::TypeError::Arithmetic(val.typ_simple())))
    }

    /// Converts the value at the given index to a string.
    pub fn to_string(&self, idx: isize) -> Result<String> {
        let i = self.convert_idx(idx)?;
        Ok(self.stack[i].to_string_with_heap(&self.heap))
    }

    /// Returns the type of the value in the given acceptable index.
    /// Returns Nil type if the index is invalid.
    /// Note: For objects (tables/functions), this returns the correct type
    /// by looking up the object in the heap.
    pub fn typ(&self, idx: isize) -> super::LuaType {
        match self.at_index(idx) {
            Ok(val) => val.typ(&self.heap),
            Err(_) => super::LuaType::Nil,
        }
    }

    /// Returns true if the values at the two indices are primitively equal.
    /// Does not invoke __eq metamethod.
    pub fn raw_equal(&self, idx1: isize, idx2: isize) -> bool {
        match (self.at_index(idx1), self.at_index(idx2)) {
            (Ok(v1), Ok(v2)) => v1 == v2,
            _ => false,
        }
    }

    /// Get the value at the given index. Returns error if out of bounds.
    pub(super) fn at_index(&self, idx: isize) -> Result<Val> {
        let i = self.convert_idx(idx)?;
        Ok(self.stack[i].clone())
    }

    /// Given a relative index, convert it to an absolute index to the stack.
    pub(super) fn convert_idx(&self, fake_idx: isize) -> Result<usize> {
        let stack_top = self.stack.len() as isize;
        let stack_bottom = self.stack_bottom as isize;
        let stack_len = stack_top - stack_bottom;
        if fake_idx > 0 && fake_idx <= stack_len {
            Ok((fake_idx - 1 + stack_bottom) as usize)
        } else if fake_idx < 0 && fake_idx >= -stack_len {
            Ok((stack_top + fake_idx) as usize)
        } else {
            Err(Error::without_location(ErrorKind::InvalidStackIndex {
                index: fake_idx,
            }))
        }
    }

    /// Balances a stack after an operation that returns an indefinite number of
    /// results.
    pub(super) fn balance_stack(&mut self, expected: usize, received: usize) {
        match expected.cmp(&received) {
            Ordering::Greater => {
                for _ in received..expected {
                    self.push_nil();
                }
            }
            Ordering::Less => {
                for _ in expected..received {
                    self.pop_val();
                }
            }
            Ordering::Equal => (),
        }
    }

    /// Check that we have room for `n` more values on the stack.
    /// Returns an error if adding `n` values would exceed the stack limit.
    pub(super) fn check_stack_space(&self, n: usize) -> Result<()> {
        let new_size = self.stack.len().saturating_add(n);
        if new_size > MAX_STACK_SIZE {
            return Err(Error::without_location(ErrorKind::StackOverflow {
                size: new_size,
            }));
        }
        Ok(())
    }
}
