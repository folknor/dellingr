//! This module provides the `State` struct, which handles the primary
//! components of the VM.

mod anchor;
mod eval;
mod frame;
mod lua_val;
mod metamethod;
mod object;
mod stack;
mod table;
mod table_ops;

pub use anchor::Anchor;
pub use lua_val::LuaType;
pub use lua_val::RustFunc;

use indexmap::IndexMap;
use rand::SeedableRng;
use std::sync::Arc;

use super::Instr;
use super::Result;
use super::compiler;
use super::compiler::Bytecode;
use super::compiler::RuntimeCaches;
use super::error::Error;
use super::error::ErrorKind;
use super::error::StackFrame;
use super::error::TypeError;
use super::host::{DefaultCallbacks, HostCallbacks};
use super::instr::{ArgCount, Builtin, RetCount};

use anchor::Registry;

pub(super) use lua_val::Val;
pub(super) use object::ObjectPtr;
use object::{GcHeap, Markable, UpvaluePool, UpvalueRef};
use table::Table;

/// Marks all GC roots. Called before garbage collection to identify reachable objects.
///
/// This function is the single source of truth for what constitutes a GC root.
/// All allocation functions that may trigger GC must call this with the same set of roots.
///
/// # Arguments
/// * `heap` - The GC heap (needed to access objects for recursive marking)
/// * `stack` - The VM stack containing local values and temporaries
/// * `globals` - Global variables table
/// * `builtins` - Fast-access array for builtin functions
/// * `string_literals` - String constants from active frames
/// * `active_call_roots` - Lua closures currently being executed
/// * `upvalue_pool` - Pool of upvalues (closed upvalues contain values that need marking)
#[hotpath::measure]
#[allow(
    clippy::too_many_arguments,
    reason = "single-call-site GC marking entry point; bundling into a struct adds boilerplate without clarifying anything"
)]
pub(super) fn mark_gc_roots(
    heap: &GcHeap,
    stack: &[Val],
    globals: &IndexMap<String, Val>,
    builtins: &[Val],
    string_literals: &[Val],
    active_call_roots: &[Val],
    upvalue_pool: &UpvaluePool,
    registry: &Registry,
) {
    // Mark all roots - closed upvalues are now marked transitively when
    // marking LuaFn closures that reference them
    stack.mark_reachable(heap, upvalue_pool);
    globals.mark_reachable(heap, upvalue_pool);
    builtins.mark_reachable(heap, upvalue_pool);
    string_literals.mark_reachable(heap, upvalue_pool);
    active_call_roots.mark_reachable(heap, upvalue_pool);
    registry.mark_reachable(heap, upvalue_pool);
    // Note: open upvalues point to stack (already marked), closed upvalues
    // are marked transitively through the closures that reference them
}

/// Information about an active function call, used for stack traces.
#[derive(Clone)]
pub(super) struct CallInfo {
    /// The bytecode being executed.
    pub(super) bytecode: Arc<Bytecode>,
    /// Current instruction pointer.
    pub(super) ip: usize,
}

