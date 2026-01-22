//! This module provides the `State` struct, which handles the primary
//! components of the VM.

mod eval;
mod frame;
mod lua_val;
mod metamethod;
mod object;
mod stack;
mod table;
mod table_ops;

pub use lua_val::LuaType;
pub use lua_val::RustFunc;

use std::collections::HashMap;

use super::compiler;
use super::error::Error;
use super::error::ErrorKind;
use super::error::TypeError;
use super::Chunk;
use super::Instr;
use super::Result;

use lua_val::Val;
use object::{GcHeap, Markable, Upvalue, UpvalueRef};
use table::Table;

/// The main interface into the Lua VM.
pub struct State {
    /// The global environment. This may be changed to an actual Table in the future.
    pub(super) globals: HashMap<String, Val>,
    /// The main stack which stores values.
    pub(super) stack: Vec<Val>,
    /// The bottom index of the current frame in the stack.
    pub(super) stack_bottom: usize,
    /// The heap which holds any garbage-collected Objects.
    pub(super) heap: GcHeap,
    /// The string literals (as `Val`s) of every active `Frame`.
    pub(super) string_literals: Vec<Val>,
    /// Open upvalues currently pointing to stack slots.
    /// Each entry is (stack_index, upvalue_ref). Kept sorted by stack_index descending
    /// so we can efficiently close them when a function returns.
    pub(super) open_upvalues: Vec<(usize, UpvalueRef)>,
    /// Stack of call base positions for dynamic argument counting.
    /// Pushed by MarkCallBase, popped by Call(255, ...).
    /// Supports nested function calls where each level needs its own base.
    pub(super) vararg_call_bases: Vec<usize>,
    /// Cost budget remaining. When this reaches 0 or below, operations with cost > 0
    /// will fail. The action that pushes you over budget completes before stopping.
    /// Uses i64 to allow going negative (the final action that exceeds budget completes).
    pub(super) cost_remaining: i64,
    /// The original cost budget (for error reporting).
    pub(super) cost_budget: i64,
    /// Total cost consumed (for reporting).
    pub(super) cost_used: u64,
    /// Current metamethod call depth (for __index/__newindex chains).
    /// Prevents stack overflow from circular metamethod references.
    pub(super) metamethod_depth: u32,
    /// Current function call depth. Prevents stack overflow from deep recursion.
    pub(super) call_depth: u32,
}

/// Maximum call depth to prevent stack overflow from deep recursion.
/// Lua's default is 200, we use 1000 for a bit more headroom.
const MAX_CALL_DEPTH: u32 = 1000;

/// Maximum stack size (number of values) to prevent memory exhaustion.
const MAX_STACK_SIZE: usize = 1_000_000;

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
            stack: Vec::with_capacity(256), // Pre-size for typical function depth * locals
            stack_bottom: 0,
            heap: GcHeap::with_threshold(Self::GC_INITIAL_THRESHOLD),
            string_literals: Vec::with_capacity(64), // Pre-size for string literals
            open_upvalues: Vec::new(),
            vararg_call_bases: Vec::new(),
            cost_remaining: i64::MAX,
            cost_budget: i64::MAX,
            cost_used: 0,
            metamethod_depth: 0,
            call_depth: 0,
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
    #[inline(always)]
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

    /// Pushes onto the stack the value of the global `name`.
    pub fn get_global(&mut self, name: &str) {
        let val = self.globals.get(name).cloned().unwrap_or_default();
        self.stack.push(val);
    }

    /// Pops a value from the stack and sets it as the new value of global
    /// `name`.
    pub fn set_global(&mut self, name: &str) {
        let val = self.pop_val();
        self.globals.insert(name.to_string(), val);
    }

    /// Allocates a string on the heap.
    pub(super) fn alloc_string(&mut self, s: String) -> Val {
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

    pub fn error(&self, kind: ErrorKind) -> Error {
        // TODO actually find position
        let pos = 0;
        let column = 0;
        Error::new(kind, pos, column)
    }

    pub(super) fn type_error(&self, e: TypeError) -> Error {
        self.error(ErrorKind::TypeError(e))
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
