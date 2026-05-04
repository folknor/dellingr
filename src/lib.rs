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

#![warn(future_incompatible)]
#![warn(non_ascii_idents)]
#![warn(rust_2018_idioms)]
#![warn(single_use_lifetimes)]
#![warn(trivial_casts)]
#![warn(trivial_numeric_casts)]
#![warn(unreachable_pub)]
#![warn(unused)]
#![warn(variant_size_differences)]
#![deny(clippy::disallowed_methods)]
// Style & correctness lints
#![deny(clippy::uninlined_format_args)]
#![deny(clippy::cloned_instead_of_copied)]
#![deny(clippy::redundant_closure_for_method_calls)]
#![deny(clippy::semicolon_if_nothing_returned)]
#![deny(clippy::manual_string_new)]
#![deny(clippy::explicit_iter_loop)]
#![deny(clippy::implicit_clone)]
#![deny(clippy::needless_pass_by_value)]
#![deny(clippy::unnecessary_sort_by)]
#![deny(clippy::collapsible_match)]
#![deny(clippy::if_same_then_else)]
#![deny(clippy::needless_borrow)]
#![deny(clippy::ptr_arg)]
#![deny(clippy::redundant_locals)]
#![deny(clippy::regex_creation_in_loops)]
#![deny(clippy::unnecessary_unwrap)]
#![deny(clippy::wrong_self_convention)]
#![deny(clippy::let_underscore_must_use)]
#![deny(clippy::unwrap_in_result)]
#![deny(clippy::ok_expect)]
#![deny(clippy::too_many_arguments)]
#![deny(clippy::large_enum_variant)]
#![deny(clippy::match_same_arms)]
#![deny(clippy::redundant_else)]
#![deny(clippy::unnested_or_patterns)]
#![deny(clippy::map_unwrap_or)]

mod compiler;
mod host;
mod instr;
mod lua_std;
mod vm;
mod vm_aux;

pub mod error;

pub use host::{DefaultCallbacks, HostCallbacks};
pub use instr::{ArgCount, Builtin, RetCount};
pub use vm::LuaType;
pub use vm::RustFunc;
pub use vm::State;

use compiler::Chunk;
use instr::Instr;

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
    /// Number of table field writes (t.x = v, t[k] = v)
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
    fn analyze_chunk(chunk: &Chunk, name: String) -> Self {
        let mut scope = ScopeCost {
            name,
            ..Default::default()
        };

        for inst in &chunk.code {
            scope.instructions += 1;
            match inst.opcode() {
                // Arithmetic (cost 1 each)
                Instr::OP_ADD | Instr::OP_SUBTRACT | Instr::OP_MULTIPLY |
                Instr::OP_DIVIDE | Instr::OP_POW | Instr::OP_MOD => {
                    scope.arithmetic_ops += 1;
                    scope.own_cost += 1;
                }
                // Unary negation (cost 1)
                Instr::OP_NEGATE => {
                    scope.negations += 1;
                    scope.own_cost += 1;
                }
                // Table creation (cost 1)
                Instr::OP_NEW_TABLE => {
                    scope.table_creations += 1;
                    scope.own_cost += 1;
                }
                // Table writes (cost 1 each)
                Instr::OP_INIT_FIELD | Instr::OP_INIT_INDEX |
                Instr::OP_SET_FIELD | Instr::OP_SET_TABLE => {
                    scope.table_writes += 1;
                    scope.own_cost += 1;
                }
                // Array initialization (cost = n elements)
                Instr::OP_SET_LIST => {
                    let n = inst.a();
                    if n == 0 {
                        scope.own_cost += 1;
                    } else {
                        scope.array_elements += n as u64;
                        scope.own_cost += n as u64;
                    }
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
            writeln!(f, "{}{}: cost {} (own) / {} (total)",
                     pad, self.name, self.own_cost, self.total_cost)?;
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
            writeln!(f, "  Arithmetic (+,-,*,/,%,^): {} ops", totals.arithmetic_ops)?;
        }
        if totals.negations > 0 {
            writeln!(f, "  Unary negation (-):       {} ops", totals.negations)?;
        }
        if totals.table_creations > 0 {
            writeln!(f, "  Table creation {{}}:        {} ops", totals.table_creations)?;
        }
        if totals.table_writes > 0 {
            writeln!(f, "  Table writes (t[k]=v):    {} ops", totals.table_writes)?;
        }
        if totals.array_elements > 0 {
            writeln!(f, "  Array elements:           {} elements", totals.array_elements)?;
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
pub fn analyze_cost(source: &str) -> Result<CostAnalysis> {
    let chunk = compiler::parse_str(source)?;
    let root = ScopeCost::analyze_chunk(&chunk, "main".to_string());
    Ok(CostAnalysis { root })
}
