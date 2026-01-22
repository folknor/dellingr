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

/// Maximum depth for metamethod chains (__index/__newindex).
/// Prevents stack overflow from circular metamethod references.
/// Lua uses 2000, but 200 is plenty for normal use cases.
pub(super) const MAX_METAMETHOD_DEPTH: u32 = 200;

impl State {
    /// Internal helper for table access with __index support.
    pub(super) fn get_table_with_key(&mut self, idx: usize, key: Val) -> Result<()> {
        let table_val = &mut self.stack[idx].clone();
        match table_val.as_table() {
            Some(t) => {
                let val = t.get(&key);
                if matches!(val, Val::Nil) {
                    // Check for __index metamethod
                    if let Some(mt_ptr) = t.get_metatable() {
                        let index_key = self.alloc_string("__index".to_string());
                        if let Some(mt) = Val::Obj(mt_ptr).as_table() {
                            let index_handler = mt.get(&index_key);
                            if !matches!(index_handler, Val::Nil) {
                                return self.handle_index_metamethod(index_handler, idx, key);
                            }
                        }
                    }
                }
                self.stack.push(val);
                Ok(())
            }
            None => Err(self.type_error(super::TypeError::TableIndex(self.stack[idx].typ()))),
        }
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
                if ptr.as_table_ref().is_some() {
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
                } else if ptr.as_lua_function().is_some() {
                    // __index is a function: call it with (table, key)
                    let table_val = self.stack[table_idx].clone();
                    self.stack.push(Val::Obj(ptr));
                    self.stack.push(table_val);
                    self.stack.push(key);
                    self.call(2, 1)?;
                    Ok(())
                } else {
                    self.push_nil();
                    Ok(())
                }
            }
            Val::RustFn(f) => {
                // __index is a Rust function: call it with (table, key)
                let table_val = self.stack[table_idx].clone();
                self.stack.push(Val::RustFn(f));
                self.stack.push(table_val);
                self.stack.push(key);
                self.call(2, 1)?;
                Ok(())
            }
            _ => {
                self.push_nil();
                Ok(())
            }
        }
    }

    /// Internal helper for table assignment with __newindex support.
    /// The table should be at stack[idx]. Does not pop anything from the stack.
    pub(super) fn set_table_with_key(&mut self, idx: usize, key: Val, val: Val) -> Result<()> {
        let table_val = &mut self.stack[idx].clone();
        match table_val.as_table() {
            Some(t) => {
                // Check if key already exists - __newindex only triggers for new keys
                let existing = t.get(&key);
                if matches!(existing, Val::Nil) {
                    // Check for __newindex metamethod
                    if let Some(mt_ptr) = t.get_metatable() {
                        let newindex_key = self.alloc_string("__newindex".to_string());
                        if let Some(mt) = Val::Obj(mt_ptr).as_table() {
                            let newindex_handler = mt.get(&newindex_key);
                            if !matches!(newindex_handler, Val::Nil) {
                                return self.handle_newindex_metamethod(
                                    newindex_handler,
                                    idx,
                                    key,
                                    val,
                                );
                            }
                        }
                    }
                }
                // No __newindex or key exists: do normal assignment
                // Need to get the actual table from the stack since we cloned earlier
                if let Some(t) = self.stack[idx].as_table() {
                    t.insert(key, val)?;
                }
                Ok(())
            }
            None => Err(self.type_error(super::TypeError::TableIndex(self.stack[idx].typ()))),
        }
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
                if ptr.as_table_ref().is_some() {
                    // __newindex is a table: set the value in that table instead
                    self.stack.push(Val::Obj(ptr));
                    let new_idx = self.stack.len() - 1;
                    self.set_table_with_key(new_idx, key, val)?;
                    self.pop(1); // Remove the __newindex table
                    Ok(())
                } else if ptr.as_lua_function().is_some() {
                    // __newindex is a function: call it with (table, key, value)
                    let table_val = self.stack[table_idx].clone();
                    self.stack.push(Val::Obj(ptr));
                    self.stack.push(table_val);
                    self.stack.push(key);
                    self.stack.push(val);
                    self.call(3, 0)?;
                    Ok(())
                } else {
                    // Not a table or function, just do normal assignment
                    if let Some(t) = self.stack[table_idx].as_table() {
                        t.insert(key, val)?;
                    }
                    Ok(())
                }
            }
            Val::RustFn(f) => {
                // __newindex is a Rust function: call it with (table, key, value)
                let table_val = self.stack[table_idx].clone();
                self.stack.push(Val::RustFn(f));
                self.stack.push(table_val);
                self.stack.push(key);
                self.stack.push(val);
                self.call(3, 0)?;
                Ok(())
            }
            _ => {
                // Not callable, just do normal assignment
                if let Some(t) = self.stack[table_idx].as_table() {
                    t.insert(key, val)?;
                }
                Ok(())
            }
        }
    }
}
