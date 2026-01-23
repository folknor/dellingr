//! An `Object` is some data which:
//! - Has an unknown lifetime
//! - May have references to other `Object`s
//!
//! Because of this, it needs to be garbage collected.
//!
//! This implementation uses slotmap for safe generational arena storage.
//! Each ObjectPtr contains a generation that is validated on access,
//! preventing use-after-free bugs.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::rc::Rc;

use slotmap::{new_key_type, SlotMap};

use super::Chunk;
use super::LuaType;
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
}

// ============================================================================
// Object Types
// ============================================================================

/// A Lua closure: a function with captured upvalues.
#[derive(Clone, Debug)]
pub(super) struct Closure {
    pub(super) chunk: Rc<Chunk>,
    pub(super) upvalues: Vec<UpvalueRef>,
}

/// The raw object data managed by GC.
pub(crate) enum RawObject {
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
pub(crate) struct WrappedObject {
    pub(crate) raw: RawObject,
    pub(crate) color: Cell<Color>,
}

// ============================================================================
// ObjectPtr - Safe generational key
// ============================================================================

// Define our own key type for the slotmap
new_key_type! {
    pub struct ObjectKey;
}

/// A safe pointer to a GC-managed object.
/// Uses slotmap's generational indices to prevent use-after-free:
/// - Each key contains a generation number
/// - When a slot is freed and reused, the generation increments
/// - Accessing with an old key panics instead of causing memory corruption
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ObjectPtr(pub(crate) ObjectKey);

impl ObjectPtr {
    /// Get the type of this object.
    pub(super) fn typ(self, heap: &GcHeap) -> LuaType {
        heap.get(self).raw.typ()
    }
}

impl fmt::Display for ObjectPtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // We can't display the actual content without heap access,
        // so just show the key debug representation
        write!(f, "object: {:?}", self.0)
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
    /// When the heap grows this large, run the GC.
    threshold: usize,
    /// Pool for interned strings.
    strings: StringPool,
}

impl GcHeap {
    /// Create a new heap, with the given initial threshold.
    pub(super) fn with_threshold(threshold: usize) -> Self {
        Self {
            objects: SlotMap::with_key(),
            threshold,
            strings: StringPool::new(),
        }
    }

    // ========================================================================
    // Object access - these replace ObjectPtr's self-dereferencing methods
    // ========================================================================

    /// Get a reference to the wrapped object.
    /// Panics if the key is invalid (use-after-free detection).
    #[inline]
    pub(crate) fn get(&self, ptr: ObjectPtr) -> &WrappedObject {
        self.objects.get(ptr.0).expect("Invalid ObjectPtr: object was freed (use-after-free detected)")
    }

