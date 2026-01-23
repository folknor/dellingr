//! An `Object` is some data which:
//! - Has an unknown lifetime
//! - May have references to other `Object`s
//!
//! Because of this, it needs to be garbage collected.
//!
//! This implementation uses a chunked arena for cache-friendly iteration
//! during garbage collection. Each chunk is a fixed-size array that never
//! moves, so pointers to objects remain stable.

use std::borrow::Borrow;
use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::ptr::NonNull;
use std::rc::Rc;

use super::Chunk;
use super::LuaType;
use super::Table;
use super::Val;

// ============================================================================
// Upvalue Pool - Arena-based upvalue storage with reference counting
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UpvalueRef(u32);

impl UpvalueRef {
    fn new(idx: u32) -> Self {
        Self(idx)
    }

    fn index(self) -> usize {
        self.0 as usize
    }
}

/// Pool for upvalue storage. Avoids per-upvalue heap allocations by storing
/// all upvalues contiguously. Upvalues are never freed until the VM is dropped,
/// which is fine for game scripting where VMs have short lifetimes.
pub(crate) struct UpvaluePool {
    slots: Vec<Upvalue>,
}

impl Default for UpvaluePool {
    fn default() -> Self {
        Self::new()
    }
}

impl UpvaluePool {
    pub(super) fn new() -> Self {
        Self {
            slots: Vec::with_capacity(64),
        }
    }

    /// Allocate a new upvalue and return its reference.
    pub(super) fn alloc(&mut self, upvalue: Upvalue) -> UpvalueRef {
        let idx = self.slots.len() as u32;
        self.slots.push(upvalue);
        UpvalueRef::new(idx)
    }

    /// Get immutable access to an upvalue.
    #[inline]
    pub(super) fn get(&self, uv_ref: UpvalueRef) -> &Upvalue {
        &self.slots[uv_ref.index()]
    }

    /// Get mutable access to an upvalue.
    #[inline]
    pub(super) fn get_mut(&mut self, uv_ref: UpvalueRef) -> &mut Upvalue {
        &mut self.slots[uv_ref.index()]
    }

    /// Mark all closed upvalues' values for GC.
    /// Called during garbage collection to ensure closed upvalue contents are preserved.
    pub(super) fn mark_closed_upvalues(&self) {
        for upvalue in &self.slots {
            if let Upvalue::Closed(val) = upvalue {
                val.mark_reachable();
            }
        }
    }
}

/// A Lua closure: a function with captured upvalues.
/// Uses Rc<Chunk> to share chunk data between closure instances,
/// avoiding expensive clones of the entire chunk on every closure copy.
#[derive(Clone, Debug)]
pub(super) struct Closure {
    pub(super) chunk: Rc<Chunk>,
    pub(super) upvalues: Vec<UpvalueRef>,
}

// ============================================================================
// Arena-Based GC Implementation
// ============================================================================

/// Number of slots per arena chunk. Chosen to balance:
/// - Cache efficiency (chunk fits in L2 cache)
/// - Allocation frequency (fewer chunk allocations)
const CHUNK_SIZE: usize = 256;

/// The raw object data managed by GC.
enum RawObject {
    // Wrap this in a box to reduce the memory usage. Minimal performance impact
    // because functions are rarely accessed.
    LuaFn(Box<Closure>),
    Table(Table),
}

impl RawObject {
    #[must_use]
    pub(super) const fn typ(&self) -> LuaType {
        match self {
            RawObject::LuaFn(_) => LuaType::Function,
            RawObject::Table(_) => LuaType::Table,
        }
    }
}

#[derive(Clone, Copy)]
enum Color {
    Unmarked,
    Reachable,
}

/// A GC-managed object with its metadata.
struct WrappedObject {
    raw: RawObject,
    color: Cell<Color>,
}

/// A slot in the arena: either occupied with an object or free.
enum Slot {
    /// An occupied slot containing a GC-managed object.
    Occupied(WrappedObject),
    /// A free slot, with index of next free slot (u32::MAX = end of free list).
    Free { next_free: u32 },
}

impl Default for Slot {
    fn default() -> Self {
        Slot::Free { next_free: u32::MAX }
    }
}