/// The main interface into the Lua VM.
pub struct State {
    /// The global environment. Uses IndexMap for deterministic iteration order
    /// (GC marking, restrict_globals). May be changed to an actual Table in the future.
    pub(super) globals: IndexMap<String, Val>,
    /// Fast array for well-known builtin globals (print, pairs, type, etc.).
    /// Indexed by Builtin enum. Avoids IndexMap lookup for common globals.
    pub(super) builtins: [Val; Builtin::COUNT],
    /// Bumped when the whole global environment is swapped.
    pub(super) globals_version: u64,
    /// The main stack which stores values.
    pub(super) stack: Vec<Val>,
    /// The bottom index of the current frame in the stack.
    pub(super) stack_bottom: usize,
    /// The heap which holds any garbage-collected Objects.
    pub(super) heap: GcHeap,
    /// The string literals (as `Val`s) of every active `Frame`.
    pub(super) string_literals: Vec<Val>,
    /// Lua closure objects removed from the visible stack while they execute.
    pub(super) active_call_roots: Vec<Val>,
    /// Pool for upvalue storage. Avoids per-upvalue heap allocations.
    pub(super) upvalue_pool: UpvaluePool,
    /// Open upvalues currently pointing to stack slots.
    /// Each entry is (stack_index, upvalue_ref). Kept sorted by stack_index ascending
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
    /// Call stack for generating stack traces on errors.
    /// Each entry represents an active Lua function call.
    pub(super) call_stack: Vec<CallInfo>,
    /// Host callbacks for print output, error handling, etc.
    pub(super) callbacks: Box<dyn HostCallbacks + Send>,
    /// Current source name (for callback context).
    /// Updated when loading a new chunk.
    pub(super) current_source: Option<String>,
    /// User-defined data that RustFuncs can access.
    /// Use `set_user_data<T>()` and `user_data<T>()` to store/retrieve.
    user_data: Option<Box<dyn std::any::Any + Send>>,
    /// Seeded RNG for deterministic math.random(). Defaults to seed 0.
    /// Use `set_rng_seed()` to set a specific seed for replay.
    pub(super) rng: rand::rngs::StdRng,
    /// Registry of values retained from Rust via `Anchor` handles. Acts as
    /// an additional GC root set; participates in `mark_gc_roots`. Carries
    /// the State's process-unique `state_id` so cross-State misuse of an
    /// `Anchor` is caught.
    pub(super) registry: Registry,
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

// State marking is done through mark_gc_roots() which has direct heap access

impl State {
    const GC_INITIAL_THRESHOLD: usize = 20;

    /// Creates a new, independent state with default callbacks (stdout).
    pub fn new() -> Self {
        Self::with_callbacks(Box::new(DefaultCallbacks))
    }

    /// Creates a new state with custom host callbacks.
    ///
    /// # Example
    ///
    /// ```ignore
    /// struct MyCallbacks { output: Vec<String> }
    /// impl HostCallbacks for MyCallbacks {
    ///     fn on_print(&mut self, _source: Option<&str>, _line: u32, message: &str) {
    ///         self.output.push(message.to_string());
    ///     }
    /// }
    ///
    /// let mut state = State::with_callbacks(Box::new(MyCallbacks { output: vec![] }));
    /// ```
    pub fn with_callbacks(callbacks: Box<dyn HostCallbacks + Send>) -> Self {
        let mut me = Self::empty_with_callbacks(callbacks);
        me.open_libs();
        me
    }

    /// Creates a new state without opening any of the standard libs.
    /// The global namespace of this state is entirely empty. This corresponds
    /// to the `lua_newstate' function in the C API.
    pub fn empty() -> Self {
        Self::empty_with_callbacks(Box::new(DefaultCallbacks))
    }

    /// Creates an empty state with custom callbacks.
    pub(crate) fn empty_with_callbacks(callbacks: Box<dyn HostCallbacks + Send>) -> Self {
        let state_id = anchor::next_state_id();
        Self {
            globals: IndexMap::new(),
            builtins: std::array::from_fn(|_| Val::Nil),
            globals_version: 0,
            stack: Vec::with_capacity(256), // Pre-size for typical function depth * locals
            stack_bottom: 0,
            heap: GcHeap::with_threshold(Self::GC_INITIAL_THRESHOLD),
            string_literals: Vec::with_capacity(64), // Pre-size for string literals
            active_call_roots: Vec::with_capacity(64),
            upvalue_pool: UpvaluePool::new(),
            open_upvalues: Vec::new(),
            vararg_call_bases: Vec::new(),
            cost_remaining: i64::MAX,
            cost_budget: i64::MAX,
            cost_used: 0,
            metamethod_depth: 0,
            call_depth: 0,
            call_stack: Vec::with_capacity(64), // Pre-size for call stack
            callbacks,
            current_source: None,
            user_data: None,
            rng: rand::rngs::StdRng::seed_from_u64(0),
            registry: Registry::new(state_id),
        }
    }

