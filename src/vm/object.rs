//! An `Object` is some data which:
//! - Has an unknown lifetime
//! - May have references to other `Object`s
//!
//! Because of this, it needs to be garbage collected.
//!
//! Objects and strings live in generational arenas (`arena.rs`). Each
//! ObjectPtr contains a generation that is validated on access, preventing
//! use-after-free bugs.

mod arena;
mod collector;

use arena::{Arena, ArenaKey, Occupancy, SlotKey};
pub(in crate::vm) use collector::LiveChunks;

use crate::det_hash::IndexMap;
use crate::limits::tuning;
use std::fmt;
use std::sync::Arc;

use super::Bytecode;
use super::BytecodeRuntime;
use super::LuaType;
use super::Table;
use super::Val;
use super::table::{HashProbe, RouteState, SlotRef, TableEditor, TemplatePrototype};

// ============================================================================
// Upvalue Pool - Arena-based upvalue storage
// ============================================================================

/// An upvalue - either open (pointing to stack) or closed (holding value).
#[derive(Clone, Debug)]
pub(crate) enum Upvalue {
    /// Open upvalue pointing to an absolute stack index
    Open(usize),
    /// Closed upvalue holding the value directly
    Closed(Val),
}

/// Index into the upvalue pool.
///
/// `usize`, the width of the backing `Vec`, so every slot the pool can hold
/// has a distinct reference. A narrower index would wrap once the pool passed
/// its range and alias a new capture onto an old slot; the pool has no quota
/// (docs/limits.md, "State-local growth"), so that range is reachable by a
/// script retaining enough captures or growing the pool with automatic
/// collection disabled. Widening costs four bytes per capture-list entry and
/// keeps capture infallible; no pool index is serialized (snapshots assign
/// their own dense ids).
///
/// Only the pool mints references outside tests, and a pool index is below
/// the `Vec` length, so `usize::MAX` is never a real reference: `Captures`
/// uses it as its private empty marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UpvalueRef(usize);

impl UpvalueRef {
    /// The value no pool index can take, reserved for `Captures`.
    const NONE: Self = Self(usize::MAX);

    #[cfg(test)]
    pub(crate) fn new(idx: usize) -> Self {
        assert_ne!(idx, usize::MAX, "usize::MAX is the empty-capture marker");
        Self(idx)
    }

    pub(crate) fn index(self) -> usize {
        self.0
    }
}

/// A closure's capture list, shared by the closure and its active frames.
///
/// The list is immutable once built (writes go through the `UpvaluePool`
/// slots the refs point at), so cloning it for a call admission is enough to
/// give the frame the same cells. Up to one capture is stored inline and
/// cloned by copy: no allocation when the closure is made, and no reference
/// count traffic when it is called. Longer lists share one `Arc` slice.
/// Both forms are two words (`size_of` is pinned below), so neither
/// `Closure` nor `Frame` widened for it.
#[derive(Clone, Debug)]
pub(super) struct Captures(CapturesRepr);

#[derive(Clone, Debug)]
enum CapturesRepr {
    /// No capture when the ref is `UpvalueRef::NONE`, else exactly one.
    Inline(UpvalueRef),
    /// Two or more captures.
    Shared(Arc<[UpvalueRef]>),
}

const _: () = assert!(size_of::<Captures>() == size_of::<Arc<[UpvalueRef]>>());

impl Captures {
    pub(super) const EMPTY: Self = Self(CapturesRepr::Inline(UpvalueRef::NONE));

    /// Build a capture list in iteration order. An exact-size iterator, so a
    /// list of two or more allocates once with no staging buffer.
    pub(super) fn from_exact<I>(refs: I) -> Self
    where
        I: ExactSizeIterator<Item = UpvalueRef>,
    {
        let mut refs = refs;
        match refs.len() {
            0 => Self::EMPTY,
            1 => {
                let only = refs
                    .next()
                    .expect("an iterator of length one yields one item");
                Self(CapturesRepr::Inline(only))
            }
            _ => Self(CapturesRepr::Shared(refs.collect())),
        }
    }

    /// The captures as a slice, indexed by upvalue number.
    #[inline]
    pub(super) fn as_slice(&self) -> &[UpvalueRef] {
        match &self.0 {
            CapturesRepr::Inline(only) if *only == UpvalueRef::NONE => &[],
            CapturesRepr::Inline(only) => std::slice::from_ref(only),
            CapturesRepr::Shared(refs) => refs,
        }
    }
}

/// Pool for upvalue storage. Avoids per-upvalue heap allocations by storing
/// all upvalues contiguously. Slots whose `UpvalueRef` is unreachable at GC
/// time are swept onto a free list and reused by later allocations, so a
/// long-lived State does not leak one slot per closure-with-captures ever
/// created. Reuse order (LIFO) is a pure function of execution history, so
/// determinism holds.
pub(crate) struct UpvaluePool {
    slots: Vec<Upvalue>,
    /// Which slots are occupied (not on the free list) and which are marked
    /// this collection; the sweep visits only non-empty words, so a pool that
    /// once held many captures does not pay for its vacant slots.
    bits: Occupancy,
    /// Indices of free slots, consumed LIFO by `alloc`.
    free: Vec<usize>,
}

impl Default for UpvaluePool {
    fn default() -> Self {
        Self::new()
    }
}

impl UpvaluePool {
    pub(super) fn new() -> Self {
        Self {
            slots: Vec::with_capacity(tuning::INITIAL_UPVALUE_POOL_CAPACITY),
            bits: Occupancy::default(),
            free: Vec::new(),
        }
    }

    /// Allocate a new upvalue and return its reference.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(super) fn alloc(&mut self, upvalue: Upvalue) -> UpvalueRef {
        if let Some(idx) = self.free.pop() {
            self.slots[idx] = upvalue;
            self.bits.occupy(idx);
            return UpvalueRef(idx);
        }
        let idx = self.slots.len();
        self.slots.push(upvalue);
        self.bits.occupy(idx);
        UpvalueRef(idx)
    }

    /// Mark one slot reachable during the GC mark phase, and on its first
    /// mark submit the value a closed slot holds. `Reachable` therefore
    /// means the slot's outgoing edge is already marked, so every further
    /// closure sharing the cell stops at the colour check. An open slot's
    /// referent is a stack slot, which the stack root covers. This is the
    /// only way a slot is marked, closures and open-upvalue roots alike, so
    /// no path can colour a closed cell without tracing its value.
    pub(super) fn mark(&self, uv_ref: UpvalueRef, heap: &GcHeap, worklist: &mut Vec<ObjectPtr>) {
        let idx = uv_ref.index();
        assert!(
            self.bits.is_occupied(idx),
            "a reachable closure or open-upvalue entry holds an UpvalueRef \
             whose slot was swept; the pool root set is incomplete, and \
             marking it could let sweep add the same free slot twice"
        );
        if !self.bits.mark(idx) {
            return;
        }
        if let Upvalue::Closed(val) = &self.slots[uv_ref.index()] {
            val.mark_reachable(heap, worklist);
        }
    }

    /// Sweep unmarked slots onto the free list. Call after the heap mark
    /// phase, once every reachable closure and open upvalue has passed
    /// through `mark`.
    pub(super) fn sweep(&mut self) {
        let slots = &mut self.slots;
        let free = &mut self.free;
        self.bits.sweep(|idx| {
            // Drop the held value so a swept closed upvalue cannot pin heap
            // objects; the slot is dead until reallocated.
            slots[idx] = Upvalue::Closed(Val::Nil);
            free.push(idx);
        });
    }

    #[cfg(feature = "snapshot")]
    pub(super) fn alloc_closed_nil(&mut self) -> UpvalueRef {
        self.alloc(Upvalue::Closed(Val::Nil))
    }

    #[cfg(feature = "snapshot")]
    pub(super) fn set_closed(&mut self, uv_ref: UpvalueRef, val: Val) {
        self.slots[uv_ref.index()] = Upvalue::Closed(val);
    }

    /// Test-only: undo a mark phase without sweeping.
    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn reset_marks(&self) {
        self.bits.clear_marks();
    }

    /// Slots currently holding a live upvalue (total minus free-listed).
    #[cfg(test)]
    pub(super) fn occupied_count(&self) -> usize {
        self.slots.len() - self.free.len()
    }

    /// Total slots ever allocated, including free-listed ones.
    #[cfg(test)]
    pub(super) fn total_slots(&self) -> usize {
        self.slots.len()
    }

    /// Get immutable access to an upvalue.
    #[inline]
    pub(super) fn get(&self, uv_ref: UpvalueRef) -> &Upvalue {
        debug_assert!(
            self.bits.is_occupied(uv_ref.index()),
            "read through an UpvalueRef whose slot was swept"
        );
        &self.slots[uv_ref.index()]
    }

    /// Get mutable access to an upvalue.
    #[inline]
    pub(super) fn get_mut(&mut self, uv_ref: UpvalueRef) -> &mut Upvalue {
        debug_assert!(
            self.bits.is_occupied(uv_ref.index()),
            "write through an UpvalueRef whose slot was swept"
        );
        &mut self.slots[uv_ref.index()]
    }
}

// ============================================================================
// Object Types
// ============================================================================