/// A fixed-size chunk of slots. Once allocated, a chunk never moves,
/// so pointers to objects within it remain stable.
struct ArenaChunk {
    slots: Box<[Slot; CHUNK_SIZE]>,
}

impl ArenaChunk {
    fn new() -> Self {
        Self {
            slots: Box::new(std::array::from_fn(|_| Slot::default())),
        }
    }
}

/// The internal pointer type objects use to point to each other.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ObjectPtr {
    ptr: NonNull<WrappedObject>,
}

impl ObjectPtr {
    pub(super) fn as_lua_function(self) -> Option<Closure> {
        match &self.deref().raw {
            RawObject::LuaFn(closure) => Some((**closure).clone()),
            _ => None,
        }
    }

    pub(super) fn as_table(&mut self) -> Option<&mut Table> {
        match &mut self.deref_mut().raw {
            RawObject::Table(t) => Some(t),
            _ => None,
        }
    }

    pub(super) fn as_table_ref(&self) -> Option<&Table> {
        match &self.deref().raw {
            RawObject::Table(t) => Some(t),
            _ => None,
        }
    }

    pub(super) fn typ(self) -> LuaType {
        self.deref().raw.typ()
    }

    fn deref(&self) -> &WrappedObject {
        unsafe { self.ptr.as_ref() }
    }

    fn deref_mut(&mut self) -> &mut WrappedObject {
        unsafe { self.ptr.as_mut() }
    }
}

impl fmt::Display for ObjectPtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.deref().raw {
            RawObject::LuaFn(_) => write!(f, "function: {:p}", self.ptr),
            RawObject::Table(_) => write!(f, "table: {:p}", self.ptr),
        }
    }
}

/// A collection of objects which need to be garbage-collected.
/// Uses a chunked arena for cache-friendly iteration during GC.
pub(crate) struct GcHeap {
    /// Chunks of slots. Old chunks are never moved, ensuring stable pointers.
    chunks: Vec<ArenaChunk>,
    /// Index of first free slot (u32::MAX = no free slots).
    free_head: u32,
    /// Number of occupied slots.
    size: usize,
    /// Total capacity across all chunks.
    capacity: usize,
    /// When the heap grows this large, run the GC.
    threshold: usize,
    /// Pool for interned strings (chunked arena + open-addressed hash table).
    strings: StringPool,
}

impl GcHeap {
    /// Create a new heap, with the given initial threshold.
    pub(super) fn with_threshold(threshold: usize) -> Self {
        Self {
            chunks: Vec::new(),
            free_head: u32::MAX,
            size: 0,
            capacity: 0,
            threshold,
            strings: StringPool::new(),
        }
    }

    /// Get a reference to a slot by global index.
    #[inline]
    fn get_slot(&self, idx: usize) -> &Slot {
        let chunk_idx = idx / CHUNK_SIZE;
        let slot_idx = idx % CHUNK_SIZE;
        &self.chunks[chunk_idx].slots[slot_idx]
    }

    /// Get a mutable reference to a slot by global index.
    #[inline]
    fn get_slot_mut(&mut self, idx: usize) -> &mut Slot {
        let chunk_idx = idx / CHUNK_SIZE;
        let slot_idx = idx % CHUNK_SIZE;
        &mut self.chunks[chunk_idx].slots[slot_idx]
    }

    /// Ensure we have at least one free slot, allocating a new chunk if needed.
    fn ensure_capacity(&mut self) {
        if self.free_head != u32::MAX {
            return; // Already have free slots
        }

        // Allocate a new chunk
        let mut chunk = ArenaChunk::new();
        let base = self.capacity;

        // Link all slots in the new chunk into the free list (in reverse order
        // so that slot 0 is at the head, giving sequential allocation)
        for i in (0..CHUNK_SIZE).rev() {
            chunk.slots[i] = Slot::Free { next_free: self.free_head };
            self.free_head = (base + i) as u32;
        }

        self.chunks.push(chunk);
        self.capacity += CHUNK_SIZE;
    }