    /// Sets the RNG seed for deterministic math.random() behavior.
    pub fn set_rng_seed(&mut self, seed: u64) {
        self.rng = rand::rngs::StdRng::seed_from_u64(seed);
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
    ///
    /// Use this in RustFuncs to charge for expensive operations.
    #[inline(always)]
    pub fn consume_cost(&mut self, cost: u64) -> Result<()> {
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

    // ========================================================================
    // User data
    // ========================================================================

    /// Store arbitrary user data that RustFuncs can access.
    ///
    /// Useful for passing context to Rust callbacks, like a command collector.
    /// `T` must be `Send` because `State` is `Send`: any data the embedder
    /// hands to the VM must be safe to move across threads with the State.
    ///
    /// # Example
    /// ```ignore
    /// let collector = Arc::new(Mutex::new(CommandCollector::default()));
    /// state.set_user_data(collector.clone());
    ///
    /// state.push_rust_fn(|state| {
    ///     let collector = state.user_data::<Arc<Mutex<CommandCollector>>>().unwrap();
    ///     collector.lock().unwrap().turn = Some(0.5);
    ///     Ok(0)
    /// });
    /// ```
    pub fn set_user_data<T: Send + 'static>(&mut self, data: T) {
        self.user_data = Some(Box::new(data));
    }

    /// Get a reference to the stored user data.
    /// Returns None if no data is stored or if the type doesn't match.
    pub fn user_data<T: Send + 'static>(&self) -> Option<&T> {
        self.user_data.as_ref()?.downcast_ref()
    }

    /// Get a mutable reference to the stored user data.
    /// Returns None if no data is stored or if the type doesn't match.
    pub fn user_data_mut<T: Send + 'static>(&mut self) -> Option<&mut T> {
        self.user_data.as_mut()?.downcast_mut()
    }

    /// Clear the stored user data.
    pub fn clear_user_data(&mut self) {
        self.user_data = None;
    }

    // ========================================================================
    // Anchor registry: retain Lua values from Rust without polluting globals
    // ========================================================================

    /// Pop the top of stack and store it in this State's registry. Returns a
    /// `Copy` `Anchor` handle the embedder can store and use later to push
    /// or call the value.
    ///
    /// Errors with `ErrorKind::AnchorNil` if the top of stack is `nil`
    /// (use `Option<Anchor>` for an absent-value sentinel; nil carries no
    /// GC weight and has no use case for a stable handle). The stack is
    /// only popped on success - errors leave it untouched.
    pub fn anchor(&mut self) -> Result<Anchor> {
        // Surface "stack empty" as an embedder error rather than the
        // VM-bug panic in pop_val. Validate before popping so the stack
        // is untouched on error - matches `anchor_at` and the rest of
        // the host API.
        let val = self.at_index(-1)?;
        if matches!(val, Val::Nil) {
            return Err(Error::without_location(ErrorKind::AnchorNil));
        }
        let anchor = self.registry.insert(val);
        self.pop_val();
        Ok(anchor)
    }

    /// Like `anchor`, but reads the value at `idx` without popping. The
    /// stack is left untouched on both success and error.
    pub fn anchor_at(&mut self, idx: isize) -> Result<Anchor> {
        let val = self.at_index(idx)?;
        if matches!(val, Val::Nil) {
            return Err(Error::without_location(ErrorKind::AnchorNil));
        }
        Ok(self.registry.insert(val))
    }

    /// Like `anchor`, but additionally requires the value to be a function
    /// (Lua closure or `RustFunc`). Use this when you want the type error
    /// at registration time rather than at the first `call_anchor`.
    ///
    /// This is strict: tables with `__call` are rejected. To anchor a
    /// callable table, use `anchor` and let the existing dispatch handle
    /// `__call` at call time.
    pub fn anchor_function(&mut self) -> Result<Anchor> {
        let val = self.at_index(-1)?;
        let typ = val.typ(&self.heap);
        if typ != LuaType::Function {
            return Err(self.type_error(TypeError::FunctionCall(typ)));
        }
        let anchor = self.registry.insert(val);
        self.pop_val();
        Ok(anchor)
    }

    /// Like `anchor_at`, but additionally requires the value to be a
    /// function. See `anchor_function` for the strictness note.
    pub fn anchor_function_at(&mut self, idx: isize) -> Result<Anchor> {
        let val = self.at_index(idx)?;
        let typ = val.typ(&self.heap);
        if typ != LuaType::Function {
            return Err(self.type_error(TypeError::FunctionCall(typ)));
        }
        Ok(self.registry.insert(val))
    }

