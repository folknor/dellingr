//! A Lua VM designed for game scripting with cost budgets and host callbacks.
//!
//! # Features
//!
//! - **Cost budgets**: Control script execution with configurable operation costs
//! - **Host callbacks**: Redirect print output and receive error notifications (`on_error` observes failures; it cannot handle or suppress them)
//! - **Stack traces**: Detailed error messages with source locations
//!
//! # Example
//!
//! ```
//! use dellingr::{ArgCount, Program, RetCount, State};
//!
//! // Source and chunk name are bytes: Lua strings are bytes, and so is the
//! // source a script is written in.
//! let program = Program::compile("print('Hello!')", Some(b"hello.lua".as_slice())).unwrap();
//! let mut state = State::new();
//! state.load(&program).unwrap();
//! state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
//! ```

// The package-wide deny set lives in Cargo.toml. These attributes add library
// documentation warnings and two denies that are disabled for unit tests.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::float_cmp))]
#![warn(missing_docs)]
// Library output goes through `HostCallbacks`, never straight to a process
// stream. These catch the printing macros; `std::io::stdout`/`stderr` are
// banned in clippy.toml, with a scoped allow on `DefaultCallbacks::on_print`.
#![deny(clippy::print_stdout, clippy::print_stderr)]

// Fault injection is test instrumentation. Keep it out of release builds,
// where dependency feature unification would otherwise expose it to hosts.
#[cfg(all(feature = "fault-injection", not(debug_assertions)))]
compile_error!("the fault-injection feature requires debug assertions");

mod compiler;
mod cost_meter;
mod crmath;
mod det_hash;
mod disassemble;
mod gmatch_schema;
mod host;
mod instr;
pub mod limits;
mod lua_std;
mod numeral;
mod patterns;
mod vm;
mod vm_aux;

/// Error types returned by the VM and parser. Surfaced through [`Result`].
pub mod error;

pub use cost_meter::COST_MODEL_VERSION;
pub use disassemble::Disassembly;
pub use host::{DefaultCallbacks, HostCallbacks};
pub use instr::{ArgCount, RetCount};
pub use vm::Anchor;
pub use vm::LuaType;
pub use vm::RustFunc;
pub use vm::State;
#[cfg(feature = "snapshot")]
pub use vm::{LoadError, SaveDiagnostics, SaveError, SaveState, SnapshotError, TableLoadLocation};

/// Save and load (the `snapshot` feature): the result types and every typed
/// reason inside a [`SaveError`] or [`SnapshotError`]. The six main types are
/// also exported at the crate root.
#[cfg(feature = "snapshot")]
pub mod snapshot {
    pub use crate::compiler::verify::{
        BytecodeOperand, BytecodePool, CacheFamily, ValidationReason,
    };
    pub use crate::vm::{
        CorruptionReason, DecodeReason, DuplicateEntry, EnvironmentCorruption, GmatchCorruption,
        InvalidBytecodeReason, KeyForm, LoadError, MaterializedPart, SaveCollection,
        SaveDiagnostics, SaveError, SaveInternalError, SaveState, SavedReference, SnapshotError,
        SnapshotField, TableCorruption, TableLoadLocation, TableRestoreFailure, TableRestoreStep,
    };
}

use compiler::Bytecode;
use instr::Instr;
use std::sync::Arc;

// Compile-time witnesses for the thread-sharing contract. `State` can be
// moved across threads but is deliberately NOT `Sync`; cost-budgeted dispatch
// and bytecode-cache invariants only have well-defined semantics under
// exclusive access. `Program` can be shared across threads, and `Error`
// travels across them (its host-error payload is `Send + Sync`).
const _: () = {
    const fn assert_send<T: Send>() {}
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send::<State>();
    assert_send_sync::<Program>();
    assert_send_sync::<error::Error>();
    // A handle may be dropped on any thread; its release queue is a Mutex.
    assert_send_sync::<Anchor>();
};

/// Custom result type for evaluating Lua.
pub type Result<T> = std::result::Result<T, error::Error>;

/// Shared text used by the command-line interface and library hosts.
///
/// Hosts can pair this warning prefix with [`CostAnalysis::caveat`] when
/// presenting a static analysis.
pub mod cli {
    /// Prefix used for the static analysis caveat.
    pub const ANALYSIS_WARNING_PREFIX: &str = "Warning: ";
}

/// Cost breakdown for a single scope (function or main chunk).
/// Category counts are available through the corresponding accessor methods.
#[derive(Debug, Default, Clone)]
pub struct ScopeCost {
    /// Name of this scope
    pub name: String,
    /// Static cost of this scope's own opcodes, each counted once at its
    /// occurrence in the bytecode.
    pub own_cost: u64,
    /// `own_cost` plus the `total_cost` of every nested function body, each
    /// counted once regardless of whether that function is invoked.
    pub total_cost: u64,
    // Category counters intentionally use one private array; use the accessors
    // below. This keeps analysis, aggregation, and rendering on the same index.
    category_counts: [u64; instr::OpcodeCost::COUNT],
    /// Total instruction count
    pub instructions: u64,
    /// Nested scopes (functions defined in this scope)
    pub nested: Vec<ScopeCost>,
}

