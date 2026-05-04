//! Host callbacks for customizing VM behavior.
//!
//! The game engine can implement the `HostCallbacks` trait to:
//! - Redirect `print()` output to a per-script console
//! - Get notified when errors occur
//!
//! # Example (Game Engine)
//!
//! ```ignore
//! struct FleetCallbacks {
//!     fleet_id: FleetId,
//!     output: Vec<String>,
//! }
//!
//! impl HostCallbacks for FleetCallbacks {
//!     fn on_print(&mut self, source: Option<&str>, line: u32, message: &str) {
//!         self.output.push(format!("[{}:{}] {}",
//!             source.unwrap_or("?"), line, message));
//!     }
//! }
//!
//! // Create state with custom callbacks
//! let callbacks = FleetCallbacks { fleet_id, output: Vec::new() };
//! let mut state = State::with_callbacks(Box::new(callbacks));
//! ```

use crate::error::Error;

/// Trait for host-provided callbacks.
///
/// Implement this trait to customize how the VM interacts with your application.
/// The default implementation prints to stdout, suitable for CLI usage.
pub trait HostCallbacks {
    /// Called when Lua's `print()` function is executed.
    ///
    /// # Arguments
    /// * `source` - The source file/chunk name (e.g., "main.lua" or "[fleet:123]")
    /// * `line` - The current line number (0 if unknown)
    /// * `message` - The formatted message (all arguments joined with tabs)
    fn on_print(&mut self, _source: Option<&str>, _line: u32, message: &str) {
        // Default: print to stdout (for CLI usage)
        println!("{message}");
    }

    /// Called when an error occurs during script execution.
    ///
    /// This is called in addition to returning the error from `call()`.
    /// Use it for logging or telemetry.
    ///
    /// # Arguments
    /// * `source` - The source file/chunk name where the error originated
    /// * `error` - The error that occurred (includes stack trace)
    fn on_error(&mut self, source: Option<&str>, error: &Error) {
        // Default: no-op (host handles via Result)
        let _ = (source, error);
    }
}

/// Default callbacks that print to stdout.
///
/// This is used when no custom callbacks are provided, suitable for CLI tools.
#[derive(Default)]
pub struct DefaultCallbacks;

impl HostCallbacks for DefaultCallbacks {}