    /// Push the anchored value onto the stack. Errors with
    /// `ErrorKind::InvalidAnchor` if the handle is stale, released, or
    /// belongs to a different `State`.
    pub fn push_anchor(&mut self, a: Anchor) -> Result<()> {
        match self.registry.get(a) {
            Some(val) => {
                self.stack.push(val);
                Ok(())
            }
            None => Err(Error::without_location(ErrorKind::InvalidAnchor)),
        }
    }

    /// Push the anchored value and call it. Convenience over
    /// `push_anchor` + `call`. Cost charges through the existing dispatch
    /// path; `anchor` and `release_anchor` themselves charge nothing.
    pub fn call_anchor(&mut self, a: Anchor, args: ArgCount, rets: RetCount) -> Result<()> {
        // The function must be pushed BEFORE the args are arranged on the
        // stack by the caller. Embedder protocol: push args, then
        // call_anchor (which inserts the function below the args).
        // Implementation: push the function on top, then rotate it under
        // the args.
        let val = match self.registry.get(a) {
            Some(val) => val,
            None => return Err(Error::without_location(ErrorKind::InvalidAnchor)),
        };
        let n_args = match args {
            ArgCount::Fixed(n) => n as usize,
            ArgCount::Dynamic => {
                // ArgCount::Dynamic relies on a vararg_call_base pushed by
                // OP_MARK_CALL_BASE inside bytecode; no public host API
                // populates that stack, so a host-side call_anchor with
                // Dynamic would either panic on the missing base or read
                // a stale one set up by prior bytecode and corrupt the
                // call. Reject it explicitly.
                return Err(Error::without_location(ErrorKind::InternalError(
                    "call_anchor does not support ArgCount::Dynamic; use ArgCount::Fixed".into(),
                )));
            }
        };
        // Insert the fn below the n_args topmost values.
        let insert_at = self.stack.len().checked_sub(n_args).ok_or_else(|| {
            Error::without_location(ErrorKind::InvalidStackIndex {
                index: -(n_args as isize) - 1,
            })
        })?;
        self.stack.insert(insert_at, val);
        self.call(args, rets)
    }

    /// Release an anchor. Returns `true` if the handle was live and the
    /// value was freed; `false` for stale, already-released, or
    /// wrong-State handles. Idempotent: never errors, never panics.
    pub fn release_anchor(&mut self, a: Anchor) -> bool {
        self.registry.remove(a)
    }

    /// Returns the type of an anchored value. `None` if the handle is
    /// not live in this State.
    pub fn anchor_type(&self, a: Anchor) -> Option<LuaType> {
        self.registry.get(a).map(|val| val.typ(&self.heap))
    }

    /// Number of live anchors. For embedder leak diagnostics.
    pub fn anchor_count(&self) -> usize {
        self.registry.len()
    }

    // ========================================================================
    // Callbacks
    // ========================================================================

    /// Get a mutable reference to the host callbacks.
    ///
    /// Use this to retrieve collected print output or other callback state.
    pub fn callbacks_mut(&mut self) -> &mut dyn HostCallbacks {
        self.callbacks.as_mut()
    }

    /// Replace the host callbacks with new ones, returning the old callbacks.
    pub fn replace_callbacks(
        &mut self,
        callbacks: Box<dyn HostCallbacks + Send>,
    ) -> Box<dyn HostCallbacks + Send> {
        std::mem::replace(&mut self.callbacks, callbacks)
    }

    // ========================================================================
    // Memory tracking
    // ========================================================================

    /// Returns the number of GC-managed objects (tables and closures).
    pub fn object_count(&self) -> usize {
        self.heap.object_count()
    }

    /// Returns the number of interned strings.
    pub fn string_count(&self) -> usize {
        self.heap.string_count()
    }

    /// Returns the total number of heap allocations (objects + strings).
    pub fn heap_size(&self) -> usize {
        self.heap.object_count() + self.heap.string_count()
    }

    // ========================================================================
    // Host-controlled GC
    // ========================================================================