impl ScopeCost {
    /// Number of arithmetic operations (+, -, *, /, %, ^).
    pub fn arithmetic_ops(&self) -> u64 {
        self.category_count(instr::OpcodeCost::Arithmetic)
    }

    /// Number of unary negation operations.
    pub fn negations(&self) -> u64 {
        self.category_count(instr::OpcodeCost::Negation)
    }

    /// Number of table creations ({}).
    pub fn table_creations(&self) -> u64 {
        self.category_count(instr::OpcodeCost::TableCreation)
    }

    /// Number of table field writes (`t.x = v`, `t[k] = v`).
    pub fn table_writes(&self) -> u64 {
        self.category_count(instr::OpcodeCost::TableWrite)
    }

    /// Number of array elements initialized.
    pub fn array_elements(&self) -> u64 {
        self.category_count(instr::OpcodeCost::ArrayElements)
    }

    /// Number of call instructions (`OP_CALL`). Generic-`for` iterator calls
    /// and metamethod-driven calls are not call instructions and are not
    /// counted.
    pub fn function_calls(&self) -> u64 {
        self.category_count(instr::OpcodeCost::Call)
    }

    fn category_count(&self, category: instr::OpcodeCost) -> u64 {
        category
            .counter_index()
            .map_or(0, |index| self.category_counts[index])
    }

