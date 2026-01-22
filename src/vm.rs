//! This module provides the `State` struct, which handles the primary
//! components of the VM.

mod frame;
mod lua_val;
mod object;
mod table;

pub use lua_val::LuaType;
pub use lua_val::RustFunc;

use std::cmp::Ordering;
use std::collections::HashMap;

use super::compiler;
use super::error::Error;
use super::error::ErrorKind;
use super::error::TypeError;
use super::Chunk;
use super::Instr;
use super::Result;

use frame::Frame;
use lua_val::Val;
use object::{Closure, GcHeap, Markable, Upvalue, UpvalueRef};
use table::Table;

/// The main interface into the Lua VM.
pub struct State {
    /// The global environment. This may be changed to an actual Table in the future.
    globals: HashMap<String, Val>,
    /// The main stack which stores values.
    stack: Vec<Val>,
    /// The bottom index of the current frame in the stack.
    stack_bottom: usize,
    /// The heap which holds any garbage-collected Objects.
    heap: GcHeap,
    /// The string literals (as `Val`s) of every active `Frame`.
    string_literals: Vec<Val>,
    /// Open upvalues currently pointing to stack slots.
    /// Each entry is (stack_index, upvalue_ref). Kept sorted by stack_index descending
    /// so we can efficiently close them when a function returns.
    open_upvalues: Vec<(usize, UpvalueRef)>,
    /// Stack position marked before a vararg function call.
    /// Used to calculate arg count when `...` is passed as an argument.
    vararg_call_base: Option<usize>,
    /// Cost budget remaining. When this reaches 0 or below, operations with cost > 0
    /// will fail. The action that pushes you over budget completes before stopping.
    /// Uses i64 to allow going negative (the final action that exceeds budget completes).
    cost_remaining: i64,
    /// The original cost budget (for error reporting).
    cost_budget: i64,
    /// Total cost consumed (for reporting).
    cost_used: u64,
}

// Important note on how the stack is tracked:
// A State uses a single stack for all local variables, temporary values,
// function arguments, and function return values. Both Lua frames and Rust
// frames use this stack. `self.stack_bottom` refers to the first value in the
// stack which belongs to the current frame. Note that Rust functions access
// the stack using 1-based indexing, but Lua code uses 0-based indexing.

impl Markable for State {
    fn mark_reachable(&self) {
        self.stack.mark_reachable();
        self.globals.mark_reachable();
        self.string_literals.mark_reachable();
        // Mark closed upvalues (open ones point to stack which is already marked)
        for (_, uv_ref) in &self.open_upvalues {
            if let Upvalue::Closed(val) = &*uv_ref.borrow() {
                val.mark_reachable();
            }
        }
    }
}

impl State {
    const GC_INITIAL_THRESHOLD: usize = 20;

    /// Creates a new, independent state.
    pub fn new() -> Self {
        let mut me = Self::empty();
        me.open_libs();
        me
    }

    /// Creates a new state without opening any of the standard libs.
    /// The global namespace of this state is entirely empty. This corresponds
    /// to the `lua_newstate' function in the C API.
    pub fn empty() -> Self {
        Self {
            globals: HashMap::new(),
            stack: Vec::new(),
            stack_bottom: 0,
            heap: GcHeap::with_threshold(Self::GC_INITIAL_THRESHOLD),
            string_literals: Vec::new(),
            open_upvalues: Vec::new(),
            vararg_call_base: None,
            cost_remaining: i64::MAX,
            cost_budget: i64::MAX,
            cost_used: 0,
        }
    }

    /// Sets the cost budget for this VM.
    /// When the budget is exhausted, operations with cost > 0 will fail.
    /// The action that pushes you over budget always completes before stopping.
    pub fn set_cost_budget(&mut self, budget: i64) {
        self.cost_budget = budget;
        self.cost_remaining = budget;
        self.cost_used = 0;
    }

    /// Returns the total cost consumed since the last budget reset.
    pub fn cost_used(&self) -> u64 {
        self.cost_used
    }

    /// Returns the cost remaining in the budget.
    /// Can be negative if the last action pushed over budget.
    pub fn cost_remaining(&self) -> i64 {
        self.cost_remaining
    }