    /// Returns true if the GC threshold has been reached.
    /// Use this to check if `gc_collect()` should be called.
    pub fn gc_should_run(&self) -> bool {
        self.heap.is_full()
    }

    /// Returns the current GC threshold.
    pub fn gc_threshold(&self) -> usize {
        self.heap.threshold()
    }

    /// Sets the GC threshold. Collection triggers when object_count >= threshold.
    /// Set to `usize::MAX` to effectively disable automatic GC.
    pub fn gc_set_threshold(&mut self, threshold: usize) {
        self.heap.set_threshold(threshold);
    }

    /// Disables automatic GC by setting threshold to usize::MAX.
    /// After calling this, GC only runs when you explicitly call `gc_collect()`.
    pub fn gc_disable_auto(&mut self) {
        self.heap.set_threshold(usize::MAX);
    }

    /// Forces a full garbage collection cycle.
    /// This marks all reachable objects and frees unreachable ones.
    #[hotpath::measure]
    pub fn gc_collect(&mut self) {
        // Mark all roots
        mark_gc_roots(
            &self.heap,
            &self.stack,
            &self.globals,
            &self.builtins,
            &self.string_literals,
            &self.active_call_roots,
            &self.upvalue_pool,
            &self.registry,
        );
        // Sweep unmarked objects
        self.heap.collect();
    }

    // ========================================================================
    // Host callbacks
    // ========================================================================

    /// Called by the built-in `print()` function.
    /// Routes output through host callbacks with source context.
    pub(crate) fn host_print(&mut self, message: &str) {
        // Get current line from call stack (if available)
        let line = self
            .call_stack
            .last()
            .and_then(|info| {
                info.bytecode
                    .line_info
                    .get(info.ip.saturating_sub(1))
                    .copied()
            })
            .unwrap_or(0);

        let source = self.current_source.as_deref();
        self.callbacks.on_print(source, line, message);
    }

    /// Called when an error occurs. Notifies host callbacks.
    pub(crate) fn host_error(&mut self, error: &Error) {
        let source = self.current_source.as_deref();
        self.callbacks.on_error(source, error);
    }

    /// Returns the current source name (if set).
    pub fn current_source(&self) -> Option<&str> {
        self.current_source.as_deref()
    }

    /// Pushes onto the stack the value of the global `name`.
    #[hotpath::measure]
    pub fn get_global(&mut self, name: &str) {
        // Check builtins first for common names
        let val = if let Some(slot) = Builtin::from_name(name) {
            self.builtins[slot as usize]
        } else {
            self.globals.get(name).copied().unwrap_or_default()
        };
        self.stack.push(val);
    }

    /// Instr::pop()s a value from the stack and sets it as the new value of global
    /// `name`.
    #[hotpath::measure]
    pub fn set_global(&mut self, name: &str) {
        let val = self.pop_val();
        self.set_global_value(name, val);
    }

    pub(super) fn set_global_value(&mut self, name: &str, val: Val) {
        self.set_global_value_owned(name.to_string(), val);
    }

    pub(super) fn set_global_value_owned(&mut self, name: String, val: Val) {
        // Update builtins array if this is a well-known name. Rebinding a
        // builtin slot poisons inline caches that hold a direct ObjectPtr
        // to the previous library table (string-method IC, method-lookup
        // IC reaching the lib via __index), so bump globals_version to
        // force re-resolution. User globals don't poison those ICs - the
        // ICs only key on builtin slots - so the bump stays narrow.
        if let Some(slot) = Builtin::from_name(&name) {
            self.builtins[slot as usize] = val;
            self.globals_version = self.globals_version.wrapping_add(1);
        }
        self.globals.insert(name, val);
    }

