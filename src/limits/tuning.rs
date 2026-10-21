//! Crate-private defaults for implementation tuning.
//!
//! These values size internal batches, caches, default per-State GC policy,
//! and initial allocations. They are not fixed script-level resource limits.

/// Cost-accounting batch size, forwarded through the public `limits` module.
pub(crate) const COST_CHECK_INTERVAL: u64 = 64;

/// Maximum cached patterns per State, forwarded through the public `limits` module.
pub(crate) const GMATCH_PATTERN_CACHE_ENTRIES: usize = 256;

/// Maximum source-pattern bytes cached per State, forwarded through the public
/// `limits` module.
pub(crate) const GMATCH_PATTERN_CACHE_BYTES: usize = crate::limits::MAX_STRING_BYTES;

/// Default lower bound for automatic GC thresholds.
pub(crate) const DEFAULT_GC_MIN_THRESHOLD: usize = 20;

/// Initial automatic GC threshold for a State using the default policy.
pub(crate) const DEFAULT_GC_INITIAL_THRESHOLD: usize = DEFAULT_GC_MIN_THRESHOLD;

/// Automatic GC threshold multiplier for a State using the default policy.
pub(crate) const DEFAULT_GC_THRESHOLD_GROWTH_FACTOR: usize = 2;

/// Initial VM value-stack reservation.
pub(crate) const INITIAL_VM_STACK_CAPACITY: usize = 256;

/// Initial transient-root value reservation.
pub(crate) const INITIAL_TRANSIENT_ROOT_VALUES_CAPACITY: usize = 64;

/// Initial call-stack reservation.
pub(crate) const INITIAL_CALL_STACK_CAPACITY: usize = 64;

/// Initial upvalue slot and color-array reservations.
pub(crate) const INITIAL_UPVALUE_POOL_CAPACITY: usize = 64;

/// Extra global-binding capacity reserved for host-defined globals.
pub(crate) const HOST_GLOBAL_CAPACITY_HEADROOM: usize = 4;
