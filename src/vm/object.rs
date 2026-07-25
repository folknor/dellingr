//! An `Object` is some data which:
//! - Has an unknown lifetime
//! - May have references to other `Object`s
//!
//! Because of this, it needs to be garbage collected.
//!
//! This implementation uses slotmap for safe generational arena storage.
//! Each ObjectPtr contains a generation that is validated on access,
//! preventing use-after-free bugs.

use indexmap::IndexMap;
use std::cell::Cell;
use std::fmt;
use std::sync::Arc;

use slotmap::{SlotMap, new_key_type};

use super::Bytecode;
use super::LuaType;
use super::RuntimeCaches;
use super::Table;
use super::Val;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UpvalueRef(u32);

impl UpvalueRef {
    pub(crate) fn new(idx: u32) -> Self {
        Self(idx)
    }

    pub(crate) fn index(self) -> usize {
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

    #[cfg(feature = "snapshot")]
    pub(super) fn alloc_closed_nil(&mut self) -> UpvalueRef {
        self.alloc(Upvalue::Closed(Val::Nil))
    }

    #[cfg(feature = "snapshot")]
    pub(super) fn set_closed(&mut self, uv_ref: UpvalueRef, val: Val) {
        self.slots[uv_ref.index()] = Upvalue::Closed(val);
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
}

// ============================================================================
// Object Types
// ============================================================================

/// A Lua closure: a function with captured upvalues.
///
/// `bytecode` is the immutable, `Arc`-shareable compiled code. `caches` are
/// the per-execution lookup caches; they share an `Arc` between this closure
/// and any frames currently executing it (recursive calls all see each
/// other's cache writes through the shared `Arc`). Both Arcs are cheap to
/// clone since they only refcount.
#[derive(Clone, Debug)]
pub(super) struct Closure {
    pub(super) bytecode: Arc<Bytecode>,
    pub(super) caches: Arc<RuntimeCaches>,
    pub(super) upvalues: Vec<UpvalueRef>,
}

/// The raw object data managed by GC.
pub(super) enum RawObject {
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Color {
    Unmarked,
    Reachable,
}

/// A GC-managed object with its metadata.
pub(super) struct WrappedObject {
    pub(super) raw: RawObject,
    pub(super) color: Cell<Color>,
}

// ============================================================================
// ObjectPtr - Safe generational key
// ============================================================================

// Define our own key types for the slotmap
new_key_type! {
    pub struct ObjectKey;
}

new_key_type! {
    pub struct StringKey;
}

/// A safe pointer to a GC-managed object.
/// Uses slotmap's generational indices to prevent use-after-free:
/// - Each key contains a generation number
/// - When a slot is freed and reused, the generation increments
/// - Accessing with an old key panics instead of causing memory corruption
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ObjectPtr(pub(crate) ObjectKey);

impl ObjectPtr {
    /// Get the type of this object.
    pub(super) fn typ(self, heap: &GcHeap) -> LuaType {
        heap.get(self).raw.typ()
    }
}

// ============================================================================
// GcHeap - SlotMap-based garbage collected heap
// ============================================================================

/// A collection of objects which need to be garbage-collected.
/// Uses slotmap for safe, generational access to objects.
pub(crate) struct GcHeap {
    /// SlotMap storing all GC-managed objects.
    objects: SlotMap<ObjectKey, WrappedObject>,
    /// When total heap allocations (objects plus distinct interned strings)
    /// reach this threshold, run the GC.
    threshold: usize,
    /// Pool for interned strings.
    strings: StringPool,
    /// Reused scratch space for the iterative GC mark phase.
    mark_worklist: Vec<ObjectPtr>,
}

impl GcHeap {
    /// Create a new heap, with the given initial threshold.
    pub(super) fn with_threshold(threshold: usize) -> Self {
        Self {
            objects: SlotMap::with_key(),
            threshold,
            strings: StringPool::new(),
            mark_worklist: Vec::new(),
        }
    }

    pub(super) fn reserve(&mut self, additional_objects: usize, additional_strings: usize) {
        self.objects.reserve(additional_objects);
        self.strings.reserve(additional_strings);
    }

    // ========================================================================
    // Object access - these replace ObjectPtr's self-dereferencing methods
    // ========================================================================

    /// Get a reference to the wrapped object.
    /// Panics if the key is invalid (use-after-free detection).
    #[inline]
    pub(super) fn get(&self, ptr: ObjectPtr) -> &WrappedObject {
        self.objects
            .get(ptr.0)
            .expect("Invalid ObjectPtr: object was freed (use-after-free detected)")
    }

    /// Get a mutable reference to the wrapped object.
    /// Panics if the key is invalid (use-after-free detection).
    #[inline]
    pub(super) fn get_mut(&mut self, ptr: ObjectPtr) -> &mut WrappedObject {
        self.objects
            .get_mut(ptr.0)
            .expect("Invalid ObjectPtr: object was freed (use-after-free detected)")
    }

    /// Get the object as a Lua function (closure), if it is one.
    pub(super) fn as_lua_function(&self, ptr: ObjectPtr) -> Option<Closure> {
        match &self.get(ptr).raw {
            RawObject::LuaFn(closure) => Some((**closure).clone()),
            _ => None,
        }
    }

    /// Get a mutable reference to the object as a table, if it is one.
    pub(super) fn as_table(&mut self, ptr: ObjectPtr) -> Option<&mut Table> {
        match &mut self.get_mut(ptr).raw {
            RawObject::Table(t) => Some(t),
            _ => None,
        }
    }

    /// Get an immutable reference to the object as a table, if it is one.
    pub(super) fn as_table_ref(&self, ptr: ObjectPtr) -> Option<&Table> {
        match &self.get(ptr).raw {
            RawObject::Table(t) => Some(t),
            _ => None,
        }
    }

    /// Get a string's content by its pointer.
    /// Panics if the string was freed (use-after-free detection).
    pub(super) fn get_string(&self, ptr: StringPtr) -> &[u8] {
        self.strings.get(ptr)
    }

    // ========================================================================
    // Allocation
    // ========================================================================

    /// Allocate a new Lua function.
    /// Note: Caller must check is_full() and run GC if needed before calling.
    #[hotpath::measure]
    pub(super) fn alloc_lua_fn(
        &mut self,
        bytecode: Arc<Bytecode>,
        upvalues: Vec<UpvalueRef>,
    ) -> ObjectPtr {
        let caches = Arc::new(RuntimeCaches::new(&bytecode));
        let closure = Closure {
            bytecode,
            caches,
            upvalues,
        };
        let raw = RawObject::LuaFn(Box::new(closure));
        let wrapped = WrappedObject {
            raw,
            color: Cell::new(Color::Unmarked),
        };
        ObjectPtr(self.objects.insert(wrapped))
    }

    /// Allocate a new table.
    /// Note: Caller must check is_full() and run GC if needed before calling.
    #[hotpath::measure]
    pub(super) fn alloc_table(&mut self) -> ObjectPtr {
        let raw = RawObject::Table(Table::default());
        let wrapped = WrappedObject {
            raw,
            color: Cell::new(Color::Unmarked),
        };
        ObjectPtr(self.objects.insert(wrapped))
    }

    pub(super) fn alloc_table_with_capacity(&mut self, capacity: usize) -> ObjectPtr {
        let raw = RawObject::Table(Table::with_capacity(capacity));
        let wrapped = WrappedObject {
            raw,
            color: Cell::new(Color::Unmarked),
        };
        ObjectPtr(self.objects.insert(wrapped))
    }

    pub(super) fn alloc_table_with_template(
        &mut self,
        key_ids: &[u8],
        string_literals: &[Val],
        string_literal_start: usize,
    ) -> ObjectPtr {
        let raw = RawObject::Table(Table::with_template_keys(
            key_ids,
            string_literals,
            string_literal_start,
        ));
        let wrapped = WrappedObject {
            raw,
            color: Cell::new(Color::Unmarked),
        };
        ObjectPtr(self.objects.insert(wrapped))
    }

    /// Allocate or intern a string.
    /// Note: Caller must check is_full() and run GC if needed before calling.
    #[hotpath::measure]
    pub(super) fn alloc_string(&mut self, bytes: &[u8]) -> StringPtr {
        let hash = StringPool::hash_string(bytes);
        if let Some(ptr) = self.strings.find_by_hash(bytes, hash) {
            return ptr;
        }
        self.strings.insert_with_hash(bytes.into(), hash)
    }

    // ========================================================================
    // Garbage Collection
    // ========================================================================

    /// Check if GC should run.
    #[must_use]
    pub(super) fn is_full(&self) -> bool {
        self.allocation_count() >= self.threshold
    }

    /// Mark an object as reachable. Call this for all root objects.
    /// The upvalue_pool is needed to mark closed upvalues referenced by closures.
    #[hotpath::measure]
    pub(super) fn mark(&self, ptr: ObjectPtr, worklist: &mut Vec<ObjectPtr>) {
        if let Some(obj) = self.objects.get(ptr.0)
            && obj.color.get() == Color::Unmarked
        {
            obj.color.set(Color::Reachable);
            worklist.push(ptr);
        }
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
            self.mark_children(self.get(ptr), upvalue_pool, worklist);
        }
    }

    /// Mark a string as reachable.
    pub(super) fn mark_string(&self, ptr: StringPtr) {
        self.strings.mark(ptr);
    }

    /// Mark objects referenced by this object.
    #[hotpath::measure]
    fn mark_children(
        &self,
        obj: &WrappedObject,
        upvalue_pool: &UpvaluePool,
        worklist: &mut Vec<ObjectPtr>,
    ) {
        match &obj.raw {
            RawObject::LuaFn(closure) => {
                // Mark values stored in closed upvalues
                for uv_ref in &closure.upvalues {
                    if let Upvalue::Closed(val) = upvalue_pool.get(*uv_ref) {
                        val.mark_reachable(self, worklist);
                    }
                }
            }
            RawObject::Table(tbl) => {
                tbl.mark_values(self, worklist);
            }
        }
    }

    /// Run the garbage collector sweep phase.
    /// Call this after marking all roots.
    #[hotpath::measure(label = "object::heap_collect")]
    pub(super) fn collect(&mut self) {
        #[cfg(feature = "debug_gc")]
        {
            println!("Running garbage collector");
            println!("Initial size: {}", self.objects.len());
        }

        // Sweep phase: remove unmarked objects
        self.objects.retain(|_, obj| match obj.color.get() {
            Color::Reachable => {
                obj.color.set(Color::Unmarked);
                true
            }
            Color::Unmarked => false,
        });

        // String collection
        self.strings.collect();

        // Dynamic threshold: double the surviving allocation count (min 20).
        // A usize::MAX threshold is the auto-GC-disabled sentinel
        // (gc_disable_auto / gc_set_threshold(usize::MAX)); preserve it so an
        // explicit collect does not silently re-enable automatic GC.
        if self.threshold != usize::MAX {
            self.threshold = self.allocation_count().saturating_mul(2).max(20);
        }

        #[cfg(feature = "debug_gc")]
        println!("Final size: {}", self.objects.len());
    }

    // ========================================================================
    // Memory tracking
    // ========================================================================

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
        self.threshold
    }

    /// Set the GC threshold.
    pub(super) fn set_threshold(&mut self, threshold: usize) {
        self.threshold = threshold;
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
// String Pool - SlotMap-based with generational indices for safety
// ============================================================================

/// Entry for an interned string.
struct StringEntry {
    data: Box<[u8]>,
    hash: u64,
    color: Cell<Color>,
}

/// A safe pointer to an interned string.
/// Uses slotmap's generational indices to prevent use-after-free:
/// - Each key contains a generation number
/// - When a slot is freed and reused, the generation increments
/// - Accessing with an old key panics instead of causing memory corruption
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct StringPtr(StringKey);

impl fmt::Display for StringPtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Without heap access, we can only show the key
        write!(f, "string: {:?}", self.0)
    }
}

/// String pool using SlotMap for safe generational access.
/// Provides string interning with use-after-free detection.
pub(crate) struct StringPool {
    /// SlotMap storing all interned strings.
    strings: SlotMap<StringKey, StringEntry>,
    /// Hash -> bucket of StringKeys for O(1) interner lookup.
    /// Multiple keys per bucket only on hash collision; the typical bucket
    /// holds a single key. Iterated only during GC sweep, never during a
    /// program-visible operation, so non-deterministic IndexMap iteration
    /// order would not affect program output - but we still use IndexMap
    /// over HashMap to keep determinism honest across hosts.
    hash_index: IndexMap<u64, Vec<StringKey>>,
}

impl StringPool {
    fn new() -> Self {
        Self {
            strings: SlotMap::with_key(),
            hash_index: IndexMap::new(),
        }
    }