/// A Lua closure: a function with captured upvalues.
///
/// `runtime` is State-local, shared by every closure of its bytecode and any
/// executing frames, and owns that immutable bytecode (`Self::bytecode`), so
/// a closure holds one handle for both; its caches are never serialized and
/// its literals are GC roots. Stored inline in its heap slot: the slot is
/// table-sized anyway, and a box would cost an allocation per closure and a
/// dependent load on every call admission.
#[derive(Clone, Debug)]
pub(super) struct Closure {
    pub(super) runtime: Arc<BytecodeRuntime>,
    /// Shared with admitted frames (see `Captures`). Up to one capture is
    /// inline; longer lists stay one shared `Arc` slice rather than size
    /// classes inline in the heap slot, which would force a copy of the
    /// whole list into every frame (frames cannot borrow the heap).
    pub(super) upvalues: Captures,
}

impl Closure {
    /// The closure's immutable bytecode, owned by its runtime.
    pub(super) fn bytecode(&self) -> &Arc<Bytecode> {
        &self.runtime.bytecode
    }
}

/// A `string.gmatch` iterator: a function value whose state is this typed
/// record rather than a Lua closure over a state table. Calling it runs the
/// catalog step `lua_std::GMATCH_STEP` with the iterator itself as the
/// callback's first argument (`State::gmatch_step_of`), so it takes the
/// ordinary Rust callback ABI and its errors are located at the caller.
///
/// It holds only values a live iterator can reach, and snapshot load checks
/// a saved one against the same bounds, so the step trusts the record. The
/// general iterator's 5.4 empty-match rule needs the end of the previous
/// accepted match only when it equals `pos` (every accepted match sets both
/// to its end), so `resumed` records that and nothing else.
#[derive(Clone, Copy, Debug)]
pub(crate) enum GmatchIter {
    /// A nonempty pattern over `subject`. `pos` is at most the subject's
    /// length plus one (one past it is exhausted); a resumed `pos` is at most
    /// the length.
    General {
        subject: StringPtr,
        pattern: StringPtr,
        pos: usize,
        resumed: bool,
    },
    /// The empty pattern: one empty match at every boundary `pos..=len`,
    /// examining no byte, so the subject itself is not kept.
    Empty { pos: usize, len: usize },
}

/// A gmatch iterator's heap object: the record and, for a general
/// iterator, the compiled program of its pattern, held so a step does not
/// resolve it again. The program is never saved; creation and load take it
/// from the State's pattern cache, which shares one program per key among
/// all its owners (`State::cached_pattern`). Compilation is uncharged, so
/// holding it moves no cost.
pub(super) struct GmatchObject {
    pub(super) iter: GmatchIter,
    pub(super) program: Option<crate::patterns::SharedPattern>,
}

/// The raw object data managed by GC.
/// Tables and closures share these slots, so inline table metadata also
/// affects closure storage. Strings use the separate StringPool and do not
/// pay for this enum's largest variant. Boxing tables would add an allocation
/// and indirection to every table, including small records; keep them inline
/// unless measurements justify that tradeoff.
#[cfg_attr(
    debug_assertions,
    expect(
        clippy::large_enum_variant,
        reason = "Table is stored inline so table access takes no extra indirection; its debug-only state pushes the gap to the inline closure past the lint threshold, which release builds stay under"
    )
)]
pub(super) enum RawObject {
    LuaFn(Closure),
    Table(Table),
    Gmatch(GmatchObject),
}

impl RawObject {
    #[must_use]
    pub(super) const fn typ(&self) -> LuaType {
        match self {
            RawObject::LuaFn(_) | RawObject::Gmatch(_) => LuaType::Function,
            RawObject::Table(_) => LuaType::Table,
        }
    }
}

// ============================================================================
// ObjectPtr - Safe generational key
// ============================================================================

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ObjectKey(SlotKey);

impl ArenaKey for ObjectKey {
    fn from_slot(key: SlotKey) -> Self {
        Self(key)
    }
    fn slot(self) -> SlotKey {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct StringKey(SlotKey);

impl ArenaKey for StringKey {
    fn from_slot(key: SlotKey) -> Self {
        Self(key)
    }
    fn slot(self) -> SlotKey {
        self.0
    }
}

/// A safe pointer to a GC-managed object.
/// Uses the arena's generational indices to prevent use-after-free:
/// - Each key contains a generation number
/// - When a slot is freed and reused, the generation increments
/// - Accessing with an old key panics instead of causing memory corruption
///
/// Generations wrap after about 2^31 reuses of one slot, so an old key is
/// not an identity proof forever: anything holding handles weakly must drop
/// them before their slot can come round again (see `val_is_live` and
/// [`CACHE_PRUNE_INTERVAL`]).
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ObjectPtr(pub(crate) ObjectKey);

/// How many collections may pass between two runs of the runtime-cache
/// prune (`State::drop_dead_cache_entries`).
///
/// The prune is only an alias guard. Every cache hit compares the cached
/// handle with a live one before it dereferences anything, so an entry
/// naming a freed object just misses, unless its slot's generation has come
/// all the way round to the cached value. That takes 2^31 reuses: the arena
/// (`object/arena.rs`, which reproduces slotmap 1.1's representation) keeps
/// a `u32` version per slot, bumped once by a remove and once by an insert
/// into the vacant slot (`version_steps_by_two_per_reuse` pins the step,
/// the arena's `version_wraps_like_slotmap` the wrap). The sweep in `GcHeap::collect` is the only thing that frees an
/// object, so one collection cycle reuses a slot at most once, and an entry
/// pruned within this many collections of its referent's death can never
/// alias. Pruning on every collection would make each one walk every cache
/// cell of the loaded code, a cost that tracks code size, not garbage.
///
/// Warmth is unobservable (`RuntimeCaches`), so this cadence changes no
/// result, cost, collection timing or snapshot. The other weak tables
/// (compiled patterns, `%p` ids) still prune every collection: they hold
/// memory, and a stale `%p` id would be visible.
pub(super) const CACHE_PRUNE_INTERVAL: u32 = 1 << 16;
const _: () = assert!(CACHE_PRUNE_INTERVAL > 0 && CACHE_PRUNE_INTERVAL < 1 << 31);

/// Arena keys are process-local, so diagnostics keep the pointer opaque.
impl fmt::Debug for ObjectPtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<object>")
    }
}

impl ObjectPtr {
    /// Get the type of this object.
    pub(super) fn typ(self, heap: &GcHeap) -> LuaType {
        heap.get(self).typ()
    }
}

// ============================================================================
// GcHeap - arena-based garbage collected heap
// ============================================================================

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GcPolicy {
    initial_threshold: usize,
    minimum_threshold: usize,
    growth_factor: usize,
}

impl GcPolicy {
    pub(crate) const fn new(
        initial_threshold: usize,
        minimum_threshold: usize,
        growth_factor: usize,
    ) -> Self {
        Self {
            initial_threshold,
            minimum_threshold,
            growth_factor,
        }
    }
}