    pub(crate) fn analyze_chunk(chunk: &Bytecode, name: String) -> Self {
        let mut scope = ScopeCost {
            name,
            ..Default::default()
        };

        for inst in &chunk.code {
            scope.instructions += 1;
            scope.own_cost += inst.static_units();
            let category = Instr::cost_class(inst.opcode());
            if let Some(index) = category.counter_index() {
                let count = match category {
                    instr::OpcodeCost::ArrayElements => inst.set_list_count().map_or(0, u64::from),
                    _ => 1,
                };
                scope.category_counts[index] += count;
            }
        }

        // Recursively analyze nested functions
        // Anonymous functions are numbered among themselves, so a named
        // function between two anonymous ones does not skew the numbering.
        let mut anonymous_count = 0usize;
        for nested_chunk in &chunk.nested {
            let nested_name = match &nested_chunk.name {
                Some(name) => name.to_string(),
                None => {
                    anonymous_count += 1;
                    format!("anonymous #{anonymous_count}")
                }
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

        if self.own_cost == 0 && self.nested.is_empty() && self.function_calls() == 0 {
            // Open SET_LIST constructors still pay for table creation. TFOR_CALL
            // is not an OP_CALL but can invoke a costed iterator, so zero here
            // describes static cost only.
            writeln!(f, "{}{}: cost 0 (static)", pad, self.name)?;
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

        // Calls are statically free but their instruction count still belongs
        // in the per-scope breakdown.
        if self.own_cost > 0 || self.function_calls() > 0 {
            let inner_pad = "  ".repeat(indent + 1);
            for labels in &instr::OPCODE_COST_LABELS {
                let index = labels.category as usize;
                let count = self.category_counts[index];
                if count > 0 {
                    writeln!(f, "{}{}: {}", inner_pad, labels.scope_label, count)?;
                }
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
    /// Return the standard explanation of the limits of this static estimate.
    ///
    /// The command-line interface prints this text after its analysis report.
    pub const fn caveat() -> &'static str {
        concat!(
            "Static cost analysis counts each costed opcode once across the main chunk and every nested function body, ",
            "whether or not that body runs. Loops and branches count once. ",
            "Call opcodes have no static charge, and the analyzer does not inspect their callees, ",
            "so it excludes all native library charges, flat and data-dependent. It is not a runtime-cost bound or a native-cost estimate."
        )
    }

    /// Collect totals across all scopes
    fn totals(&self) -> ScopeTotals {
        // analyze_chunk already computed the recursive total for the root.
        // Reuse it while accumulating the remaining display statistics.
        let mut totals = ScopeTotals {
            total_cost: self.root.total_cost,
            ..ScopeTotals::default()
        };
        self.root.accumulate(&mut totals);
        totals
    }
}

#[derive(Default)]
struct ScopeTotals {
    total_cost: u64,
    category_counts: [u64; instr::OpcodeCost::COUNT],
    instructions: u64,
    function_count: u64,
}

impl ScopeCost {
    fn accumulate(&self, totals: &mut ScopeTotals) {
        for (total, count) in totals.category_counts.iter_mut().zip(&self.category_counts) {
            *total += count;
        }
        totals.instructions += self.instructions;
        totals.function_count += self.nested.len() as u64;
        for nested in &self.nested {
            nested.accumulate(totals);
        }
    }
}

fn write_summary_categories(
    f: &mut std::fmt::Formatter<'_>,
    counts: &[u64; instr::OpcodeCost::COUNT],
    section: instr::CostSummarySection,
) -> std::fmt::Result {
    for labels in &instr::OPCODE_COST_LABELS {
        if labels.summary_section != section {
            continue;
        }
        let index = labels.category as usize;
        let count = counts[index];
        if count > 0 || labels.summary_when_zero {
            writeln!(
                f,
                "{}{}{}",
                labels.summary_prefix, count, labels.summary_suffix
            )?;
        }
    }
    Ok(())
}

impl std::fmt::Display for CostAnalysis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let totals = self.totals();

        writeln!(f, "=== Cost Analysis ===")?;
        writeln!(f)?;
        writeln!(f, "Static cost estimate: {}", totals.total_cost)?;
        writeln!(f)?;
        writeln!(f, "--- Costed Operations ---")?;
        write_summary_categories(
            f,
            &totals.category_counts,
            instr::CostSummarySection::CostedOperations,
        )?;
        writeln!(f)?;
        writeln!(f, "--- Statistics ---")?;
        writeln!(f, "  Total instructions:   {}", totals.instructions)?;
        writeln!(f, "  Function definitions: {}", totals.function_count)?;
        write_summary_categories(
            f,
            &totals.category_counts,
            instr::CostSummarySection::Statistics,
        )?;
        writeln!(f)?;
        writeln!(f, "--- Per-Scope Breakdown ---")?;
        self.root.fmt_indent(f, 0)?;
        Ok(())
    }
}

/// A compiled, executable Lua program. Cheap to clone (refcounted) and safe
/// to share across threads: compile once, then [`State::load`] it into as
/// many `State`s as needed, on any thread.
///
/// ```
/// use dellingr::{ArgCount, Program, RetCount, State};
/// let program = Program::compile("return 1 + 2", None).unwrap();
///
/// // Load the same compiled program into a State for each worker.
/// let mut state = State::new();
/// state.load(&program).unwrap();
/// state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
/// ```
#[derive(Clone, Debug)]
pub struct Program(Arc<Bytecode>);

impl Program {
    /// Compile Lua source into an executable `Program`. This is the only way
    /// to compile Lua.
    ///
    /// `source` is bytes, as Lua strings are: string literals and comments may
    /// hold any bytes, and a literal keeps them exactly. `name` is the chunk
    /// name that error messages, tracebacks and `print` locations report
    /// (typically a file name); it is byte-exact too, kept and handed back to
    /// the host exactly as given, and rendered lossily only by `Display`.
    /// `None` compiles an unnamed chunk, whose locations render without a
    /// name. Names exceeding [`limits::MAX_SOURCE_NAME_BYTES`] are rejected
    /// before the source is examined.
    pub fn compile(source: impl AsRef<[u8]>, name: Option<&[u8]>) -> Result<Program> {
        Ok(Program(Arc::new(compiler::compile(source, name)?)))
    }

    /// Statically analyze the cost of this program, without executing it.
    ///
    /// Returns a `CostAnalysis` with per-scope cost breakdown. The actual
    /// runtime cost depends on control flow and loop iterations.
    /// [`CostAnalysis::caveat`] returns the standard explanation of the
    /// estimate's limits for display in a host application.
    pub fn analyze_cost(&self) -> CostAnalysis {
        CostAnalysis {
            root: ScopeCost::analyze_chunk(&self.0, "main".to_string()),
        }
    }

    /// The chunk name given at compile time, byte-exact.
    pub fn source_name(&self) -> Option<&[u8]> {
        self.0.source.as_deref()
    }

    /// A human-readable listing of the compiled bytecode: every function
    /// (`main`, then nested functions as `main/0`, `main/0/1`, ... by their
    /// `Closure` index) with its literal pools, cache-slot counts and one
    /// annotated line per instruction.
    ///
    /// This is the finalized bytecode that executes. The text is deterministic
    /// for a given compiled program but is diagnostic only: its format may
    /// change between versions and it is not replay data.
    pub fn disassemble(&self) -> Disassembly<'_> {
        Disassembly::new(&self.0)
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
        self.check_poisoned()?;
        self.push_chunk(Arc::clone(&program.0))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze_cost(source: &str) -> Result<CostAnalysis> {
        Program::compile(source, None).map(|program| program.analyze_cost())
    }

    #[test]
    fn dynamic_set_list_has_zero_minimum_cost_when_empty() {
        let analysis = analyze_cost("local function f(...) return {...} end\nf()")
            .expect("variadic table constructor should parse");
        let nested = &analysis.root.nested[0];

        assert_eq!(nested.own_cost, 1);
        assert_eq!(nested.array_elements(), 0);
        assert_eq!(analysis.root.total_cost, 1);

        let mut state = State::new();
        crate::Program::compile("local function f(...) return {...} end\nf()", None)
            .and_then(|program| state.load(&program))
            .expect("variadic table constructor should load");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("no-argument variadic call should run");
        assert_eq!(state.cost_used(), 1);

        let fixed = analyze_cost("return {1, 2}").expect("fixed table constructor should parse");
        assert_eq!(fixed.root.own_cost, 3);
        assert_eq!(fixed.root.array_elements(), 2);
    }

    // Keep this as a tripwire against the old "(free)" label returning.
    #[test]
    fn zero_static_cost_generic_for_is_not_labeled_free() {
        let analysis = analyze_cost("for value in iterator do end")
            .expect("generic-for iterator program should parse");
        assert_eq!(analysis.root.own_cost, 0);
        assert_eq!(analysis.root.function_calls(), 0);

        let rendered = analysis.to_string();
        assert!(rendered.contains("main: cost 0 (static)"), "{rendered}");
        assert!(!rendered.contains("(free)"), "{rendered}");
    }

    #[test]
    fn cost_analysis_display_preserves_category_report_bytes() {
        let analysis = CostAnalysis {
            root: ScopeCost {
                name: "main".to_string(),
                own_cost: 6,
                total_cost: 6,
                category_counts: [1, 1, 1, 1, 2, 1],
                instructions: 7,
                nested: Vec::new(),
            },
        };

        assert_eq!(
            analysis.to_string(),
            concat!(
                "=== Cost Analysis ===\n",
                "\n",
                "Static cost estimate: 6\n",
                "\n",
                "--- Costed Operations ---\n",
                "  Arithmetic (+,-,*,/,%,^): 1 ops\n",
                "  Unary negation (-):       1 ops\n",
                "  Table creation {}:        1 ops\n",
                "  Table writes (t[k]=v):    1 ops\n",
                "  Array elements:           2 elements\n",
                "\n",
                "--- Statistics ---\n",
                "  Total instructions:   7\n",
                "  Function definitions: 0\n",
                "  Call instructions:    1\n",
                "\n",
                "--- Per-Scope Breakdown ---\n",
                "main: cost 6\n",
                "  arithmetic: 1\n",
                "  negation: 1\n",
                "  table creation: 1\n",
                "  table writes: 1\n",
                "  array elements: 2\n",
                "  call instructions: 1\n",
            )
        );
    }

    #[test]
    fn static_analysis_excludes_flat_and_data_dependent_native_charges() {
        let source = "math.floor(1.5); table.pack(1); string.upper('abcd')";
        let analysis = analyze_cost(source).expect("native-call program should parse");
        assert_eq!(analysis.root.total_cost, 0);

        let mut state = State::new();
        crate::Program::compile(source, None)
            .and_then(|program| state.load(&program))
            .expect("native-call program should load");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("native-call program should run");
        assert_eq!(state.cost_used(), 6);
    }

    #[test]
    fn static_analysis_counts_costed_loop_body_once() {
        let closure_free = analyze_cost(
            "local x = 0; for i = 1, 3 do do local y = i; x = x + y end end; return x",
        )
        .expect("closure-free loop should parse");
        // Static analysis counts the loop body's single add once; the
        // runtime sweep below observes it three times.
        assert_eq!(closure_free.root.own_cost, 1);
        assert_eq!(closure_free.root.total_cost, 1);

        let dynamic = analyze_cost("local function f(...) return ... end; return f(1, f())")
            .expect("dynamic-call program should parse");
        assert_eq!(dynamic.root.own_cost, 0);
        assert_eq!(dynamic.root.total_cost, 0);

        let capture = analyze_cost(
            "local function make() local x = 1; return function() return x end end; return make()()",
        )
        .expect("closure-capture program should parse");
        assert_eq!(capture.root.own_cost, 0);
        assert_eq!(capture.root.total_cost, 0);

        for (source, returns, expected_cost) in [
            (
                "local x = 0; for i = 1, 3 do do local y = i; x = x + y end end; return x",
                RetCount::Fixed(1),
                3,
            ),
            (
                "local function f(...) return ... end; return f(1, f())",
                RetCount::All,
                0,
            ),
            (
                "local function make() local x = 1; return function() return x end end; return make()()",
                RetCount::Fixed(1),
                0,
            ),
        ] {
            let mut state = State::new();
            crate::Program::compile(source, None)
                .and_then(|program| state.load(&program))
                .expect("cost fixture should load");
            state
                .call(ArgCount::Fixed(0), returns)
                .expect("cost fixture should run");
            assert_eq!(state.cost_used(), expected_cost, "{source}");
        }
    }
}
