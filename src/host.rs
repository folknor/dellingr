//! Host callbacks for customizing VM behavior.
//!
//! The game engine can implement the `HostCallbacks` trait to:
//! - Redirect `print()` output to a per-script console
//! - Get notified when errors occur
//!
//! # Example (Game Engine)
//!
//! ```
//! use dellingr::{ArgCount, HostCallbacks, Program, RetCount, State};
//!
//! struct GameCallbacks;
//!
//! impl HostCallbacks for GameCallbacks {
//!     fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, _message: &[u8]) {}
//! }
//!
//! let program = Program::compile("print('Hello!')", Some(b"hello.lua".as_slice())).unwrap();
//! let mut state = State::with_callbacks(Box::new(GameCallbacks));
//! state.load(&program).unwrap();
//! state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
//! ```

use crate::error::Error;

/// Trait for host-provided callbacks.
///
/// Implement this trait to customize how the VM interacts with your application.
/// [`DefaultCallbacks`] prints to stdout, suitable for CLI usage.
///
/// `Send` is required because `State` is `Send`; the host callbacks travel
/// with the State and must be safe to move across threads.
pub trait HostCallbacks: Send {
    /// Called when Lua's `print()` function is executed.
    ///
    /// Both byte slices are exact: Lua strings are bytes, so a script can
    /// print bytes that are not valid UTF-8, and the host receives them
    /// unchanged. Decide at the host how to display them.
    ///
    /// # Arguments
    /// * `source` - The active Lua frame's chunk name (`None` for direct
    ///   native calls and unnamed chunks)
    /// * `line` - The current line number (0 if unknown)
    /// * `message` - The rendered arguments, joined with tabs; no newline
    fn on_print(&mut self, source: Option<&[u8]>, line: u32, message: &[u8]);

    /// Called when execution fails. Use it for logging or telemetry.
    ///
    /// Each execution error is reported exactly once, before any host code
    /// can see it: before it is returned from `call()`, `call_anchor()`,
    /// `get_table()`, `tostring_bytes()` or `to_string_with_meta()`. That holds
    /// at every depth: when a Rust callback makes one of these calls and
    /// handles the error instead of returning it, `on_error` has already
    /// fired for it. Reporting follows the `Error` value: propagating the same
    /// error further, or returning it again later, does not report it again,
    /// while a different `Error` a callback builds from it is a new error.
    /// Where a Lua activation encloses the failure, the error carries the
    /// location, traceback and source name it is returned with; an error
    /// from a host-initiated call of a Rust function has no Lua context.
    ///
    /// Execution errors are those raised in Lua bytecode, by Rust functions
    /// (invoked from Lua or directly by the host), by limits and budgets, and
    /// in resolving and setting up a call: calling a non-callable value,
    /// `__call` chains (including cyclic chains that reach the metamethod
    /// depth limit, and argument insertion), Lua frame setup, result padding
    /// and the native call depth limit. A host table read or `tostring`
    /// rendering fails the same way: a receiver that cannot be indexed, any
    /// failure of `__index` resolution or `__tostring`, or a `__tostring`
    /// result that is not a string or number.
    ///
    /// It is NOT fired for host API misuse rejected before anything executes:
    /// `ArgCount::Dynamic`, insufficient stack values for a call, an invalid
    /// stack index, a missing `get_table` key, a wrong-State anchor, or no
    /// stack room to insert the anchored callee. Nor for a refusal of a
    /// poisoned State (`StatePoisoned`), or for `to_string_with_meta()`'s
    /// UTF-8 size check after rendering succeeded. The host observes those
    /// solely through the `Result`; a callback that returns one of them as its
    /// own failure reports it then, like any other callback error.
    ///
    /// Function labels in displayed argument errors are best-effort diagnostic
    /// text. Generic Rust callback checks can resolve a label from current
    /// globals or, with `snapshot`, a registered function id; aliases and build
    /// settings can affect the result. Do not treat that label as a stable id.
    ///
    /// # Arguments
    /// * `error` - The error that occurred (with source name and stack trace
    ///   where a Lua activation encloses it)
    fn on_error(&mut self, error: &Error) {
        // Default: no-op (host handles via Result)
        let _ = error;
    }
}

/// Default callbacks that print to stdout.
///
/// This is used when no custom callbacks are provided, suitable for CLI tools.
/// The message bytes are written unchanged, followed by a newline.
/// Since `on_print` cannot return an I/O error, a failed stdout write is ignored.
#[derive(Default)]
pub struct DefaultCallbacks;

impl HostCallbacks for DefaultCallbacks {
    // Printing to stdout is this callback's job; the library-wide stream ban
    // exists so that nothing else does.
    #[allow(clippy::disallowed_methods)]
    fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, message: &[u8]) {
        use std::io::Write;

        let mut stdout = std::io::stdout().lock();
        // Deliberately discarded: on_print has no error channel (see above).
        #[allow(clippy::let_underscore_must_use)]
        let _ = stdout
            .write_all(message)
            .and_then(|()| stdout.write_all(b"\n"));
    }
}