    /// Execute a function with a restricted global environment.
    /// Only globals in the whitelist are accessible during execution.
    /// The original environment is restored after the function completes (or errors).
    pub fn with_restricted_env<F, R>(&mut self, whitelist: &[&str], f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        // Build restricted environment
        let mut restricted_globals = IndexMap::new();
        let mut restricted_builtins: [Val; Builtin::COUNT] = std::array::from_fn(|_| Val::Nil);

        for name in whitelist {
            // Copy from builtins if it's a well-known name
            if let Some(slot) = Builtin::from_name(name) {
                restricted_builtins[slot as usize] = self.builtins[slot as usize];
            }
            // Also copy from globals
            if let Some(val) = self.globals.get(*name) {
                restricted_globals.insert((*name).to_string(), *val);
            }
        }

        // Swap to restricted environment
        let saved_globals = std::mem::replace(&mut self.globals, restricted_globals);
        let saved_builtins = std::mem::replace(&mut self.builtins, restricted_builtins);
        self.globals_version = self.globals_version.wrapping_add(1);

        // Execute the function
        let result = f(self);

        // Restore original environment
        self.globals = saved_globals;
        self.builtins = saved_builtins;
        self.globals_version = self.globals_version.wrapping_add(1);

        result
    }

    /// Allocates a string on the heap.
    #[hotpath::measure]
    pub(super) fn alloc_string(&mut self, bytes: impl AsRef<[u8]>) -> Val {
        // Check if GC is needed before allocating
        if self.heap.is_full() {
            self.gc_collect();
        }
        let ptr = self.heap.alloc_string(bytes.as_ref());
        Val::Str(ptr)
    }

    /// Construct an [`Error`] of the given kind with source position
    /// drawn from the current frame (placeholder until line tracking lands).
    pub fn error(&self, kind: ErrorKind) -> Error {
        // TODO actually find position
        let pos = 0;
        let column = 0;
        Error::new(kind, pos, column)
    }

    pub(super) fn type_error(&self, e: TypeError) -> Error {
        self.error(ErrorKind::TypeError(e))
    }

    /// Build a stack trace from the current call stack and the active frame.
    /// The frame represents the innermost (current) function where the error occurred.
    #[allow(private_interfaces)]
    pub(super) fn build_stack_trace(&self, current_frame: &frame::Frame) -> Vec<StackFrame> {
        let mut trace = Vec::with_capacity(self.call_stack.len() + 1);

        // First entry: the current frame where the error occurred
        trace.push(current_frame.to_stack_frame());

        // Add entries from call_stack (most recent first).
        // Skip the last entry (current function) since we already have it from current_frame.
        // The remaining entries are the callers, with ip pointing to their call sites.
        for call_info in self.call_stack.iter().rev().skip(1) {
            // Get line number from the ip (call site)
            let line = if call_info.ip > 0 {
                call_info
                    .bytecode
                    .line_info
                    .get(call_info.ip - 1)
                    .copied()
                    .unwrap_or(0)
            } else {
                call_info.bytecode.line_info.first().copied().unwrap_or(0)
            };
            trace.push(StackFrame {
                function_name: call_info.bytecode.name.clone(),
                source: call_info.bytecode.source.clone(),
                line,
            });
        }

        trace
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::Bytecode;
    use super::Instr;
    use super::State;
    use super::compiler::parse_str;
    use super::lua_val::Val;
    use crate::instr::RetCount;

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
        let input = Bytecode {
            code: vec![
                Instr::push_string(1),
                Instr::push_string(2),
                Instr::concat(2),
                Instr::set_global(0),
                Instr::ret(RetCount::Fixed(0)),
            ],
            string_literals: vec!["key".into(), "a".into(), "b".into()],
            ..Bytecode::default()
        };
        state.eval_chunk(input, 0).unwrap();
        let val = state.globals.get("key").unwrap();
        assert_eq!(val.as_string(&state.heap), Some(&b"ab"[..]));
    }

    #[test]
    fn vm_test04() {
        let mut state = State::new();
        let input = Bytecode {
            code: vec![
                Instr::push_num(0),
                Instr::push_num(0),
                Instr::equal(),
                Instr::set_global(0),
                Instr::ret(RetCount::Fixed(0)),
            ],
            number_literals: vec![2.5],
            string_literals: vec!["a".into()],
            ..Bytecode::default()
        };
        state.eval_chunk(input, 0).unwrap();
        assert_eq!(Val::Bool(true), *state.globals.get("a").unwrap());
    }

