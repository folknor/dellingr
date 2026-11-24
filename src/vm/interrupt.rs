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
//! - every taken backward jump, before the cursor moves (and, for a `for`
//!   loop, before the loop counters or the generic-for control are
//!   written). `JMP`, the compare-and-branch and test opcodes, `FOR_LOOP`
//!   and `TFOR_LOOP` (the generic `for`'s back edge) poll, and so does the
//!   typed numeric loop region at each `FOR_LOOP` it runs
//!   (`vm/numeric_loop.rs`); admission refuses
//!   a backward `FOR_PREP`, so bytecode of any provenance has no unpolled
//!   cycle;
//! - the admission of every Lua function call, from Lua or from Rust (a host
//!   call, a metamethod handler, an iterator, a library callback), after the
//!   call-depth check and before anything is pushed, so an interruption there
//!   is a refusal reported at the caller's call instruction. A numeric leaf
//!   run without an activation (`State::try_numeric_leaf`) reads the same
//!   poll flag at that point and declines to canonical admission when it is
//!   set, so the poll there is the one that reports. A typed loop region
//!   calling a numeric leaf reads it at each such call the same way and,
//!   when it is set, leaves the region at that `CALL` for canonical
//!   admission to report. Recursion needs
//!   no backward jump, and tail recursion, which runs at constant depth, is
//!   still a call admission each time; a return needs no poll, since every
//!   return pairs with a call that polled;
//! - entry to every Rust callback, before its bookkeeping is changed or its
//!   body runs, including library functions and Rust metamethod handlers.
//!   A library leaf the dispatch loop executes directly (`vm/leaf.rs`) takes
//!   the same poll once it commits, before its charge;
//!   The generic `for` fast paths for `next` and the `ipairs` iterator run
//!   without entering a callback and skip this poll; the loop's back edge
//!   polls instead when the iteration continues. A step that ends the loop
//!   falls through without a poll, so a pending interrupt is observed at the
//!   next safepoint after the loop;
//! - native library work that can repeat a bounded pass without bound: each
//!   pattern-matcher step or batch of steps (a matcher kernel that scans a
//!   class run, a run of single atoms, skipped start positions, a balance or
//!   a backreference polls once before its scan, which is at most one pass
//!   over the subject), each candidate of a plain substring search, each
//!   heap-sort step, each iteration of the library loops whose trip count
//!   a script chooses by a number (`table.concat`, `table.move`, the
//!   metamethod paths of `table.insert`, `table.remove` and `table.sort`),
//!   each subject position of the empty-pattern `string.gsub`, each
//!   rejected draw of an integer `math.random` (`VmRng::random_range_i64`
//!   says why its retries are not counted as bounded), and each
//!   `string.format` directive and `%%` escape, before a directive is parsed
//!   and its argument rendered (the literal bytes between them are one pass
//!   over the format string, a string capped by `MAX_STRING_BYTES`).
//!
//! Between two polls the work is bounded by the VM's fixed limits, never by
//! the script: a run of instructions with no backward jump and no call
//! (bounded by code length and call depth, since returns do not poll), where
//! each instruction or library step makes at most one linear pass over a
//! string capped by `MAX_STRING_BYTES` or over a table's storage; plus any
//! garbage collection, which traces the whole heap; plus the body of a host
//! callback, which the VM cannot stop and which must poll its own condition
//! if it needs to be stoppable. It bounds work, not elapsed time;
//! `docs/budget.md` ("Stopping a script from the host") states the same
//! contract. The `safepoint-loops` script check
//! (`scripts/safepoint_loops.py`) holds every loop in the standard library,
//! the pattern matcher, dispatch leaves, numeric regions, table operations
//! and the `math.random` sampler to the per-step pass: each polls, or records
//! why it is bounded or where it polls instead.

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
