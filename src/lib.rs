//! A Lua VM designed for game scripting with cost budgets and host callbacks.
//!
//! # Features
//!
//! - **Cost budgets**: Control script execution with configurable operation costs
//! - **Host callbacks**: Redirect print output and handle errors
//! - **Stack traces**: Detailed error messages with source locations
//!
//! # Example
//!
//! ```
//! use dellingr::{State, ArgCount, RetCount};
//!
//! let mut state = State::new();
//! state.load_string("print('Hello!')").unwrap();
//! state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
//! ```

#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::float_cmp))]
#![warn(missing_docs)]

mod compiler;
mod host;
mod instr;
mod lua_std;
mod numeral;
#[doc(hidden)]
mod patterns;
mod vm;
mod vm_aux;

/// Error types returned by the VM and parser. Surfaced through [`Result`].
pub mod error;

pub use host::{DefaultCallbacks, HostCallbacks};
pub use instr::{ArgCount, RetCount};
pub use vm::Anchor;
pub use vm::LuaType;
pub use vm::RustFunc;
pub use vm::State;
#[cfg(feature = "snapshot")]
pub use vm::{LoadError, SaveDiagnostics, SaveError, SaveState};

use compiler::Bytecode;
use instr::Instr;
use std::sync::Arc;

// Compile-time witness that `State` can be moved across threads. This is the
// load-bearing property for sharing dellingr with multi-threaded async
// runtimes (axum, tokio worker pools): embedders can hold a `Mutex<State>`
// behind an `Arc` and dispatch calls from any worker. `State` is
// deliberately NOT `Sync` - cost-budgeted dispatch and the bytecode cache
// invariants only have well-defined semantics under exclusive access.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<State>();
};

/// Custom result type for evaluating Lua.
pub type Result<T> = std::result::Result<T, error::Error>;

/// Cost breakdown for a single scope (function or main chunk).
#[derive(Debug, Default, Clone)]
pub struct ScopeCost {
    /// Name of this scope
    pub name: String,
    /// Minimum cost of this scope alone (not including nested)
    pub own_cost: u64,
    /// Total cost including all nested scopes
    pub total_cost: u64,
    /// Number of arithmetic operations (+, -, *, /, %, ^)
    pub arithmetic_ops: u64,
    /// Number of unary negation operations
    pub negations: u64,
    /// Number of table creations ({})
    pub table_creations: u64,
    /// Number of table field writes (`t.x = v`, `t[k] = v`)
    pub table_writes: u64,
    /// Number of array elements initialized
    pub array_elements: u64,
    /// Number of function calls
    pub function_calls: u64,
    /// Total instruction count
    pub instructions: u64,
    /// Nested scopes (functions defined in this scope)
    pub nested: Vec<ScopeCost>,
}

impl ScopeCost {
    fn analyze_chunk(chunk: &Bytecode, name: String) -> Self {
        let mut scope = ScopeCost {
            name,
            ..Default::default()
        };

        for inst in &chunk.code {
            scope.instructions += 1;
            match inst.opcode() {
                // Arithmetic (cost 1 each)
                Instr::OP_ADD
                | Instr::OP_SUBTRACT
                | Instr::OP_MULTIPLY
                | Instr::OP_DIVIDE
                | Instr::OP_POW
                | Instr::OP_MOD => {
                    scope.arithmetic_ops += 1;
                    scope.own_cost += 1;
                }
                // Unary negation (cost 1)
                Instr::OP_NEGATE => {
                    scope.negations += 1;
                    scope.own_cost += 1;
                }
                // Table creation (cost 1)
                Instr::OP_NEW_TABLE
                | Instr::OP_NEW_TABLE_PRESIZED
                | Instr::OP_NEW_TABLE_TEMPLATE
                | Instr::OP_NEW_TABLE_TRACKED => {
                    scope.table_creations += 1;
                    scope.own_cost += 1;
                }
                // Table writes (cost 1 each)
                Instr::OP_INIT_FIELD
                | Instr::OP_INIT_FIELD_PINNED
                | Instr::OP_INIT_INDEX
                | Instr::OP_SET_FIELD
                | Instr::OP_SET_TABLE => {
                    scope.table_writes += 1;
                    scope.own_cost += 1;
                }
                // Array initialization (cost = n elements)
                Instr::OP_SET_LIST => {
                    let n = u64::from(inst.a());
                    scope.array_elements += n;
                    scope.own_cost += n;
                }
                // Function calls
                Instr::OP_CALL => {
                    scope.function_calls += 1;
                }
                _ => {}
            }
        }

        // Recursively analyze nested functions
        for (i, nested_chunk) in chunk.nested.iter().enumerate() {
            let nested_name = match &nested_chunk.name {
                Some(name) => name.clone(),
                None => format!("anonymous #{}", i + 1),
            };
            let nested_scope = Self::analyze_chunk(nested_chunk, nested_name);
            scope.nested.push(nested_scope);
        }

        // Calculate total cost (own + all nested)
        scope.total_cost = scope.own_cost + scope.nested.iter().map(|n| n.total_cost).sum::<u64>();

        scope
    }