    fn reserve(&mut self, additional: usize) {
        self.strings.reserve(additional);
        self.hash_index.reserve(additional);
    }

    pub(super) fn len(&self) -> usize {
        self.strings.len()
    }

    pub(super) fn hash_string(bytes: &[u8]) -> u64 {
        const FX_HASH_MUL: u64 = 0x517cc1b727220a95;

        #[inline]
        fn mix(hash: u64, word: u64) -> u64 {
            (hash.rotate_left(5) ^ word).wrapping_mul(FX_HASH_MUL)
        }

        let mut hash = bytes.len() as u64;
        let (chunks, remainder) = bytes.as_chunks::<8>();
        for chunk in chunks {
            let word = u64::from_le_bytes(*chunk);
            hash = mix(hash, word);
        }

        let mut tail = 0u64;
        for (i, byte) in remainder.iter().enumerate() {
            tail |= u64::from(*byte) << (i * 8);
        }
        if !remainder.is_empty() {
            hash = mix(hash, tail);
        }
        hash
    }

    /// Get a string's content by its pointer.
    /// Panics if the string was freed (use-after-free detection).
    pub(super) fn get(&self, ptr: StringPtr) -> &[u8] {
        &self
            .strings
            .get(ptr.0)
            .expect("Invalid StringPtr: string was freed (use-after-free detected)")
            .data
    }