impl Default for GcPolicy {
    fn default() -> Self {
        Self::new(
            tuning::DEFAULT_GC_INITIAL_THRESHOLD,
            tuning::DEFAULT_GC_MIN_THRESHOLD,
            tuning::DEFAULT_GC_THRESHOLD_GROWTH_FACTOR,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GcThreshold {
    /// Automatic mode remains active even when a computed threshold saturates
    /// to `usize::MAX`; a later collection can recompute it from live count.
    Automatic(usize),
    Manual(usize),
    Disabled,
}

impl GcThreshold {
    /// `usize::MAX` disables the initial automatic policy. Computed
    /// thresholds use `Automatic` directly so saturation does not disable it.
    fn from_initial(value: usize) -> Self {
        if value == usize::MAX {
            Self::Disabled
        } else {
            Self::Automatic(value)
        }
    }

    fn manual(value: usize) -> Self {
        if value == usize::MAX {
            Self::Disabled
        } else {
            Self::Manual(value)
        }
    }

    fn value(self) -> usize {
        match self {
            Self::Automatic(value) | Self::Manual(value) => value,
            Self::Disabled => usize::MAX,
        }
    }
}

/// Bytes in table backing and interned string payloads owned by the heap,
/// including table backing temporarily held by suspended environments.
pub(super) struct ResidentBytes {
    bytes: usize,
    // The first byte count that makes the automatic score reach its threshold.
    // MAX also serves as the sentinel for disabled or unreachable watermarks
    // and while a pending poll already covers further growth.
    watermark: usize,
    interrupt: super::Interrupt,
    poll_source: super::Interrupt,
}

impl Default for ResidentBytes {
    fn default() -> Self {
        Self::new(0)
    }
}

// This request is private to the collector and is never reset or handed out
// to hosts. Each State selects it independently when it has GC pressure.
static GC_PENDING: std::sync::LazyLock<super::Interrupt> = std::sync::LazyLock::new(|| {
    let flag = super::Interrupt::new();
    flag.trigger();
    flag
});

impl ResidentBytes {
    pub(super) fn new(bytes: usize) -> Self {
        let interrupt = super::Interrupt::new();
        Self {
            bytes,
            watermark: usize::MAX,
            poll_source: interrupt.clone(),
            interrupt,
        }
    }

    pub(super) fn add(&mut self, bytes: usize) {
        self.bytes += bytes;
        if self.bytes >= self.watermark {
            self.arm_gc_poll();
        }
    }

    pub(super) fn remove(&mut self, bytes: usize) {
        self.bytes -= bytes;
    }

    #[cold]
    fn arm_gc_poll(&mut self) {
        self.poll_source = GC_PENDING.clone();
        self.watermark = usize::MAX;
    }

    fn refresh_watermark(&mut self, watermark: usize) {
        if self.bytes >= watermark {
            if self.poll_source.same_as(&self.interrupt) {
                self.arm_gc_poll();
            }
        } else {
            self.watermark = watermark;
            if !self.poll_source.same_as(&self.interrupt) {
                self.poll_source = self.interrupt.clone();
            }
        }
    }

    #[cfg(any(feature = "snapshot", test))]
    pub(super) fn replace(&mut self, old: usize, new: usize) {
        self.remove(old);
        self.add(new);
    }
}

/// The saturated route epoch: no inherited route fills or hits at it.
const ROUTE_EPOCH_DISABLED: u64 = u64::MAX;

/// A collection of objects which need to be garbage-collected.
/// Uses a generational arena for safe access to objects.
pub(crate) struct GcHeap {
    /// Every GC-managed object, with its mark bit.
    objects: Arena<ObjectKey, RawObject>,
    /// When the pacing score (automatic mode) or the allocation count
    /// (manual mode) reaches this threshold, run the GC.
    threshold: GcThreshold,
    /// Automatic threshold changes are derived from this policy after each
    /// collection. Manual thresholds and disabled mode are kept separately.
    policy: GcPolicy,
    /// Pool for interned strings.
    strings: StringPool,
    resident: ResidentBytes,
    /// Reused scratch space for the iterative GC mark phase.
    mark_worklist: Vec<ObjectPtr>,
    /// Reused scratch space for the runtime-cache sweep's chunk retention.
    live_chunks: LiveChunks,
    /// The object whose mutation `watched_epoch` counts: the global table,
    /// once the State has one (`watch`).
    watched: ObjectPtr,
    /// Advances on every mutable access to `watched` through `get_mut`, the
    /// one mutable route to an object, so an unchanged epoch proves the
    /// object unchanged without reaching it through the slot map, except for
    /// the stores `watched_table_unmarked` admits. The builtin mirror
    /// (`GlobalEnv`) is validated by it.
    watched_epoch: u64,
    /// The route epoch and the `__index` name it classifies by. The epoch
    /// advances, clearing the table's `on_route` bit, when a write that may
    /// change a route reaches a marked table: any mutation through a
    /// `TableEditor` but a live non-`__index` overwrite (the editor
    /// classifies each one) and any `as_table` handout. The cached global
    /// store (`store_watched_probe`) refuses a marked table, and
    /// `overwrite_live_slot` is the caller-proven overwrite, so neither moves
    /// it. An inherited-route cache entry records the epoch after marking
    /// every table its route read, so an unchanged epoch proves none of them
    /// changed. It saturates at `ROUTE_EPOCH_DISABLED`, where no route fills
    /// or hits: a wrapped epoch could match a stale entry.
    routes: RouteState,
    /// Collections left before the next one clears runtime cache entries
    /// naming freed objects (`GcHeap::runtime_cache_prune_due`).
    cache_prune_countdown: u32,
}

impl GcHeap {
    pub(super) fn with_policy(policy: GcPolicy) -> Self {
        let mut heap = Self {
            objects: Arena::new(),
            threshold: GcThreshold::from_initial(policy.initial_threshold),
            policy,
            strings: StringPool::new(),
            resident: ResidentBytes::default(),
            mark_worklist: Vec::new(),
            live_chunks: LiveChunks::default(),
            watched: ObjectPtr(ObjectKey(SlotKey::NULL)),
            watched_epoch: 0,
            routes: RouteState::default(),
            cache_prune_countdown: CACHE_PRUNE_INTERVAL,
        };
        heap.refresh_gc_watermark();
        heap
    }

    /// Count mutable accesses to `ptr` in [`Self::watched_epoch`] from now on.
    pub(super) fn watch(&mut self, ptr: ObjectPtr) {
        self.watched = ptr;
        self.watched_epoch = self.watched_epoch.wrapping_add(1);
    }

    /// The watched object's mutation epoch (see the field).
    #[inline(always)]
    pub(super) fn watched_epoch(&self) -> u64 {
        self.watched_epoch
    }

    /// The route epoch (see the field); `None` once it has saturated.
    #[inline(always)]
    pub(super) fn route_epoch(&self) -> Option<u64> {
        (self.routes.epoch != ROUTE_EPOCH_DISABLED).then_some(self.routes.epoch)
    }

    /// Record the interned `__index` name, the one key whose live overwrite
    /// can relink a route (`TableEditor` classifies by it). Until then every
    /// key counts as a possible link.
    pub(super) fn set_route_link_key(&mut self, index_name: StringPtr) {
        self.routes.link_key = Some(index_name);
    }

    /// Create a heap with a chosen threshold for collector tests.
    #[cfg(test)]
    pub(super) fn with_threshold(threshold: usize) -> Self {
        Self {
            objects: Arena::new(),
            threshold: GcThreshold::manual(threshold),
            policy: GcPolicy::default(),
            strings: StringPool::new(),
            resident: ResidentBytes::default(),
            mark_worklist: Vec::new(),
            live_chunks: LiveChunks::default(),
            watched: ObjectPtr(ObjectKey(SlotKey::NULL)),
            watched_epoch: 0,
            routes: RouteState::default(),
            cache_prune_countdown: CACHE_PRUNE_INTERVAL,
        }
    }

    pub(super) fn reserve(&mut self, additional_objects: usize, additional_strings: usize) {
        self.objects.reserve(additional_objects);
        self.strings.reserve(additional_strings);
    }

    // ========================================================================
    // Object access - these replace ObjectPtr's self-dereferencing methods
    // ========================================================================

    /// Get a reference to the object.
    /// Panics if the key is invalid (use-after-free detection).
    #[inline]
    pub(super) fn get(&self, ptr: ObjectPtr) -> &RawObject {
        self.objects
            .get(ptr.0)
            .expect("Invalid ObjectPtr: object was freed (use-after-free detected)")
    }

    // Keep invalidation here: `_G` is an ordinary table and can be mutated
    // through aliases, rawset and library operations without GlobalEnv. At
    // this boundary the key and intended operation are unknown, so even a
    // non-builtin write conservatively invalidates the mirror. Moving this
    // into GlobalEnv alone misses those routes; classifying each generic
    // table write would add key checks to ordinary table mutations.
    /// Get a mutable reference to the object, with the backing meter and
    /// the route witness. Inherited routes are not retired here: an editor
    /// classifies each mutation (`TableEditor`), and `as_table`, whose raw
    /// `&mut Table` cannot, retires at the handout.
    /// Panics if the key is invalid (use-after-free detection).
    #[inline]
    fn get_mut(&mut self, ptr: ObjectPtr) -> (&mut RawObject, &mut ResidentBytes, &mut RouteState) {
        if ptr == self.watched {
            self.mark_watched_changed();
        }
        let object = self
            .objects
            .get_mut(ptr.0)
            .expect("Invalid ObjectPtr: object was freed (use-after-free detected)");
        (object, &mut self.resident, &mut self.routes)
    }

    /// Move the watched object's epoch, for a store made through
    /// [`Self::edit_watched_unmarked`] that can change what it guards.
    #[inline]
    pub(super) fn mark_watched_changed(&mut self) {
        self.watched_epoch = self.watched_epoch.wrapping_add(1);
    }

    // This bypass is for the global table's keyed and version-validated
    // cached stores only. Their builtin bit is classified from the key, so
    // the steady non-builtin store needs neither a key lookup nor an epoch
    // bump.
    /// The watched table's editor, without moving the watched epoch. Only
    /// for a store that provably cannot change what the epoch guards (a
    /// global store whose key is not a builtin name), or whose caller moved
    /// it with [`Self::mark_watched_changed`]. Such a store may insert,
    /// rehash or compact, which move slots (the table version retires those
    /// probes) but change no other key's value, so the builtin mirror stays
    /// exact. The builtin mirror's exemption is not the route epoch's: a
    /// non-builtin global store can shadow an inherited key, so the editor
    /// classifies it like any other store.
    #[inline]
    pub(super) fn edit_watched_unmarked(&mut self) -> Option<TableEditor<'_>> {
        match self.objects.get_mut(self.watched.0)? {
            RawObject::Table(table) => Some(table.edit(&mut self.resident, &mut self.routes)),
            RawObject::LuaFn(_) | RawObject::Gmatch(_) => None,
        }
    }

    /// `Table::store_probe` into the watched table, under
    /// [`Self::edit_watched_unmarked`]'s rule, for the cached global store
    /// inlined into the dispatch loop. A table some route reads refuses
    /// (false, changing nothing), so the caller's keyed fallback stores
    /// through an editor, which classifies the store; a call to classify it
    /// here grew the dispatch loop's release native frame by 16 bytes.
    /// `None` when there is no watched table.
    #[inline(always)]
    pub(super) fn store_watched_probe(&mut self, probe: &HashProbe, val: &Val) -> Option<bool> {
        match self.objects.get_mut(self.watched.0)? {
            RawObject::Table(table) => Some(!table.is_on_route() && table.store_probe(probe, val)),
            RawObject::LuaFn(_) | RawObject::Gmatch(_) => None,
        }
    }

    /// Borrow the object as a Lua closure, if it is one, without cloning it.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(super) fn as_lua_function_ref(&self, ptr: ObjectPtr) -> Option<&Closure> {
        match self.get(ptr) {
            RawObject::LuaFn(closure) => Some(closure),
            _ => None,
        }
    }

    /// The object's gmatch iterator record, if it is one.
    pub(super) fn as_gmatch(&self, ptr: ObjectPtr) -> Option<GmatchIter> {
        match self.get(ptr) {
            RawObject::Gmatch(object) => Some(object.iter),
            _ => None,
        }
    }

    /// A matcher over gmatch iterator `ptr`'s held program, if it holds one.
    pub(super) fn gmatch_matcher(&self, ptr: ObjectPtr) -> Option<crate::patterns::LuaPattern> {
        match self.get(ptr) {
            RawObject::Gmatch(GmatchObject {
                program: Some(program),
                ..
            }) => Some(program.matcher()),
            _ => None,
        }
    }

    /// Overwrite gmatch iterator `ptr`'s record with `iter` (the step's
    /// commit); false, changing nothing, when `ptr` is not an iterator.
    pub(super) fn set_gmatch(&mut self, ptr: ObjectPtr, iter: GmatchIter) -> bool {
        match self.get_mut(ptr).0 {
            RawObject::Gmatch(record) => {
                record.iter = iter;
                true
            }
            _ => false,
        }
    }

    /// Get the object as a Lua function (closure), if it is one.
    /// Test-only: runtime paths borrow through `as_lua_function_ref`.
    #[cfg(test)]
    pub(super) fn as_lua_function(&self, ptr: ObjectPtr) -> Option<Closure> {
        match self.get(ptr) {
            RawObject::LuaFn(closure) => Some(closure.clone()),
            _ => None,
        }
    }

    /// Get a mutable reference to the object as a table, if it is one. The
    /// raw borrow can make any change, so it retires inherited routes
    /// through the table at the handout; a store an editor can classify
    /// goes through [`Self::edit_table`] instead.
    pub(super) fn as_table(&mut self, ptr: ObjectPtr) -> Option<&mut Table> {
        match self.get_mut(ptr) {
            (RawObject::Table(t), _, routes) => {
                routes.retire_through(t);
                Some(t)
            }
            _ => None,
        }
    }

    /// Overwrite live `slot` of table `ptr` with non-nil `val`
    /// ([`Table::store_slot`]), without retiring inherited routes and
    /// without the editor's classification: the caller has already proved
    /// the slot's key is not `__index` (the set cache is never filled for
    /// an `__index` store; a field leaf checks its keys at entry), so the
    /// hot cached store skips the `on_route` test and the key read
    /// `TableEditor::store_slot` would make. A live overwrite under any other key
    /// creates no key, so no intermediate miss on a route turns into a hit
    /// (a table holding the written key live was never missing it), changes
    /// no metatable link or `__index` value, and moves no table version; a
    /// route ending at the slot reads it live. A dead slot is refused,
    /// changing nothing: reviving it creates a key, and the keyed fallback's
    /// editor retires routes for it. The watched table's epoch
    /// still moves, since the written key may be a builtin's name.
    #[inline(always)]
    pub(super) fn overwrite_live_slot(&mut self, ptr: ObjectPtr, slot: SlotRef, val: Val) -> bool {
        if ptr == self.watched {
            self.mark_watched_changed();
        }
        match self
            .objects
            .get_mut(ptr.0)
            .expect("Invalid ObjectPtr: object was freed (use-after-free detected)")
        {
            RawObject::Table(table) => table.store_slot(slot, val),
            RawObject::LuaFn(_) | RawObject::Gmatch(_) => false,
        }
    }

    /// Borrow the table, its backing meter and the route witness for
    /// writes. The editor decides per mutation whether inherited routes
    /// retire (`TableEditor`).
    pub(super) fn edit_table(&mut self, ptr: ObjectPtr) -> Option<TableEditor<'_>> {
        let (object, resident, routes) = self.get_mut(ptr);
        match object {
            RawObject::Table(table) => Some(table.edit(resident, routes)),
            RawObject::LuaFn(_) | RawObject::Gmatch(_) => None,
        }
    }