    /// Allocate a new object slot, returning a stable pointer to it.
    fn alloc_slot(&mut self, raw: RawObject) -> ObjectPtr {
        self.ensure_capacity();

        let idx = self.free_head as usize;
        let slot = self.get_slot_mut(idx);

        // Get next free index before overwriting the slot
        let next_free = match slot {
            Slot::Free { next_free } => *next_free,
            Slot::Occupied(_) => panic!("free_head points to occupied slot"),
        };

        // Create the wrapped object and store it in the slot
        let wrapped = WrappedObject {
            raw,
            color: Cell::new(Color::Unmarked),
        };

        *slot = Slot::Occupied(wrapped);
        self.free_head = next_free;
        self.size += 1;

        // Get a stable pointer to the object (safe because chunks never move)
        let ptr = match self.get_slot(idx) {
            Slot::Occupied(obj) => NonNull::from(obj),
            Slot::Free { .. } => unreachable!(),
        };

        ObjectPtr { ptr }
    }

    /// Free a slot by index, adding it to the free list.
    fn free_slot(&mut self, idx: usize) {
        debug_assert!(idx < self.capacity, "free_slot: index {} out of bounds (capacity {})", idx, self.capacity);

        let old_free_head = self.free_head;
        let slot = self.get_slot_mut(idx);

        // Check for double-free: slot must be Occupied, not already Free
        debug_assert!(
            matches!(slot, Slot::Occupied(_)),
            "free_slot: double-free detected at index {} (slot is already free)",
            idx
        );

        *slot = Slot::Free { next_free: old_free_head };
        self.free_head = idx as u32;
        self.size -= 1;
    }

    /// Run the garbage-collector.
    /// Make sure you mark all the roots before calling this function.
    pub(super) fn collect(&mut self) {
        #[cfg(feature = "debug_gc")]
        {
            println!("Running garbage collector");
            println!("Initial size: {}", self.size);
        }

        // Sweep phase: iterate contiguously through all slots (cache-friendly)
        for idx in 0..self.capacity {
            let should_free = {
                let slot = self.get_slot(idx);
                match slot {
                    Slot::Occupied(obj) => match obj.color.get() {
                        Color::Reachable => {
                            // Reset color for next cycle
                            obj.color.set(Color::Unmarked);
                            false
                        }
                        Color::Unmarked => true,
                    },
                    Slot::Free { .. } => false,
                }
            };

            if should_free {
                self.free_slot(idx);
            }
        }

        // String collection: sweep through StringPool's chunked arena
        self.strings.collect();

        // Dynamic threshold: double the surviving size (minimum 20)
        self.threshold = (self.size * 2).max(20);

        #[cfg(feature = "debug_gc")]
        println!("Final size: {}", self.size);
    }

    #[must_use]
    pub(super) const fn is_full(&self) -> bool {
        self.size >= self.threshold
    }

    pub(super) fn new_lua_fn(
        &mut self,
        chunk: Chunk,
        upvalues: Vec<UpvalueRef>,
        mark: impl FnOnce(),
    ) -> ObjectPtr {
        if self.is_full() {
            mark();
            self.collect();
        }
        let closure = Closure { chunk: Rc::new(chunk), upvalues };
        let raw = RawObject::LuaFn(Box::new(closure));
        self.alloc_slot(raw)
    }

    pub(super) fn new_string(&mut self, s: String, mark: impl FnOnce()) -> StringPtr {
        // Check if string is already interned (fast path - no GC check needed)
        let hash = StringPool::hash_string(&s);
        if let Some(ptr) = self.strings.find_by_hash(&s, hash) {
            return ptr;
        }

        // String not found - may need to trigger GC before allocating
        if self.is_full() {
            mark();
            self.collect();
        }

        // Insert new string into pool
        self.strings.insert_with_hash(s, hash)
    }

    pub(super) fn new_table(&mut self, mark: impl FnOnce()) -> ObjectPtr {
        if self.is_full() {
            mark();
            self.collect();
        }
        let raw = RawObject::Table(Table::default());
        self.alloc_slot(raw)
    }

    // ========================================================================
    // Memory tracking and GC control (for host-controlled GC)
    // ========================================================================

    /// Number of GC-managed objects (tables and closures).
    pub(super) fn object_count(&self) -> usize {
        self.size
    }

    /// Number of interned strings.
    pub(super) fn string_count(&self) -> usize {
        self.strings.len()
    }

    /// Current GC threshold (collection triggers when object_count >= threshold).
    pub(super) fn threshold(&self) -> usize {
        self.threshold
    }