    /// Get a mutable reference to the wrapped object.
    /// Panics if the key is invalid (use-after-free detection).
    #[inline]
    pub(crate) fn get_mut(&mut self, ptr: ObjectPtr) -> &mut WrappedObject {
        self.objects.get_mut(ptr.0).expect("Invalid ObjectPtr: object was freed (use-after-free detected)")
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

    // ========================================================================
    // Allocation
    // ========================================================================

    /// Allocate a new Lua function.
    /// Note: Caller must check is_full() and run GC if needed before calling.
    pub(super) fn alloc_lua_fn(&mut self, chunk: Chunk, upvalues: Vec<UpvalueRef>) -> ObjectPtr {
        let closure = Closure { chunk: Rc::new(chunk), upvalues };
        let raw = RawObject::LuaFn(Box::new(closure));
        let wrapped = WrappedObject {
            raw,
            color: Cell::new(Color::Unmarked),
        };
        ObjectPtr(self.objects.insert(wrapped))
    }

    /// Allocate a new table.
    /// Note: Caller must check is_full() and run GC if needed before calling.
    pub(super) fn alloc_table(&mut self) -> ObjectPtr {
        let raw = RawObject::Table(Table::default());
        let wrapped = WrappedObject {
            raw,
            color: Cell::new(Color::Unmarked),
        };
        ObjectPtr(self.objects.insert(wrapped))
    }

    /// Allocate or intern a string.
    /// Note: Caller must check is_full() and run GC if needed before calling.
    pub(super) fn alloc_string(&mut self, s: String) -> StringPtr {
        let hash = StringPool::hash_string(&s);
        if let Some(ptr) = self.strings.find_by_hash(&s, hash) {
            return ptr;
        }
        self.strings.insert_with_hash(s, hash)
    }

    // ========================================================================
    // Garbage Collection
    // ========================================================================

    /// Check if GC should run.
    #[must_use]
    pub(super) fn is_full(&self) -> bool {
        self.objects.len() >= self.threshold
    }

    /// Mark an object as reachable. Call this for all root objects.
    pub(super) fn mark(&self, ptr: ObjectPtr) {
        if let Some(obj) = self.objects.get(ptr.0) {
            if obj.color.get() == Color::Unmarked {
                obj.color.set(Color::Reachable);
                // Recursively mark objects referenced by this object
                self.mark_children(obj);
            }
        }
    }

    /// Mark a string as reachable.
    pub(super) fn mark_string(&self, ptr: StringPtr) {
        self.strings.mark(ptr);
    }

    /// Mark objects referenced by this object.
    fn mark_children(&self, obj: &WrappedObject) {
        match &obj.raw {
            RawObject::LuaFn(_) => {
                // Upvalues are marked separately through the upvalue pool
            }
            RawObject::Table(tbl) => {
                tbl.mark_values(self);
            }
        }
    }

    /// Run the garbage collector sweep phase.
    /// Call this after marking all roots.
    pub(super) fn collect(&mut self) {
        #[cfg(feature = "debug_gc")]
        {
            println!("Running garbage collector");
            println!("Initial size: {}", self.objects.len());
        }

        // Sweep phase: remove unmarked objects
        self.objects.retain(|_, obj| {
            match obj.color.get() {
                Color::Reachable => {
                    obj.color.set(Color::Unmarked);
                    true
                }
                Color::Unmarked => false,
            }
        });

        // String collection
        self.strings.collect();

        // Dynamic threshold: double the surviving size (minimum 20)
        self.threshold = (self.objects.len() * 2).max(20);

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
    fn mark_reachable(&self, heap: &GcHeap);
}

impl Markable for Val {
    fn mark_reachable(&self, heap: &GcHeap) {
        match self {
            Val::Obj(ptr) => heap.mark(*ptr),
            Val::Str(ptr) => heap.mark_string(*ptr),
            _ => (),
        }
    }
}

impl<T: Markable> Markable for [T] {
    fn mark_reachable(&self, heap: &GcHeap) {
        for val in self {
            val.mark_reachable(heap);
        }
    }
}

impl<K, V: Markable> Markable for HashMap<K, V> {
    fn mark_reachable(&self, heap: &GcHeap) {
        for val in self.values() {
            val.mark_reachable(heap);
        }
    }
}

// ============================================================================
// String Pool (unchanged except for mark method)
// ============================================================================

const STRING_POOL_CHUNK_SIZE: usize = 64;
const STRING_POOL_INITIAL_BUCKETS: usize = 64;

struct StringEntry {
    data: String,
    hash: u64,
    color: Cell<Color>,
}

enum StringSlot {
    Occupied(StringEntry),
    Free { next_free: u32 },
}

impl Default for StringSlot {
    fn default() -> Self {
        StringSlot::Free { next_free: u32::MAX }
    }
}

struct StringChunk {
    slots: Box<[StringSlot; STRING_POOL_CHUNK_SIZE]>,
}

impl StringChunk {
    fn new() -> Self {
        Self {
            slots: Box::new(std::array::from_fn(|_| StringSlot::default())),
        }
    }
}

/// A pointer to an interned string.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StringPtr(std::ptr::NonNull<StringEntry>);

impl StringPtr {
    pub fn as_str(&self) -> &str {
        unsafe { &self.0.as_ref().data }
    }
}

impl std::ops::Deref for StringPtr {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for StringPtr {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for StringPtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub(crate) struct StringPool {
    chunks: Vec<StringChunk>,
    free_head: u32,
    size: usize,
    capacity: usize,
    buckets: Vec<u32>,
    bucket_mask: usize,
}

impl StringPool {
    fn new() -> Self {
        Self {
            chunks: Vec::new(),
            free_head: u32::MAX,
            size: 0,
            capacity: 0,
            buckets: vec![u32::MAX; STRING_POOL_INITIAL_BUCKETS],
            bucket_mask: STRING_POOL_INITIAL_BUCKETS - 1,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.size
    }

    pub(super) fn hash_string(s: &str) -> u64 {
        use std::hash::Hasher;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut hasher);
        hasher.finish()
    }

    fn get_slot(&self, idx: usize) -> &StringSlot {
        let chunk_idx = idx / STRING_POOL_CHUNK_SIZE;
        let slot_idx = idx % STRING_POOL_CHUNK_SIZE;
        &self.chunks[chunk_idx].slots[slot_idx]
    }

    fn get_slot_mut(&mut self, idx: usize) -> &mut StringSlot {
        let chunk_idx = idx / STRING_POOL_CHUNK_SIZE;
        let slot_idx = idx % STRING_POOL_CHUNK_SIZE;
        &mut self.chunks[chunk_idx].slots[slot_idx]
    }

    fn ensure_capacity(&mut self) {
        if self.free_head != u32::MAX {
            return;
        }

        let mut chunk = StringChunk::new();
        let base = self.capacity;

        for i in (0..STRING_POOL_CHUNK_SIZE).rev() {
            chunk.slots[i] = StringSlot::Free { next_free: self.free_head };
            self.free_head = (base + i) as u32;
        }

        self.chunks.push(chunk);
        self.capacity += STRING_POOL_CHUNK_SIZE;
    }

    fn alloc_slot(&mut self, entry: StringEntry) -> (u32, StringPtr) {
        self.ensure_capacity();

        let idx = self.free_head as usize;
        let slot = self.get_slot_mut(idx);

        let next_free = match slot {
            StringSlot::Free { next_free } => *next_free,
            StringSlot::Occupied(_) => panic!("free_head points to occupied slot"),
        };

        *slot = StringSlot::Occupied(entry);
        self.free_head = next_free;
        self.size += 1;

        let ptr = match self.get_slot(idx) {
            StringSlot::Occupied(e) => StringPtr(std::ptr::NonNull::from(e)),
            StringSlot::Free { .. } => unreachable!(),
        };

        (idx as u32, ptr)
    }

    fn free_slot(&mut self, idx: usize) {
        let old_free_head = self.free_head;
        let slot = self.get_slot_mut(idx);
        *slot = StringSlot::Free { next_free: old_free_head };
        self.free_head = idx as u32;
        self.size -= 1;
    }

    pub(super) fn find_by_hash(&self, s: &str, hash: u64) -> Option<StringPtr> {
        let bucket = (hash as usize) & self.bucket_mask;
        let mut idx = self.buckets[bucket];

        while idx != u32::MAX {
            if let StringSlot::Occupied(entry) = self.get_slot(idx as usize) {
                if entry.hash == hash && entry.data == s {
                    return Some(StringPtr(std::ptr::NonNull::from(entry)));
                }
            }
            idx = idx.wrapping_add(1);
            if idx as usize >= self.capacity {
                idx = 0;
            }
            if idx == self.buckets[bucket] {
                break;
            }
        }
        None
    }

    pub(super) fn insert_with_hash(&mut self, s: String, hash: u64) -> StringPtr {
        if self.size * 2 >= self.buckets.len() {
            self.rehash();
        }

        let entry = StringEntry {
            data: s,
            hash,
            color: Cell::new(Color::Unmarked),
        };
        let (idx, ptr) = self.alloc_slot(entry);

        let bucket = (hash as usize) & self.bucket_mask;
        if self.buckets[bucket] == u32::MAX {
            self.buckets[bucket] = idx;
        }

        ptr
    }

    fn rehash(&mut self) {
        let new_size = self.buckets.len() * 2;
        self.buckets = vec![u32::MAX; new_size];
        self.bucket_mask = new_size - 1;

        for idx in 0..self.capacity {
            if let StringSlot::Occupied(entry) = self.get_slot(idx) {
                let bucket = (entry.hash as usize) & self.bucket_mask;
                if self.buckets[bucket] == u32::MAX {
                    self.buckets[bucket] = idx as u32;
                }
            }
        }
    }

    /// Mark a string as reachable.
    pub(super) fn mark(&self, ptr: StringPtr) {
        unsafe {
            ptr.0.as_ref().color.set(Color::Reachable);
        }
    }

    pub(super) fn collect(&mut self) {
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
                self.free_slot(idx);
            }
        }
        // Note: We don't rehash here. Rehashing is only done in insert_with_hash
        // when the load factor gets too high. Rehashing after every GC would
        // cause exponential bucket growth since rehash() doubles the bucket array.
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
        heap.mark(kept);
        heap.collect();

        // First table should survive, second should be freed
        assert!(heap.as_table_ref(kept).is_some());
        assert_eq!(heap.object_count(), 1);
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
}
