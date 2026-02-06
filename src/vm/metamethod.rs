//! Metamethod handling for __index and __newindex.
//!
//! This module handles Lua metamethods that intercept table access and assignment.
//! Metamethods allow tables to define custom behavior when:
//! - A key is accessed but doesn't exist in the table (__index)
//! - A new key is being assigned to the table (__newindex)

use super::lua_val::Val;
use super::Result;
use super::State;
use crate::error::ErrorKind;
use crate::instr::{ArgCount, RetCount};

/// Maximum depth for metamethod chains (__index/__newindex).
/// Prevents stack overflow from circular metamethod references.
/// Lua uses 2000, but 200 is plenty for normal use cases.
pub(super) const MAX_METAMETHOD_DEPTH: u32 = 200;

impl State {
    /// Internal helper for table access with __index support.
    pub(super) fn get_table_with_key(&mut self, idx: usize, key: Val) -> Result<()> {
        let table_val = self.stack[idx];
        let obj_ptr = table_val.as_object_ptr();

        // Get the value and metatable pointer in one heap access
        let (val, mt_ptr) = match obj_ptr.and_then(|ptr| self.heap.as_table_ref(ptr)) {
            Some(t) => {
                let val = t.get(&key);
                let mt_ptr = t.get_metatable();
                (val, mt_ptr)
            }
            None => {
                return Err(self.type_error(super::TypeError::TableIndex(
                    self.stack[idx].typ_simple(),
                )));
            }
        };

        if matches!(val, Val::Nil) {
            // Check for __index metamethod
            if let Some(mt_ptr) = mt_ptr {
                // Protect key from GC during string allocation
                self.stack.push(key);
                let index_key = self.alloc_string("__index".to_string());
                let key = self.pop_val();

                let index_handler = self
                    .heap
                    .as_table_ref(mt_ptr)
                    .map(|mt| mt.get(&index_key))
                    .unwrap_or(Val::Nil);

                if !matches!(index_handler, Val::Nil) {
                    return self.handle_index_metamethod(index_handler, idx, key);
                }
            }
        }

        self.stack.push(val);
        Ok(())
    }

    /// Handle the __index metamethod which can be a table or a function.
    fn handle_index_metamethod(&mut self, handler: Val, table_idx: usize, key: Val) -> Result<()> {
        // Check metamethod depth to prevent infinite recursion
        if self.metamethod_depth >= MAX_METAMETHOD_DEPTH {
            return Err(self.error(ErrorKind::MetamethodDepthExceeded {
                depth: self.metamethod_depth,
            }));
        }
        self.metamethod_depth += 1;
        let result = self.handle_index_metamethod_inner(handler, table_idx, key);
        self.metamethod_depth -= 1;
        result
    }

    fn handle_index_metamethod_inner(
        &mut self,
        handler: Val,
        table_idx: usize,
        key: Val,
    ) -> Result<()> {
        match handler {
            Val::Obj(ptr) => {
                // Check if it's a table or function
                let is_table = self.heap.as_table_ref(ptr).is_some();
                let is_function = self.heap.as_lua_function(ptr).is_some();

                if is_table {
                    // __index is a table: look up key in that table
                    self.stack.push(Val::Obj(ptr));
                    let new_idx = self.stack.len() - 1;
                    self.get_table_with_key(new_idx, key)?;
                    // Stack: [... __index_table, result]
                    // Remove the __index table, keep the result
                    let val = self.pop_val();
                    self.pop(1);
                    self.stack.push(val);
                    Ok(())
                } else if is_function {
                    // __index is a function: call it with (table, key)
                    let table_val = self.stack[table_idx];
                    self.stack.push(Val::Obj(ptr));
                    self.stack.push(table_val);
                    self.stack.push(key);
                    self.call(ArgCount::Fixed(2), RetCount::Fixed(1))?;
                    Ok(())
                } else {
                    Err(self.type_error(super::TypeError::TableIndex(Val::Obj(ptr).typ_simple())))
                }
            }
            Val::RustFn(f) => {
                // __index is a Rust function: call it with (table, key)
                let table_val = self.stack[table_idx];
                self.stack.push(Val::RustFn(f));
                self.stack.push(table_val);
                self.stack.push(key);
                self.call(ArgCount::Fixed(2), RetCount::Fixed(1))?;
                Ok(())
            }
            _ => {
                Err(self.type_error(super::TypeError::TableIndex(handler.typ_simple())))
            }
        }
    }