    /// Set the GC threshold. Use usize::MAX to effectively disable auto-GC.
    pub(super) fn set_threshold(&mut self, threshold: usize) {
        self.threshold = threshold;
    }

    /// Force a full garbage collection. The caller must mark roots first.
    pub(super) fn force_collect(&mut self, mark: impl FnOnce()) {
        mark();
        self.collect();
    }
}

impl Drop for GcHeap {
    fn drop(&mut self) {
        // Chunks and their slots are automatically dropped when Vec is dropped.
        // StringPool also handles its own cleanup via Drop.
    }
}

// ============================================================================
// Markable Trait (unchanged from linked-list implementation)
// ============================================================================

/// An item is `Markable` if it can be marked as reachable, and thus it and
/// anything it references will not be collected by the GC.
pub(super) trait Markable {
    /// Mark this item and the references it contains as reachable.
    fn mark_reachable(&self);
}

impl Markable for WrappedObject {
    fn mark_reachable(&self) {
        if let Color::Unmarked = self.color.get() {
            self.color.set(Color::Reachable);
            self.raw.mark_reachable();
        }
    }
}

impl Markable for RawObject {
    fn mark_reachable(&self) {
        match self {
            RawObject::LuaFn(_closure) => {
                // Upvalues are marked through State::mark_reachable via the upvalue pool.
                // The closure's upvalue indices don't need separate marking.
            }
            RawObject::Table(tbl) => tbl.mark_reachable(),
        }
    }
}

impl Markable for ObjectPtr {
    fn mark_reachable(&self) {
        self.deref().mark_reachable()
    }
}

/// This impl is mainly for any `Vec<Val>`s we use.
impl<T: Markable> Markable for [T] {
    fn mark_reachable(&self) {
        for val in self {
            val.mark_reachable();
        }
    }
}

/// This is just for the `globals` field of `State`. It can be removed once
/// globals are stored in a normal `Table`.
impl<K, V: Markable> Markable for HashMap<K, V> {
    fn mark_reachable(&self) {
        for val in self.values() {
            val.mark_reachable();
        }
    }
}

// ============================================================================
// String Pool - Chunked arena with open-addressed hash table
// ============================================================================

const STRING_POOL_CHUNK_SIZE: usize = 64;

/// Initial hash table size (must be power of 2).
const STRING_POOL_INITIAL_BUCKETS: usize = 64;

/// A string entry in the pool with cached hash for fast lookup.
struct StringEntry {
    data: String,
    hash: u64,
    color: Cell<Color>,
}

/// A slot in the string arena.
enum StringSlot {
    Occupied(StringEntry),
    Free { next_free: u32 },
}

impl Default for StringSlot {
    fn default() -> Self {
        StringSlot::Free { next_free: u32::MAX }
    }
}

/// A chunk of string slots. Once allocated, never moves.
struct StringPoolChunk {
    slots: Box<[StringSlot; STRING_POOL_CHUNK_SIZE]>,
}

impl StringPoolChunk {
    fn new() -> Self {
        Self {
            slots: Box::new(std::array::from_fn(|_| StringSlot::default())),
        }
    }
}

/// Pool for interned strings with chunked arena storage and open-addressed hash table.
///
/// Benefits over HashSet<StringPtr>:
/// - Chunked storage: better cache locality during GC sweep
/// - Open addressing with linear probing: fewer indirections than chained hashing
/// - Cached hash values: avoid rehashing strings on lookup
/// - Stable pointers: chunks never move, so StringPtr remains valid
pub(super) struct StringPool {
    /// Chunked storage. Old chunks never move, ensuring stable pointers.
    chunks: Vec<StringPoolChunk>,
    /// Head of free list (u32::MAX = no free slots).
    free_head: u32,
    /// Open-addressed hash table with linear probing.
    /// Each bucket stores a slot index (u32::MAX = empty).
    buckets: Vec<u32>,
    /// Number of interned strings.
    size: usize,
    /// Total slot capacity across all chunks.
    capacity: usize,
}

impl StringPool {
    pub(super) fn new() -> Self {
        Self {
            chunks: Vec::new(),
            free_head: u32::MAX,
            buckets: vec![u32::MAX; STRING_POOL_INITIAL_BUCKETS],
            size: 0,
            capacity: 0,
        }
    }

