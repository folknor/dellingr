//! Host-triggered interruption of running Lua.
//!
//! The cost budget never stops a loop of free opcodes (`docs/budget.md`), so
//! a host that must be able to stop a script on demand (Ctrl-C in a bench
//! runner, a "stop" button in an editor) triggers the State's [`Interrupt`].
//! Running Lua polls it at safepoints and fails with
//! [`ErrorKind::Interrupted`](crate::error::ErrorKind::Interrupted), which unwinds
//! through the ordinary error boundaries.
//!
//! The safepoints, chosen so that no unbounded execution can avoid one:
//!
//! - every taken backward jump, before the cursor moves (and, for a numeric
//!   `for`, before the loop counters are written). `JMP`, the
//!   compare-and-branch and test opcodes and `FOR_LOOP` poll; admission
//!   refuses a backward `FOR_PREP` or `TFOR_LOOP`, so bytecode of any
//!   provenance has no unpolled cycle;
//! - the admission of every Lua function call, from Lua or from Rust (a host
//!   call, a metamethod handler, an iterator, a library callback), after the
//!   call-depth check and before anything is pushed, so an interruption there
//!   is a refusal reported at the caller's call instruction. Recursion needs
//!   no backward jump; a return needs no poll, since every return pairs with
//!   a call that polled;
//! - native library work that can repeat a bounded pass without bound: each
//!   pattern-matcher step, each candidate of a plain substring search, each
//!   heap-sort step, and each iteration of the library loops whose trip count
//!   a script chooses by a number (`table.concat`, `table.move`, the
//!   metamethod paths of `table.insert`, `table.remove` and `table.sort`, and
//!   the empty-pattern `string.gsub`).
//!
//! Between two safepoints the work is at most one linear pass over a string
//! capped by `MAX_STRING_BYTES` or over a table's storage. The
//! `safepoint-loops` script check (`scripts/safepoint_loops.py`) holds every
//! loop in the standard library, the pattern matcher and the table
//! operations to that: each polls, or records why it is bounded or where it
//! polls instead.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A shareable request to stop running Lua.
///
/// Cloning shares the request: every clone, and every State built with or
/// given one ([`crate::StateBuilder::interrupt`], [`crate::State::set_interrupt`]),
/// reads the same flag, so one trigger stops whichever of those States is
/// running. Triggering is a single relaxed atomic store, safe from any
/// thread and from a signal handler.
///
/// The request is sticky. The VM only reads it, never clears it: every
/// safepoint any State reaches while it is set fails with
/// [`ErrorKind::Interrupted`](crate::error::ErrorKind::Interrupted), including the
/// first one of a later call. A Rust callback that swallows the error lets
/// its Lua caller continue only until that caller's next safepoint. Call
/// [`Interrupt::reset`] to run Lua again; a reset while a script still runs
/// can withdraw a request before any safepoint observed it.
///
/// The flag is host state outside the deterministic contract: it is not
/// saved in snapshots, and an interrupted run ends at a point that depends
/// on timing.
#[derive(Clone, Debug, Default)]
pub struct Interrupt(Arc<AtomicBool>);

impl Interrupt {
    /// A new request, not triggered, shared with nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask every State reading this request to stop at its next safepoint.
    pub fn trigger(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Withdraw the request, so Lua runs again.
    pub fn reset(&self) {
        self.0.store(false, Ordering::Relaxed);
    }

    /// Whether the request is set.
    #[must_use]
    pub fn is_triggered(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Whether `self` and `other` are the same request (clones of one handle).
    #[must_use]
    pub fn same_as(&self, other: &Interrupt) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// The flag safepoints read, for work that cannot hold the State.
    pub(crate) fn flag(&self) -> &AtomicBool {
        &self.0
    }
}

/// Whether `flag` is set: the poll a safepoint makes. Relaxed, since the flag
/// publishes no data.
#[inline(always)]
pub(crate) fn requested(flag: &AtomicBool) -> bool {
    flag.load(Ordering::Relaxed)
}