    pub(super) fn charge_table_backing(&mut self, table: &Table) {
        self.resident.add(table.resident_bytes());
    }

    pub(super) fn release_table_backing(&mut self, table: &Table) {
        self.resident.remove(table.resident_bytes());
    }

    #[cfg(debug_assertions)]
    fn check_resident_bytes(&self, suspended: usize) {
        let tables: usize = self
            .objects
            .values()
            .map(|object| match object {
                RawObject::Table(table) => table.resident_bytes(),
                RawObject::LuaFn(_) | RawObject::Gmatch(_) => 0,
            })
            .sum();
        assert_eq!(
            self.resident.bytes,
            tables + self.strings.resident_bytes() + suspended
        );
    }

    /// Get an immutable reference to the object as a table, if it is one.
    pub(super) fn as_table_ref(&self, ptr: ObjectPtr) -> Option<&Table> {
        match self.get(ptr) {
            RawObject::Table(t) => Some(t),
            _ => None,
        }
    }

    /// Get a string's content by its pointer.
    /// Panics if the string was freed (use-after-free detection).
    pub(super) fn get_string(&self, ptr: StringPtr) -> &[u8] {
        self.strings.get(ptr)
    }

    /// The interned string with these bytes, if one exists. Never allocates,
    /// so a lookup by name leaves the string pool unchanged: a name nothing
    /// has interned cannot be a table key.
    pub(super) fn find_string(&self, bytes: &[u8]) -> Option<StringPtr> {
        self.strings
            .find_by_hash(bytes, crate::det_hash::hash_bytes(bytes))
    }

    // ========================================================================
    // Allocation
    // ========================================================================