    /// Consume cost from the budget. Returns an error if budget is exhausted
    /// and cost > 0. The action that pushes you over budget completes before
    /// stopping (checked at the START of each operation).
    pub(crate) fn consume_cost(&mut self, cost: u64) -> Result<()> {
        if cost > 0 && self.cost_remaining <= 0 {
            return Err(self.error(ErrorKind::BudgetExceeded {
                used: self.cost_used,
                budget: self.cost_budget,
            }));
        }
        self.cost_remaining -= cost as i64;
        self.cost_used += cost;
        Ok(())
    }

    /// Find an existing open upvalue for the given stack index, or create a new one.
    fn find_or_create_upvalue(&mut self, stack_idx: usize) -> UpvalueRef {
        // Check if we already have an open upvalue for this stack slot
        for (idx, uv_ref) in &self.open_upvalues {
            if *idx == stack_idx {
                return uv_ref.clone();
            }
        }
        // Create a new open upvalue
        let uv_ref = std::rc::Rc::new(std::cell::RefCell::new(Upvalue::Open(stack_idx)));
        // Insert in order (sorted by stack index descending for efficient closing)
        let pos = self
            .open_upvalues
            .iter()
            .position(|(idx, _)| *idx < stack_idx)
            .unwrap_or(self.open_upvalues.len());
        self.open_upvalues.insert(pos, (stack_idx, uv_ref.clone()));
        uv_ref
    }

    /// Close all open upvalues at or above the given stack level.
    /// This is called when a function returns to capture the values from the stack
    /// before they are popped.
    pub(crate) fn close_upvalues(&mut self, level: usize) {
        while let Some(&(idx, _)) = self.open_upvalues.first() {
            if idx < level {
                break;
            }
            let (_, uv_ref) = self.open_upvalues.remove(0);
            let val = self.stack[idx].clone();
            *uv_ref.borrow_mut() = Upvalue::Closed(val);
        }
    }

    /// Calls a function.
    ///
    /// To call a function you must use the following protocol: first, the
    /// function to be called is pushed onto the stack; then, the arguments to
    /// the function are pushed in direct order; that is, the first argument is
    /// pushed first. Finally you call `lua_call`; `num_args` is the number of
    /// arguments that you pushed onto the stack. All arguments and the function
    /// value are popped from the stack when the function is called. The
    /// function results are pushed onto the stack when the function returns.
    /// The number of results is adjusted to `num_ret_expected`. The function
    /// results are pushed onto the stack in direct order (the first result is
    /// pushed first), so that after the call the last result is on the top of
    /// the stack.
    pub fn call(&mut self, num_args: u8, num_ret_expected: u8) -> Result<()> {
        // Handle vararg call: num_args == 255 means calculate from vararg_call_base
        let (idx, actual_num_args) = if num_args == u8::MAX {
            let base = self.vararg_call_base.take()
                .expect("Call with 255 args but no vararg_call_base set");
            let actual = (self.stack.len() - base - 1) as u8;
            (base, actual)
        } else {
            (self.stack.len() - num_args as usize - 1, num_args)
        };
        let func_val = self.stack.remove(idx);
        let num_ret_actual = if let Val::RustFn(f) = func_val {
            let old_stack_bottom = self.stack_bottom;
            self.stack_bottom = idx;
            let num_ret_reported = f(self)?;
            let num_ret_actual = self.get_top() as u8;
            match num_ret_reported.cmp(&num_ret_actual) {
                Ordering::Greater => {
                    for _ in num_ret_actual..num_ret_reported {
                        self.push_nil();
                    }
                }
                Ordering::Less => {
                    let slc = &mut self.stack[self.stack_bottom..];
                    slc.rotate_right(num_ret_reported as usize);
                    let new_len =
                        self.stack.len() - num_ret_actual as usize + num_ret_reported as usize;
                    self.stack.truncate(new_len);
                }
                Ordering::Equal => (),
            }
            self.stack_bottom = old_stack_bottom;
            num_ret_reported
        } else if let Some(closure) = func_val.as_lua_function() {
            self.eval_closure(closure, actual_num_args)?
        } else if let Some(t) = func_val.as_table_ref() {
            // Check for __call metamethod
            if let Some(mt_ptr) = t.get_metatable() {
                let call_key = self.alloc_string("__call".to_string());
                if let Some(mt) = Val::Obj(mt_ptr).as_table() {
                    let call_handler = mt.get(&call_key);
                    if !matches!(call_handler, Val::Nil) {
                        // Insert the table as first argument and call the handler
                        // Stack is currently: [arg1, arg2, ..., argN]
                        // We need: [handler, table, arg1, arg2, ..., argN]
                        self.stack.insert(idx, func_val.clone());
                        self.stack.insert(idx, call_handler);
                        // Now call with actual_num_args + 1 (table is first arg)
                        return self.call(actual_num_args + 1, num_ret_expected);
                    }
                }
            }
            return Err(self.type_error(TypeError::FunctionCall(func_val.typ())));
        } else {
            return Err(self.type_error(TypeError::FunctionCall(func_val.typ())));
        };
        // 255 means "return all" - don't adjust the stack
        if num_ret_expected != u8::MAX {
            self.balance_stack(num_ret_expected as usize, num_ret_actual as usize);
        }
        Ok(())
    }

