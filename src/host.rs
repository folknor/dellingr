//! Host callbacks for customizing VM behavior.
//!
//! The game engine can implement the `HostCallbacks` trait to:
//! - Redirect `print()` output to a per-script console
//! - Get notified when errors occur
//!
//! # Example (Game Engine)
//!
//! ```
//! use dellingr::{ArgCount, Engine, HostCallbacks, RetCount};
//!
//! struct GameCallbacks;
//!
//! impl HostCallbacks for GameCallbacks {
//!     fn on_print(&mut self, _source: Option<&str>, _line: u32, _message: &str) {}
//! }
//!
//! let engine = Engine::new();
//! let mut state = engine.new_state_with_callbacks(Box::new(GameCallbacks));
//! state.load_string("print('Hello!')").unwrap();
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
    /// # Arguments
    /// * `source` - The active Lua frame source name (`None` for direct native calls)
    /// * `line` - The current line number (0 if unknown)
    /// * `message` - The formatted message (all arguments joined with tabs)
    fn on_print(&mut self, source: Option<&str>, line: u32, message: &str);

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
    /// insufficient stack values for the call, a dead anchor, or no stack
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
    /// * `source` - The Lua frame source at first reporting (`None` without a Lua frame)
    /// * `error` - The error that occurred (includes stack trace)
    fn on_error(&mut self, source: Option<&str>, error: &Error) {
        // Default: no-op (host handles via Result)
        let _ = (source, error);
    }
}

/// Default callbacks that print to stdout.
///
/// This is used when no custom callbacks are provided, suitable for CLI tools.
/// Since `on_print` cannot return an I/O error, a failed stdout write is ignored.
#[derive(Default)]
pub struct DefaultCallbacks;

impl HostCallbacks for DefaultCallbacks {
    fn on_print(&mut self, _source: Option<&str>, _line: u32, message: &str) {
        use std::io::Write;

        // Deliberately discarded: on_print has no error channel (see above).
        #[allow(clippy::let_underscore_must_use)]
        let _ = writeln!(std::io::stdout().lock(), "{message}");
    }
}