    /// Internal helper for table assignment with __newindex support.
    /// The table should be at stack[idx]. Does not pop anything from the stack.
    pub(super) fn set_table_with_key(&mut self, idx: usize, key: Val, val: Val) -> Result<()> {
        let table_val = self.stack[idx];
        let obj_ptr = table_val.as_object_ptr();

        // Get existing value and metatable pointer in one heap access
        let (existing, mt_ptr) = match obj_ptr.and_then(|ptr| self.heap.as_table_ref(ptr)) {
            Some(t) => {
                let existing = t.get(&key);
                let mt_ptr = t.get_metatable();
                (existing, mt_ptr)
            }
            None => {
                return Err(self.type_error(super::TypeError::TableIndex(
                    self.stack[idx].typ_simple(),
                )));
            }
        };

        if matches!(existing, Val::Nil) {
            // Check for __newindex metamethod
            if let Some(mt_ptr) = mt_ptr {
                // Protect key and val from GC during string allocation by pushing clones
                // (clones share the same underlying heap objects, so marking them marks originals)
                self.stack.push(key);
                self.stack.push(val);
                let newindex_key = self.alloc_string("__newindex".to_string());
                self.pop(2); // Discard protections

                let newindex_handler = self
                    .heap
                    .as_table_ref(mt_ptr)
                    .map(|mt| mt.get(&newindex_key))
                    .unwrap_or(Val::Nil);

                if !matches!(newindex_handler, Val::Nil) {
                    return self.handle_newindex_metamethod(newindex_handler, idx, key, val);
                }
            }
        }

        // No __newindex or key exists: do normal assignment
        if let Some(ptr) = obj_ptr
            && let Some(t) = self.heap.as_table(ptr) {
                t.insert(key, val)?;
            }
        Ok(())
    }

    /// Handle the __newindex metamethod which can be a table or a function.
    fn handle_newindex_metamethod(
        &mut self,
        handler: Val,
        table_idx: usize,
        key: Val,
        val: Val,
    ) -> Result<()> {
        // Check metamethod depth to prevent infinite recursion
        if self.metamethod_depth >= MAX_METAMETHOD_DEPTH {
            return Err(self.error(ErrorKind::MetamethodDepthExceeded {
                depth: self.metamethod_depth,
            }));
        }
        self.metamethod_depth += 1;
        let result = self.handle_newindex_metamethod_inner(handler, table_idx, key, val);
        self.metamethod_depth -= 1;
        result
    }

    fn handle_newindex_metamethod_inner(
        &mut self,
        handler: Val,
        table_idx: usize,
        key: Val,
        val: Val,
    ) -> Result<()> {
        match handler {
            Val::Obj(ptr) => {
                // Check if it's a table or function
                let is_table = self.heap.as_table_ref(ptr).is_some();
                let is_function = self.heap.as_lua_function(ptr).is_some();

                if is_table {
                    // __newindex is a table: set the value in that table instead
                    self.stack.push(Val::Obj(ptr));
                    let new_idx = self.stack.len() - 1;
                    self.set_table_with_key(new_idx, key, val)?;
                    self.pop(1); // Remove the __newindex table
                    Ok(())
                } else if is_function {
                    // __newindex is a function: call it with (table, key, value)
                    let table_val = self.stack[table_idx];
                    self.stack.push(Val::Obj(ptr));
                    self.stack.push(table_val);
                    self.stack.push(key);
                    self.stack.push(val);
                    self.call(ArgCount::Fixed(3), RetCount::Fixed(0))?;
                    Ok(())
                } else {
                    Err(self.type_error(super::TypeError::TableIndex(Val::Obj(ptr).typ_simple())))
                }
            }
            Val::RustFn(f) => {
                // __newindex is a Rust function: call it with (table, key, value)
                let table_val = self.stack[table_idx];
                self.stack.push(Val::RustFn(f));
                self.stack.push(table_val);
                self.stack.push(key);
                self.stack.push(val);
                self.call(ArgCount::Fixed(3), RetCount::Fixed(0))?;
                Ok(())
            }
            _ => {
                Err(self.type_error(super::TypeError::TableIndex(handler.typ_simple())))
            }
        }
    }
}