    /// Pops `n` values from the stack, concatenates them, and pushes the
    /// result. If `n` is 1, the result is the single value on the stack (that
    /// is, the function does nothing); if `n` is 0, the result is the empty
    /// string.
    pub fn concat(&mut self, n: usize) -> Result<()> {
        assert!(n == 2, "Can only concatenate two at a time for now");
        self.concat_helper(n)
    }

    /// Copies the element at `from` into the valid index `to`, replacing the
    /// value at that position. Equivalent to Lua's `lua_copy`.
    pub fn copy_val(&mut self, from: isize, to: isize) {
        let val = self.at_index(from);
        let to = self.convert_idx(to);
        self.stack[to] = val;
    }

    /// Pushes onto the stack the value of the global `name`.
    pub fn get_global(&mut self, name: &str) {
        let val = self.globals.get(name).cloned().unwrap_or_default();
        self.stack.push(val);
    }

    /// Pushes onto the stack the value `t[k]`, where `t` is the value at the given
    /// valid index and `k` is the value at the top of the stack.
    ///
    /// This function pops the key from the stack (putting the resulting value in
    /// its place). As in Lua, this function may trigger a metamethod for the
    /// "index" event.
    pub fn get_table(&mut self, i: isize) -> Result<()> {
        let idx = self.convert_idx(i);
        assert!(idx != self.stack.len() - 1);
        let key = self.pop_val();
        self.get_table_with_key(idx, key)
    }