    #[test]
    fn vm_test05() {
        let mut state = State::new();
        let input = Bytecode {
            code: vec![
                Instr::push_bool(true),
                Instr::branch_false_keep(2),
                Instr::pop(),
                Instr::push_bool(false),
                Instr::set_global(0),
                Instr::ret(RetCount::Fixed(0)),
            ],
            string_literals: vec!["key".into()],
            ..Bytecode::default()
        };
        state.eval_chunk(input, 0).unwrap();
        assert_eq!(Val::Bool(false), *state.globals.get("key").unwrap());
    }

    #[test]
    fn vm_test06() {
        let mut state = State::new();
        let code = vec![
            Instr::push_bool(true),
            Instr::branch_false(3),
            Instr::push_num(0),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ];
        let chunk = Bytecode {
            code,
            number_literals: vec![5.0],
            string_literals: vec!["a".into()],
            ..Bytecode::default()
        };
        state.eval_chunk(chunk, 0).unwrap();
        assert_eq!(Val::Num(5.0), *state.globals.get("a").unwrap());
    }

    #[test]
    fn vm_test07() {
        let mut state = State::new();
        let code = vec![
            Instr::push_num(0),
            Instr::push_num(0),
            Instr::less(),
            Instr::branch_false(2),
            Instr::push_bool(true),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ];
        let chunk = Bytecode {
            code,
            number_literals: vec![2.0],
            string_literals: vec!["a".into()],
            ..Bytecode::default()
        };
        state.eval_chunk(chunk, 0).unwrap();
        assert!(state.globals.get("a").is_none());
    }

    #[test]
    fn vm_test08() {
        let code = vec![
            Instr::push_num(2), // a = 2
            Instr::set_global(0),
            Instr::get_global(0), // a <0
            Instr::push_num(0),
            Instr::less(),
            Instr::branch_false(5),
            Instr::get_global(0),
            Instr::push_num(1),
            Instr::add(),
            Instr::set_global(0),
            Instr::jump(-9),
            Instr::ret(RetCount::Fixed(0)),
        ];
        let chunk = Bytecode {
            code,
            number_literals: vec![1.0, 10.0, 0.0],
            string_literals: vec!["a".into()],
            ..Bytecode::default()
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
            Instr::push_num(0),
            Instr::set_local(0),
            Instr::get_local(0),
            Instr::push_num(1),
            Instr::less(),
            Instr::branch_false(5),
            Instr::get_local(0),
            Instr::push_num(2),
            Instr::add(),
            Instr::set_local(0),
            Instr::jump(-9),
            Instr::get_local(0),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ];
        let chunk = Bytecode {
            code,
            number_literals: vec![1.0, 10.0, 1.0],
            string_literals: vec!["x".into()],
            num_locals: 1,
            ..Bytecode::default()
        };
        let mut state = State::new();
        state.eval_chunk(chunk, 0).unwrap();
        assert_eq!(Val::Num(10.0), *state.globals.get("x").unwrap());
    }