    /// Get a slot by global index.
    #[inline]
    fn get_slot(&self, idx: usize) -> &StringSlot {
        let chunk_idx = idx / STRING_POOL_CHUNK_SIZE;
        let slot_idx = idx % STRING_POOL_CHUNK_SIZE;
        &self.chunks[chunk_idx].slots[slot_idx]
    }

    /// Get a mutable slot by global index.
    #[inline]
    fn get_slot_mut(&mut self, idx: usize) -> &mut StringSlot {
        let chunk_idx = idx / STRING_POOL_CHUNK_SIZE;
        let slot_idx = idx % STRING_POOL_CHUNK_SIZE;
        &mut self.chunks[chunk_idx].slots[slot_idx]
    }

    /// Get the StringEntry at a slot index (panics if slot is free).
    #[inline]
    fn get_entry(&self, idx: usize) -> &StringEntry {
        match self.get_slot(idx) {
            StringSlot::Occupied(entry) => entry,
            StringSlot::Free { .. } => panic!("get_entry on free slot"),
        }
    }

    /// Ensure we have at least one free slot.
    fn ensure_capacity(&mut self) {
        if self.free_head != u32::MAX {
            return;
        }

        let mut chunk = StringPoolChunk::new();
        let base = self.capacity;

        // Link all slots into free list (reverse order so slot 0 is head)
        for i in (0..STRING_POOL_CHUNK_SIZE).rev() {
            chunk.slots[i] = StringSlot::Free { next_free: self.free_head };
            self.free_head = (base + i) as u32;
        }

        self.chunks.push(chunk);
        self.capacity += STRING_POOL_CHUNK_SIZE;
    }

