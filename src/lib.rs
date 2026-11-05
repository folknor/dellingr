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
// banned in clippy.toml, with an `expect` on `DefaultCallbacks::on_print`.
#![deny(clippy::print_stdout, clippy::print_stderr)]

// The debug-only features are test instrumentation. Keep them out of release
// builds, where dependency feature unification would otherwise expose them to
// hosts.
#[cfg(all(feature = "fault-injection", not(debug_assertions)))]
compile_error!("the fault-injection feature requires debug assertions");
#[cfg(all(feature = "disable-inline-caches", not(debug_assertions)))]
compile_error!("the disable-inline-caches feature requires debug assertions");
#[cfg(all(feature = "cost-check-interval", not(debug_assertions)))]
compile_error!("the cost-check-interval feature requires debug assertions");
#[cfg(all(feature = "native-call-trace", not(debug_assertions)))]
compile_error!("the native-call-trace feature requires debug assertions");

// Every number a script sees is IEEE binary64 rounded once per operation: VM
// arithmetic, numeral parsing, number rendering and the `crmath` kernels all
// rely on it, and replays depend on the bits. x87 evaluation (x86 without
// SSE2, i586 and older) carries excess range and precision, so such targets
// are refused rather than built to diverge.
#[cfg(all(target_arch = "x86", not(target_feature = "sse2")))]
compile_error!(
    "dellingr needs SSE2 binary64 arithmetic on x86; x87-only targets (i586 and older) are not supported"
);

// With tracing disabled, preserve the call expression without adding a block,
// local, closure or guard to any frame that can stay live across re-entry.
/// Mark a native call edge for coverage tracing.
///
/// With `native-call-trace`, the edge becomes live before `$call` is evaluated,
/// including its argument expressions. Keep arguments free of calls that can
/// re-enter Lua, or that re-entry will be attributed to this edge. With tracing
/// disabled, this macro expands to the bare call expression.
#[cfg(not(feature = "native-call-trace"))]
#[doc(hidden)]
#[macro_export]
macro_rules! native_call {
    ($edge:literal, $call:expr $(,)?) => {
        $call
    };
}

/// Mark a native call edge for coverage tracing.
///
/// With `native-call-trace`, the edge becomes live before `$call` is evaluated,
/// including its argument expressions. Keep arguments free of calls that can
/// re-enter Lua, or that re-entry will be attributed to this edge. With tracing
/// disabled, this macro expands to the bare call expression.
#[cfg(feature = "native-call-trace")]
#[doc(hidden)]
#[macro_export]
macro_rules! native_call {
    ($edge:literal, $call:expr $(,)?) => {{
        let _edge = $crate::native_call_trace::record($edge);
        $call
    }};
}

// Public only because the exported macro reaches `record` through `$crate`;
// the test entry point is re-exported at the root beside the other hooks.
#[cfg(feature = "native-call-trace")]
#[doc(hidden)]
pub mod native_call_trace;

mod compiler;
mod cost_analysis;
mod cost_meter;
mod cost_model;
mod crmath;
mod det_hash;
mod disassemble;
mod fp_env;
mod gmatch_schema;
mod host;
mod instr;
pub mod limits;
mod lua_std;
mod numeral;
mod patterns;
mod saturate;
mod vm;
mod vm_aux;

/// The example corpus for the library's unit tests, from the checked-in list
/// the integration tests also read (`tests/common/examples.rs`).
#[cfg(test)]
mod example_list {
    /// A listed example: its repository-relative path and its source path.
    pub(crate) struct Example {
        pub(crate) relative: &'static str,
        pub(crate) path: &'static str,
    }

    macro_rules! examples {
        ($($name:ident: $path:literal,)*) => {
            /// Every listed example, in list order.
            pub(crate) const EXAMPLES: &[Example] = &[$(Example {
                relative: $path,
                path: concat!(env!("CARGO_MANIFEST_DIR"), "/", $path),
            },)*];
        };
    }

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/common/examples.rs"
    ));
}

/// Error types returned by the VM and parser. Surfaced through [`Result`].
pub mod error;

pub use cost_analysis::{CostAnalysis, ScopeCost};
pub use cost_meter::COST_MODEL_VERSION;
#[cfg(feature = "cost-check-interval")]
#[doc(hidden)]
pub use cost_meter::interval_test::with_cost_check_interval;
pub use cost_model::{Charge, CostModel, Costing};
pub use disassemble::Disassembly;
pub use host::{DefaultCallbacks, HostCallbacks};
pub use instr::{ArgCount, RetCount};
#[cfg(feature = "native-call-trace")]
#[doc(hidden)]
pub use native_call_trace::{active_path, is_observing, with_native_call_trace};
pub use vm::Anchor;
pub use vm::Interrupt;
pub use vm::LuaType;
pub use vm::RawField;
pub use vm::RustFnHandle;
pub use vm::RustFunc;
pub use vm::State;
pub use vm::StateBuilder;
#[cfg(feature = "fault-injection")]
#[doc(hidden)]
pub use vm::{FaultEvent, FaultSite};
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
        CorruptionReason, DecodeReason, DuplicateEntry, GmatchCorruption, InvalidBytecodeReason,
        KeyForm, LoadError, MaterializedPart, SaveCollection, SaveDiagnostics, SaveError,
        SaveInternalError, SaveState, SavedReference, SnapshotError, SnapshotField,
        TableCorruption, TableLoadLocation, TableRestoreFailure,
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
    // Triggered from another thread or a signal handler while the State runs.
    assert_send_sync::<Interrupt>();
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
    ///
    /// Compilation parses numerals and folds constant arithmetic, so an
    /// unsupported floating-point environment on the calling thread is
    /// refused with [`error::ErrorKind::UnsupportedFloatEnvironment`] before
    /// parsing starts (after `source.as_ref()`, which is caller code).
    pub fn compile(source: impl AsRef<[u8]>, name: Option<&[u8]>) -> Result<Program> {
        let source = source.as_ref();
        if fp_env::unsupported() {
            return Err(State::float_environment_error());
        }
        Ok(Program(Arc::new(compiler::compile(source, name)?)))
    }

    /// Statically analyze the cost of this program, without executing it.
    ///
    /// Returns a [`CostAnalysis`] with per-scope cost breakdown. The actual
    /// runtime cost depends on control flow and loop iterations.
    /// [`CostAnalysis::caveat`] returns the standard explanation of the
    /// estimate's limits for display in a host application.
    ///
    /// Costs are under the default [`CostModel`]; see
    /// [`Program::analyze_cost_with`].
    pub fn analyze_cost(&self) -> CostAnalysis {
        self.analyze_cost_with(&CostModel::default())
    }

    /// [`Program::analyze_cost`] under `model`, which the analysis records.
    /// Unit and instruction counts do not depend on the model; costs are
    /// derived from them under it, and saturate.
    pub fn analyze_cost_with(&self, model: &CostModel) -> CostAnalysis {
        // Deliberately no entry point that reprices an existing analysis
        // under another model: the walk is one linear pass over the bytecode,
        // the host keeps the Program, and a second pricing path would only
        // duplicate this one.
        CostAnalysis::analyze(&self.0, model)
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
        self.check_usable()?;
        self.push_chunk(Arc::clone(&program.0))?;
        Ok(())
    }
}
