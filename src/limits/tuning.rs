//! Crate-private defaults for implementation tuning.
//!
//! These values size internal batches, caches, default per-State GC policy,
//! and initial allocations. They are not fixed script-level resource limits.

/// Cost-accounting batch size, forwarded through the public `limits` module.
pub(crate) const COST_CHECK_INTERVAL: u64 = 64;

/// Maximum cached patterns per State, forwarded through the public `limits` module.
pub(crate) const PATTERN_CACHE_ENTRIES: usize = 256;

/// Maximum source-pattern bytes cached per State, forwarded through the public
/// `limits` module.
pub(crate) const PATTERN_CACHE_BYTES: usize = crate::limits::MAX_STRING_BYTES;

/// Default lower bound for automatic GC thresholds.
pub(crate) const DEFAULT_GC_MIN_THRESHOLD: usize = 20;

/// Initial automatic GC threshold for a State using the default policy.
pub(crate) const DEFAULT_GC_INITIAL_THRESHOLD: usize = DEFAULT_GC_MIN_THRESHOLD;

/// Automatic GC threshold multiplier for a State using the default policy.
pub(crate) const DEFAULT_GC_THRESHOLD_GROWTH_FACTOR: usize = 2;

/// Resident bytes (table backing capacity and interned string payload) that
/// count as one unit of the automatic GC pacing score, alongside one per
/// object and one per distinct string. It is an exchange rate between payload
/// and objects, set to about one heap object's footprint (16 `Val` slots), so
/// the score reads as resident memory in object-sized units. It is reasoned,
/// not measured: it does not affect correctness, only how much dead payload a
/// collection cycle admits (about `W` bytes per live object plus the live
/// bytes, times growth minus one) against how many slots it rescans per unit
/// of headroom (about 16 at this value). A fixed literal, never a
/// `size_of`, because collection timing is replay-visible and object layout
/// differs between debug and release builds.
pub(crate) const GC_RESIDENT_BYTES_PER_UNIT: usize = 256;

/// Resident bytes charged per slot of a table's array capacity. A fixed
/// literal for the same reason as [`GC_RESIDENT_BYTES_PER_UNIT`]: the charge
/// is part of collection timing, so it must not follow the in-memory width of
/// a value, which differs between targets and changes with the value
/// representation.
pub(crate) const RESIDENT_BYTES_PER_ARRAY_SLOT: usize = 16;

/// Resident bytes charged per slot of a table hash part's entry capacity (a
/// key and a value); fixed for the reason [`RESIDENT_BYTES_PER_ARRAY_SLOT`]
/// gives. The hash index's own allocation is charged separately.
pub(crate) const RESIDENT_BYTES_PER_HASH_ENTRY: usize = 32;

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

/// Maximum entries a table keeps in inline storage before promoting to a map.
pub(crate) const TABLE_INLINE_CAPACITY: usize = 4;

/// Hash-part size above which debug builds check a table's dead-slot counter
/// against its O(1) bound instead of walking every slot. The full walk is O(n)
/// per mutation, which makes hash-heavy scripts quadratic in debug builds.
#[cfg(debug_assertions)]
pub(crate) const TABLE_DEBUG_FULL_WALK_LIMIT: usize = 16;

/// Named fields a table-constructor template stores inline in the parser
/// before spilling to a heap vector.
pub(crate) const INLINE_TEMPLATE_FIELDS: usize = 4;

/// Bytes of a global name kept in a save-diagnostic value path.
#[cfg(feature = "snapshot")]
pub(crate) const SAVE_PATH_GLOBAL_BYTES: usize = 96;

/// Path segments rendered in a save diagnostic before eliding the rest.
#[cfg(feature = "snapshot")]
pub(crate) const SAVE_RENDERED_PATH_SEGMENTS: usize = 32;

/// Bytes of a rendered save-diagnostic path before truncation.
#[cfg(feature = "snapshot")]
pub(crate) const SAVE_RENDERED_PATH_BYTES: usize = 512;

/// Bytes of script-supplied text quoted in a save diagnostic.
#[cfg(feature = "snapshot")]
pub(crate) const SAVE_LOGGED_USER_TEXT_BYTES: usize = 128;