    #[test]
    fn vm_test10() {
        let code = vec![
            // For loop control variables
            Instr::push_num(0), // start = 6
            Instr::push_num(1), // limit = 2
            Instr::push_num(1), // step = 2
            // Start loop
            Instr::for_prep(0, 3),
            Instr::push_num(0),
            Instr::set_global(0), // a = 2
            // End loop
            Instr::for_loop(0, -3),
            Instr::ret(RetCount::Fixed(0)),
        ];
        let chunk = Bytecode {
            code,
            number_literals: vec![6.0, 2.0],
            string_literals: vec!["a".into()],
            num_locals: 4,
            ..Bytecode::default()
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
        let chunk = parse_str(text).unwrap();
        let mut state = State::new();
        state.eval_chunk(chunk, 0).unwrap();
        let a = state.globals.get("a").unwrap().as_num().unwrap();
        assert_eq!(a, 6.0);
    }

    #[test]
    fn gc_host_controlled() {
        let mut state = State::new();

        // Check initial state
        let initial_objects = state.object_count();
        let initial_strings = state.string_count();
        assert!(state.heap_size() >= initial_objects + initial_strings);

        // Disable auto-GC
        state.gc_disable_auto();
        assert_eq!(state.gc_threshold(), usize::MAX);

        // Create some tables - GC won't trigger automatically
        let code = parse_str("t1 = {} t2 = {} t3 = {}").unwrap();
        state.eval_chunk(code, 0).unwrap();

        // Should have more objects now
        assert!(state.object_count() > initial_objects);

        // Manually trigger GC - tables are reachable so they survive
        let before_gc = state.object_count();
        state.gc_collect();
        assert_eq!(state.object_count(), before_gc); // All tables are reachable

        // Remove references and collect
        let code = parse_str("t1 = nil t2 = nil t3 = nil").unwrap();
        state.eval_chunk(code, 0).unwrap();
        state.gc_collect();

        // Now the tables should be collected
        assert!(state.object_count() < before_gc);
    }

    #[test]
    fn gc_threshold_control() {
        let mut state = State::empty(); // Empty state, no stdlib tables

        // Set a custom threshold
        state.gc_set_threshold(100);
        assert_eq!(state.gc_threshold(), 100);

        // Should not need GC yet
        assert!(!state.gc_should_run());

        // Set very low threshold
        state.gc_set_threshold(1);

        // Create a table to exceed threshold
        let code = parse_str("t = {}").unwrap();
        state.eval_chunk(code, 0).unwrap();

        // Now GC should be needed (but won't auto-run since we're just checking)
        // Note: threshold may have been adjusted by auto-GC during eval
    }

    /// Test the callback pattern used by fcomm2:
    /// - Main chunk defines local functions and global callbacks that capture them
    /// - Main chunk finishes (upvalues should be closed)
    /// - Later, the global callback is called from Rust (simulating game tick)
    #[test]
    fn callback_pattern_local_upvalue() {
        use crate::{ArgCount, RetCount};

        let mut state = State::new();

        // Load and execute main chunk that defines a global callback
        // capturing a local function
        let code = r#"
            local function helper()
                return 42
            end

            function on_tick()
                return helper()
            end
        "#;

        state.load_string(code).unwrap();
        state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

        // Now the main chunk has finished. The upvalue for `helper` should be closed.
        // Call the global callback from "outside" (simulating fcomm2's callback pattern)
        state.get_global("on_tick");
        assert_eq!(state.typ(-1), crate::LuaType::Function);

        state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

        // Should get 42 back
        let result = state.to_number(-1).unwrap();
        assert_eq!(result, 42.0);
    }

    /// More complex callback pattern with mutable upvalue
    #[test]
    fn callback_pattern_mutable_upvalue() {
        use crate::{ArgCount, RetCount};

        let mut state = State::new();

        let code = r#"
            local counter = 0

            local function increment()
                counter = counter + 1
                return counter
            end

            function tick()
                return increment()
            end
        "#;

        state.load_string(code).unwrap();
        state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

        // Call tick multiple times
        for expected in 1..=5 {
            state.get_global("tick");
            state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
            let result = state.to_number(-1).unwrap();
            state.pop(1);
            assert_eq!(result, expected as f64);
        }
    }

    /// Nested local functions with upvalues
    #[test]
    fn callback_pattern_nested_locals() {
        use crate::{ArgCount, RetCount};

        let mut state = State::new();

        let code = r#"
            local base = 100

            local function inner()
                return base
            end

            local function outer()
                return inner() + 10
            end

            function callback()
                return outer() + 1
            end
        "#;

        state.load_string(code).unwrap();
        state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

        state.get_global("callback");
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
        let result = state.to_number(-1).unwrap();
        assert_eq!(result, 111.0); // 100 + 10 + 1
    }

    /// Test that error line numbers are accurate
    #[test]
    fn error_line_numbers() {
        use crate::{ArgCount, RetCount};

        let mut state = State::new();

        // Error is on line 3 (t() call), not line 2 (t = {})
        let code = "-- comment\nlocal t = {}\nt()";

        state.load_string(code).unwrap();
        let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));

        assert!(result.is_err());
        let err = result.unwrap_err();
        // Check the stack trace points to line 3
        assert!(!err.stack_trace.is_empty());
        assert_eq!(err.stack_trace[0].line, 3);
    }
}