    /// Find an existing interned string by content and hash.
    /// O(1) via hash_index; only scans within a hash bucket on collision.
    #[hotpath::measure]
    pub(super) fn find_by_hash(&self, bytes: &[u8], hash: u64) -> Option<StringPtr> {
        let bucket = self.hash_index.get(&hash)?;
        for key in bucket {
            if let Some(entry) = self.strings.get(*key)
                && entry.data.as_ref() == bytes
            {
                return Some(StringPtr(*key));
            }
        }
        None
    }

    /// Insert a new string with precomputed hash. Updates the hash index.
    #[hotpath::measure]
    pub(super) fn insert_with_hash(&mut self, bytes: Box<[u8]>, hash: u64) -> StringPtr {
        let entry = StringEntry {
            data: bytes,
            hash,
            color: Cell::new(Color::Unmarked),
        };
        let key = self.strings.insert(entry);
        self.hash_index.entry(hash).or_default().push(key);
        StringPtr(key)
    }

    /// Mark a string as reachable.
    pub(super) fn mark(&self, ptr: StringPtr) {
        if let Some(entry) = self.strings.get(ptr.0) {
            entry.color.set(Color::Reachable);
        }
    }

    /// Collect unreachable strings.
    #[hotpath::measure(label = "object::string_pool_collect")]
    pub(super) fn collect(&mut self) {
        let mut removed: Vec<(StringKey, u64)> = Vec::new();
        self.strings.retain(|key, entry| match entry.color.get() {
            Color::Reachable => {
                entry.color.set(Color::Unmarked);
                true
            }
            Color::Unmarked => {
                removed.push((key, entry.hash));
                false
            }
        });
        // Drop dead keys from hash_index buckets, and drop empty buckets.
        for (key, hash) in removed {
            if let Some(bucket) = self.hash_index.get_mut(&hash) {
                bucket.retain(|k| *k != key);
                if bucket.is_empty() {
                    self.hash_index.shift_remove(&hash);
                }
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_allocation() {
        let mut heap = GcHeap::with_threshold(100);
        let t1 = heap.alloc_table();
        let t2 = heap.alloc_table();

        assert!(heap.as_table_ref(t1).is_some());
        assert!(heap.as_table_ref(t2).is_some());
        assert_eq!(heap.object_count(), 2);
    }

    #[test]
    fn test_gc_collect() {
        let mut heap = GcHeap::with_threshold(100);
        let kept = heap.alloc_table();
        let _freed = heap.alloc_table();

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
        let mut heap = GcHeap::with_threshold(100);
        let mut root = heap.alloc_table();
        for _ in 0..100_000 {
            let next = heap.alloc_table();
            heap.as_table(next)
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
    }

    #[test]
    #[should_panic(expected = "use-after-free")]
    fn test_use_after_free_detection() {
        let mut heap = GcHeap::with_threshold(100);
        let ptr = heap.alloc_table();

        // Don't mark it, then collect
        heap.collect();

        // This should panic with use-after-free detection
        let _ = heap.as_table_ref(ptr);
    }

    #[test]
    fn test_string_allocation() {
        let mut heap = GcHeap::with_threshold(100);
        let s1 = heap.alloc_string(b"hello");
        let s2 = heap.alloc_string(b"world");
        let s3 = heap.alloc_string(b"hello"); // Should return same ptr as s1 (interned)

        assert_eq!(heap.get_string(s1), b"hello");
        assert_eq!(heap.get_string(s2), b"world");
        assert_eq!(s1, s3); // Same string should be interned
        assert_eq!(heap.string_count(), 2);
    }

    #[test]
    fn test_string_hash_is_pinned() {
        assert_eq!(StringPool::hash_string(b""), 0x0000000000000000);
        assert_eq!(StringPool::hash_string(b"hello"), 0xd76e0ef553a10d68);
    }

    #[test]
    fn test_string_gc_collect() {
        let mut heap = GcHeap::with_threshold(100);
        let kept = heap.alloc_string(b"keep");
        let _freed = heap.alloc_string(b"free");

        // Mark only the first string
        heap.mark_string(kept);
        heap.collect();

        // First string should survive
        assert_eq!(heap.get_string(kept), b"keep");
        assert_eq!(heap.string_count(), 1);
    }

    #[test]
    #[should_panic(expected = "use-after-free")]
    fn test_string_use_after_free_detection() {
        let mut heap = GcHeap::with_threshold(100);
        let ptr = heap.alloc_string(b"test");

        // Don't mark it, then collect
        heap.collect();

        // This should panic with use-after-free detection
        let _ = heap.get_string(ptr);
    }
}
