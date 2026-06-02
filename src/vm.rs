//! This module provides the `State` struct, which handles the primary
//! components of the VM.

mod anchor;
mod eval;
mod eval_control;
mod eval_index;
mod eval_store;
mod frame;
mod lua_val;
mod metamethod;
mod object;
mod rng;
#[cfg(feature = "snapshot")]
mod save_state;
mod stack;
mod table;
mod table_ops;

pub use anchor::Anchor;
pub use lua_val::LuaType;
pub use lua_val::RustFunc;
#[cfg(feature = "snapshot")]
pub use save_state::{LoadError, SaveDiagnostics, SaveError, SaveState};

use indexmap::IndexMap;
#[cfg(feature = "snapshot")]
use std::collections::BTreeMap;
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
use rng::VmRng;
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
    pub(super) rng: VmRng,
    /// Registry of values retained from Rust via `Anchor` handles. Acts as
    /// an additional GC root set; participates in `mark_gc_roots`. Carries
    /// the State's process-unique `state_id` so cross-State misuse of an
    /// `Anchor` is caught.
    pub(super) registry: Registry,
    /// Stable ids for Rust functions that are allowed to survive save/load.
    #[cfg(feature = "snapshot")]
    pub(super) rust_fns_by_id: BTreeMap<String, RustFunc>,
    /// Reverse address lookup for save-time Rust function naming.
    #[cfg(feature = "snapshot")]
    pub(super) rust_fn_ids_by_addr: BTreeMap<usize, String>,
    /// Canonical environment objects (library tables, `_G`, and `_G`'s
    /// metatable) captured once at construction, each mapped to its save token.
    /// Keyed on the objects `open_libs` built, so user shadowing of a builtin
    /// slot (`math = {}`) cannot make the save classifier mistake a user table
    /// for an environment object. See `vm/save_state.rs`.
    #[cfg(feature = "snapshot")]
    pub(super) env_tokens: BTreeMap<ObjectPtr, String>,
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
        #[cfg(feature = "snapshot")]
        me.capture_env_tokens();
        me
    }

    /// Record the canonical environment objects right after `open_libs`, before
    /// any user code can shadow a builtin slot. Both save (object -> token) and
    /// load (token -> object) resolve through this snapshot.
    #[cfg(feature = "snapshot")]
    fn capture_env_tokens(&mut self) {
        let mut map = BTreeMap::new();
        for slot in [Builtin::Math, Builtin::String, Builtin::Table, Builtin::G] {
            if let Val::Obj(ptr) = self.builtins[slot as usize] {
                map.insert(ptr, slot.name().to_string());
                if slot == Builtin::G
                    && let Some(table) = self.heap.as_table_ref(ptr)
                    && let Some(mt) = table.get_metatable()
                {
                    map.insert(mt, "_G.metatable".to_string());
                }
            }
        }
        self.env_tokens = map;
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
            rng: VmRng::seed_from_u64(0),
            registry: Registry::new(state_id),
            #[cfg(feature = "snapshot")]
            rust_fns_by_id: BTreeMap::new(),
            #[cfg(feature = "snapshot")]
            rust_fn_ids_by_addr: BTreeMap::new(),
            #[cfg(feature = "snapshot")]
            env_tokens: BTreeMap::new(),
        }
    }

    pub(crate) fn reserve_stdlib_capacity(&mut self) {
        self.globals.reserve(Builtin::COUNT + 4);
        self.heap.reserve(8, 96);
    }

    /// Sets the RNG seed for deterministic math.random() behavior.
    pub fn set_rng_seed(&mut self, seed: u64) {
        self.rng = VmRng::seed_from_u64(seed);
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

    #[cfg(not(feature = "snapshot"))]
    pub(crate) fn set_global_rust_fn(&mut self, name: &str, func: RustFunc) {
        self.set_global_value(name, Val::RustFn(func));
    }

    /// Register a Rust function under a stable id and install it as a global.
    ///
    /// Save/load uses `id` to resolve reachable [`RustFunc`] values across
    /// processes; ids should be stable strings owned by the embedder.
    #[cfg(feature = "snapshot")]
    pub fn set_global_named_rust_fn(
        &mut self,
        name: &str,
        id: &str,
        func: RustFunc,
    ) -> std::result::Result<(), SaveError> {
        self.register_rust_fn(id, func)?;
        self.set_global_value(name, Val::RustFn(func));
        Ok(())
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn set_global_stdlib_rust_fn(&mut self, name: &str, id: &str, func: RustFunc) {
        self.set_global_named_rust_fn(name, id, func)
            .expect("stdlib Rust function registration cannot fail");
    }

    /// Register a Rust function id without installing the function anywhere.
    ///
    /// This is useful when the host pushes or stores the function manually but
    /// still wants it to survive save/load if Lua makes it reachable.
    #[cfg(feature = "snapshot")]
    pub fn register_rust_fn(
        &mut self,
        id: &str,
        func: RustFunc,
    ) -> std::result::Result<(), save_state::SaveError> {
        let addr = func as usize;
        // The only genuine error is an id collision: one id bound to two
        // different function addresses (an embedder bug). Re-registering the
        // same id->fn pair is idempotent.
        if let Some(existing_func) = self.rust_fns_by_id.get(id) {
            if *existing_func as usize != addr {
                return Err(save_state::SaveError::DuplicateFunctionRegistration {
                    id: id.to_string(),
                });
            }
            return Ok(());
        }
        self.rust_fns_by_id.insert(id.to_string(), func);
        // Several ids may legitimately share one address: intentional aliasing,
        // or identical-code folding collapsing distinct fns under release LTO.
        // Both are behaviorally safe (same code), so we never error here; we
        // keep the lexicographically-smallest id as the reverse name so saves
        // stay deterministic regardless of registration order.
        match self.rust_fn_ids_by_addr.get(&addr) {
            Some(existing_id) if existing_id.as_str() <= id => {}
            _ => {
                self.rust_fn_ids_by_addr.insert(addr, id.to_string());
            }
        }
        Ok(())
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
mod tests;