    /// Allocate a new Lua function.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn alloc_lua_fn_raw(&mut self, runtime: Arc<BytecodeRuntime>, upvalues: Captures) -> ObjectPtr {
        self.publish_object(RawObject::LuaFn(Closure { runtime, upvalues }))
    }

    /// Allocate a gmatch iterator.
    fn alloc_gmatch_raw(
        &mut self,
        iter: GmatchIter,
        program: Option<crate::patterns::SharedPattern>,
    ) -> ObjectPtr {
        self.publish_object(RawObject::Gmatch(GmatchObject { iter, program }))
    }

    /// Allocate a new table.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn alloc_table_raw(&mut self) -> ObjectPtr {
        self.publish_object(RawObject::Table(Table::default()))
    }

    /// Allocate a new table with room for `capacity` entries.
    fn alloc_table_with_capacity_raw(&mut self, capacity: usize) -> ObjectPtr {
        let table = Table::with_capacity(capacity);
        self.charge_table_backing(&table);
        self.publish_object(RawObject::Table(table))
    }

    /// Allocate a new table pre-filled from a constructor template.
    fn alloc_table_with_template_raw(&mut self, prototype: &TemplatePrototype) -> ObjectPtr {
        let table = Table::from_template(prototype);
        self.charge_table_backing(&table);
        self.publish_object(RawObject::Table(table))
    }

    #[inline]
    fn publish_object(&mut self, object: RawObject) -> ObjectPtr {
        let ptr = ObjectPtr(self.objects.insert(object));
        self.refresh_gc_watermark();
        ptr
    }

    /// Allocate or intern a string.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn alloc_string_raw<B: AsRef<[u8]> + Into<Box<[u8]>>>(&mut self, bytes: B) -> StringPtr {
        let count = self.strings.len();
        let hash = crate::det_hash::hash_bytes(bytes.as_ref());
        let ptr = self.strings.intern_storage(bytes, hash, &mut self.resident);
        if self.strings.len() != count {
            self.refresh_gc_watermark();
        }
        ptr
    }

    /// Intern `range` of the pooled string `source`, reading the bytes in
    /// place: a hit copies nothing, a miss boxes the range once. `None` when
    /// the range lies outside the source.
    fn alloc_string_from_range_raw(
        &mut self,
        source: StringPtr,
        range: std::ops::Range<usize>,
    ) -> Option<StringPtr> {
        let count = self.strings.len();
        let ptr = self.strings.intern_range(source, range, &mut self.resident);
        if self.strings.len() != count {
            self.refresh_gc_watermark();
        }
        ptr
    }

    // ========================================================================
    // Garbage Collection
    // ========================================================================

    pub(crate) fn interrupt(&self) -> &super::Interrupt {
        &self.resident.interrupt
    }

    pub(super) fn set_interrupt(&mut self, interrupt: super::Interrupt) {
        self.resident.interrupt = interrupt;
        self.resident.poll_source = self.resident.interrupt.clone();
        self.refresh_gc_watermark();
    }

    #[inline(always)]
    pub(super) fn gc_poll_requested(&self) -> bool {
        super::interrupt_requested(self.resident.poll_source.flag())
    }

    #[inline]
    pub(super) fn refresh_gc_watermark(&mut self) {
        let watermark = match self.threshold {
            GcThreshold::Automatic(threshold) => threshold
                .saturating_sub(self.allocation_count())
                .saturating_mul(tuning::GC_RESIDENT_BYTES_PER_UNIT),
            GcThreshold::Manual(_) | GcThreshold::Disabled => usize::MAX,
        };
        self.resident.refresh_watermark(watermark);
    }

    pub(super) fn automatic_gc_due(&self) -> bool {
        matches!(self.threshold, GcThreshold::Automatic(threshold) if self.pacing_score() >= threshold)
    }

    /// Check whether the configured threshold has been reached.
    #[must_use]
    pub(super) fn is_full(&self) -> bool {
        match self.threshold {
            GcThreshold::Automatic(threshold) => self.pacing_score() >= threshold,
            GcThreshold::Manual(threshold) => self.allocation_count() >= threshold,
            GcThreshold::Disabled => false,
        }
    }

    /// The automatic pacing score: objects plus distinct strings plus
    /// resident payload bytes in units of `GC_RESIDENT_BYTES_PER_UNIT`.
    /// Manual thresholds keep counting allocations only.
    #[inline]
    pub(super) fn pacing_score(&self) -> usize {
        self.allocation_count()
            .saturating_add(self.resident.bytes / tuning::GC_RESIDENT_BYTES_PER_UNIT)
    }

    /// Mark an object as reachable. Call this for all root objects.
    /// The worklist traversal follows children, including closure upvalues.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(super) fn mark(&self, ptr: ObjectPtr, worklist: &mut Vec<ObjectPtr>) {
        // Marking accepts strong roots and graph edges only. Weak caches
        // must be filtered by their owners. Table tombstone keys are strong
        // edges until compaction, so they always name a live object.
        debug_assert!(
            self.objects.contains(ptr.0),
            "GC marked a vacant object key"
        );
        if self.objects.mark(ptr.0) == Some(true) {
            worklist.push(ptr);
        }
    }

    pub(super) fn take_live_chunks(&mut self) -> LiveChunks {
        std::mem::take(&mut self.live_chunks)
    }

    /// Keep the scratch for the next collection, emptied.
    pub(super) fn restore_live_chunks(&mut self, mut live: LiveChunks) {
        live.trees.clear();
        live.pending.clear();
        self.live_chunks = live;
    }

    pub(super) fn take_mark_worklist(&mut self) -> Vec<ObjectPtr> {
        let mut worklist = std::mem::take(&mut self.mark_worklist);
        worklist.clear();
        worklist
    }

    pub(super) fn restore_mark_worklist(&mut self, mut worklist: Vec<ObjectPtr>) {
        worklist.clear();
        self.mark_worklist = worklist;
    }

    pub(super) fn drain_mark_worklist(
        &self,
        worklist: &mut Vec<ObjectPtr>,
        upvalue_pool: &UpvaluePool,
    ) {
        while let Some(ptr) = worklist.pop() {
            // Everything on the worklist must have been coloured by
            // `GcHeap::mark`. Pushing a pointer straight onto the worklist
            // traces its children but leaves the object itself Unmarked, so
            // sweep frees it and later reads dangle - a root added that way
            // looks correct until something actually collects. This assertion
            // turns "remember to call mark" into a checked invariant; it costs
            // nothing in release builds.
            debug_assert!(
                self.objects.is_marked(ptr.0).is_none_or(|marked| marked),
                "GC worklist entry was not marked reachable; it was pushed \
                 directly instead of through GcHeap::mark"
            );
            self.mark_children(self.get(ptr), upvalue_pool, worklist);
        }
    }

    /// Mark a string as reachable.
    pub(super) fn mark_string(&self, ptr: StringPtr) {
        self.strings.mark(ptr);
    }

    /// Mark objects referenced by this object.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn mark_children(
        &self,
        obj: &RawObject,
        upvalue_pool: &UpvaluePool,
        worklist: &mut Vec<ObjectPtr>,
    ) {
        match obj {
            RawObject::LuaFn(closure) => {
                // The runtime's literals are rooted once per collection as
                // `GcRoot::ChunkLiterals`, since every closure's runtime is
                // the State's cached one (checked in debug builds by
                // `State::check_closure_runtimes`), so marking them again per
                // closure would only repeat work. What the closure does add
                // is that its runtime's entry must survive the cache sweep.
                closure.runtime.caches.mark_reached();
                // Mark the upvalue slots themselves (so the pool sweep keeps
                // them) and the values stored in closed ones.
                for uv_ref in closure.upvalues.as_slice() {
                    upvalue_pool.mark(*uv_ref, self, worklist);
                }
            }
            RawObject::Table(tbl) => {
                tbl.mark_values(self, worklist);
            }
            RawObject::Gmatch(GmatchObject {
                iter:
                    GmatchIter::General {
                        subject, pattern, ..
                    },
                ..
            }) => {
                self.mark_string(*subject);
                self.mark_string(*pattern);
            }
            RawObject::Gmatch(GmatchObject {
                iter: GmatchIter::Empty { .. },
                ..
            }) => {}
        }
    }

    /// Run the garbage collector sweep phase.
    /// Call this after marking all roots.
    #[cfg_attr(feature = "hotpath", hotpath::measure(label = "object::heap_collect"))]
    fn collect(&mut self) {
        let resident = &mut self.resident;
        // Sweep phase: free unmarked objects in ascending slot order. Only
        // occupied words are visited and only dead objects are touched, so
        // the cost does not track the arena's high-water mark.
        self.objects.sweep(|_, obj| {
            if let RawObject::Table(table) = obj {
                resident.remove(table.resident_bytes());
            }
        });

        // String collection
        self.strings.collect(resident);

        // Preserve manual and disabled modes across explicit collections.
        // Automatic mode follows the State's policy rather than replacing it.
        // The `live + 1` floor gives growth factors 0 and 1 one unit of
        // headroom whenever the score is below `usize::MAX`, so an unchanged
        // heap (a rooted intern hit straight after this collection) does not
        // collect again at once. It promises nothing beyond that one unit.
        if matches!(self.threshold, GcThreshold::Automatic(_)) {
            let live = self.pacing_score();
            self.threshold = GcThreshold::Automatic(
                live.saturating_mul(self.policy.growth_factor)
                    .max(self.policy.minimum_threshold)
                    .max(live.saturating_add(1)),
            );
        }
        self.refresh_gc_watermark();
    }

    /// Count one collection toward the runtime-cache prune, and report
    /// whether this one must run it (every [`CACHE_PRUNE_INTERVAL`]-th).
    pub(super) fn runtime_cache_prune_due(&mut self) -> bool {
        self.cache_prune_countdown -= 1;
        if self.cache_prune_countdown == 0 {
            self.cache_prune_countdown = CACHE_PRUNE_INTERVAL;
            true
        } else {
            false
        }
    }

    /// Make the next collection run the runtime-cache prune.
    #[cfg(test)]
    pub(super) fn force_runtime_cache_prune(&mut self) {
        self.cache_prune_countdown = 1;
    }

    /// Every closure in the heap, for the debug check that each one's runtime
    /// is the State's cached runtime for its bytecode.
    #[cfg(debug_assertions)]
    pub(super) fn closures(&self) -> impl Iterator<Item = &Closure> {
        self.objects.values().filter_map(|object| match object {
            RawObject::LuaFn(closure) => Some(closure),
            RawObject::Table(_) | RawObject::Gmatch(_) => None,
        })
    }

    // ========================================================================
    // Memory tracking
    // ========================================================================

    /// Whether a Val's referent still resolves in the heap. Non-heap values
    /// are always live. Exact for a value that was live before the latest
    /// sweep, asked before the next allocation: a freed slot stays vacant
    /// until then. A handle kept across later allocations can alias one,
    /// since generations wrap; that is why weak side tables are pruned with
    /// this: `format_pointer_ids` and the pattern cache at every collection,
    /// runtime caches within [`CACHE_PRUNE_INTERVAL`] collections. Asked
    /// later than right after a sweep it is still exact for a freed handle
    /// whose slot has not been reused 2^31 times since.
    pub(super) fn val_is_live(&self, val: &Val) -> bool {
        match val {
            Val::Obj(ptr) => self.objects.contains(ptr.0),
            Val::Str(ptr) => self.strings.contains(*ptr),
            _ => true,
        }
    }

    /// Test-only: the heap handles a mark phase coloured, after which every
    /// object and string colour is back to Unmarked, as a sweep would leave
    /// them, without freeing anything, and no runtime is left flagged as
    /// reached for the next collection's cache sweep. The caller resets the
    /// upvalue pool.
    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn take_marked_objects_and_strings(
        &self,
    ) -> (
        std::collections::BTreeSet<ObjectPtr>,
        std::collections::BTreeSet<StringPtr>,
    ) {
        let mut marked = std::collections::BTreeSet::new();
        for (key, obj) in self.objects.iter() {
            if self.objects.is_marked(key) == Some(true) {
                marked.insert(ObjectPtr(key));
            }
            if let RawObject::LuaFn(closure) = obj {
                closure.runtime.caches.take_reached();
            }
        }
        self.objects.clear_marks();
        let mut marked_strings = std::collections::BTreeSet::new();
        for (key, _) in self.strings.strings.iter() {
            if self.strings.strings.is_marked(key) == Some(true) {
                marked_strings.insert(StringPtr(key));
            }
        }
        self.strings.reset_marks();
        (marked, marked_strings)
    }

    /// Number of GC-managed objects (tables and closures).
    pub(super) fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Number of interned strings.
    pub(super) fn string_count(&self) -> usize {
        self.strings.len()
    }

    /// Total heap allocations counted toward the GC threshold: GC-managed
    /// objects plus distinct interned strings.
    #[inline]
    pub(super) fn allocation_count(&self) -> usize {
        self.objects.len().saturating_add(self.strings.len())
    }

    /// Current GC threshold.
    pub(super) fn threshold(&self) -> usize {
        self.threshold.value()
    }

    /// Set the GC threshold.
    pub(super) fn set_threshold(&mut self, threshold: usize) {
        self.threshold = GcThreshold::manual(threshold);
        self.refresh_gc_watermark();
    }

    /// Replace the automatic collection policy and restart at its initial
    /// threshold.
    pub(super) fn set_policy(&mut self, policy: GcPolicy) {
        self.policy = policy;
        self.threshold = GcThreshold::from_initial(policy.initial_threshold);
        self.refresh_gc_watermark();
    }

    /// Resume automatic collection at the configured initial threshold.
    pub(super) fn reset_auto_threshold(&mut self) {
        self.threshold = GcThreshold::from_initial(self.policy.initial_threshold);
        self.refresh_gc_watermark();
    }

    /// Disable automatic collection while preserving explicit collection.
    pub(super) fn disable_auto_gc(&mut self) {
        self.threshold = GcThreshold::Disabled;
        self.refresh_gc_watermark();
    }
}