    fn fmt_indent(&self, f: &mut std::fmt::Formatter<'_>, indent: usize) -> std::fmt::Result {
        let pad = "  ".repeat(indent);

        if self.own_cost == 0 && self.nested.is_empty() {
            writeln!(f, "{}{}: cost 0 (free)", pad, self.name)?;
            return Ok(());
        }

        // Header with cost
        if self.nested.is_empty() {
            writeln!(f, "{}{}: cost {}", pad, self.name, self.own_cost)?;
        } else {
            writeln!(
                f,
                "{}{}: cost {} (own) / {} (total)",
                pad, self.name, self.own_cost, self.total_cost
            )?;
        }

        // Breakdown if there's any cost
        if self.own_cost > 0 {
            let inner_pad = "  ".repeat(indent + 1);
            if self.arithmetic_ops > 0 {
                writeln!(f, "{}arithmetic: {}", inner_pad, self.arithmetic_ops)?;
            }
            if self.negations > 0 {
                writeln!(f, "{}negation: {}", inner_pad, self.negations)?;
            }
            if self.table_creations > 0 {
                writeln!(f, "{}table creation: {}", inner_pad, self.table_creations)?;
            }
            if self.table_writes > 0 {
                writeln!(f, "{}table writes: {}", inner_pad, self.table_writes)?;
            }
            if self.array_elements > 0 {
                writeln!(f, "{}array elements: {}", inner_pad, self.array_elements)?;
            }
        }

        // Nested scopes
        for nested in &self.nested {
            nested.fmt_indent(f, indent + 1)?;
        }

        Ok(())
    }
}

/// Static cost analysis of a Lua script.
///
/// This analyzes the bytecode without executing it. The actual runtime cost
/// depends on which code paths are taken and how many loop iterations occur.
#[derive(Debug, Default)]
pub struct CostAnalysis {
    /// Root scope (main chunk)
    pub root: ScopeCost,
}

impl CostAnalysis {
    /// Collect totals across all scopes
    fn totals(&self) -> ScopeTotals {
        let mut totals = ScopeTotals::default();
        self.root.accumulate(&mut totals);
        totals
    }
}

#[derive(Default)]
struct ScopeTotals {
    total_cost: u64,
    arithmetic_ops: u64,
    negations: u64,
    table_creations: u64,
    table_writes: u64,
    array_elements: u64,
    function_calls: u64,
    instructions: u64,
    function_count: u64,
}

impl ScopeCost {
    fn accumulate(&self, totals: &mut ScopeTotals) {
        totals.total_cost += self.own_cost;
        totals.arithmetic_ops += self.arithmetic_ops;
        totals.negations += self.negations;
        totals.table_creations += self.table_creations;
        totals.table_writes += self.table_writes;
        totals.array_elements += self.array_elements;
        totals.function_calls += self.function_calls;
        totals.instructions += self.instructions;
        totals.function_count += self.nested.len() as u64;
        for nested in &self.nested {
            nested.accumulate(totals);
        }
    }
}

impl std::fmt::Display for CostAnalysis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let totals = self.totals();

        writeln!(f, "=== Cost Analysis ===")?;
        writeln!(f)?;
        writeln!(f, "Minimum cost (static): {}", totals.total_cost)?;
        writeln!(f)?;
        writeln!(f, "--- Costed Operations ---")?;
        if totals.arithmetic_ops > 0 {
            writeln!(
                f,
                "  Arithmetic (+,-,*,/,%,^): {} ops",
                totals.arithmetic_ops
            )?;
        }
        if totals.negations > 0 {
            writeln!(f, "  Unary negation (-):       {} ops", totals.negations)?;
        }
        if totals.table_creations > 0 {
            writeln!(
                f,
                "  Table creation {{}}:        {} ops",
                totals.table_creations
            )?;
        }
        if totals.table_writes > 0 {
            writeln!(f, "  Table writes (t[k]=v):    {} ops", totals.table_writes)?;
        }
        if totals.array_elements > 0 {
            writeln!(
                f,
                "  Array elements:           {} elements",
                totals.array_elements
            )?;
        }
        writeln!(f)?;
        writeln!(f, "--- Statistics ---")?;
        writeln!(f, "  Total instructions:   {}", totals.instructions)?;
        writeln!(f, "  Function definitions: {}", totals.function_count)?;
        writeln!(f, "  Function calls:       {}", totals.function_calls)?;
        writeln!(f)?;
        writeln!(f, "--- Per-Scope Breakdown ---")?;
        self.root.fmt_indent(f, 0)?;
        Ok(())
    }
}