    /// Compute hash of a string using FNV-1a (fast, good distribution).
    #[inline]
    pub(super) fn hash_string(s: &str) -> u64 {
        // FNV-1a hash
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in s.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    /// Find bucket index for a hash value.
    #[inline]
    fn bucket_index(&self, hash: u64) -> usize {
        (hash as usize) & (self.buckets.len() - 1)
    }

    /// Rehash the table when load factor is too high.
    fn rehash(&mut self) {
        let new_size = self.buckets.len() * 2;
        self.buckets = vec![u32::MAX; new_size];

        // Re-insert all occupied slots
        for idx in 0..self.capacity {
            if let StringSlot::Occupied(entry) = self.get_slot(idx) {
                let hash = entry.hash;
                let mut bucket = self.bucket_index(hash);

                // Linear probing to find empty bucket
                while self.buckets[bucket] != u32::MAX {
                    bucket = (bucket + 1) & (self.buckets.len() - 1);
                }
                self.buckets[bucket] = idx as u32;
            }
        }
    }

    /// Look up a string by pre-computed hash. Returns None if not found.
    /// Used to check if string exists before triggering GC.
    pub(super) fn find_by_hash(&self, s: &str, hash: u64) -> Option<StringPtr> {
        let mut bucket = self.bucket_index(hash);

        loop {
            let slot_idx = self.buckets[bucket];
            if slot_idx == u32::MAX {
                return None; // Empty bucket - string not found
            }

            let entry = self.get_entry(slot_idx as usize);
            if entry.hash == hash && entry.data == s {
                // Found existing string
                let ptr = match self.get_slot(slot_idx as usize) {
                    StringSlot::Occupied(e) => NonNull::from(e),
                    StringSlot::Free { .. } => unreachable!(),
                };
                return Some(StringPtr(ptr));
            }

            // Collision - continue probing
            bucket = (bucket + 1) & (self.buckets.len() - 1);
        }
    }

    /// Insert a string with pre-computed hash. Assumes string is not already present.
    /// Used after find_by_hash returns None and GC has been triggered if needed.
    pub(super) fn insert_with_hash(&mut self, s: String, hash: u64) -> StringPtr {
        // Allocate new slot
        self.ensure_capacity();

        let slot_idx = self.free_head as usize;
        let slot = self.get_slot_mut(slot_idx);
        let next_free = match slot {
            StringSlot::Free { next_free } => *next_free,
            StringSlot::Occupied(_) => panic!("free_head points to occupied slot"),
        };

        *slot = StringSlot::Occupied(StringEntry {
            data: s,
            hash,
            color: Cell::new(Color::Unmarked),
        });

        self.free_head = next_free;
        self.size += 1;

        // Find bucket for insertion
        let mut bucket = self.bucket_index(hash);
        while self.buckets[bucket] != u32::MAX {
            bucket = (bucket + 1) & (self.buckets.len() - 1);
        }
        self.buckets[bucket] = slot_idx as u32;

        // Rehash if load factor > 0.75
        if self.size * 4 > self.buckets.len() * 3 {
            self.rehash();
        }

        // Return pointer to the new entry
        let ptr = match self.get_slot(slot_idx) {
            StringSlot::Occupied(e) => NonNull::from(e),
            StringSlot::Free { .. } => unreachable!(),
        };
        StringPtr(ptr)
    }

    /// Look up or insert a string, returning its pointer.
    #[allow(dead_code)]
    pub(super) fn intern(&mut self, s: String) -> StringPtr {
        let hash = Self::hash_string(&s);
        if let Some(ptr) = self.find_by_hash(&s, hash) {
            return ptr;
        }
        self.insert_with_hash(s, hash)
    }

    /// Collect unreachable strings. Called during GC.
    /// Sweeps through all slots linearly (cache-friendly) and rebuilds hash table.
    pub(super) fn collect(&mut self) {
        // First pass: sweep and free unreachable strings
        for idx in 0..self.capacity {
            let should_free = {
                let slot = self.get_slot(idx);
                match slot {
                    StringSlot::Occupied(entry) => match entry.color.get() {
                        Color::Reachable => {
                            entry.color.set(Color::Unmarked);
                            false
                        }
                        Color::Unmarked => true,
                    },
                    StringSlot::Free { .. } => false,
                }
            };

            if should_free {
                // Free the slot - should_free is only true for Occupied slots,
                // but add debug assertion to catch any logic errors
                debug_assert!(
                    matches!(self.get_slot(idx), StringSlot::Occupied(_)),
                    "StringPool::collect: freeing already-free slot at index {}",
                    idx
                );

                let old_free_head = self.free_head;
                let slot = self.get_slot_mut(idx);
                *slot = StringSlot::Free { next_free: old_free_head };
                self.free_head = idx as u32;
                self.size -= 1;
            }
        }

        // Second pass: rebuild hash table with survivors
        self.buckets.fill(u32::MAX);
        for idx in 0..self.capacity {
            if let StringSlot::Occupied(entry) = self.get_slot(idx) {
                let hash = entry.hash;
                let mut bucket = self.bucket_index(hash);

                while self.buckets[bucket] != u32::MAX {
                    bucket = (bucket + 1) & (self.buckets.len() - 1);
                }
                self.buckets[bucket] = idx as u32;
            }
        }
    }

    /// Number of interned strings.
    pub(super) fn len(&self) -> usize {
        self.size
    }
}

impl Drop for StringPool {
    fn drop(&mut self) {
        // Slots are automatically dropped when chunks are dropped.
        // String data in each StringEntry is also dropped automatically.
    }
}

/// Wrapper type around a pointer to a StringEntry.
/// Behaves like a String for the purposes of Hashing and Equality.
#[derive(Clone, Debug)]
pub(crate) struct StringPtr(NonNull<StringEntry>);

impl StringPtr {
    #[must_use]
    pub(crate) fn eq_physical(a: &Self, b: &Self) -> bool {
        a.0 == b.0
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &(unsafe { self.0.as_ref() }).data
    }
}

impl Borrow<str> for StringPtr {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl Hash for StringPtr {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Use cached hash for speed
        let entry = unsafe { self.0.as_ref() };
        entry.hash.hash(state);
    }
}

impl PartialEq for StringPtr {
    fn eq(&self, other: &Self) -> bool {
        // Fast path: pointer equality (same interned string)
        if self.0 == other.0 {
            return true;
        }
        // Slow path: compare contents
        self.as_str() == other.as_str()
    }
}
impl Eq for StringPtr {}

impl Markable for StringPtr {
    fn mark_reachable(&self) {
        unsafe { self.0.as_ref().color.set(Color::Reachable) }
    }
}

impl fmt::Display for StringPtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn gc_test_strings() {
        let mut gc = GcHeap::with_threshold(2000);
        gc.new_string("good".into(), || {});
        assert_eq!(1, gc.strings.len());
        gc.new_string("good".into(), || {});
        assert_eq!(1, gc.strings.len());
        gc.new_string("jorts".into(), || {});
        assert_eq!(2, gc.strings.len());
        gc.collect();
        assert_eq!(0, gc.strings.len());
    }