// ============================================================================
// Markable Trait - for non-object types that contain references
// ============================================================================

/// An item is `Markable` if it can be marked as reachable given heap access.
pub(super) trait Markable {
    /// Mark this item and the references it contains as reachable.
    fn mark_reachable(&self, heap: &GcHeap, worklist: &mut Vec<ObjectPtr>);
}

impl Markable for Val {
    // Forced inline: left to LLVM, table scans (`Table::mark_values`) and
    // the other marking loops call it out of line once per key and value,
    // which made a collection over many small tables about a quarter more
    // retired instructions (bench/host/many_chunks).
    #[inline(always)]
    fn mark_reachable(&self, heap: &GcHeap, worklist: &mut Vec<ObjectPtr>) {
        match self {
            Val::Obj(ptr) => heap.mark(*ptr, worklist),
            Val::Str(ptr) => heap.mark_string(*ptr),
            _ => (),
        }
    }
}

impl<T: Markable> Markable for [T] {
    fn mark_reachable(&self, heap: &GcHeap, worklist: &mut Vec<ObjectPtr>) {
        for val in self {
            val.mark_reachable(heap, worklist);
        }
    }
}

impl<K, V: Markable> Markable for IndexMap<K, V> {
    fn mark_reachable(&self, heap: &GcHeap, worklist: &mut Vec<ObjectPtr>) {
        for val in self.values() {
            val.mark_reachable(heap, worklist);
        }
    }
}

// ============================================================================
// String Pool - arena with generational indices for safety
// ============================================================================

/// Entry for an interned string.
struct StringEntry {
    data: StringData,
    /// The hash the string was interned under: the index's rehash and the
    /// sweep's removal read it, never recompute it (tests intern under a
    /// forced hash to make collisions).
    hash: u64,
}

/// Longest string stored inline in its arena slot. Chosen so `StringData`
/// is no larger than the `Box<[u8]>` arm plus its tag: 24 bytes, a 40-byte
/// slot with the hash and version (pinned below).
const INLINE_STRING_BYTES: usize = 22;

/// A string's bytes: inline beside the entry header when short, so a
/// distinct short string costs no allocator allocation and no pointer
/// chase, otherwise one boxed allocation. The storage follows the length
/// alone, whatever the source (a borrowed slice, a pooled range or a
/// producer's owned `Vec`, which a short string copies and drops), so the
/// first interning of some bytes cannot choose a different layout from a
/// later one. Identity is the arena key either way.
enum StringData {
    Inline {
        len: u8,
        bytes: [u8; INLINE_STRING_BYTES],
    },
    Heap(Box<[u8]>),
}

// Layout tripwire: the inline cap and stride are a design choice, so a
// compiler layout change or a cap edit fails the build rather than silently
// widening every string slot.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(INLINE_STRING_BYTES == 22);
    assert!(size_of::<StringData>() == 24);
    assert!(size_of::<arena::Slot<StringEntry>>() == 40);
};

impl StringData {
    /// Inline copy of `bytes`, or `None` when they do not fit.
    fn inline(bytes: &[u8]) -> Option<Self> {
        let mut buf = [0; INLINE_STRING_BYTES];
        buf.get_mut(..bytes.len())?.copy_from_slice(bytes);
        let len = u8::try_from(bytes.len()).expect("the inline cap fits a u8");
        Some(Self::Inline { len, bytes: buf })
    }

    /// Storage for a miss: inline when short, else the source converted to
    /// a box (an owned `Vec` through `into_boxed_slice`, a slice copied).
    fn new<B: AsRef<[u8]> + Into<Box<[u8]>>>(bytes: B) -> Self {
        Self::inline(bytes.as_ref()).unwrap_or_else(|| Self::Heap(bytes.into()))
    }

    #[inline]
    fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Inline { len, bytes } => &bytes[..usize::from(*len)],
            Self::Heap(data) => data,
        }
    }
}

/// A safe pointer to an interned string.
/// Uses the arena's generational indices to prevent use-after-free:
/// - Each key contains a generation number
/// - When a slot is freed and reused, the generation increments
/// - Accessing with an old key panics instead of causing memory corruption
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct StringPtr(StringKey);

/// String pool over a generational arena.
/// Provides string interning with use-after-free detection.
pub(crate) struct StringPool {
    /// Every interned string, with its mark bit.
    strings: Arena<StringKey, StringEntry>,
    /// Content index for interning: one key per live string, placed by the
    /// string's `det_hash::hash_bytes` hash (kept in its `StringEntry`, which
    /// is what the rehash closures read) and matched by content. Only ever
    /// probed, never iterated, so its layout cannot reach program-visible
    /// order: string identity comes from the arena, and table keys hash the
    /// `StringPtr`, not the content. Every key here must resolve in
    /// `strings` whenever a closure that rehashes can run.
    index: hashbrown::HashTable<StringKey>,
}

impl StringPool {
    fn new() -> Self {
        Self {
            strings: Arena::new(),
            index: hashbrown::HashTable::new(),
        }
    }

    fn reserve(&mut self, additional: usize) {
        self.strings.reserve(additional);
        let strings = &self.strings;
        self.index
            .reserve(additional, |key| Self::entry_hash(strings, *key));
    }

    /// Hash of a live string. The index only holds live keys, so a miss is a
    /// broken pool invariant.
    fn entry_hash(strings: &Arena<StringKey, StringEntry>, key: StringKey) -> u64 {
        strings
            .get(key)
            .expect("string index holds only live string keys")
            .hash
    }

    pub(super) fn len(&self) -> usize {
        self.strings.len()
    }

    pub(super) fn contains(&self, ptr: StringPtr) -> bool {
        self.strings.contains(ptr.0)
    }

    /// Get a string's content by its pointer.
    /// Panics if the string was freed (use-after-free detection).
    pub(super) fn get(&self, ptr: StringPtr) -> &[u8] {
        Self::data(&self.strings, ptr)
    }

    /// `get` over the arena alone, so a caller can keep the borrow while
    /// it mutates the index.
    fn data(strings: &Arena<StringKey, StringEntry>, ptr: StringPtr) -> &[u8] {
        strings
            .get(ptr.0)
            .expect("Invalid StringPtr: string was freed (use-after-free detected)")
            .data
            .as_bytes()
    }

