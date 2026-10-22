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

    /// Called when an error is raised while executing code reached through
    /// `call()`. Fired exactly once per error, in addition to that error being
    /// returned from `call()`. Use it for logging or telemetry.
    ///
    /// This covers errors raised in Lua bytecode and failures of Rust functions
    /// invoked from Lua, as well as a Rust function that fails when a host calls
    /// it directly (with no Lua frame on the stack). Errors from host-initiated
    /// table `__call` chains are covered too, including cyclic chains that reach
    /// the metamethod depth limit, and direct calls of non-callable values.
    /// It is NOT fired for host API misuse that `call()` or `call_anchor()`
    /// rejects before dispatch, when nothing has executed: `ArgCount::Dynamic`,
    /// insufficient stack values for the call, a wrong-State anchor, or no stack
    /// room to insert the anchored callee. The host observes those solely
    /// through the `Result`. Once dispatch begins, setup failures (Lua frame
    /// setup, `__call` argument insertion, result padding) do notify.
    ///
    /// Function labels in displayed argument errors are best-effort diagnostic
    /// text. Generic Rust callback checks can resolve a label from current
    /// globals or, with `snapshot`, a registered function id; aliases and build
    /// settings can affect the result. Do not treat that label as a stable id.
    ///
    /// # Arguments
    /// * `error` - The error that occurred (includes source name and stack trace)
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