    #[test]
    fn arena_alloc_and_collect() {
        let mut gc = GcHeap::with_threshold(100);

        // Allocate some tables
        let t1 = gc.new_table(|| {});
        let _t2 = gc.new_table(|| {});
        let _t3 = gc.new_table(|| {});

        assert_eq!(gc.size, 3);

        // Mark t1 as reachable
        t1.mark_reachable();

        // Collect - should free t2 and t3
        gc.collect();

        assert_eq!(gc.size, 1);

        // Can still use t1
        assert!(t1.as_table_ref().is_some());
    }

    #[test]
    fn arena_free_list_reuse() {
        let mut gc = GcHeap::with_threshold(100);

        // Allocate and free
        let _t1 = gc.new_table(|| {});
        assert_eq!(gc.size, 1);

        gc.collect(); // t1 is unreachable, gets freed

        assert_eq!(gc.size, 0);
        assert!(gc.free_head != u32::MAX); // Free list has entry

        // Allocate again - should reuse the slot
        let _t2 = gc.new_table(|| {});
        assert_eq!(gc.size, 1);
    }

    #[test]
    fn arena_chunk_growth() {
        let mut gc = GcHeap::with_threshold(1000);

        // Allocate more than one chunk's worth
        for _ in 0..(CHUNK_SIZE + 10) {
            gc.new_table(|| {});
        }

        assert_eq!(gc.chunks.len(), 2);
        assert_eq!(gc.size, CHUNK_SIZE + 10);
        assert_eq!(gc.capacity, CHUNK_SIZE * 2);
    }

    #[test]
    fn string_pool_interning() {
        let mut pool = StringPool::new();

        // Intern same string twice - should return same pointer
        let ptr1 = pool.intern("hello".into());
        let ptr2 = pool.intern("hello".into());
        assert!(StringPtr::eq_physical(&ptr1, &ptr2));
        assert_eq!(pool.len(), 1);

        // Different string - different pointer
        let ptr3 = pool.intern("world".into());
        assert!(!StringPtr::eq_physical(&ptr1, &ptr3));
        assert_eq!(pool.len(), 2);

        // Verify string content
        assert_eq!(ptr1.as_str(), "hello");
        assert_eq!(ptr3.as_str(), "world");
    }

    #[test]
    fn string_pool_hash_table_growth() {
        let mut pool = StringPool::new();

        // Insert enough strings to trigger rehash (load factor > 0.75)
        // Initial bucket size is 64, so we need > 48 strings
        for i in 0..100 {
            pool.intern(format!("string_{}", i));
        }

        assert_eq!(pool.len(), 100);

        // Verify all strings can still be found
        for i in 0..100 {
            let s = format!("string_{}", i);
            let ptr1 = pool.intern(s.clone());
            let ptr2 = pool.intern(s);
            assert!(StringPtr::eq_physical(&ptr1, &ptr2));
        }

        // Size should still be 100 (no duplicates added)
        assert_eq!(pool.len(), 100);
    }

    #[test]
    fn string_pool_collect() {
        let mut pool = StringPool::new();

        // Intern some strings
        let kept = pool.intern("kept".into());
        let _dropped = pool.intern("dropped".into());
        assert_eq!(pool.len(), 2);

        // Mark one as reachable
        kept.mark_reachable();

        // Collect - should free the unreachable one
        pool.collect();
        assert_eq!(pool.len(), 1);

        // Can still access the kept string
        assert_eq!(kept.as_str(), "kept");

        // Intern the same string again - should return same pointer
        let kept2 = pool.intern("kept".into());
        assert!(StringPtr::eq_physical(&kept, &kept2));
        assert_eq!(pool.len(), 1);

        // Intern the previously dropped string - should get new slot
        let new_dropped = pool.intern("dropped".into());
        assert_eq!(new_dropped.as_str(), "dropped");
        assert_eq!(pool.len(), 2);
    }
}