    /// Internal helper for table access with __index support.
    fn get_table_with_key(&mut self, idx: usize, key: Val) -> Result<()> {
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
            None => Err(self.type_error(TypeError::TableIndex(self.stack[idx].typ()))),
        }
    }

    /// Handle the __index metamethod which can be a table or a function.
    fn handle_index_metamethod(&mut self, handler: Val, table_idx: usize, key: Val) -> Result<()> {
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
    fn set_table_with_key(&mut self, idx: usize, key: Val, val: Val) -> Result<()> {
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
            None => Err(self.type_error(TypeError::TableIndex(self.stack[idx].typ()))),
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

    /// Returns the next key-value pair from a table, for use with `pairs`.
    /// Takes the table index and pops the key from the stack.
    /// Pushes the next key and value onto the stack (or just nil if done).
    pub fn table_next(&mut self, table_idx: isize) -> Result<bool> {
        let idx = self.convert_idx(table_idx);
        let key = self.pop_val();
        let table_val = &self.stack[idx];
        match table_val.as_table_ref() {
            Some(t) => {
                let (next_key, next_val) = t.next(&key);
                if matches!(next_key, Val::Nil) {
                    self.stack.push(Val::Nil);
                    Ok(false)
                } else {
                    self.stack.push(next_key);
                    self.stack.push(next_val);
                    Ok(true)
                }
            }
            None => Err(self.type_error(TypeError::TableIndex(table_val.typ()))),
        }
    }

    /// Gets `t[k]` without invoking metamethods.
    /// `t` is at the given index, `k` is at the top of the stack.
    /// Pops the key and pushes the result.
    pub fn get_table_raw(&mut self, i: isize) -> Result<()> {
        let idx = self.convert_idx(i);
        let key = self.pop_val();
        let table = &self.stack[idx];
        let typ = table.typ();
        match table.as_table_ref() {
            Some(t) => {
                let val = t.get(&key);
                self.stack.push(val);
                Ok(())
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    /// Does the equivalent of `t[k] = v`, where `t` is the value at the given
    /// valid index, `k` is the value at the top of the stack minus 1, and `v`
    /// is the value at the top of the stack.
    ///
    /// This function pops both the key and the value from the stack.
    pub fn set_table_raw(&mut self, i: isize) -> Result<()> {
        let idx = self.convert_idx(i);
        let key = self.pop_val();
        let val = self.pop_val();
        let table = &mut self.stack[idx];
        let typ = table.typ();
        match table.as_table() {
            Some(t) => {
                t.insert(key, val)?;
                Ok(())
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    /// Returns the index of the top element in the stack. Because indices start
    /// at 1, this result is equal to the number of elements in the stack (and
    /// so 0 means an empty stack).
    pub fn get_top(&self) -> usize {
        self.stack.len() - self.stack_bottom
    }

    /// Moves the top element into the given valid index, shifting up the
    /// elements above this index to open space.
    pub fn insert(&mut self, index: isize) {
        let idx = self.convert_idx(index);
        let slice = &mut self.stack[idx..];
        slice.rotate_right(1);
    }

    /// Loads a string as a Lua chunk. This function uses `load` to load the
    /// chunk in the string `s`.
    pub fn load_string(&mut self, s: impl AsRef<str>) -> Result<()> {
        let c = compiler::parse_str(s)?;
        self.push_chunk(c);
        Ok(())
    }

    /// Creates a new empty table and pushes it onto the stack.
    pub fn new_table(&mut self) {
        let val = self.alloc_table();
        self.stack.push(val);
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

    /// Pushes a boolean onto the stack.
    pub fn push_boolean(&mut self, b: bool) {
        self.stack.push(Val::Bool(b));
    }

    /// Pushes a `nil` value onto the stack.
    pub fn push_nil(&mut self) {
        self.stack.push(Val::Nil);
    }

    /// Pushes a number with value `n` onto the stack.
    pub fn push_number(&mut self, n: f64) {
        self.stack.push(Val::Num(n));
    }

    /// Pushes a Rust function onto the stack.
    pub fn push_rust_fn(&mut self, f: RustFunc) {
        self.stack.push(Val::RustFn(f));
    }

    /// Pushes the given string onto the stack.
    pub fn push_string(&mut self, s: String) {
        let val = self.alloc_string(s);
        self.stack.push(val);
    }

    /// Pushes a copy of the element at the given index onto the stack.
    pub fn push_value(&mut self, i: isize) {
        // TODO: figure out what lua does when index is invalid
        let val = self.at_index(i);
        self.stack.push(val);
    }

    pub fn remove(&mut self, i: isize) {
        let idx = self.convert_idx(i);
        self.stack.remove(idx);
    }

    /// Pops a value from the stack, then replaces the value at the given index
    /// with that value.
    pub fn replace(&mut self, i: isize) {
        let idx = self.convert_idx(i);
        let val = self.stack.pop().unwrap();
        self.stack[idx] = val;
    }

    /// Pops a value from the stack and sets it as the new value of global
    /// `name`.
    pub fn set_global(&mut self, name: &str) {
        let val = self.pop_val();
        self.globals.insert(name.to_string(), val);
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

    /// Returns whether the value at the given index is not `false` or `nil`.
    pub fn to_boolean(&self, idx: isize) -> bool {
        let val = self.at_index(idx);
        val.truthy()
    }

    /// Attempts to convert the value at the given index to a number.
    pub fn to_number(&self, idx: isize) -> Result<f64> {
        let i = self.convert_idx(idx);
        let val = &self.stack[i];
        val.as_num()
            .ok_or_else(|| self.type_error(TypeError::Arithmetic(val.typ())))
    }

    /// Converts the value at the given index to a string.
    pub fn to_string(&self, idx: isize) -> String {
        let i = self.convert_idx(idx);
        self.stack[i].to_string()
    }

    /// Converts the value at the given index to a string, checking for __tostring metamethod.
    /// If the value is a table with a __tostring metamethod, calls it and returns the result.
    pub fn to_string_with_meta(&mut self, idx: isize) -> Result<String> {
        let i = self.convert_idx(idx);
        let val = self.stack[i].clone();

        // Check if it's a table with __tostring
        if let Some(t) = val.as_table_ref() {
            if let Some(mt_ptr) = t.get_metatable() {
                let tostring_key = self.alloc_string("__tostring".to_string());
                if let Some(mt) = Val::Obj(mt_ptr).as_table() {
                    let tostring_handler = mt.get(&tostring_key);
                    if !matches!(tostring_handler, Val::Nil) {
                        // Call the __tostring metamethod
                        self.stack.push(tostring_handler);
                        self.stack.push(val);
                        self.call(1, 1)?;
                        let result = self.pop_val();
                        return Ok(result.to_string());
                    }
                }
            }
        }

        // No __tostring, use default
        Ok(val.to_string())
    }

    /// Returns the type of the value in the given acceptable index.
    pub fn typ(&self, idx: isize) -> LuaType {
        self.at_index(idx).typ()
    }

    /// Returns the array length of the table at the given index.
    pub fn table_len(&self, idx: isize) -> usize {
        let i = self.convert_idx(idx);
        let table_val = &self.stack[i];
        match table_val.as_table_ref() {
            Some(t) => t.array_len(),
            None => 0,
        }
    }

    /// Gets the metatable of the value at the given index.
    /// For tables, returns the table's metatable.
    /// For other types, returns nil (we don't support type metatables yet).
    pub fn get_metatable_of(&mut self, idx: isize) {
        let i = self.convert_idx(idx);
        let val = &self.stack[i];
        match val.as_table_ref() {
            Some(t) => {
                if let Some(mt) = t.get_metatable() {
                    self.stack.push(Val::Obj(mt));
                } else {
                    self.push_nil();
                }
            }
            None => self.push_nil(),
        }
    }

    /// Sets the metatable of the table at the given index.
    /// The metatable should be at the top of the stack (or nil to remove).
    /// Pops the metatable from the stack.
    pub fn set_metatable_of(&mut self, table_idx: isize) -> Result<()> {
        let mt_val = self.pop_val();
        let idx = self.convert_idx(table_idx);
        let typ = self.stack[idx].typ();

        let mt = match &mt_val {
            Val::Nil => None,
            Val::Obj(ptr) => {
                if ptr.as_table_ref().is_some() {
                    Some(*ptr)
                } else {
                    return Err(self.type_error(TypeError::TableIndex(mt_val.typ())));
                }
            }
            _ => return Err(self.type_error(TypeError::TableIndex(mt_val.typ()))),
        };

        match self.stack[idx].as_table() {
            Some(t) => {
                t.set_metatable(mt);
                Ok(())
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    /// Inserts a value into a table at a position, shifting elements.
    /// Stack: [t, pos, value] -> []
    /// The pos and value are popped from the stack.
    pub fn table_insert_at(&mut self, table_idx: isize) -> Result<()> {
        let value = self.pop_val();
        let pos = self.pop_val().as_num().unwrap_or(1.0) as usize;
        let idx = self.convert_idx(table_idx);
        let typ = self.stack[idx].typ();
        match self.stack[idx].as_table() {
            Some(t) => {
                t.array_insert(pos, value);
                Ok(())
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    /// Removes a value from a table at a position, shifting elements.
    /// Pushes the removed value onto the stack.
    pub fn table_remove_at(&mut self, table_idx: isize, pos: usize) -> Result<()> {
        let idx = self.convert_idx(table_idx);
        let typ = self.stack[idx].typ();
        let removed = match self.stack[idx].as_table() {
            Some(t) => t.array_remove(pos),
            None => return Err(self.type_error(TypeError::TableIndex(typ))),
        };
        self.stack.push(removed);
        Ok(())
    }

    /// Sorts the array portion of a table in place.
    /// If has_comp is true, uses the function at stack index 2 as comparator.
    pub fn table_sort(&mut self, table_idx: isize, has_comp: bool) -> Result<()> {
        let idx = self.convert_idx(table_idx);

        // Get the array values
        let mut arr = {
            let table_val = &self.stack[idx];
            match table_val.as_table_ref() {
                Some(t) => t.get_array(),
                None => return Err(self.type_error(TypeError::TableIndex(table_val.typ()))),
            }
        };

        if arr.is_empty() {
            return Ok(());
        }

        if has_comp {
            // Use the comparator function at stack index 2
            // We need to do a stable sort with the comparator
            let comp_idx = self.convert_idx(2);

            // Bubble sort to keep it simple (not efficient but works)
            let n = arr.len();
            for i in 0..n {
                for j in 0..n - 1 - i {
                    // Call comp(arr[j], arr[j+1])
                    let a = arr[j].clone();
                    let b = arr[j + 1].clone();

                    // Push comp function
                    self.stack.push(self.stack[comp_idx].clone());
                    // Push args
                    self.stack.push(a.clone());
                    self.stack.push(b.clone());
                    // Call
                    self.call(2, 1)?;
                    // Get result
                    let result = self.pop_val();

                    // If comp(a, b) is false, swap
                    if !result.truthy() {
                        arr.swap(j, j + 1);
                    }
                }
            }
        } else {
            // Default: sort by < operator (numbers first, then strings)
            arr.sort_by(|a, b| {
                match (a.as_num(), b.as_num()) {
                    (Some(na), Some(nb)) => na.partial_cmp(&nb).unwrap_or(std::cmp::Ordering::Equal),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => {
                        // Try string comparison
                        match (a.as_string(), b.as_string()) {
                            (Some(sa), Some(sb)) => sa.cmp(sb),
                            _ => std::cmp::Ordering::Equal,
                        }
                    }
                }
            });
        }

        // Put sorted array back
        let typ = self.stack[idx].typ();
        match self.stack[idx].as_table() {
            Some(t) => {
                t.set_array(arr);
                Ok(())
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    fn alloc_string(&mut self, s: String) -> Val {
        let Self {
            stack,
            globals,
            string_literals,
            ..
        } = self;
        let ptr = self.heap.new_string(s, || {
            stack.mark_reachable();
            globals.mark_reachable();
            string_literals.mark_reachable();
        });
        Val::Str(ptr)
    }

    fn alloc_table(&mut self) -> Val {
        let Self {
            stack,
            globals,
            string_literals,
            ..
        } = self;
        let obj = self.heap.new_table(|| {
            stack.mark_reachable();
            globals.mark_reachable();
            string_literals.mark_reachable();
        });
        Val::Obj(obj)
    }

    /// Get the value at the given index. Panics if out of bounds.
    fn at_index(&self, idx: isize) -> Val {
        let i = self.convert_idx(idx);
        self.stack[i].clone()
    }

    /// Balances a stack after an operation that returns an indefinite number of
    /// results.
    fn balance_stack(&mut self, expected: usize, received: usize) {
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

    fn concat_helper(&mut self, n: usize) -> Result<()> {
        let mut buffer = String::new();
        let idx = self.stack.len() - n;
        let drain = self.stack.drain(idx..);
        let mut abort = None;
        for val in drain {
            if let Some(s) = val.as_string() {
                buffer.push_str(s);
            } else {
                abort = Some(TypeError::Concat(val.typ()));
                break;
            }
        }
        if let Some(e) = abort {
            return Err(self.type_error(e));
        }

        let val = self.alloc_string(buffer);
        self.stack.push(val);
        Ok(())
    }

    /// Given a relative index, convert it to an absolute index to the stack.
    fn convert_idx(&self, fake_idx: isize) -> usize {
        let stack_top = self.stack.len() as isize;
        let stack_bottom = self.stack_bottom as isize;
        let stack_len = stack_top - stack_bottom;
        if fake_idx > 0 && fake_idx <= stack_len {
            (fake_idx - 1 + stack_bottom) as usize
        } else if fake_idx < 0 && fake_idx >= -stack_len {
            (stack_top + fake_idx) as usize
        } else {
            panic!("index out of bounds");
        }
    }

    pub fn error(&self, kind: ErrorKind) -> Error {
        // TODO actually find position
        let pos = 0;
        let column = 0;
        Error::new(kind, pos, column)
    }

    fn eval_closure(&mut self, closure: Closure, num_args: u8) -> Result<u8> {
        let old_stack_bottom = self.stack_bottom;
        self.stack_bottom = self.stack.len() - num_args as usize;

        let num_params = closure.chunk.num_params;
        let num_locals = closure.chunk.num_locals;
        let is_vararg = closure.chunk.is_vararg;

        // Collect varargs if this is a vararg function
        let varargs = if is_vararg && num_args > num_params {
            let num_varargs = (num_args - num_params) as usize;
            let vararg_start = self.stack.len() - num_varargs;
            self.stack.drain(vararg_start..).collect()
        } else {
            Vec::new()
        };

        match num_args.cmp(&num_params) {
            Ordering::Less => {
                for _ in num_args..num_params {
                    self.push_nil();
                }
            }
            Ordering::Greater => {
                if !is_vararg {
                    self.pop((num_args - num_params) as isize);
                }
                // If is_vararg, we already collected the extra args above
            }
            Ordering::Equal => (),
        }

        for _ in 0..num_locals {
            self.push_nil();
        }

        let mut frame = self.initialize_frame(closure, varargs);
        let num_vals_returned = frame.eval(self)?;

        // Handle Return(255) which means "return all values on stack"
        let actual_num_returned = if num_vals_returned == u8::MAX {
            // Calculate how many values are above the frame's locals
            let frame_base = self.stack_bottom + num_params as usize + num_locals as usize;
            (self.stack.len() - frame_base) as u8
        } else {
            num_vals_returned
        };

        // Save return values from the top of the stack
        let ret_start = self.stack.len() - actual_num_returned as usize;
        let ret_vals: Vec<Val> = self.stack.drain(ret_start..).collect();

        // Close any open upvalues in this frame before clearing the stack
        self.close_upvalues(self.stack_bottom);

        // Clear the frame's stack space
        self.stack.truncate(self.stack_bottom);
        self.stack_bottom = old_stack_bottom;

        // Push return values back onto the stack
        self.stack.extend(ret_vals);

        Ok(actual_num_returned)
    }

    fn initialize_frame(&mut self, closure: Closure, varargs: Vec<Val>) -> Frame {
        let string_literal_start = self.string_literals.len();
        for s in &closure.chunk.string_literals {
            let string_ptr = {
                let Self {
                    stack,
                    globals,
                    string_literals,
                    open_upvalues,
                    ..
                } = self;
                self.heap.new_string(s.into(), || {
                    stack.mark_reachable();
                    globals.mark_reachable();
                    string_literals.mark_reachable();
                    for (_, uv_ref) in open_upvalues {
                        if let Upvalue::Closed(val) = &*uv_ref.borrow() {
                            val.mark_reachable();
                        }
                    }
                })
            };
            self.string_literals.push(Val::Str(string_ptr));
        }
        Frame::new(closure.chunk, closure.upvalues, varargs, string_literal_start, self.stack_bottom)
    }

    /// Pop a value from the stack
    fn pop_val(&mut self) -> Val {
        self.stack.pop().unwrap()
    }

    fn push_chunk(&mut self, chunk: Chunk) {
        self.push_closure(chunk, Vec::new());
    }

    fn push_closure(&mut self, chunk: Chunk, upvalues: Vec<UpvalueRef>) {
        let Self {
            stack,
            globals,
            string_literals,
            open_upvalues,
            ..
        } = self;
        let obj = self.heap.new_lua_fn(chunk, upvalues, || {
            stack.mark_reachable();
            globals.mark_reachable();
            string_literals.mark_reachable();
            for (_, uv_ref) in open_upvalues {
                if let Upvalue::Closed(val) = &*uv_ref.borrow() {
                    val.mark_reachable();
                }
            }
        });
        self.stack.push(Val::Obj(obj));
    }

    fn type_error(&self, e: TypeError) -> Error {
        self.error(ErrorKind::TypeError(e))
    }

    /// Helper for tests: evaluate a chunk with no upvalues.
    #[cfg(test)]
    fn eval_chunk(&mut self, chunk: Chunk, num_args: u8) -> Result<u8> {
        let closure = Closure {
            chunk,
            upvalues: Vec::new(),
        };
        self.eval_closure(closure, num_args)
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::compiler::parse_str;
    use super::lua_val::Val;
    use super::Chunk;
    use super::Instr::*;
    use super::State;

    #[test]
    fn vm_test01() {
        let mut state = State::new();
        let input = parse_str("a = 1").unwrap();
        state.eval_chunk(input, 0).unwrap();
        assert_eq!(Val::Num(1.0), *state.globals.get("a").unwrap());
    }

    #[test]
    fn vm_test02() {
        let mut state = State::new();
        let input = Chunk {
            code: vec![
                PushString(1),
                PushString(2),
                Concat,
                SetGlobal(0),
                Return(0),
            ],
            string_literals: vec!["key".to_string(), "a".to_string(), "b".to_string()],
            ..Chunk::default()
        };
        state.eval_chunk(input, 0).unwrap();
        let val = state.globals.get("key").unwrap();
        assert_eq!("ab".to_string(), val.as_string().unwrap());
    }

    #[test]
    fn vm_test04() {
        let mut state = State::new();
        let input = Chunk {
            code: vec![PushNum(0), PushNum(0), Equal, SetGlobal(0), Return(0)],
            number_literals: vec![2.5],
            string_literals: vec!["a".to_string()],
            ..Chunk::default()
        };
        state.eval_chunk(input, 0).unwrap();
        assert_eq!(Val::Bool(true), *state.globals.get("a").unwrap());
    }

    #[test]
    fn vm_test05() {
        let mut state = State::new();
        let input = Chunk {
            code: vec![
                PushBool(true),
                BranchFalseKeep(2),
                Pop,
                PushBool(false),
                SetGlobal(0),
                Return(0),
            ],
            string_literals: vec!["key".to_string()],
            ..Chunk::default()
        };
        state.eval_chunk(input, 0).unwrap();
        assert_eq!(Val::Bool(false), *state.globals.get("key").unwrap());
    }

    #[test]
    fn vm_test06() {
        let mut state = State::new();
        let code = vec![
            PushBool(true),
            BranchFalse(3),
            PushNum(0),
            SetGlobal(0),
            Return(0),
        ];
        let chunk = Chunk {
            code,
            number_literals: vec![5.0],
            string_literals: vec!["a".to_string()],
            ..Chunk::default()
        };
        state.eval_chunk(chunk, 0).unwrap();
        assert_eq!(Val::Num(5.0), *state.globals.get("a").unwrap());
    }

    #[test]
    fn vm_test07() {
        let mut state = State::new();
        let code = vec![
            PushNum(0),
            PushNum(0),
            Less,
            BranchFalse(2),
            PushBool(true),
            SetGlobal(0),
            Return(0),
        ];
        let chunk = Chunk {
            code,
            number_literals: vec![2.0],
            string_literals: vec!["a".to_string()],
            ..Chunk::default()
        };
        state.eval_chunk(chunk, 0).unwrap();
        assert!(state.globals.get("a").is_none());
    }

    #[test]
    fn vm_test08() {
        let code = vec![
            PushNum(2), // a = 2
            SetGlobal(0),
            GetGlobal(0), // a <0
            PushNum(0),
            Less,
            BranchFalse(5),
            GetGlobal(0),
            PushNum(1),
            Add,
            SetGlobal(0),
            Jump(-9),
            Return(0),
        ];
        let chunk = Chunk {
            code,
            number_literals: vec![1.0, 10.0, 0.0],
            string_literals: vec!["a".to_string()],
            ..Chunk::default()
        };
        let mut state = State::new();
        state.eval_chunk(chunk, 0).unwrap();
    }

    #[test]
    fn vm_test09() {
        // local a = 1
        // while a < 10 do
        //   a = a + 1
        // end
        // x = a
        let code = vec![
            PushNum(0),
            SetLocal(0),
            GetLocal(0),
            PushNum(1),
            Less,
            BranchFalse(5),
            GetLocal(0),
            PushNum(2),
            Add,
            SetLocal(0),
            Jump(-9),
            GetLocal(0),
            SetGlobal(0),
            Return(0),
        ];
        let chunk = Chunk {
            code,
            number_literals: vec![1.0, 10.0, 1.0],
            string_literals: vec!["x".to_string()],
            num_locals: 1,
            ..Chunk::default()
        };
        let mut state = State::new();
        state.eval_chunk(chunk, 0).unwrap();
        assert_eq!(Val::Num(10.0), *state.globals.get("x").unwrap());
    }

    #[test]
    fn vm_test10() {
        let code = vec![
            // For loop control variables
            PushNum(0), // start = 6
            PushNum(1), // limit = 2
            PushNum(1), // step = 2
            // Start loop
            ForPrep(0, 3),
            PushNum(0),
            SetGlobal(0), // a = 2
            // End loop
            ForLoop(0, -3),
            Return(0),
        ];
        let chunk = Chunk {
            code,
            number_literals: vec![6.0, 2.0],
            string_literals: vec!["a".to_string()],
            num_locals: 4,
            ..Chunk::default()
        };
        let mut state = State::new();
        state.eval_chunk(chunk, 0).unwrap();
        assert!(state.globals.get("a").is_none());
    }

    #[test]
    fn vm_test11() {
        let text = "
            a = 0
            for i = 1, 3 do
                a = a + i
            end";
        let chunk = parse_str(&text).unwrap();
        let mut state = State::new();
        state.eval_chunk(chunk, 0).unwrap();
        let a = state.globals.get("a").unwrap().as_num().unwrap();
        assert_eq!(a, 6.0);
    }
}