/// Analyze the cost of a Lua script without executing it.
///
/// Returns a `CostAnalysis` with per-scope cost breakdown.
/// The actual runtime cost depends on control flow and loop iterations.
///
/// For repeated analysis of the same source, prefer `Engine::compile` followed
/// by `Engine::analyze_cost(&program)` so the parse is paid once.
pub fn analyze_cost(source: &str) -> Result<CostAnalysis> {
    let bc = compiler::parse_str(source)?;
    let root = ScopeCost::analyze_chunk(&bc, "main".to_string());
    Ok(CostAnalysis { root })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_set_list_has_zero_minimum_cost_when_empty() {
        let analysis = analyze_cost("local function f(...) return {...} end\nf()")
            .expect("variadic table constructor should parse");
        let nested = &analysis.root.nested[0];

        assert_eq!(nested.own_cost, 1);
        assert_eq!(nested.array_elements, 0);
        assert_eq!(analysis.root.total_cost, 1);

        let mut state = State::new();
        state
            .load_string("local function f(...) return {...} end\nf()")
            .expect("variadic table constructor should load");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("no-argument variadic call should run");
        assert_eq!(state.cost_used(), 1);

        let fixed = analyze_cost("return {1, 2}").expect("fixed table constructor should parse");
        assert_eq!(fixed.root.own_cost, 3);
        assert_eq!(fixed.root.array_elements, 2);
    }
}

/// A factory for compiling Lua source and creating new `State`s.
///
/// `Engine` is `Send + Sync`: a single instance can be shared across worker
/// threads via `Arc`. Compile a `Program` once on the engine, then load it
/// into per-thread (or per-request) `State`s.
///
/// ```ignore
/// use std::sync::Arc;
/// let engine = Arc::new(dellingr::Engine::new());
/// let program = engine.compile("return 1 + 2").unwrap();
///
/// // On each worker thread:
/// let mut state = engine.new_state();
/// state.load(&program).unwrap();
/// state.call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(1)).unwrap();
/// ```
#[derive(Debug)]
pub struct Engine {
    install_stdlib: bool,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// Create an engine that installs the standard library on each new `State`.
    pub fn new() -> Self {
        Self {
            install_stdlib: true,
        }
    }

    /// Create an engine whose new `State`s start with empty global namespaces,
    /// matching `lua_newstate` in the reference C API.
    pub fn raw() -> Self {
        Self {
            install_stdlib: false,
        }
    }

    /// Compile a Lua source string into an executable `Program`.
    pub fn compile(&self, source: &str) -> Result<Program> {
        let bc = compiler::parse_str(source)?;
        Ok(Program(Arc::new(bc)))
    }

    /// Compile a Lua source string with a source name used in error messages
    /// and stack traces (e.g. a filename or `"[fleet:123]"`).
    pub fn compile_named(&self, source: &str, name: impl Into<String>) -> Result<Program> {
        let bc = compiler::parse_str_named(source, Some(name.into()))?;
        Ok(Program(Arc::new(bc)))
    }

    /// Statically analyze the cost of a compiled `Program`. No execution.
    pub fn analyze_cost(&self, program: &Program) -> CostAnalysis {
        let root = ScopeCost::analyze_chunk(&program.0, "main".to_string());
        CostAnalysis { root }
    }

    /// Create a new `State` configured by this engine.
    pub fn new_state(&self) -> State {
        self.new_state_with_callbacks(Box::new(DefaultCallbacks))
    }

    /// Create a new `State` configured by this engine with custom callbacks.
    pub fn new_state_with_callbacks(&self, callbacks: Box<dyn HostCallbacks + Send>) -> State {
        if self.install_stdlib {
            State::with_callbacks(callbacks)
        } else {
            State::empty_with_callbacks(callbacks)
        }
    }
}

/// A compiled, executable Lua program. Cheap to clone (refcounted) and safe
/// to share across threads. Load with `State::load` to execute.
#[derive(Clone, Debug)]
pub struct Program(Arc<Bytecode>);

impl Program {
    /// Returns the optional source name attached at compile time.
    pub fn source_name(&self) -> Option<&str> {
        self.0.source.as_deref()
    }
}

impl State {
    /// Load a compiled `Program` onto this `State`'s stack as a callable
    /// closure. Pair with `state.call(ArgCount::Fixed(0), ...)` to execute.
    ///
    /// The same `Program` can be loaded into many different `State`s and run
    /// concurrently from different threads (each State holds its own caches
    /// and heap; only the immutable bytecode is shared).
    pub fn load(&mut self, program: &Program) -> Result<()> {
        // Mirror load_string_named: track the source for callback context.
        self.current_source = program.0.source.clone();
        self.push_chunk(Arc::clone(&program.0));
        Ok(())
    }
}