    /// Find an existing interned string by content and hash.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(super) fn find_by_hash(&self, bytes: &[u8], hash: u64) -> Option<StringPtr> {
        let strings = &self.strings;
        self.index
            .find(hash, |key| {
                strings
                    .get(*key)
                    .is_some_and(|entry| entry.data.as_bytes() == bytes)
            })
            .map(|key| StringPtr(*key))
    }

    /// Intern `bytes` under `hash` (the caller's `det_hash::hash_bytes` of
    /// them; tests force collisions through it). One index probe serves
    /// both the hit and the miss: `entry` does any reserve, and so every
    /// rehash, before the new key exists, and the vacant entry's insert
    /// writes the prepared bucket without rehashing. At exhausted capacity
    /// a hit can take that reserve; it is the one the next miss would take.
    ///
    /// `bytes` is a borrowed slice or a producer's owned `Vec`. A miss of at
    /// most `INLINE_STRING_BYTES` copies them inline (dropping a `Vec`); a
    /// longer one boxes a slice (a copy) or converts the `Vec`
    /// (`into_boxed_slice`, which shrinks when capacity exceeds length and
    /// may still move the bytes then) instead of copying it. A hit drops
    /// the `Vec`. The probe, identity and resident
    /// length (never the capacity) are the same either way.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn intern_storage<B: AsRef<[u8]> + Into<Box<[u8]>>>(
        &mut self,
        bytes: B,
        hash: u64,
        resident: &mut ResidentBytes,
    ) -> StringPtr {
        let strings = &self.strings;
        let content = bytes.as_ref();
        let vacant = match self.index.entry(
            hash,
            |key| {
                strings
                    .get(*key)
                    .is_some_and(|e| e.data.as_bytes() == content)
            },
            |key| Self::entry_hash(strings, *key),
        ) {
            hashbrown::hash_table::Entry::Occupied(found) => return StringPtr(*found.get()),
            hashbrown::hash_table::Entry::Vacant(vacant) => vacant,
        };
        let bytes_len = content.len();
        let key = self.strings.insert(StringEntry {
            data: StringData::new(bytes),
            hash,
        });
        vacant.insert(key);
        resident.add(bytes_len);
        StringPtr(key)
    }

    /// Intern `range` of the pooled string `source`, reading the bytes in
    /// place: a hit copies nothing, a miss boxes the range once. `None` when
    /// the range lies outside the source. Same single probe as `intern`.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(super) fn intern_range(
        &mut self,
        source: StringPtr,
        range: std::ops::Range<usize>,
        resident: &mut ResidentBytes,
    ) -> Option<StringPtr> {
        let strings = &self.strings;
        let bytes = Self::data(strings, source).get(range)?;
        let hash = crate::det_hash::hash_bytes(bytes);
        let vacant = match self.index.entry(
            hash,
            |key| {
                strings
                    .get(*key)
                    .is_some_and(|e| e.data.as_bytes() == bytes)
            },
            |key| Self::entry_hash(strings, *key),
        ) {
            hashbrown::hash_table::Entry::Occupied(found) => return Some(StringPtr(*found.get())),
            hashbrown::hash_table::Entry::Vacant(vacant) => vacant,
        };
        let bytes_len = bytes.len();
        // Built before the insert, which can grow the arena and move the
        // (possibly inline) source bytes.
        let data = StringData::new(bytes);
        let key = self.strings.insert(StringEntry { data, hash });
        vacant.insert(key);
        resident.add(bytes_len);
        Some(StringPtr(key))
    }

    /// Test shorthand: `intern_storage` of a borrowed slice.
    #[cfg(test)]
    fn intern(&mut self, bytes: &[u8], hash: u64, resident: &mut ResidentBytes) -> StringPtr {
        self.intern_storage(bytes, hash, resident)
    }

    /// Mark a string as reachable.
    pub(super) fn mark(&self, ptr: StringPtr) {
        debug_assert!(
            self.strings.contains(ptr.0),
            "GC marked a vacant string key"
        );
        self.strings.mark(ptr.0);
    }

    /// Test-only: undo a mark phase without sweeping.
    #[cfg(all(test, feature = "snapshot"))]
    fn reset_marks(&self) {
        self.strings.clear_marks();
    }

    #[cfg(any(debug_assertions, test))]
    fn resident_bytes(&self) -> usize {
        self.strings
            .values()
            .map(|entry| entry.data.as_bytes().len())
            .sum()
    }

    /// Collect unreachable strings.
    ///
    /// The arena sweep frees dead strings in ascending slot order (so freed
    /// slots join the free list in the same order as a full slot walk would)
    /// and touches only the dead entries. Each dead key leaves the index in
    /// the same step: `find_entry` matches by key, reads no entry, and
    /// neither reserves nor rehashes, so it is safe while the arena is
    /// mid-sweep. Driving the sweep from the index instead
    /// (`HashTable::retain`) was weighed: it reads every live entry in hash
    /// order to save the per-dead probes, which wins only after a large share
    /// of the pool dies, and it would change the free-list order.
    #[cfg_attr(
        feature = "hotpath",
        hotpath::measure(label = "object::string_pool_collect")
    )]
    fn collect(&mut self, resident: &mut ResidentBytes) {
        let index = &mut self.index;
        self.strings.sweep(|key, entry| {
            resident.remove(entry.data.as_bytes().len());
            match index.find_entry(entry.hash, |k| *k == key) {
                Ok(found) => {
                    found.remove();
                }
                Err(_) => unreachable!("every interned string has one index entry"),
            }
        });
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::tests::on_iterative_gc_test_stack;

    #[test]
    fn capture_lists_keep_their_refs_in_order_at_every_length() {
        assert!(Captures::EMPTY.as_slice().is_empty());
        for len in 0..4 {
            let refs: Vec<UpvalueRef> = (0..len).map(|i| UpvalueRef::new(i * 7)).collect();
            let captures = Captures::from_exact(refs.iter().copied());
            assert_eq!(captures.as_slice(), refs.as_slice());
            assert_eq!(captures.clone().as_slice(), refs.as_slice());
        }
        // Index 0 is a real reference, not the empty marker.
        let zero = Captures::from_exact(std::iter::once(UpvalueRef::new(0)));
        assert_eq!(zero.as_slice(), &[UpvalueRef::new(0)]);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "GC marked a vacant object key")]
    fn marking_a_swept_object_reports_the_stale_root() {
        let mut heap = GcHeap::with_threshold(100);
        let ptr = heap.no_gc_allocator().alloc_table();
        heap.collect();
        heap.mark(ptr, &mut Vec::new());
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "pushed directly")]
    fn pushing_an_unmarked_object_to_the_worklist_is_detected() {
        let mut heap = GcHeap::with_threshold(100);
        let ptr = heap.no_gc_allocator().alloc_table();
        let mut worklist = vec![ptr];
        heap.drain_mark_worklist(&mut worklist, &UpvaluePool::new());
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "GC marked a vacant string key")]
    fn marking_a_swept_string_reports_the_stale_root() {
        let mut pool = StringPool::new();
        let mut resident = ResidentBytes::default();
        let ptr = pool.intern(b"unrooted", 1, &mut resident);
        pool.collect(&mut resident);
        pool.mark(ptr);
    }

    /// `CACHE_PRUNE_INTERVAL` rests on one collection cycle moving a slot's
    /// version by exactly two: one step when the sweep frees the object, one
    /// when an allocation reuses the slot. A change to that stepping fails
    /// here.
    #[test]
    fn version_steps_by_two_per_reuse() {
        fn index_and_version(ptr: ObjectPtr) -> (u32, u32) {
            ptr.0.0.parts()
        }
        let mut heap = GcHeap::with_threshold(100);
        let mut previous = index_and_version(heap.no_gc_allocator().alloc_table());
        for _ in 0..4 {
            heap.collect();
            let next = index_and_version(heap.no_gc_allocator().alloc_table());
            assert_eq!(next.0, previous.0, "the freed slot is reused");
            assert_eq!(next.1, previous.1 + 2, "a reuse moves the version by two");
            previous = next;
        }
    }

    /// The runtime-cache prune runs on every `CACHE_PRUNE_INTERVAL`-th
    /// collection and no other.
    #[test]
    fn runtime_cache_prune_runs_once_per_interval() {
        let mut heap = GcHeap::with_threshold(100);
        for _ in 0..2 {
            for _ in 1..CACHE_PRUNE_INTERVAL {
                assert!(!heap.runtime_cache_prune_due());
            }
            assert!(heap.runtime_cache_prune_due());
        }
    }

    #[test]
    fn test_basic_allocation() {
        let mut heap = GcHeap::with_threshold(100);
        let t1 = heap.no_gc_allocator().alloc_table();
        let t2 = heap.no_gc_allocator().alloc_table();

        assert!(heap.as_table_ref(t1).is_some());
        assert!(heap.as_table_ref(t2).is_some());
        assert_eq!(heap.object_count(), 2);
    }

    #[test]
    fn saturated_automatic_threshold_is_recomputed_after_collection() {
        let mut heap = GcHeap::with_policy(GcPolicy::new(1, 1, usize::MAX));
        let kept = heap.no_gc_allocator().alloc_table();

        let mut worklist = heap.take_mark_worklist();
        heap.mark(kept, &mut worklist);
        heap.drain_mark_worklist(&mut worklist, &UpvaluePool::new());
        heap.restore_mark_worklist(worklist);
        heap.collect();

        assert_eq!(heap.threshold(), usize::MAX);
        assert!(matches!(heap.threshold, GcThreshold::Automatic(usize::MAX)));

        // Once the live count falls, automatic mode derives a lower threshold.
        heap.collect();
        assert_eq!(heap.threshold(), 1);
    }

    #[test]
    fn maximum_initial_threshold_stays_disabled_after_collection() {
        let mut heap = GcHeap::with_policy(GcPolicy::new(usize::MAX, 1, 2));
        heap.collect();

        assert_eq!(heap.threshold(), usize::MAX);
        assert!(matches!(heap.threshold, GcThreshold::Disabled));
    }

    #[test]
    #[should_panic(expected = "marking it could let sweep add the same free slot twice")]
    fn marking_a_swept_upvalue_slot_is_rejected() {
        let mut pool = UpvaluePool::new();
        let uv_ref = pool.alloc(Upvalue::Closed(Val::Nil));
        pool.sweep();

        pool.mark(uv_ref, &GcHeap::with_threshold(100), &mut Vec::new());
    }

    #[test]
    fn a_shared_closed_upvalue_is_traced_on_its_first_mark_only() {
        let mut heap = GcHeap::with_threshold(100);
        let captured = heap.no_gc_allocator().alloc_table();
        let mut pool = UpvaluePool::new();
        let uv_ref = pool.alloc(Upvalue::Closed(Val::Obj(captured)));
        let mut worklist = Vec::new();
        pool.mark(uv_ref, &heap, &mut worklist);
        assert_eq!(worklist, vec![captured]);
        heap.drain_mark_worklist(&mut worklist, &pool);
        // A second closure sharing the cell: the slot is already Reachable,
        // and the captured table stays marked from the first visit.
        pool.mark(uv_ref, &heap, &mut worklist);
        assert!(worklist.is_empty());
        heap.collect();
        pool.sweep();
        assert!(heap.as_table_ref(captured).is_some());
    }

    #[test]
    fn test_gc_collect() {
        let mut heap = GcHeap::with_threshold(100);
        let kept = heap.no_gc_allocator().alloc_table();
        let _freed = heap.no_gc_allocator().alloc_table();

        // Mark only the first table
        let mut worklist = heap.take_mark_worklist();
        heap.mark(kept, &mut worklist);
        heap.drain_mark_worklist(&mut worklist, &UpvaluePool::new());
        heap.restore_mark_worklist(worklist);
        heap.collect();

        // First table should survive, second should be freed
        assert!(heap.as_table_ref(kept).is_some());
        assert_eq!(heap.object_count(), 1);
    }

    #[test]
    fn deep_table_chain_marks_iteratively_and_reuses_the_empty_worklist() {
        on_iterative_gc_test_stack("gc-deep-table-chain", || {
            let mut heap = GcHeap::with_threshold(100);
            let mut root = heap.no_gc_allocator().alloc_table();
            for _ in 0..100_000 {
                let next = heap.no_gc_allocator().alloc_table();
                heap.edit_table(next)
                    .expect("newly allocated object is a table")
                    .insert(Val::Num(1.0), Val::Obj(root))
                    .expect("table key is valid");
                root = next;
            }
            for _ in 0..2 {
                let mut worklist = heap.take_mark_worklist();
                heap.mark(root, &mut worklist);
                heap.drain_mark_worklist(&mut worklist, &UpvaluePool::new());
                assert!(worklist.is_empty());
                heap.restore_mark_worklist(worklist);
                heap.collect();
                assert_eq!(heap.object_count(), 100_001);
                assert!(heap.mark_worklist.is_empty());
            }
        });
    }

    #[test]
    #[should_panic(expected = "use-after-free")]
    fn test_use_after_free_detection() {
        let mut heap = GcHeap::with_threshold(100);
        let ptr = heap.no_gc_allocator().alloc_table();

        // Don't mark it, then collect
        heap.collect();

        // This should panic with use-after-free detection
        let _ = heap.as_table_ref(ptr);
    }

    #[test]
    fn test_string_allocation() {
        let mut heap = GcHeap::with_threshold(100);
        let s1 = heap.no_gc_allocator().alloc_string(b"hello");
        let s2 = heap.no_gc_allocator().alloc_string(b"world");
        let s3 = heap.no_gc_allocator().alloc_string(b"hello"); // Should return same ptr as s1 (interned)

        assert_eq!(heap.get_string(s1), b"hello");
        assert_eq!(heap.get_string(s2), b"world");
        assert_eq!(s1, s3); // Same string should be interned
        assert_eq!(heap.string_count(), 2);
    }

    #[test]
    fn string_pool_survives_growth_across_collisions_sweeps_and_slot_reuse() {
        // Grow the index past collisions, sweep part of it, reuse freed
        // slots, then grow again: every rehash must still find each live
        // key's hash, and lookups must still resolve by content.
        let mut pool = StringPool::new();
        let mut resident = ResidentBytes::default();
        let mut live = Vec::new();
        for i in 0..200u32 {
            let bytes = format!("s{i}").into_bytes();
            // Every fourth string shares a forced hash with its neighbours.
            let hash = if i % 4 == 0 { 7 } else { u64::from(i) };
            live.push((pool.intern(&bytes, hash, &mut resident), bytes, hash));
        }
        for (index, (ptr, _, _)) in live.iter().enumerate() {
            if index % 3 == 0 {
                pool.mark(*ptr);
            }
        }
        pool.collect(&mut resident);
        let mut survivors: Vec<_> = live
            .into_iter()
            .enumerate()
            .filter(|(index, _)| index % 3 == 0)
            .map(|(_, entry)| entry)
            .collect();
        assert_eq!(pool.len(), survivors.len());
        for i in 0..400u32 {
            let bytes = format!("t{i}").into_bytes();
            let hash = if i % 2 == 0 { 7 } else { 1000 + u64::from(i) };
            survivors.push((pool.intern(&bytes, hash, &mut resident), bytes, hash));
        }
        pool.reserve(1000);
        for (ptr, bytes, hash) in &survivors {
            assert_eq!(pool.find_by_hash(bytes, *hash), Some(*ptr));
            assert_eq!(pool.get(*ptr), bytes.as_slice());
            // A hit through the fused probe returns the live pointer.
            assert_eq!(pool.intern(bytes, *hash, &mut resident), *ptr);
        }
        assert_eq!(pool.find_by_hash(b"s1", 1), None);
        assert_eq!(
            resident.bytes,
            survivors
                .iter()
                .map(|(_, bytes, _)| bytes.len())
                .sum::<usize>()
        );
    }

    #[test]
    fn test_string_gc_collect() {
        let mut heap = GcHeap::with_threshold(100);
        let kept = heap.no_gc_allocator().alloc_string(b"keep");
        let _freed = heap.no_gc_allocator().alloc_string(b"free");

        // Mark only the first string
        heap.mark_string(kept);
        heap.collect();

        // First string should survive
        assert_eq!(heap.get_string(kept), b"keep");
        assert_eq!(heap.string_count(), 1);
    }

    #[test]
    fn string_pool_sweep_preserves_moved_and_colliding_buckets() {
        let mut pool = StringPool::new();
        let mut resident = ResidentBytes::default();
        // Control the hashes to exercise a partially collected collision
        // bucket and live buckets moved by removal of an earlier bucket.
        pool.intern(b"dead first", 1, &mut resident);
        let kept = pool.intern(b"keep", 2, &mut resident);
        let collided = pool.intern(b"keep collision", 3, &mut resident);
        pool.intern(b"dead collision", 3, &mut resident);
        pool.mark(kept);
        pool.mark(collided);
        pool.collect(&mut resident);

        assert_eq!(pool.len(), 2);
        assert_eq!(pool.find_by_hash(b"dead first", 1), None);
        assert_eq!(pool.find_by_hash(b"dead collision", 3), None);
        assert_eq!(pool.find_by_hash(b"keep", 2), Some(kept));
        assert_eq!(pool.find_by_hash(b"keep collision", 3), Some(collided));

        let replacement = pool.intern(b"replacement", 1, &mut resident);
        assert_eq!(pool.find_by_hash(b"replacement", 1), Some(replacement));
        pool.mark(collided);
        pool.collect(&mut resident);
        assert_eq!(pool.len(), 1);
        assert_eq!(resident.bytes, b"keep collision".len());
        assert_eq!(pool.find_by_hash(b"keep", 2), None);
        assert_eq!(pool.find_by_hash(b"replacement", 1), None);
        assert_eq!(pool.find_by_hash(b"keep collision", 3), Some(collided));
        assert_eq!(pool.get(collided), b"keep collision");
    }

    /// An owned buffer with spare capacity interns to the same identity as
    /// the borrowed bytes, counts its length (not its capacity) as resident,
    /// and on a hit leaves the pool as it was.
    #[test]
    fn owned_intern_matches_borrowed_intern() {
        // Each side of the inline cap: a short string is stored inline, a
        // long one adopts the Vec; neither changes identity or accounting.
        for content in [
            &b"owned"[..],
            &[b'x'; INLINE_STRING_BYTES][..],
            &[b'y'; INLINE_STRING_BYTES + 1][..],
        ] {
            let mut pool = StringPool::new();
            let mut resident = ResidentBytes::default();
            let mut owned = Vec::with_capacity(64);
            owned.extend_from_slice(content);
            let adopted = pool.intern_storage(owned, 7, &mut resident);
            assert_eq!(pool.get(adopted), content);
            assert_eq!(resident.bytes, content.len());
            assert_eq!(pool.intern(content, 7, &mut resident), adopted);
            let mut again = Vec::with_capacity(content.len());
            again.extend_from_slice(content);
            assert_eq!(pool.intern_storage(again, 7, &mut resident), adopted);
            assert_eq!(pool.len(), 1);
            assert_eq!(resident.bytes, content.len());
            assert_eq!(pool.resident_bytes(), content.len());
        }
    }

    #[test]
    fn range_intern_across_the_inline_cap_matches_borrowed_intern() {
        let det_hash = crate::det_hash::hash_bytes;
        let mut pool = StringPool::new();
        let mut resident = ResidentBytes::default();
        let short = b"0123456789abcdefghij";
        let long = [b'z'; 2 * INLINE_STRING_BYTES];
        let short_src = pool.intern(short, det_hash(short), &mut resident);
        let long_src = pool.intern(&long, det_hash(&long), &mut resident);
        // Ranges of an inline source, and inline and boxed ranges of a long
        // one; the misses grow the arena while the source bytes are read.
        let cases = [
            (short_src, 2..9),
            (long_src, 0..INLINE_STRING_BYTES),
            (long_src, 1..INLINE_STRING_BYTES + 3),
        ];
        let mut made = Vec::new();
        for (source, range) in cases {
            let expect = pool.get(source)[range.clone()].to_vec();
            let ptr = pool
                .intern_range(source, range, &mut resident)
                .expect("range lies inside the source");
            assert_eq!(pool.get(ptr), &expect[..]);
            made.push((ptr, expect));
        }
        for _ in 0..200 {
            let n = pool.len();
            let filler = format!("filler {n}");
            pool.intern(
                filler.as_bytes(),
                det_hash(filler.as_bytes()),
                &mut resident,
            );
        }
        for (ptr, bytes) in &made {
            assert_eq!(pool.get(*ptr), &bytes[..]);
            assert_eq!(pool.intern(bytes, det_hash(bytes), &mut resident), *ptr);
        }
        assert_eq!(pool.get(short_src), short);
        assert_eq!(resident.bytes, pool.resident_bytes());
    }

    #[test]
    #[should_panic(expected = "use-after-free")]
    fn test_string_use_after_free_detection() {
        let mut heap = GcHeap::with_threshold(100);
        let ptr = heap.no_gc_allocator().alloc_string(b"test");

        // Don't mark it, then collect
        heap.collect();

        // This should panic with use-after-free detection
        let _ = heap.get_string(ptr);
    }
}
