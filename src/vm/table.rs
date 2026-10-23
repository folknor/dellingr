use std::cell::Cell;
use std::hash::{Hash, Hasher};

use indexmap::Equivalent;

use crate::det_hash::{IndexMap, map_with_capacity};

// Storage reports key and snapshot-shape failures locally. The conversion to
// VM `Error` preserves script-visible text; snapshot loading keeps its
// location in `LoadError` when it consumes the local reason directly.
use super::Error;
use super::TypeError;
use super::Val;
use super::object::{GcHeap, Markable, ObjectPtr};
#[cfg(feature = "snapshot")]
use crate::error::{InternalFault, SnapshotTableFault};

use crate::limits::tuning::TABLE_INLINE_CAPACITY as INLINE_CAPACITY;

/// Tag bit marking a slot index (as handed to inline caches and the `pairs`
/// cursor) as an array-part slot. Untagged indices are hash-part positions.
/// A tagged index carries the absolute key minus one, not a position in the
/// physical window, so moving the window invalidates no cached index, and
/// tagging keeps hash positions independent of the array's length, so
/// growing the array invalidates none either.
const ARRAY_SLOT_TAG: usize = 1 << (usize::BITS - 1);

/// The largest logical array length a table can reach. It is a
/// representation limit, not a semantic one: below it every key in
/// `1..=len + 1` is an exact, distinct `f64`, `len + 1` cannot overflow, and
/// `ARRAY_SLOT_TAG | (key - 1)` cannot collide with the tag. Growing past it
/// panics, the same failure class as the physical allocation the logical
/// length used to be (2^53 appends on a 64-bit host, 2^31 on a 32-bit one).
/// Snapshot loading rejects a larger length rather than panicking.
pub(super) const MAX_ARRAY_LEN: usize = {
    const EXACT_F64_LIMIT: u64 = (1 << 53) - 2;
    let tag_limit = (ARRAY_SLOT_TAG - 2) as u64;
    (if tag_limit < EXACT_F64_LIMIT {
        tag_limit
    } else {
        EXACT_F64_LIMIT
    }) as usize
};

/// How many already-nil slots past the dead-prefix hint one append, or one
/// nil write at the hint, may step over. A fixed bound keeps every operation
/// O(1) while letting a FIFO-shaped workload close any lag between the hint
/// and the true dead prefix within O(lag / LEAD_CATCH_UP) pushes.
const LEAD_CATCH_UP: usize = 8;

/// Capacity a window keeps when it is emptied, so a queue that drains and
/// refills does not reallocate on every cycle while a large emptied array
/// still returns its storage.
const EMPTIED_WINDOW_CAPACITY: usize = 16;

/// The table-key forms rejected by Lua before hashing or equality is used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InvalidTableKey {
    Nil,
    Nan,
}

#[derive(Debug)]
pub(super) enum TableError {
    InvalidKey(InvalidTableKey),
    #[cfg(feature = "snapshot")]
    InvalidSnapshot(SnapshotTableFault),
}

impl From<TableError> for Error {
    fn from(error: TableError) -> Self {
        match error {
            TableError::InvalidKey(InvalidTableKey::Nil) => Self::new(TypeError::TableKeyNil, 0, 0),
            TableError::InvalidKey(InvalidTableKey::Nan) => Self::new(TypeError::TableKeyNan, 0, 0),
            #[cfg(feature = "snapshot")]
            TableError::InvalidSnapshot(fault) => {
                Self::internal_fault(InternalFault::SnapshotTable(fault))
            }
        }
    }
}

/// A non-negative integral number bounded by a collection's maximum index.
#[inline]
#[allow(clippy::float_cmp)]
pub(crate) fn integer_index(value: f64, maximum: usize) -> Option<usize> {
    (value >= 0.0 && value <= maximum as f64 && value == value.floor()).then_some(value as usize)
}

/// Convert a Lua array key in `1..=len` to a zero-based slot.
#[inline]
pub(super) fn array_index(value: f64, len: usize) -> Option<usize> {
    if value >= 1.0 {
        integer_index(value, len).map(|index| index - 1)
    } else {
        None
    }
}

/// Whether a numeric key extends an array of the given length.
#[inline]
#[allow(clippy::float_cmp)]
pub(super) fn is_append_key(value: f64, len: usize) -> bool {
    value == (len + 1) as f64
}

/// A table key before applying the VM's canonical numeric equality rule.
// Snapshot keys use different payload types but share the same normalizer.
pub(super) enum TableKeyInput<S, O, F> {
    Nil,
    Bool(bool),
    Number(f64),
    String(S),
    Object(O),
    RustFn(F),
}

/// A table key represented with negative zero canonicalized and invalid keys
/// removed. The reference payload types are chosen by the caller.
#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum NormalizedTableKey<S, O, F> {
    Bool(bool),
    Number(u64),
    String(S),
    Object(O),
    RustFn(F),
}

/// Apply the common table-key validity and equality rule.
///
/// Tables store the original key value so iteration preserves the first
/// inserted representation, including negative zero. Load validation uses
/// the returned key to compare serialized values without duplicating that
/// rule.
#[inline]
pub(super) fn normalize_table_key<S, O, F>(
    key: TableKeyInput<S, O, F>,
) -> std::result::Result<NormalizedTableKey<S, O, F>, InvalidTableKey> {
    match key {
        TableKeyInput::Nil => Err(InvalidTableKey::Nil),
        TableKeyInput::Bool(value) => Ok(NormalizedTableKey::Bool(value)),
        TableKeyInput::Number(value) => {
            if value.is_nan() {
                Err(InvalidTableKey::Nan)
            } else {
                let bits = value.to_bits();
                Ok(NormalizedTableKey::Number(if bits << 1 == 0 {
                    0
                } else {
                    bits
                }))
            }
        }
        TableKeyInput::String(value) => Ok(NormalizedTableKey::String(value)),
        TableKeyInput::Object(value) => Ok(NormalizedTableKey::Object(value)),
        TableKeyInput::RustFn(value) => Ok(NormalizedTableKey::RustFn(value)),
    }
}

impl Val {
    /// Validate and normalize a runtime table key without dereferencing it.
    #[inline]
    pub(super) fn table_key(
        self,
    ) -> std::result::Result<
        NormalizedTableKey<super::object::StringPtr, ObjectPtr, usize>,
        InvalidTableKey,
    > {
        let value = self;
        let input = match value {
            Val::Nil => TableKeyInput::Nil,
            Val::Bool(value) => TableKeyInput::Bool(value),
            Val::Num(value) => TableKeyInput::Number(value),
            Val::Str(value) => TableKeyInput::String(value),
            Val::Obj(value) => TableKeyInput::Object(value),
            Val::RustFn(value) => TableKeyInput::RustFn(value as usize),
        };
        normalize_table_key(input)
    }
}

/// Payload for a table key slot. Live keys carry a value validated by the
/// common normalizer; vacant slots retain only their unique physical index.
/// A vacant slot marks a key that was re-added elsewhere. The new live key
/// appends to preserve `pairs()` order, while renaming the old slot in place
/// keeps later physical indices stable. Compaction drops all vacant slots.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TableKeyKind {
    Live(Val),
    Vacant(usize),
}

/// A valid Lua table key or a vacant hash tombstone slot.
///
/// The transparent wrapper preserves the old slot representation. Live keys
/// keep their original `Val` for iteration, including negative zero.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TableKey(TableKeyKind);

// `Val` is only `PartialEq` because NaN is unequal to itself. A live
// `TableKey` has passed the table-key normalizer, so it is never NaN, and
// vacant slots compare by their index; equality here is reflexive.
impl Eq for TableKey {}

impl TableKey {
    /// Validate a runtime value and wrap it as a table-storage key.
    #[inline]
    pub(super) fn new(value: Val) -> std::result::Result<Self, InvalidTableKey> {
        value.table_key().map(|_| Self(TableKeyKind::Live(value)))
    }

    /// Wrap a key already validated by the bytecode verifier or by an earlier
    /// table insertion. The debug assertion checks that proof without adding
    /// work to release template construction or inline-table promotion.
    #[inline]
    pub(super) fn from_prevalidated(value: Val) -> Self {
        debug_assert!(
            Self::new(value).is_ok(),
            "prevalidated table keys must satisfy the table-key normalizer"
        );
        Self(TableKeyKind::Live(value))
    }

    #[inline]
    fn vacant(index: usize) -> Self {
        Self(TableKeyKind::Vacant(index))
    }

    /// Return the live value carried by this key.
    #[inline]
    pub(super) fn value(self) -> Val {
        match self.0 {
            TableKeyKind::Live(value) => value,
            TableKeyKind::Vacant(_) => {
                unreachable!("a vacant hash slot never holds a live value")
            }
        }
    }

    #[cfg(feature = "snapshot")]
    #[inline]
    fn as_live(self) -> Option<Val> {
        match self.0 {
            TableKeyKind::Live(value) => Some(value),
            TableKeyKind::Vacant(_) => None,
        }
    }
}

/// Hash a table key consistently with `Val` equality: numeric keys hash their
/// normalized bits (so -0.0 and +0.0 collide), and other key types hash their
/// pointer or scalar payload directly. `Val` deliberately has no `Hash` impl;
/// this is the only hash of runtime key values.
///
/// Forced inline: every hash-part lookup and insert runs it, and the `#[inline]`
/// hint alone was dropped when unrelated changes reshaped the field-read miss
/// path, which made a record-reading workload spend more cycles calling it
/// than looking keys up. It is a scalar match that neither allocates nor
/// re-enters, so it adds nothing to frames live across native re-entry.
#[inline(always)]
fn hash_table_key_value<H: Hasher>(value: Val, hasher: &mut H) {
    match value {
        Val::Nil => (),
        Val::Bool(value) => value.hash(hasher),
        Val::Num(number) => {
            let bits = match normalize_table_key::<(), (), ()>(TableKeyInput::Number(number)) {
                Ok(NormalizedTableKey::Number(bits)) => bits,
                Err(InvalidTableKey::Nan) => {
                    crate::numeral::CANONICAL_NAN_BITS.hash(hasher);
                    return;
                }
                Err(InvalidTableKey::Nil) | Ok(_) => {
                    unreachable!("numeric table keys normalize to numeric keys")
                }
            };
            bits.hash(hasher);
        }
        Val::Str(ptr) => ptr.hash(hasher),
        Val::Obj(ptr) => ptr.hash(hasher),
        Val::RustFn(func) => (func as usize).hash(hasher),
    }
}

/// A borrowed lookup query for `TableKey` maps that does not give `Val` a
/// general-purpose `Hash` implementation.
#[derive(Clone, Copy)]
struct TableLookupKey(Val);

impl Hash for TableLookupKey {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        hash_table_key_value(self.0, hasher);
    }
}

impl Equivalent<TableKey> for TableLookupKey {
    #[inline]
    fn equivalent(&self, key: &TableKey) -> bool {
        matches!(key.0, TableKeyKind::Live(live) if live == self.0)
    }
}

/// Cheap rejection for the only runtime values that cannot be table keys.
/// Valid keys are hashed and compared using the shared table-key semantics.
#[inline]
fn is_valid_table_key(value: &Val) -> bool {
    !matches!(value, Val::Nil) && !matches!(value, Val::Num(number) if number.is_nan())
}

impl Hash for TableKey {
    #[inline]
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        match self.0 {
            TableKeyKind::Live(value) => hash_table_key_value(value, hasher),
            TableKeyKind::Vacant(index) => index.hash(hasher),
        }
    }
}

/// Storage for the hash part. Small parts (<=4 entries) use inline array
/// storage for better cache locality and reduced allocation overhead. Larger
/// parts use IndexMap to maintain insertion order for deterministic `pairs()`
/// iteration.
///
/// Invariant: the key of a dead (nil-valued or vacant) slot may dangle.
/// `mark_values` only marks keys of live slots, so after a collection a
/// tombstone's `StringPtr`/`ObjectPtr` key can point at a freed object. Any
/// general-purpose walk that may dereference keys should use
/// `live_hash_entries`, which never yields such keys. The GC's specialized
/// hot loop checks for nil inline before reading a key. Debug invariant checks
/// may inspect raw keys but do not dereference them.
#[derive(Debug)]
enum TableStorage {
    /// Inline storage for small tables. Stores key-value pairs directly.
    /// `len` tracks how many slots are used (0..=INLINE_CAPACITY).
    Inline {
        entries: [(Val, Val); INLINE_CAPACITY],
        len: u8,
    },
    /// IndexMap storage for larger tables. Maintains insertion order.
    Map(IndexMap<TableKey, Val>),
}

/// Iterates occupied hash slots in insertion order, yielding only keys whose
/// values are live. The index is the original physical slot, including any
/// tombstones skipped before it.
#[cfg(feature = "snapshot")]
enum LiveHashEntries<'a> {
    Inline {
        entries: &'a [(Val, Val)],
        len: usize,
        index: usize,
    },
    Map {
        entries: indexmap::map::Iter<'a, TableKey, Val>,
        index: usize,
    },
}

#[cfg(feature = "snapshot")]
impl Iterator for LiveHashEntries<'_> {
    type Item = (usize, Val, Val);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            LiveHashEntries::Inline {
                entries,
                len,
                index,
            } => {
                while *index < *len {
                    let current = *index;
                    *index += 1;
                    let (key, value) = entries[current];
                    if !matches!(value, Val::Nil) {
                        return Some((current, key, value));
                    }
                }
                None
            }
            LiveHashEntries::Map { entries, index } => {
                for (key, value) in entries.by_ref() {
                    let current = *index;
                    *index += 1;
                    if matches!(value, Val::Nil) {
                        continue;
                    }
                    if let Some(key) = key.as_live() {
                        return Some((current, key, *value));
                    }
                }
                None
            }
        }
    }
}

/// The result of advancing a table iterator.
pub(super) enum TableNext {
    Pair(Val, Val),
    End,
    InvalidKey,
}

/// The result of advancing a table iterator, retaining the physical storage
/// index of a returned pair for the `pairs` cursor cache.
#[derive(Debug)]
pub(super) enum TableNextWithIndex {
    Pair { index: usize, key: Val, value: Val },
    End,
    InvalidKey,
}

/// A position in iteration order: the array part first, then the hash part.
#[derive(Clone, Copy)]
enum Position {
    Array(usize),
    Hash(usize),
}

impl Default for TableStorage {
    fn default() -> Self {
        TableStorage::Inline {
            entries: Default::default(),
            len: 0,
        }
    }
}

/// A Lua table: an array part for keys `1..=vlen` beside an
/// insertion-ordered hash part for everything else.
///
/// The array part is logical: `vlen` is its length, and only a window of it,
/// keys `base + 1..=base + array.len()`, is stored. Every array key outside
/// the window is nil. Which keys are array keys, `#`, `pairs` order and
/// where a later write lands depend only on `vlen` and the live values, never
/// on where the window sits, so moving the window is invisible to scripts.
///
/// Shape rules, which every mutation preserves:
///
/// - Key `k` with `1 <= k <= vlen` is an array key: it lives only in the
///   window (a nil there, or a key outside the window, means absent). The
///   hash part may still hold a dead tombstone for such a key, left by
///   migration, but never a live one.
/// - `vlen` grows only by appending key `vlen + 1`, after which any live hash
///   entries for the following keys migrate into the array. So the hash part
///   never holds a live entry for key `vlen + 1`. `vlen` never shrinks
///   outside a snapshot rebuild: an array key stays one for the table's
///   lifetime.
/// - The window moves only when a live value is written to an absent key or
///   the array is appended to: its dead prefix is dropped at an append, an
///   all-dead window is re-anchored at the next write outside it, and a live
///   write outside it extends it. Assigning nil never moves it, so a `pairs`
///   traversal can assign nil to any visited key; adding keys during a
///   traversal is undefined in Lua, and even then a control key in
///   `1..=vlen` resumes the scan correctly. GC and saving never move it, so
///   its position cannot depend on collection timing or on a save.
///
/// `pairs()` visits live array slots in ascending key order, then the hash
/// part in insertion order. Both are deterministic functions of the
/// operation history, and a snapshot preserves `vlen` so a loaded table
/// continues identically.
#[derive(Debug)]
pub(super) struct Table {
    /// The stored window of the array part: `array[i]` holds key
    /// `base + i + 1`.
    array: Vec<Val>,
    /// Number of array keys below the window.
    base: usize,
    /// The logical array length. `base + array.len() <= vlen`.
    vlen: usize,
    /// A lower bound on the window's dead prefix: `array[..lead]` are all
    /// nil. Storage metadata only; nothing observable depends on it.
    lead: usize,
    storage: TableStorage,
    metatable: Option<ObjectPtr>,
    /// Shape version for hash-part key/index stability. Value updates do not
    /// bump this, and neither does anything in the array part, whose tagged
    /// indices name absolute keys and so survive window moves.
    version: Cell<u64>,
    /// Number of occupied hash slots whose nil value makes them logically absent.
    dead_count: usize,
}

impl Default for Table {
    fn default() -> Self {
        Self {
            array: Vec::new(),
            base: 0,
            vlen: 0,
            lead: 0,
            storage: TableStorage::default(),
            metatable: None,
            version: Cell::new(0),
            dead_count: 0,
        }
    }
}

impl Table {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        if capacity <= INLINE_CAPACITY {
            Self::default()
        } else {
            Self {
                storage: TableStorage::Map(map_with_capacity(capacity)),
                ..Self::default()
            }
        }
    }

    pub(super) fn with_template_keys(key_ids: &[u16], literals: &[Val]) -> Self {
        let mut map = map_with_capacity(key_ids.len());
        for key_id in key_ids {
            let key = literals[*key_id as usize];
            let key = TableKey::from_prevalidated(key);
            map.insert(key, Val::Nil);
        }
        // Count the slots actually created, not the ids supplied: the bytecode
        // verifier rejects duplicate template keys, but a duplicate that got
        // through would otherwise leave phantom tombstones and underflow
        // `map.len() - dead_count`. Keep this defensive in debug builds too:
        // duplicate keys must not create phantom tombstones or underflow.
        // Template keys are verified to be strings, so none belongs in the
        // array part.
        let dead_count = map.len();
        let table = Self {
            storage: TableStorage::Map(map),
            dead_count,
            ..Self::default()
        };
        #[cfg(debug_assertions)]
        table.debug_assert_invariants();
        table
    }

    /// The window position holding `key`, if `key` is an integer in
    /// `base + 1..=base + array.len()`.
    #[inline]
    fn array_slot(&self, key: &Val) -> Option<usize> {
        match *key {
            Val::Num(n) => match integer_index(n, self.base + self.array.len()) {
                Some(key) if key > self.base => Some(key - self.base - 1),
                _ => None,
            },
            _ => None,
        }
    }

    /// The integer value of `key` if it is an array key, `1..=vlen`, whether
    /// or not the window stores it.
    #[inline]
    fn array_key(&self, key: &Val) -> Option<usize> {
        match *key {
            Val::Num(n) => array_index(n, self.vlen).map(|slot| slot + 1),
            _ => None,
        }
    }

    /// Whether `key` is `vlen + 1`, the key that extends the array.
    #[inline]
    fn is_array_append_key(&self, key: &Val) -> bool {
        matches!(key, Val::Num(n) if is_append_key(*n, self.vlen))
    }

    /// The window position of absolute array slot `slot` (key `slot + 1`).
    #[inline]
    fn window_position(&self, slot: usize) -> Option<usize> {
        slot.checked_sub(self.base)
            .filter(|position| *position < self.array.len())
    }

    /// The value at array key `key`, nil outside the window.
    #[inline]
    fn array_value(&self, key: usize) -> Val {
        key.checked_sub(1)
            .and_then(|slot| self.window_position(slot))
            .map_or(Val::Nil, |position| self.array[position])
    }

    #[inline]
    pub(super) fn version(&self) -> u64 {
        self.version.get()
    }

    #[inline]
    fn bump_version(&self) {
        self.version.set(self.version.get().wrapping_add(1));
    }

    /// Check the table shape and tombstone count while developing the VM.
    ///
    /// The window lies inside the logical array, its hinted dead prefix is
    /// nil, and a hash key at the append position would be migrated into the
    /// array before it can become visible there. The tombstone count tracks
    /// every nil-valued hash slot, including vacant map entries.
    #[cfg(debug_assertions)]
    fn debug_assert_invariants(&self) {
        debug_assert!(
            self.base + self.array.len() <= self.vlen && self.vlen <= MAX_ARRAY_LEN,
            "array window must lie inside the logical array"
        );
        debug_assert!(
            self.lead <= self.array.len()
                && self.array[self.lead.saturating_sub(64)..self.lead]
                    .iter()
                    .all(|value| matches!(value, Val::Nil)),
            "the dead-prefix hint must cover only nil slots"
        );
        let assert_hash_key_outside_array = |key: &Val| {
            debug_assert!(
                self.array_key(key).is_none() && !self.is_array_append_key(key),
                "live hash key overlaps the array part or its append slot"
            );
        };

        let dead_count = match &self.storage {
            TableStorage::Inline { entries, len } => {
                debug_assert!((*len as usize) <= INLINE_CAPACITY);
                let mut dead_count = 0;
                for (key, value) in entries.iter().take(*len as usize) {
                    if matches!(value, Val::Nil) {
                        dead_count += 1;
                    } else {
                        assert_hash_key_outside_array(key);
                    }
                }
                dead_count
            }
            TableStorage::Map(map) => {
                // Past a small size, check only the O(1) counter bound.
                if map.len() > crate::limits::tuning::TABLE_DEBUG_FULL_WALK_LIMIT {
                    debug_assert!(
                        self.dead_count <= map.len(),
                        "dead_count must equal the number of nil-valued hash slots"
                    );
                    return;
                }
                let mut dead_count = 0;
                for (key, value) in map {
                    if matches!(value, Val::Nil) {
                        dead_count += 1;
                    } else {
                        match key.0 {
                            TableKeyKind::Live(key) => {
                                assert_hash_key_outside_array(&key);
                            }
                            TableKeyKind::Vacant(_) => {
                                debug_assert!(false, "a vacant hash key cannot hold a live value");
                            }
                        }
                    }
                }
                dead_count
            }
        };

        debug_assert_eq!(
            self.dead_count, dead_count,
            "dead_count must equal the number of nil-valued hash slots"
        );
    }

    #[hotpath::measure]
    pub(super) fn get(&self, key: &Val) -> Val {
        if let Some(slot) = self.array_slot(key) {
            return self.array[slot];
        }
        self.hash_get(key)
    }

    #[inline]
    fn hash_get(&self, key: &Val) -> Val {
        if !is_valid_table_key(key) {
            return Val::Nil;
        }
        match &self.storage {
            TableStorage::Inline { entries, len } => {
                for (entry_key, entry_value) in entries.iter().take(*len as usize) {
                    if entry_key == key {
                        return *entry_value;
                    }
                }
                Val::Nil
            }
            TableStorage::Map(map) => map.get(&TableLookupKey(*key)).copied().unwrap_or_default(),
        }
    }

    #[inline]
    pub(super) fn get_with_index(&self, key: &Val) -> Option<(usize, Val)> {
        if let Some(slot) = self.array_slot(key) {
            let value = self.array[slot];
            return (!matches!(value, Val::Nil))
                .then_some((ARRAY_SLOT_TAG | (self.base + slot), value));
        }
        if !is_valid_table_key(key) {
            return None;
        }
        match &self.storage {
            TableStorage::Inline { entries, len } => {
                for (idx, (entry_key, entry_value)) in
                    entries.iter().take(*len as usize).enumerate()
                {
                    if entry_key == key {
                        return (!matches!(entry_value, Val::Nil)).then_some((idx, *entry_value));
                    }
                }
                None
            }
            TableStorage::Map(map) => {
                let (idx, _, value) = map.get_full(&TableLookupKey(*key))?;
                (!matches!(value, Val::Nil)).then_some((idx, *value))
            }
        }
    }

    #[inline]
    pub(super) fn get_index(&self, index: usize) -> Option<(Val, Val)> {
        if index & ARRAY_SLOT_TAG != 0 {
            let slot = index & !ARRAY_SLOT_TAG;
            return self.window_position(slot).and_then(|position| {
                let value = self.array[position];
                (!matches!(value, Val::Nil)).then_some((Val::Num((slot + 1) as f64), value))
            });
        }
        match &self.storage {
            TableStorage::Inline { entries, len } => {
                if index < *len as usize {
                    (!matches!(entries[index].1, Val::Nil)).then_some(entries[index])
                } else {
                    None
                }
            }
            TableStorage::Map(map) => map.get_index(index).and_then(|(key, value)| {
                (!matches!(value, Val::Nil)).then(|| (key.value(), *value))
            }),
        }
    }

    /// The value half of [`Self::get_index`], for cached reads that never look
    /// at the key. `Option<Val>` comes back in two registers, whereas the
    /// key/value pair goes through a stack out-parameter that the caller
    /// re-reads as overlapping byte/word loads, defeating store forwarding.
    #[inline]
    pub(super) fn get_index_value(&self, index: usize) -> Option<Val> {
        let value = if index & ARRAY_SLOT_TAG != 0 {
            let position = self.window_position(index & !ARRAY_SLOT_TAG)?;
            self.array[position]
        } else {
            match &self.storage {
                TableStorage::Inline { entries, len } => {
                    if index >= *len as usize {
                        return None;
                    }
                    entries[index].1
                }
                TableStorage::Map(map) => *map.get_index(index)?.1,
            }
        };
        (!matches!(value, Val::Nil)).then_some(value)
    }

    /// Iterate live hash entries in insertion order with their physical slots.
    /// Dead keys are never yielded, so callers may safely dereference them.
    #[cfg(feature = "snapshot")]
    fn live_hash_entries(&self) -> impl Iterator<Item = (usize, Val, Val)> + '_ {
        match &self.storage {
            TableStorage::Inline { entries, len } => LiveHashEntries::Inline {
                entries,
                len: *len as usize,
                index: 0,
            },
            TableStorage::Map(map) => LiveHashEntries::Map {
                entries: map.iter(),
                index: 0,
            },
        }
    }

    /// Live entries in `pairs()` order: the array part, then the hash part.
    #[cfg(feature = "snapshot")]
    pub(super) fn entries(&self) -> Vec<(Val, Val)> {
        let mut out = self.array_entries();
        self.extend_with_live_hash_entries(&mut out);
        out
    }

    /// The snapshot form: the logical array length, the number of leading
    /// entries that are array slots, and the entry list. The array slots run
    /// from the first live array key to the last one, with nil values for the
    /// empty slots between them; every array key outside that span is nil.
    /// The live hash entries follow in insertion order.
    ///
    /// The form is canonical: it depends on the logical contents and `vlen`
    /// only, never on where the window sits, so two tables holding the same
    /// logical state save the same entries whatever their window history,
    /// and saving does not touch the table. `rebuild_from_snapshot` inverts
    /// it.
    #[cfg(feature = "snapshot")]
    pub(super) fn snapshot_entries(&self) -> (usize, usize, Vec<(Val, Val)>) {
        let live = |value: &Val| !matches!(value, Val::Nil);
        let (first, last) = match (
            self.array.iter().position(live),
            self.array.iter().rposition(live),
        ) {
            (Some(first), Some(last)) => (first, last + 1),
            _ => (0, 0),
        };
        let mut out: Vec<(Val, Val)> = self.array[first..last]
            .iter()
            .enumerate()
            .map(|(offset, value)| (Val::Num((self.base + first + offset + 1) as f64), *value))
            .collect();
        let array_slots = out.len();
        self.extend_with_live_hash_entries(&mut out);
        (self.vlen, array_slots, out)
    }

    /// The whole logical array part, keys `1..=vlen` with nil for empty
    /// slots, for environment-table snapshots. Environment deltas keep that
    /// full shape rather than the canonical live span ordinary tables save,
    /// which costs `vlen` slots only for a library table a script used as an
    /// array.
    #[cfg(feature = "snapshot")]
    pub(super) fn virtual_array(&self) -> Vec<Val> {
        let mut out = vec![Val::Nil; self.vlen];
        out[self.base..self.base + self.array.len()].copy_from_slice(&self.array);
        out
    }

    #[cfg(feature = "snapshot")]
    fn array_entries(&self) -> Vec<(Val, Val)> {
        self.array
            .iter()
            .enumerate()
            .filter(|(_, value)| !matches!(value, Val::Nil))
            .map(|(position, value)| (Val::Num((self.base + position + 1) as f64), *value))
            .collect()
    }

    #[cfg(feature = "snapshot")]
    fn extend_with_live_hash_entries(&self, out: &mut Vec<(Val, Val)>) {
        out.extend(self.live_hash_entries().map(|(_, key, value)| (key, value)));
    }

    /// Rebuild from `snapshot_entries` output: `vlen` is the logical array
    /// length, the first `array_slots` entries are array slots with
    /// consecutive keys inside `1..=vlen` (nil values for empty slots), the
    /// rest live hash entries in insertion order. The stored slots become the
    /// window. The span need not be canonical: environment deltas pass the
    /// whole logical array.
    #[cfg(feature = "snapshot")]
    pub(super) fn rebuild_from_snapshot(
        &mut self,
        vlen: usize,
        array_slots: usize,
        entries: Vec<(Val, Val)>,
    ) -> std::result::Result<(), TableError> {
        if array_slots > entries.len() {
            return Err(TableError::InvalidSnapshot(
                SnapshotTableFault::ArrayLengthExceedsEntries,
            ));
        }
        if vlen > MAX_ARRAY_LEN {
            return Err(TableError::InvalidSnapshot(
                SnapshotTableFault::ArrayLengthExceedsLimit,
            ));
        }
        let base = match entries.first() {
            Some((Val::Num(n), _)) if array_slots > 0 => match array_index(*n, vlen) {
                Some(slot) if slot + array_slots <= vlen => slot,
                _ => {
                    return Err(TableError::InvalidSnapshot(
                        SnapshotTableFault::ArraySlotHasWrongKey,
                    ));
                }
            },
            _ if array_slots > 0 => {
                return Err(TableError::InvalidSnapshot(
                    SnapshotTableFault::ArraySlotHasWrongKey,
                ));
            }
            _ => 0,
        };
        // Reset to a shell sized for the incoming hash entries, then re-insert
        // in order so `pairs()` iteration order round-trips. with_capacity
        // picks inline vs map storage from the count.
        //
        // The rebuild may move keys to new indices, so the version must move
        // past every value an inline cache could have captured for this table
        // (load setup can run Lua against environment tables before this).
        // Resetting it to 0 would let a cache warmed at version 0 return the
        // wrong slot. A per-table bump suffices: caches key on table identity,
        // which the rebuild preserves.
        let next_version = self.version().wrapping_add(1);
        *self = Table::with_capacity(entries.len() - array_slots);
        let mut entries = entries.into_iter();
        let mut array = Vec::with_capacity(array_slots);
        for (position, (key, value)) in entries.by_ref().take(array_slots).enumerate() {
            if !matches!(key, Val::Num(n) if array_index(n, vlen) == Some(base + position)) {
                return Err(TableError::InvalidSnapshot(
                    SnapshotTableFault::ArraySlotHasWrongKey,
                ));
            }
            array.push(value);
        }
        self.array = array;
        self.base = base;
        self.vlen = vlen;
        for (key, value) in entries {
            // A live hash entry at an array key or the append key would have
            // been stored in the array, so it cannot have come from
            // `snapshot_entries`.
            if self.array_key(&key).is_some() || self.is_array_append_key(&key) {
                return Err(TableError::InvalidSnapshot(
                    SnapshotTableFault::HashEntryBelongsInArray,
                ));
            }
            self.insert(key, value)?;
        }
        self.version.set(next_version);
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
        Ok(())
    }

    /// Update the value at a specific entry index. Returns true on success.
    ///
    /// This is a hot-path helper for the OP_SET_FIELD inline cache: when a
    /// callsite has already verified that a key lives at a known index, the
    /// IC writes the new value through this method without re-doing the key
    /// lookup. Caller must ensure the value is non-nil (assigning nil is
    /// remove-semantics and must go through the slow path) and that the
    /// entry at `index` corresponds to the intended key (typically verified
    /// by table_version match or get_index key compare). Does not bump
    /// version (value-only change preserves (key, index) bindings).
    #[inline]
    pub(super) fn set_at_index(&mut self, index: usize, value: Val) -> bool {
        debug_assert!(
            !matches!(value, Val::Nil),
            "set_at_index requires a non-nil value"
        );
        let updated = if index & ARRAY_SLOT_TAG != 0 {
            // A live slot is never inside the dead-prefix hint, so a
            // live-to-live update leaves the hint alone.
            match self
                .window_position(index & !ARRAY_SLOT_TAG)
                .map(|position| &mut self.array[position])
            {
                Some(slot) if !matches!(*slot, Val::Nil) => {
                    *slot = value;
                    true
                }
                _ => false,
            }
        } else {
            match &mut self.storage {
                TableStorage::Inline { entries, len } => {
                    if index < *len as usize && !matches!(entries[index].1, Val::Nil) {
                        entries[index].1 = value;
                        true
                    } else {
                        false
                    }
                }
                TableStorage::Map(map) => {
                    let Some((_, v)) = map.get_index_mut(index) else {
                        return false;
                    };
                    if matches!(*v, Val::Nil) {
                        false
                    } else {
                        *v = value;
                        true
                    }
                }
            }
        };
        #[cfg(debug_assertions)]
        if updated {
            self.debug_assert_invariants();
        }
        updated
    }

    /// Initializes a reserved template slot without exposing dead slots to caches.
    pub(super) fn init_at_index(&mut self, index: usize, key: Val, value: Val) -> bool {
        if index & ARRAY_SLOT_TAG != 0 {
            return false;
        }
        // Account for both transitions. Compiled code initializes each pinned
        // slot once, so only nil -> live occurs there, but forged snapshot
        // bytecode can initialize the same slot live -> nil -> live; counting
        // only activations would underflow `dead_count`. Keeping the counter
        // exact for any sequence makes this safe without the verifier having
        // to prove single initialization.
        let (was_live, now_live) = match &mut self.storage {
            TableStorage::Inline { entries, len } if index < *len as usize => {
                let (existing_key, existing_value) = &mut entries[index];
                if *existing_key != key {
                    return false;
                }
                let was_live = !matches!(*existing_value, Val::Nil);
                *existing_value = value;
                (was_live, !matches!(value, Val::Nil))
            }
            TableStorage::Map(map) => {
                let Some((existing_key, existing_value)) = map.get_index_mut(index) else {
                    return false;
                };
                if !TableLookupKey(key).equivalent(existing_key) {
                    return false;
                }
                let was_live = !matches!(*existing_value, Val::Nil);
                *existing_value = value;
                (was_live, !matches!(value, Val::Nil))
            }
            TableStorage::Inline { .. } => return false,
        };
        match (was_live, now_live) {
            (false, true) => self.dead_count -= 1,
            (true, false) => self.dead_count += 1,
            _ => {}
        }
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
        true
    }

    /// Returns a "border" of the table per the Lua `#` operator.
    /// A border is any non-negative integer N where `t[N]` is non-nil (or N == 0)
    /// and `t[N+1]` is nil. For a sequence (no nil holes) this is the length.
    ///
    /// Only the array part is consulted: if
    /// key `vlen` is live, `vlen` is a border (the hash part never holds a
    /// live entry for the next key); otherwise a binary search over the
    /// logical array `1..=vlen` finds one, reading nil for keys outside the
    /// window. The result is a pure function of `vlen` and the live array
    /// values, which snapshots preserve, never of the window's position, and
    /// costs O(1) for a full array and O(log vlen) otherwise.
    #[hotpath::measure]
    pub(super) fn array_len(&self) -> usize {
        if self.base + self.array.len() == self.vlen
            && matches!(self.array.last(), Some(last) if !matches!(last, Val::Nil))
        {
            return self.vlen;
        }
        // Invariant: lo == 0 or t[lo] is non-nil; t[hi] is nil (or hi == 0).
        let mut lo = 0;
        let mut hi = self.vlen;
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if matches!(self.array_value(mid), Val::Nil) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        lo
    }

    #[hotpath::measure]
    pub(super) fn insert(&mut self, key: Val, value: Val) -> std::result::Result<(), TableError> {
        if let Some(position) = self.array_slot(&key) {
            self.write_window(position, value);
            #[cfg(debug_assertions)]
            self.debug_assert_invariants();
            return Ok(());
        }

        let table_key = match TableKey::new(key) {
            Ok(key) => key,
            Err(error) => return Err(TableError::InvalidKey(error)),
        };
        let key = table_key.value();

        // In Lua, assigning nil deletes the key rather than storing a nil value.
        // An array key outside the window is already absent, and the hash
        // part holds no live entry for it.
        if matches!(value, Val::Nil) {
            self.hash_remove(&key);
            return Ok(());
        }

        if self.is_array_append_key(&key) {
            self.append(value);
            return Ok(());
        }

        if let Some(array_key) = self.array_key(&key) {
            self.write_outside_window(array_key, value);
            return Ok(());
        }

        if self.dead_count == 0 {
            self.insert_without_tombstones(table_key, value);
            return Ok(());
        }

        let updated_live_inline_entry = match &mut self.storage {
            TableStorage::Inline { entries, len } => {
                if let Some(index) = entries
                    .iter()
                    .take(*len as usize)
                    .position(|entry| entry.0 == key && !matches!(entry.1, Val::Nil))
                {
                    entries[index].1 = value;
                    true
                } else {
                    false
                }
            }
            TableStorage::Map(_) => false,
        };
        if updated_live_inline_entry {
            #[cfg(debug_assertions)]
            self.debug_assert_invariants();
            return Ok(());
        }

        match &mut self.storage {
            TableStorage::Inline { entries, len } => {
                if let Some(index) = entries
                    .iter()
                    .take(*len as usize)
                    .position(|entry| entry.0 == key)
                {
                    for shift in index..*len as usize - 1 {
                        entries[shift] = std::mem::take(&mut entries[shift + 1]);
                    }
                    *len -= 1;
                    self.dead_count -= 1;
                    self.bump_version();
                } else if self.dead_count >= *len as usize - self.dead_count {
                    if self.compact_dead() {
                        self.bump_version();
                    }
                } else if *len as usize == INLINE_CAPACITY && self.compact_dead() {
                    self.bump_version();
                }
                let TableStorage::Inline { entries, len } = &mut self.storage else {
                    unreachable!("inline compaction cannot promote storage");
                };
                if (*len as usize) < INLINE_CAPACITY {
                    entries[*len as usize] = (key, value);
                    *len += 1;
                } else {
                    self.promote_to_map(table_key, value);
                }
            }
            TableStorage::Map(_) => self.insert_into_map_with_tombstones(table_key, value),
        }
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
        Ok(())
    }

    /// Write `value` into window position `position`, keeping the
    /// dead-prefix hint a lower bound.
    #[inline]
    fn write_window(&mut self, position: usize, value: Val) {
        self.array[position] = value;
        if position <= self.lead {
            self.note_prefix_write(position, value);
        }
    }

    /// Adjust the dead-prefix hint after a write at or below it: a live
    /// value below it lowers it, a nil at it extends it.
    fn note_prefix_write(&mut self, position: usize, value: Val) {
        if !matches!(value, Val::Nil) {
            self.lead = self.lead.min(position);
        } else if position == self.lead {
            self.lead += 1;
            self.advance_lead();
        }
    }

    /// Step the dead-prefix hint over at most `LEAD_CATCH_UP` nil slots.
    fn advance_lead(&mut self) {
        let end = (self.lead + LEAD_CATCH_UP).min(self.array.len());
        while self.lead < end && matches!(self.array[self.lead], Val::Nil) {
            self.lead += 1;
        }
    }

    /// Add one key to the logical array.
    fn grow_vlen(&mut self) {
        assert!(
            self.vlen < MAX_ARRAY_LEN,
            "table array part cannot exceed MAX_ARRAY_LEN keys"
        );
        self.vlen += 1;
    }

    /// Add `n` keys to the logical array.
    fn grow_vlen_by(&mut self, n: usize) {
        assert!(
            n <= MAX_ARRAY_LEN - self.vlen,
            "table array part cannot exceed MAX_ARRAY_LEN keys"
        );
        self.vlen += n;
    }

    /// Whether an append, and every append after it with no other write in
    /// between, is a bare push: the window's first slot is live (so the
    /// dead-prefix hint is 0 and cannot advance, and no prefix is dropped or
    /// window reset), the window ends at `vlen`, and the hash part holds no
    /// live entry to migrate. Appends keep all three true, nil values
    /// included.
    #[inline]
    fn appends_are_plain(&self) -> bool {
        matches!(self.array.first(), Some(first) if !matches!(first, Val::Nil))
            && self.base + self.array.len() == self.vlen
            && self.hash_live_len() == 0
    }

    /// Append `value` at key `vlen + 1`, then migrate the following keys from
    /// the hash part. A nil value is still a structural append: constructors
    /// and `table.insert` inside a full sequence extend the array with it.
    /// Ordinary nil writes never reach here.
    fn append(&mut self, value: Val) {
        self.prepare_append();
        self.grow_vlen();
        self.array.push(value);
        self.migrate_from_hash();
    }

    /// Make the window end at `vlen` so an append can push. This is the one
    /// place a window gives storage back: an all-dead window restarts at the
    /// append key, and a dead prefix of at least half the window is dropped.
    /// Both run only on an append, never on a nil write, so a traversal that
    /// clears keys never sees the window move.
    fn prepare_append(&mut self) {
        self.advance_lead();
        let len = self.array.len();
        if self.lead == len {
            if len > 0 || self.base != self.vlen {
                self.reset_window(self.vlen);
            }
            return;
        }
        if self.lead * 2 >= len {
            self.drop_dead_prefix();
        }
        if self.base + self.array.len() < self.vlen {
            self.array.resize(self.vlen - self.base, Val::Nil);
        }
    }

    /// Drop the hinted dead prefix from the window. The memmove costs at most
    /// the prefix length, which the hint paid for one step at a time.
    fn drop_dead_prefix(&mut self) {
        self.array.drain(..self.lead);
        self.base += self.lead;
        self.lead = 0;
        let keep = self.array.len().max(EMPTIED_WINDOW_CAPACITY);
        if self.array.capacity() > 4 * keep {
            self.array.shrink_to(2 * keep);
        }
    }

    /// Empty the window and restart it just above key `base`.
    fn reset_window(&mut self, base: usize) {
        self.array.clear();
        self.array.shrink_to(EMPTIED_WINDOW_CAPACITY);
        self.base = base;
        self.lead = 0;
    }

    /// Store a live `value` at array key `key`, which lies outside the
    /// window. An all-dead window restarts at `key`; otherwise the window
    /// extends to cover it with nil fill. Extending downward pads below `key`
    /// by about half the window's live span, so repeated pushes at the front
    /// of a deque stay amortized O(1).
    fn write_outside_window(&mut self, key: usize, value: Val) {
        debug_assert!(!matches!(value, Val::Nil));
        let len = self.array.len();
        if self.lead == len {
            self.reset_window(key - 1);
            self.array.push(value);
        } else if key <= self.base {
            let pad = (key - 1).min((len - self.lead).div_ceil(2));
            let new_base = key - 1 - pad;
            let grow = self.base - new_base;
            self.array.splice(0..0, std::iter::repeat_n(Val::Nil, grow));
            self.base = new_base;
            self.lead = pad;
            self.array[pad] = value;
        } else {
            self.array.resize(key - self.base, Val::Nil);
            self.array[key - self.base - 1] = value;
        }
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
    }

    /// After the array grew, move any live hash entries for the keys that now
    /// follow it into the array, restoring "no live hash entry at the append
    /// key". Each migrated entry leaves a tombstone, exactly as a removal does,
    /// so no cached hash index moves; the tombstones are compacted once they
    /// outnumber the live entries.
    fn migrate_from_hash(&mut self) {
        if self.hash_live_len() == 0 {
            #[cfg(debug_assertions)]
            self.debug_assert_invariants();
            return;
        }
        // The caller has just appended, so the window ends at `vlen`.
        debug_assert_eq!(self.base + self.array.len(), self.vlen);
        let mut migrated = false;
        loop {
            let next = Val::Num((self.vlen + 1) as f64);
            match self.hash_remove(&next) {
                Some(value) => {
                    self.grow_vlen();
                    self.array.push(value);
                    migrated = true;
                }
                None => break,
            }
        }
        if migrated && self.dead_count >= self.hash_live_len() && self.compact_dead() {
            self.bump_version();
        }
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
    }

    fn hash_live_len(&self) -> usize {
        let occupied = match &self.storage {
            TableStorage::Inline { len, .. } => *len as usize,
            TableStorage::Map(map) => map.len(),
        };
        occupied - self.dead_count
    }

    fn insert_into_map_with_tombstones(&mut self, key: TableKey, value: Val) {
        let existing = match &mut self.storage {
            TableStorage::Map(map) => map.get_full_mut(&key).map(|(index, _, existing)| {
                let was_live = !matches!(*existing, Val::Nil);
                if was_live {
                    *existing = value;
                }
                (index, was_live)
            }),
            TableStorage::Inline { .. } => unreachable!("map insertion requires map storage"),
        };
        if let Some((_, true)) = existing {
            #[cfg(debug_assertions)]
            self.debug_assert_invariants();
            return;
        }
        if let Some((index, false)) = existing {
            let TableStorage::Map(map) = &mut self.storage else {
                unreachable!("map insertion requires map storage");
            };
            // Re-adding a tombstoned key: vacate its old slot in place (it
            // stays a tombstone, so `dead_count` is unchanged) and append the
            // key below, moving it to the back of `pairs()` order. The key's
            // index changes, so caches holding the old one must re-resolve.
            map.replace_index(index, TableKey::vacant(index))
                .expect("no other slot holds the vacant key for this index");
            // `bump_version` would borrow all of `self` while `map` is live.
            self.version.set(self.version.get().wrapping_add(1));
        }
        // Both a new key and a re-added one add a slot; compacting once the
        // tombstones (vacated slots included) reach the live count keeps the
        // part within twice its live size at amortised O(1) per write.
        let live_count = match &self.storage {
            TableStorage::Map(map) => map.len() - self.dead_count,
            TableStorage::Inline { .. } => unreachable!("map insertion requires map storage"),
        };
        if self.dead_count >= live_count && self.compact_dead() {
            self.bump_version();
        }
        if let TableStorage::Map(map) = &mut self.storage {
            map.insert(key, value);
        }
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
    }

    fn insert_without_tombstones(&mut self, key: TableKey, value: Val) {
        let key_value = key.value();
        match &mut self.storage {
            TableStorage::Inline { entries, len } => {
                if let Some(index) = entries
                    .iter()
                    .take(*len as usize)
                    .position(|(entry_key, _)| *entry_key == key_value)
                {
                    entries[index].1 = value;
                } else if (*len as usize) < INLINE_CAPACITY {
                    entries[*len as usize] = (key_value, value);
                    *len += 1;
                } else {
                    self.promote_to_map(key, value);
                }
            }
            TableStorage::Map(map) => {
                map.insert(key, value);
            }
        }
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
    }

    /// Promote from inline storage to IndexMap, adding the new key-value pair.
    #[hotpath::measure]
    fn promote_to_map(&mut self, new_key: TableKey, new_value: Val) {
        debug_assert_eq!(self.dead_count, 0);
        let old_storage = std::mem::take(&mut self.storage);
        if let TableStorage::Inline { mut entries, len } = old_storage {
            // INLINE_CAPACITY + 1 would guarantee a rehash almost immediately
            // for a growing table; start at 8 to skip that first rehash.
            let mut map: IndexMap<TableKey, Val> =
                map_with_capacity((INLINE_CAPACITY + 1).next_power_of_two());
            for entry in entries.iter_mut().take(len as usize) {
                let (k, v) = std::mem::take(entry);
                let key = TableKey::from_prevalidated(k);
                map.insert(key, v);
            }
            map.insert(new_key, new_value);
            self.storage = TableStorage::Map(map);
        }
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
    }

    /// Remove a key and return its value (if any).
    #[hotpath::measure]
    fn remove(&mut self, key: &Val) -> Option<Val> {
        if let Some(slot) = self.array_slot(key) {
            let removed = self.array[slot];
            self.write_window(slot, Val::Nil);
            #[cfg(debug_assertions)]
            self.debug_assert_invariants();
            return (!matches!(removed, Val::Nil)).then_some(removed);
        }
        self.hash_remove(key)
    }

    /// Tombstone a hash-part key and return its value (if any).
    fn hash_remove(&mut self, key: &Val) -> Option<Val> {
        let removed = match &mut self.storage {
            TableStorage::Inline { entries, len } => {
                let mut removed = None;
                for (entry_key, entry_value) in entries.iter_mut().take(*len as usize) {
                    if entry_key == key {
                        let value = std::mem::replace(entry_value, Val::Nil);
                        if !matches!(value, Val::Nil) {
                            self.dead_count += 1;
                            removed = Some(value);
                        }
                        break;
                    }
                }
                removed
            }
            TableStorage::Map(map) => {
                let value = map.get_mut(&TableLookupKey(*key))?;
                let removed = std::mem::replace(value, Val::Nil);
                if matches!(removed, Val::Nil) {
                    None
                } else {
                    self.dead_count += 1;
                    Some(removed)
                }
            }
        };
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
        removed
    }

    /// Stably remove logically absent hash slots. The caller owns version changes.
    fn compact_dead(&mut self) -> bool {
        if self.dead_count == 0 {
            #[cfg(debug_assertions)]
            self.debug_assert_invariants();
            return false;
        }
        match &mut self.storage {
            TableStorage::Inline { entries, len } => {
                let mut write = 0;
                let mut packed: [(Val, Val); INLINE_CAPACITY] = Default::default();
                let mut packed_slots = packed.iter_mut();
                for entry in entries.iter_mut().take(*len as usize) {
                    let entry = std::mem::take(entry);
                    if !matches!(entry.1, Val::Nil) {
                        *packed_slots
                            .next()
                            .expect("packed table must have room for every live entry") = entry;
                        write += 1;
                    }
                }
                *entries = packed;
                *len = write as u8;
            }
            TableStorage::Map(map) => map.retain(|_, value| !matches!(value, Val::Nil)),
        }
        self.dead_count = 0;
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
        true
    }

    /// Stores the values of a table constructor's positional fields at keys
    /// `first..`. Like reference Lua, which presizes the array part for them,
    /// a nil positional value still occupies its array slot, so
    /// `#{1, nil, 3}` is 3.
    ///
    /// When the batch reaches the append position, the array reserves room
    /// for the batch's last key up front, so a constructor's positional part
    /// costs one allocation rather than a doubling sequence. The reservation
    /// is capacity only: the per-key loop below still decides, key by key,
    /// between appending (nil included) and a hash insert, and migration after
    /// each append can extend the array past the reservation.
    #[hotpath::measure]
    pub(super) fn set_list(
        &mut self,
        first: usize,
        mut values: impl ExactSizeIterator<Item = Val>,
    ) -> std::result::Result<(), TableError> {
        let count = values.len();
        if count > 0 && first <= self.vlen + 1 && self.base + self.array.len() == self.vlen {
            let last = first.saturating_add(count - 1);
            self.array.reserve(last.saturating_sub(self.vlen));
        }
        let mut key = first;
        while let Some(value) = values.next() {
            if key == self.vlen + 1 && self.appends_are_plain() {
                // Every remaining key is an append whose window preparation
                // and hash migration would do nothing, so the rest of the
                // batch lands in one extend with the same end state.
                self.grow_vlen_by(1 + values.len());
                self.array.push(value);
                self.array.extend(values);
                break;
            }
            if key == self.vlen + 1 {
                self.append(value);
            } else {
                self.insert(Val::Num(key as f64), value)?;
            }
            key += 1;
        }
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
        Ok(())
    }

    /// Inserts a value at the given array position, shifting elements up.
    /// Position is 1-based and must lie in `1..=array_len() + 1`.
    ///
    /// The border always lies inside the logical array, so the shift is one
    /// memmove of the stored elements from `pos` to the border; keys below
    /// the window are nil, and shifting nil into nil changes nothing. When
    /// the border is `vlen` the array grows by one key, nil included, exactly
    /// like an append. No slot moves to another key's index, so the version
    /// is untouched.
    #[hotpath::measure]
    pub(super) fn array_insert(&mut self, pos: usize, value: Val) {
        let len = self.array_len();
        debug_assert!(
            (1..=len + 1).contains(&pos),
            "insert position {pos} outside 1..={len}+1"
        );
        if pos == len + 1 {
            // Nothing shifts. Nil is a no-op, as `t[#t + 1] = nil` is.
            self.insert(Val::Num(pos as f64), value)
                .expect("array_insert: integer key insert cannot fail");
            #[cfg(debug_assertions)]
            self.debug_assert_invariants();
            return;
        }
        // `pos <= len`, so key `len` is live and lies inside the window; key
        // `len + 1` is nil and becomes the slot the rotation consumes.
        let structural = len == self.vlen;
        if structural {
            self.grow_vlen();
            self.array.push(Val::Nil);
        } else if len == self.base + self.array.len() {
            self.array.push(Val::Nil);
        }
        let low = pos.max(self.base + 1) - self.base - 1;
        let high = len - self.base;
        // The rotation moves nil into `low` and shifts `low..high` up by one,
        // so every slot below the dead-prefix hint stays nil.
        self.array[low..=high].rotate_right(1);
        if structural {
            // Restore "no live hash entry at the append key" before the
            // write, whose own checks assume it.
            self.migrate_from_hash();
        }
        if pos > self.base {
            self.write_window(pos - self.base - 1, value);
        } else if !matches!(value, Val::Nil) {
            self.write_outside_window(pos, value);
        }
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
    }

    /// Removes and returns the value at the given position, shifting later
    /// elements down, with reference Lua's `tremove` semantics: for a
    /// position outside `1..=array_len()` (only 0 for an empty sequence, or
    /// `array_len() + 1`) it returns `t[pos]` and clears it.
    #[hotpath::measure]
    pub(super) fn array_remove(&mut self, pos: usize) -> Val {
        let len = self.array_len();
        if (1..=len).contains(&pos) {
            // Key `len` is live, so it lies inside the window.
            let removed = self.array_value(pos);
            let end = len - self.base;
            if pos > self.base {
                let position = pos - self.base - 1;
                self.array.copy_within(position + 1..end, position);
                self.array[end - 1] = Val::Nil;
                self.lead = self.lead.min(position);
            } else {
                // Keys `pos..base` are nil and stay nil; key `base` receives
                // the window's first value.
                let first = self.array[0];
                self.array.copy_within(1..end, 0);
                self.array[end - 1] = Val::Nil;
                self.lead = 0;
                if !matches!(first, Val::Nil) {
                    self.write_outside_window(self.base, first);
                }
            }
            #[cfg(debug_assertions)]
            self.debug_assert_invariants();
            return removed;
        }
        let key = Val::Num(pos as f64);
        let removed = self.get(&key);
        self.remove(&key);
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
        removed
    }

    /// Returns the sequence `t[1..=#t]` as a Vec for sorting.
    #[hotpath::measure]
    pub(super) fn get_array(&self) -> Vec<Val> {
        let len = self.array_len();
        // Key `len` (when nonzero) is live, so `1..=len` ends inside the window.
        let below = self.base.min(len);
        let mut out = Vec::with_capacity(len);
        out.resize(below, Val::Nil);
        out.extend_from_slice(&self.array[..len - below]);
        out
    }

    /// Writes sorted values back to keys `1..=values.len()`. Entries beyond
    /// the original sort range survive even if a comparator added them.
    /// Each write is O(1), so the writeback is linear in the sorted range.
    #[hotpath::measure]
    pub(super) fn set_array(&mut self, values: Vec<Val>) {
        for (i, v) in values.into_iter().enumerate() {
            self.insert(Val::Num((i + 1) as f64), v)
                .expect("set_array: integer key insert cannot fail");
        }
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
    }

    /// Returns the metatable of this table, if any.
    pub(super) fn get_metatable(&self) -> Option<ObjectPtr> {
        self.metatable
    }

    /// Sets the metatable of this table.
    pub(super) fn set_metatable(&mut self, mt: Option<ObjectPtr>) {
        self.metatable = mt;
        #[cfg(debug_assertions)]
        self.debug_assert_invariants();
    }

    /// Scan from `position` in iteration order, skipping nil slots.
    fn next_live_from(&self, position: Position) -> TableNextWithIndex {
        let hash_start = match position {
            Position::Array(start) => {
                // `start` is an absolute slot (key - 1); array keys below the
                // window and inside the dead-prefix hint are nil.
                let first = start.saturating_sub(self.base).max(self.lead);
                if let Some((position, value)) = self
                    .array
                    .iter()
                    .enumerate()
                    .skip(first)
                    .find(|(_, value)| !matches!(value, Val::Nil))
                {
                    let slot = self.base + position;
                    return TableNextWithIndex::Pair {
                        index: ARRAY_SLOT_TAG | slot,
                        key: Val::Num((slot + 1) as f64),
                        value: *value,
                    };
                }
                0
            }
            Position::Hash(start) => start,
        };
        match &self.storage {
            TableStorage::Inline { entries, len } => entries
                .iter()
                .take(*len as usize)
                .enumerate()
                .skip(hash_start)
                .find(|(_, (_, value))| !matches!(value, Val::Nil))
                .map_or(TableNextWithIndex::End, |(index, (key, value))| {
                    TableNextWithIndex::Pair {
                        index,
                        key: *key,
                        value: *value,
                    }
                }),
            TableStorage::Map(map) => map
                .iter()
                .enumerate()
                .skip(hash_start)
                .find(|(_, (_, value))| !matches!(value, Val::Nil))
                .map_or(TableNextWithIndex::End, |(index, (key, value))| {
                    TableNextWithIndex::Pair {
                        index,
                        key: key.value(),
                        value: *value,
                    }
                }),
        }
    }

    /// Advance from `control`, retaining physical storage indices. A dead
    /// control slot remains valid, matching Lua's `next` behaviour here.
    pub(super) fn next_with_index(&self, control: &Val) -> TableNextWithIndex {
        if matches!(control, Val::Nil) {
            return self.next_live_from(Position::Array(0));
        }
        if !is_valid_table_key(control) {
            return TableNextWithIndex::InvalidKey;
        }
        // Any array key resumes the array scan, stored or not: the window may
        // have moved since the traversal visited it.
        if let Some(key) = self.array_key(control) {
            return self.next_live_from(Position::Array(key));
        }
        let index = match &self.storage {
            TableStorage::Inline { entries, len } => entries
                .iter()
                .take(*len as usize)
                .position(|(key, _)| key == control),
            TableStorage::Map(map) => map.get_index_of(&TableLookupKey(*control)),
        };
        index.map_or(TableNextWithIndex::InvalidKey, |index| {
            self.next_live_from(Position::Hash(index.saturating_add(1)))
        })
    }

    /// Validate a cursor's raw slot key, including a tombstone, then advance
    /// with the same storage-specific walk as `next_with_index`.
    pub(super) fn next_from_matching_index(
        &self,
        index: usize,
        control: &Val,
    ) -> TableNextWithIndex {
        if !is_valid_table_key(control) {
            return TableNextWithIndex::InvalidKey;
        }
        if index & ARRAY_SLOT_TAG != 0 {
            let slot = index & !ARRAY_SLOT_TAG;
            return if self.array_key(control) == Some(slot + 1) {
                self.next_live_from(Position::Array(slot + 1))
            } else {
                TableNextWithIndex::InvalidKey
            };
        }
        let matches = match &self.storage {
            TableStorage::Inline { entries, len } => {
                index < *len as usize && entries[index].0 == *control
            }
            TableStorage::Map(map) => map
                .get_index(index)
                .is_some_and(|(key, _)| TableLookupKey(*control).equivalent(key)),
        };
        if matches {
            self.next_live_from(Position::Hash(index.saturating_add(1)))
        } else {
            TableNextWithIndex::InvalidKey
        }
    }

    /// Returns the next key-value pair after the given key.
    /// If key is nil, returns the first key-value pair. Distinguishes the end
    /// of iteration from an invalid control key.
    #[hotpath::measure]
    pub(super) fn next(&self, key: &Val) -> TableNext {
        match self.next_with_index(key) {
            TableNextWithIndex::Pair { key, value, .. } => TableNext::Pair(key, value),
            TableNextWithIndex::End => TableNext::End,
            TableNextWithIndex::InvalidKey => TableNext::InvalidKey,
        }
    }
}

impl Table {
    /// Mark all values contained in this table as reachable.
    /// Called by the GC during the mark phase.
    #[hotpath::measure]
    pub(super) fn mark_values(&self, heap: &GcHeap, worklist: &mut Vec<ObjectPtr>) {
        for value in &self.array[self.lead..] {
            value.mark_reachable(heap, worklist);
        }
        match &self.storage {
            TableStorage::Inline { entries, len } => {
                for (key, value) in entries.iter().take(*len as usize) {
                    if !matches!(value, Val::Nil) {
                        key.mark_reachable(heap, worklist);
                        value.mark_reachable(heap, worklist);
                    }
                }
            }
            TableStorage::Map(map) => {
                for (key, value) in map {
                    if !matches!(value, Val::Nil) {
                        key.value().mark_reachable(heap, worklist);
                        value.mark_reachable(heap, worklist);
                    }
                }
            }
        }
        if let Some(mt) = &self.metatable {
            heap.mark(*mt, worklist);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn n(x: usize) -> Val {
        Val::Num(x as f64)
    }

    /// A number key that can never live in the array part.
    fn h(x: usize) -> Val {
        Val::Num(x as f64 + 0.5)
    }

    fn fill(t: &mut Table, range: std::ops::RangeInclusive<usize>) {
        for i in range {
            t.insert(n(i), Val::Bool(true)).unwrap();
        }
    }

    fn fill_hash(t: &mut Table, range: std::ops::RangeInclusive<usize>) {
        for i in range {
            t.insert(h(i), Val::Bool(true)).unwrap();
        }
    }

    #[test]
    fn nil_and_nan_reads_are_rejected_on_the_hash_path() {
        let mut table = Table::default();
        fill_hash(&mut table, 1..=6);

        for key in [Val::Nil, Val::Num(f64::NAN)] {
            assert!(!is_valid_table_key(&key));
            assert!(key.table_key().is_err());
            assert_eq!(table.get(&key), Val::Nil);
            assert_eq!(table.get_with_index(&key), None);
        }

        assert!(matches!(
            table.next_with_index(&Val::Num(f64::NAN)),
            TableNextWithIndex::InvalidKey
        ));
        let (index, _) = table.get_with_index(&h(1)).expect("hash key exists");
        assert!(matches!(
            table.next_from_matching_index(index, &Val::Num(f64::NAN)),
            TableNextWithIndex::InvalidKey
        ));
    }

    /// The array-part semantics before the window existed: one physical
    /// slot per logical array key, never shrinking, beside an
    /// insertion-ordered hash part. The real table must be indistinguishable
    /// from it through every observable: `pairs` order, `#`, reads and the
    /// sequence operations.
    #[derive(Default)]
    struct Model {
        array: Vec<Val>,
        hash: Vec<(Val, Val)>,
    }

    impl Model {
        fn hash_slot(&self, key: Val) -> Option<usize> {
            self.hash.iter().position(|(k, _)| *k == key)
        }

        fn array_key(&self, key: Val) -> Option<usize> {
            match key {
                Val::Num(x) => array_index(x, self.array.len()),
                _ => None,
            }
        }

        fn take_hash(&mut self, key: Val) -> Val {
            match self.hash_slot(key) {
                Some(i) => std::mem::take(&mut self.hash[i].1),
                None => Val::Nil,
            }
        }

        fn push(&mut self, value: Val) {
            self.array.push(value);
            loop {
                let next = n(self.array.len() + 1);
                let value = self.take_hash(next);
                if matches!(value, Val::Nil) {
                    break;
                }
                self.array.push(value);
            }
        }

        fn insert(&mut self, key: Val, value: Val) {
            if let Some(slot) = self.array_key(key) {
                self.array[slot] = value;
            } else if matches!(value, Val::Nil) {
                self.take_hash(key);
            } else if key == n(self.array.len() + 1) {
                self.push(value);
            } else {
                match self.hash_slot(key) {
                    Some(i) if !matches!(self.hash[i].1, Val::Nil) => self.hash[i].1 = value,
                    Some(i) => {
                        self.hash.remove(i);
                        self.hash.push((key, value));
                    }
                    None => self.hash.push((key, value)),
                }
            }
        }

        fn get(&self, key: Val) -> Val {
            match self.array_key(key) {
                Some(slot) => self.array[slot],
                None => self.hash_slot(key).map_or(Val::Nil, |i| self.hash[i].1),
            }
        }

        fn border(&self) -> usize {
            match self.array.last() {
                None => 0,
                Some(last) if !matches!(last, Val::Nil) => self.array.len(),
                Some(_) => {
                    let (mut lo, mut hi) = (0, self.array.len());
                    while hi - lo > 1 {
                        let mid = lo + (hi - lo) / 2;
                        if matches!(self.array[mid - 1], Val::Nil) {
                            hi = mid;
                        } else {
                            lo = mid;
                        }
                    }
                    lo
                }
            }
        }

        fn array_insert(&mut self, pos: usize, value: Val) {
            let len = self.border();
            if len < self.array.len() {
                self.array[pos - 1..=len].rotate_right(1);
                self.array[pos - 1] = value;
            } else if pos == len + 1 {
                self.insert(n(pos), value);
            } else {
                self.array.insert(pos - 1, value);
                let last = self.array.pop().expect("just inserted");
                self.push(last);
            }
        }

        fn array_remove(&mut self, pos: usize) -> Val {
            let len = self.border();
            if (1..=len).contains(&pos) {
                let removed = self.array[pos - 1];
                self.array.copy_within(pos..len, pos - 1);
                self.array[len - 1] = Val::Nil;
                return removed;
            }
            let removed = self.get(n(pos));
            self.insert(n(pos), Val::Nil);
            removed
        }

        fn pairs(&self) -> Vec<(Val, Val)> {
            self.array
                .iter()
                .enumerate()
                .map(|(slot, value)| (n(slot + 1), *value))
                .chain(self.hash.iter().copied())
                .filter(|(_, value)| !matches!(value, Val::Nil))
                .collect()
        }
    }

    fn pairs(t: &Table) -> Vec<(Val, Val)> {
        let mut control = Val::Nil;
        let mut out = Vec::new();
        while let TableNext::Pair(key, value) = t.next(&control) {
            out.push((key, value));
            control = key;
        }
        out
    }

    /// Drives the table and the model through queue, deque, stack, sparse
    /// and sequence-library operations from a fixed-seed generator, and
    /// requires every observable to agree after each step.
    #[test]
    fn moving_the_window_is_unobservable() {
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move |bound: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 33) as usize) % bound
        };
        for round in 0..40 {
            let mut t = Table::default();
            let mut m = Model::default();
            let mut head = 1;
            let mut tail = 0;
            for step in 0..600 {
                let value = Val::Num((round * 1000 + step) as f64);
                match next(10) {
                    // Queue push and pop.
                    0 | 1 => {
                        tail += 1;
                        t.insert(n(tail), value).unwrap();
                        m.insert(n(tail), value);
                    }
                    2 | 3 => {
                        if head <= tail {
                            t.insert(n(head), Val::Nil).unwrap();
                            m.insert(n(head), Val::Nil);
                            head += 1;
                        }
                    }
                    // Deque push at the front, or a queue reset.
                    4 => {
                        if head > 1 && next(2) == 0 {
                            head -= 1;
                            t.insert(n(head), value).unwrap();
                            m.insert(n(head), value);
                        } else if head > tail {
                            head = 1;
                            tail = 0;
                        }
                    }
                    // A write or clear anywhere near the logical array.
                    5 | 6 => {
                        let key = n(1 + next(m.array.len() + 3));
                        let value = if next(3) == 0 { Val::Nil } else { value };
                        t.insert(key, value).unwrap();
                        m.insert(key, value);
                    }
                    // A hash key, sometimes cleared.
                    7 => {
                        let key = h(next(6));
                        let value = if next(2) == 0 { Val::Nil } else { value };
                        t.insert(key, value).unwrap();
                        m.insert(key, value);
                    }
                    // table.insert / table.remove at a legal position.
                    8 => {
                        let len = m.border();
                        let pos = 1 + next(len + 1);
                        let value = if next(4) == 0 { Val::Nil } else { value };
                        t.array_insert(pos, value);
                        m.array_insert(pos, value);
                    }
                    _ => {
                        let len = m.border();
                        let pos = if len == 0 { next(2) } else { 1 + next(len + 1) };
                        assert_eq!(t.array_remove(pos), m.array_remove(pos));
                    }
                }
                assert_eq!(t.array_len(), m.border(), "round {round} step {step}");
                assert_eq!(t.vlen, m.array.len(), "round {round} step {step}");
                assert_eq!(pairs(&t), m.pairs(), "round {round} step {step}");
                assert_eq!(t.get_array(), m.array[..m.border()].to_vec());
                for key in 0..m.array.len() + 3 {
                    assert_eq!(t.get(&n(key)), m.get(n(key)));
                }
            }
        }
    }

    /// A FIFO that runs far keeps a window proportional to its live length,
    /// including after a stray out-of-order clear, and a drained queue
    /// that restarts at key 1 does not regrow its history.
    #[test]
    fn queue_window_stays_bounded() {
        let mut t = Table::default();
        fill(&mut t, 1..=3);
        t.insert(n(2), Val::Nil).unwrap();
        t.insert(n(1), Val::Nil).unwrap();
        let (mut head, mut tail) = (3, 3);
        while tail < 100_000 {
            tail += 1;
            t.insert(n(tail), Val::Bool(true)).unwrap();
            t.insert(n(head), Val::Nil).unwrap();
            head += 1;
        }
        assert!(
            t.array.len() <= 4,
            "window {} for one live key",
            t.array.len()
        );
        assert!(t.array.capacity() <= 4 * EMPTIED_WINDOW_CAPACITY);
        assert_eq!(t.array_len(), 100_000);
        // Drain, then restart from key 1 the way a reset queue does.
        t.insert(n(head), Val::Nil).unwrap();
        for i in 1..=5 {
            t.insert(n(i), Val::Bool(true)).unwrap();
        }
        assert!(t.array.len() <= 5);
        assert_eq!(t.vlen, 100_000);
        assert_eq!(keys(&t), (1..=5).map(n).collect::<Vec<_>>());
    }

    fn keys(t: &Table) -> Vec<Val> {
        let mut control = Val::Nil;
        let mut keys = Vec::new();
        while let TableNext::Pair(key, _) = t.next(&control) {
            keys.push(key);
            control = key;
        }
        keys
    }

    fn is_border(t: &Table, len: usize) -> bool {
        let after = matches!(t.get(&n(len + 1)), Val::Nil);
        let here = len == 0 || !matches!(t.get(&n(len)), Val::Nil);
        here && after
    }

    #[test]
    fn empty_table_has_border_zero() {
        let t = Table::default();
        assert_eq!(t.array_len(), 0);
    }

    #[test]
    fn small_dense_array_returns_exact_length() {
        let mut t = Table::default();
        fill(&mut t, 1..=INLINE_CAPACITY);
        assert_eq!(t.array_len(), INLINE_CAPACITY);
    }

    #[test]
    fn large_dense_array_returns_exact_length() {
        let mut t = Table::default();
        fill(&mut t, 1..=500);
        assert_eq!(t.array_len(), 500);
    }

    #[test]
    fn length_tracks_inserts_beyond_the_border() {
        let mut t = Table::default();
        fill(&mut t, 1..=10);
        assert_eq!(t.array_len(), 10);
        t.insert(n(20), Val::Bool(true)).unwrap();
        let len = t.array_len();
        assert!(is_border(&t, len), "len={len} is not a border");
    }

    #[test]
    fn dense_with_single_hole_returns_a_border() {
        // 1..=1000 with hole at 500. Borders are 499 and 1000.
        // Reference Lua 5.2/5.4 both return 1000 here.
        let mut t = Table::default();
        fill(&mut t, 1..=1000);
        t.insert(n(500), Val::Nil).unwrap();
        assert_eq!(t.array_len(), 1000);
    }

    #[test]
    fn trailing_holes_binary_search_to_a_border() {
        let mut t = Table::default();
        fill(&mut t, 1..=100);
        for key in 40..=100 {
            t.insert(n(key), Val::Nil).unwrap();
        }
        assert_eq!(t.array_len(), 39);
        t.insert(n(1), Val::Nil).unwrap();
        let len = t.array_len();
        assert!(is_border(&t, len), "len={len} is not a border");
    }

    #[test]
    fn two_dense_runs_returns_a_border() {
        // 1..=3 dense, gap at 4, 5..=7 dense. Borders: 3 and 7.
        let mut t = Table::default();
        fill(&mut t, 1..=3);
        fill(&mut t, 5..=7);
        let len = t.array_len();
        assert!(is_border(&t, len), "len={len} is not a border");
    }

    #[test]
    fn nil_at_one_returns_zero() {
        let mut t = Table::default();
        t.insert(n(2), Val::Bool(true)).unwrap();
        t.insert(n(3), Val::Bool(true)).unwrap();
        assert_eq!(t.array_len(), 0);
    }

    #[test]
    fn appending_the_gap_migrates_later_hash_keys() {
        let mut t = Table::default();
        t.insert(Val::Bool(true), Val::Bool(true)).unwrap();
        t.insert(n(3), Val::Num(3.0)).unwrap();
        t.insert(n(2), Val::Num(2.0)).unwrap();
        t.insert(n(5), Val::Num(5.0)).unwrap();
        assert_eq!(t.array_len(), 0);
        t.insert(n(1), Val::Num(1.0)).unwrap();
        assert_eq!(t.array_len(), 3);
        assert_eq!(t.array, vec![Val::Num(1.0), Val::Num(2.0), Val::Num(3.0)]);
        assert_eq!(keys(&t), vec![n(1), n(2), n(3), Val::Bool(true), n(5)]);
        // The migrated keys left tombstones; once they outnumbered the live
        // hash entries the part was compacted, which moves hash indices.
        assert_eq!(t.dead_count, 0);
        assert_eq!(t.version(), 1);
    }

    #[test]
    fn single_element_returns_one() {
        let mut t = Table::default();
        t.insert(n(1), Val::Bool(true)).unwrap();
        assert_eq!(t.array_len(), 1);
    }

    #[test]
    fn power_of_two_boundary() {
        let mut t = Table::default();
        fill(&mut t, 1..=8);
        assert_eq!(t.array_len(), 8);
    }

    #[test]
    fn set_list_keeps_nil_positional_slots_in_the_array() {
        let mut t = Table::default();
        t.set_list(1, [Val::Num(1.0), Val::Nil, Val::Num(3.0)].into_iter())
            .unwrap();
        assert_eq!(t.array.len(), 3);
        assert_eq!(t.array_len(), 3);
        t.set_list(1, [Val::Num(1.0), Val::Nil].into_iter())
            .unwrap();
        t.insert(n(3), Val::Nil).unwrap();
        assert_eq!(t.array_len(), 1);
    }

    /// `set_list` extends a plain run of appends in one step; the table it
    /// leaves must match key-by-key appends exactly, window included.
    #[test]
    fn set_list_bulk_appends_match_key_by_key_appends() {
        fn key_by_key(t: &mut Table, first: usize, values: &[Val]) {
            for (key, value) in (first..).zip(values.iter().copied()) {
                if key == t.vlen + 1 {
                    t.append(value);
                } else {
                    t.insert(n(key), value).unwrap();
                }
            }
        }
        let live = Val::Bool(true);
        let batches: [&[Val]; 4] = [
            &[live, Val::Nil, live, live],
            &[Val::Nil, live, live, Val::Nil],
            &[live; 9],
            &[Val::Nil, Val::Nil, live],
        ];
        let setups: [fn(&mut Table); 5] = [
            |_| {},
            |t| fill(t, 1..=3),
            |t| t.insert(n(3), Val::Bool(false)).unwrap(),
            |t| fill_hash(t, 1..=2),
            |t| {
                fill(t, 1..=20);
                for i in 1..=15 {
                    t.insert(n(i), Val::Nil).unwrap();
                }
            },
        ];
        for setup in setups {
            for batch in batches {
                let mut bulk = Table::default();
                let mut reference = Table::default();
                setup(&mut bulk);
                setup(&mut reference);
                let first = bulk.vlen + 1;
                bulk.set_list(first, batch.iter().copied()).unwrap();
                key_by_key(&mut reference, first, batch);
                assert_eq!(bulk.array, reference.array);
                assert_eq!(
                    (bulk.base, bulk.vlen, bulk.lead),
                    (reference.base, reference.vlen, reference.lead)
                );
                assert_eq!(bulk.hash_live_len(), reference.hash_live_len());
                for key in 1..=40 {
                    assert_eq!(bulk.get(&n(key)), reference.get(&n(key)));
                    assert_eq!(bulk.get(&h(key)), reference.get(&h(key)));
                }
            }
        }
    }

    #[test]
    fn array_remove_shifts_in_place_without_reordering() {
        let mut t = Table::default();
        t.insert(Val::Bool(false), Val::Bool(true)).unwrap();
        fill(&mut t, 1..=1000);
        for i in 1..=1000 {
            t.insert(n(i), Val::Num(i as f64)).unwrap();
        }
        let version = t.version();
        assert_eq!(t.array_remove(1), Val::Num(1.0));
        assert_eq!(t.array_len(), 999);
        assert_eq!(t.get(&n(1)), Val::Num(2.0));
        assert_eq!(t.get(&n(999)), Val::Num(1000.0));
        assert_eq!(t.get(&n(1000)), Val::Nil);
        assert_eq!(t.version(), version);
        let order = keys(&t);
        assert_eq!(order.first(), Some(&n(1)));
        assert_eq!(order.last(), Some(&Val::Bool(false)));
        // Queue idiom: push at the border, pop the front.
        t.array_insert(1000, Val::Num(-1.0));
        assert_eq!(t.array.len(), 1000);
        assert_eq!(t.array_len(), 1000);
        assert_eq!(t.array_remove(1000), Val::Num(-1.0));
    }

    #[test]
    fn array_remove_outside_the_sequence_reads_and_clears_the_key() {
        let mut t = Table::default();
        t.insert(n(0), Val::Bool(true)).unwrap();
        assert_eq!(t.array_remove(0), Val::Bool(true));
        assert_eq!(t.get(&n(0)), Val::Nil);
        assert_eq!(t.array_remove(0), Val::Nil);
    }

    #[test]
    fn array_insert_shifts_up_and_migrates() {
        let mut t = Table::default();
        fill(&mut t, 1..=3);
        t.insert(n(5), Val::Num(5.0)).unwrap();
        t.array_insert(1, Val::Num(0.0));
        assert_eq!(t.array_len(), 5);
        assert_eq!(t.get(&n(1)), Val::Num(0.0));
        assert_eq!(t.get(&n(5)), Val::Num(5.0));
        assert_eq!(keys(&t), (1..=5).map(n).collect::<Vec<_>>());
        // Appending nil is a no-op.
        t.array_insert(6, Val::Nil);
        assert_eq!(t.array.len(), 5);
    }

    #[test]
    fn set_array_writes_back_in_place() {
        let mut t = Table::default();
        t.insert(Val::Bool(true), Val::Bool(true)).unwrap();
        for i in 1..=5 {
            t.insert(n(i), Val::Num((6 - i) as f64)).unwrap();
        }
        let mut values = t.get_array();
        values.reverse();
        t.set_array(values);
        assert_eq!(t.get(&n(1)), Val::Num(1.0));
        assert_eq!(t.get(&n(5)), Val::Num(5.0));
        assert_eq!(t.dead_count, 0);
        assert_eq!(t.version(), 0);
        assert_eq!(keys(&t).last(), Some(&Val::Bool(true)));
    }

    #[test]
    fn next_distinguishes_end_from_invalid_key_array() {
        let mut t = Table::default();
        fill(&mut t, 1..=3);

        assert!(matches!(
            t.next(&Val::Nil),
            TableNext::Pair(Val::Num(1.0), Val::Bool(true))
        ));
        assert!(matches!(
            t.next(&n(2)),
            TableNext::Pair(Val::Num(3.0), Val::Bool(true))
        ));
        assert!(matches!(t.next(&n(3)), TableNext::End));
        assert!(matches!(t.next(&n(4)), TableNext::InvalidKey));
        assert!(matches!(t.next(&Val::Num(f64::NAN)), TableNext::InvalidKey));
    }

    #[test]
    fn next_distinguishes_end_from_invalid_key_map() {
        let mut t = Table::default();
        fill_hash(&mut t, 1..=5);

        assert!(matches!(
            t.next(&Val::Nil),
            TableNext::Pair(Val::Num(1.5), _)
        ));
        assert!(matches!(t.next(&h(3)), TableNext::Pair(Val::Num(4.5), _)));
        assert!(matches!(t.next(&h(5)), TableNext::End));
        assert!(matches!(t.next(&h(6)), TableNext::InvalidKey));
        assert!(matches!(t.next(&Val::Num(f64::NAN)), TableNext::InvalidKey));
    }

    #[test]
    fn indexed_next_is_the_next_oracle_for_every_storage_variant() {
        // Array part plus an inline hash part, and plus a map hash part.
        for count in [4, 5] {
            let mut table = Table::default();
            fill(&mut table, 1..=count);
            fill_hash(&mut table, 1..=count);
            table.remove(&n(1));
            table.remove(&n(3));
            table.remove(&h(1));
            table.remove(&h(3));

            let mut control = Val::Nil;
            let mut expected = Vec::new();
            let mut indexed = Vec::new();
            loop {
                match table.next(&control) {
                    TableNext::Pair(key, value) => {
                        expected.push((key, value));
                        control = key;
                    }
                    TableNext::End => break,
                    TableNext::InvalidKey => panic!("control from next must stay valid"),
                }
            }
            let live_keys: Vec<_> = (1..=count)
                .filter(|key| *key != 1 && *key != 3)
                .map(n)
                .chain((1..=count).filter(|key| *key != 1 && *key != 3).map(h))
                .collect();
            assert_eq!(
                expected.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
                live_keys
            );
            control = Val::Nil;
            loop {
                match table.next_with_index(&control) {
                    TableNextWithIndex::Pair { index, key, value } => {
                        indexed.push((index, key, value));
                        assert_eq!(table.get_index(index), Some((key, value)));
                        match table.next_from_matching_index(index, &key) {
                            TableNextWithIndex::Pair {
                                key: next, value, ..
                            } => match table.next(&key) {
                                TableNext::Pair(actual_key, actual_value) => {
                                    assert_eq!((actual_key, actual_value), (next, value));
                                }
                                _ => panic!("indexed successor must match next"),
                            },
                            TableNextWithIndex::End => {
                                assert!(matches!(table.next(&key), TableNext::End));
                            }
                            TableNextWithIndex::InvalidKey => {
                                panic!("returned index must validate")
                            }
                        }
                        control = key;
                    }
                    TableNextWithIndex::End => break,
                    TableNextWithIndex::InvalidKey => panic!("nil control must be valid"),
                }
            }
            assert_eq!(
                expected,
                indexed
                    .into_iter()
                    .map(|(_, key, value)| (key, value))
                    .collect::<Vec<_>>()
            );
            assert!(matches!(
                table.next_with_index(&n(1)),
                TableNextWithIndex::Pair { .. }
            ));
            assert!(matches!(
                table.next_with_index(&h(count + 1)),
                TableNextWithIndex::InvalidKey
            ));
            assert!(matches!(
                table.next_with_index(&Val::Num(f64::NAN)),
                TableNextWithIndex::InvalidKey
            ));
        }
    }

    #[test]
    fn indexed_next_accepts_tombstoned_control_and_reports_tail_end() {
        // count 3 stays Inline; count 5 exercises Map storage.
        for count in [3, 5] {
            let mut table = Table::default();
            fill_hash(&mut table, 1..=count);
            table.remove(&h(2));
            let control_index = match table.next_with_index(&h(1)) {
                TableNextWithIndex::Pair {
                    index,
                    key: Val::Num(3.5),
                    ..
                } => index,
                _ => panic!("expected the live successor"),
            };
            // A tombstoned control (key 2.5, dead at index 1) must validate
            // and step to the live successor - filter-in-place iteration
            // depends on it.
            assert!(matches!(
                table.next_from_matching_index(1, &h(2)),
                TableNextWithIndex::Pair {
                    key: Val::Num(3.5),
                    ..
                }
            ));
            match table.next_from_matching_index(control_index, &h(3)) {
                TableNextWithIndex::Pair {
                    key: Val::Num(next),
                    ..
                } if count == 5 => assert_eq!(next, 4.5),
                TableNextWithIndex::End if count == 3 => {}
                other => panic!("unexpected successor of key 3.5 (count {count}): {other:?}"),
            }
            let last_index = match table.next_with_index(&h(count - 1)) {
                TableNextWithIndex::Pair { index, .. } => index,
                _ => panic!("expected the last live entry"),
            };
            assert!(matches!(
                table.next_from_matching_index(last_index, &h(count)),
                TableNextWithIndex::End
            ));
        }
    }

    #[test]
    fn clearing_array_keys_during_traversal_keeps_cursors_valid() {
        for hash_count in [3, 5] {
            let mut t = Table::default();
            fill(&mut t, 1..=6);
            fill_hash(&mut t, 1..=hash_count);
            let mut next = t.next_with_index(&Val::Nil);
            let mut visited = Vec::new();
            loop {
                let (index, key) = match next {
                    TableNextWithIndex::Pair { index, key, .. } => (index, key),
                    TableNextWithIndex::End => break,
                    TableNextWithIndex::InvalidKey => panic!("cleared control must stay valid"),
                };
                t.insert(key, Val::Nil).unwrap();
                visited.push(key);
                // Follow the indexed successor itself, including the boundary
                // into each hash storage variant, without a lookup fallback.
                next = t.next_from_matching_index(index, &key);
            }
            assert_eq!(
                visited,
                (1..=6)
                    .map(n)
                    .chain((1..=hash_count).map(h))
                    .collect::<Vec<_>>()
            );
            assert_eq!(t.array_len(), 0);
            assert!(matches!(t.next(&Val::Nil), TableNext::End));
        }
    }

    #[test]
    fn rust_fn_key_reassignment_updates_in_place() {
        // Proving "no duplicate entry" needs a storage-level assertion. From
        // Lua, a stale duplicate is invisible: a `pairs` scan filtering on the
        // updated value simply skips it, so the script-level test passes either
        // way. Cover both storage arms - inline scans linearly with PartialEq,
        // and Map probes with Hash, so only the second exercises Hash at all.
        fn probe(_state: &mut crate::vm::State) -> crate::Result<u8> {
            Ok(0)
        }

        for preset in [0, 5] {
            let mut t = Table::default();
            fill_hash(&mut t, 1..=preset);
            let before = (1..=preset).count();

            let key = Val::RustFn(probe);
            t.insert(key, Val::Num(1.0)).expect("first insert succeeds");
            assert_eq!(t.get(&key), Val::Num(1.0));

            t.insert(key, Val::Num(2.0))
                .expect("second insert succeeds");
            assert_eq!(t.get(&key), Val::Num(2.0));

            assert_eq!(
                keys(&t).len(),
                before + 1,
                "reassigning a RustFn key appended a duplicate (preset {preset})"
            );
        }
    }

    #[test]
    fn tombstones_advance_iteration_and_hide_indexed_access() {
        for count in [3, 6] {
            let mut t = Table::default();
            fill_hash(&mut t, 1..=count);
            assert_eq!(t.version(), 0);
            assert_eq!(t.remove(&h(2)), Some(Val::Bool(true)));
            assert_eq!(t.version(), 0);
            assert!(matches!(t.next(&h(1)), TableNext::Pair(Val::Num(3.5), _)));
            assert!(matches!(t.next(&h(2)), TableNext::Pair(Val::Num(3.5), _)));
            assert!(matches!(t.next(&h(count + 1)), TableNext::InvalidKey));
            assert_eq!(t.get_with_index(&h(2)), None);
            assert_eq!(t.get_index(1), None);
            assert!(!t.set_at_index(1, Val::Bool(false)));
        }
    }

    #[test]
    fn array_slots_are_cache_stable() {
        let mut t = Table::default();
        fill(&mut t, 1..=3);
        let (index, _) = t.get_with_index(&n(2)).unwrap();
        fill(&mut t, 4..=100);
        assert_eq!(t.get_index(index), Some((n(2), Val::Bool(true))));
        assert!(t.set_at_index(index, Val::Bool(false)));
        assert_eq!(t.get(&n(2)), Val::Bool(false));
        t.insert(n(2), Val::Nil).unwrap();
        assert_eq!(t.get_index(index), None);
        assert!(!t.set_at_index(index, Val::Bool(true)));
        assert_eq!(t.version(), 0);
    }

    #[test]
    fn reinserted_tombstone_moves_to_back_and_bumps_once() {
        for count in [3, 5] {
            let mut t = Table::default();
            fill_hash(&mut t, 1..=count);
            t.remove(&h(2));
            t.insert(h(2), Val::Bool(false)).unwrap();
            assert_eq!(t.version(), 1);
            assert_eq!(
                keys(&t),
                (1..=count)
                    .filter(|key| *key != 2)
                    .map(h)
                    .chain(std::iter::once(h(2)))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[cfg(feature = "snapshot")]
    #[test]
    fn rebuild_moves_version_past_the_old_value() {
        let mut t = Table::default();
        fill_hash(&mut t, 1..=3);
        assert_eq!(t.version(), 0);
        t.rebuild_from_snapshot(
            0,
            0,
            vec![
                (h(3), Val::Bool(true)),
                (h(1), Val::Bool(true)),
                (h(2), Val::Bool(true)),
            ],
        )
        .unwrap();
        assert_eq!(t.version(), 1);
        assert_eq!(t.get_index(0).map(|(key, _)| key), Some(h(3)));
        t.rebuild_from_snapshot(0, 0, vec![(h(1), Val::Bool(true))])
            .unwrap();
        assert_eq!(t.version(), 2);
        assert_eq!(t.get_index(0).map(|(key, _)| key), Some(h(1)));
    }

    #[cfg(feature = "snapshot")]
    #[test]
    fn snapshot_round_trip_preserves_array_holes() {
        let mut t = Table::default();
        fill(&mut t, 1..=4);
        t.insert(Val::Bool(true), Val::Bool(true)).unwrap();
        t.insert(n(2), Val::Nil).unwrap();
        t.insert(n(3), Val::Nil).unwrap();
        let (array_len, array_slots, entries) = t.snapshot_entries();
        assert_eq!((array_len, array_slots), (4, 4));
        let mut loaded = Table::default();
        loaded
            .rebuild_from_snapshot(array_len, array_slots, entries)
            .unwrap();
        // Writing a hole must land in the array on both sides.
        t.insert(n(3), Val::Num(3.0)).unwrap();
        loaded.insert(n(3), Val::Num(3.0)).unwrap();
        assert_eq!(keys(&t), keys(&loaded));
        assert_eq!(keys(&loaded), vec![n(1), n(3), n(4), Val::Bool(true)]);
        assert_eq!(t.array_len(), loaded.array_len());

        let mut bad = Table::default();
        assert!(
            bad.rebuild_from_snapshot(1, 1, vec![(n(1), Val::Nil), (n(2), Val::Bool(true))])
                .is_err()
        );
        assert!(
            bad.rebuild_from_snapshot(1, 1, vec![(n(2), Val::Nil)])
                .is_err()
        );
        assert!(
            bad.rebuild_from_snapshot(3, 0, vec![(n(2), Val::Bool(true))])
                .is_err()
        );
        assert!(
            bad.rebuild_from_snapshot(MAX_ARRAY_LEN + 1, 0, vec![])
                .is_err()
        );
    }

    /// Two tables with the same logical contents save the same entries
    /// whatever their window history, and the loaded copy keeps `#` and the
    /// placement of later writes.
    #[cfg(feature = "snapshot")]
    #[test]
    fn snapshot_form_is_independent_of_the_window() {
        // A queue that has moved far: its window was dropped and re-anchored.
        let mut queue = Table::default();
        for i in 1..=1000 {
            queue.insert(n(i), Val::Bool(true)).unwrap();
            if i > 3 {
                queue.insert(n(i - 3), Val::Nil).unwrap();
            }
        }
        // The same logical state built without ever moving a window.
        let mut grown = Table::default();
        fill(&mut grown, 1..=1000);
        for i in 1..=997 {
            grown.insert(n(i), Val::Nil).unwrap();
        }
        assert!(queue.array.len() < 64, "the queue window must stay small");
        let saved = queue.snapshot_entries();
        assert_eq!(saved, grown.snapshot_entries());
        assert_eq!(saved.0, 1000);
        assert_eq!(saved.1, 3);
        let mut loaded = Table::default();
        loaded
            .rebuild_from_snapshot(saved.0, saved.1, saved.2.clone())
            .unwrap();
        for t in [&mut queue, &mut grown, &mut loaded] {
            assert_eq!(t.array_len(), 1000);
            t.insert(n(500), Val::Num(5.0)).unwrap();
            t.insert(n(1001), Val::Num(6.0)).unwrap();
        }
        assert_eq!(keys(&queue), keys(&loaded));
        assert_eq!(keys(&grown), keys(&loaded));
        assert_eq!(queue.snapshot_entries(), loaded.snapshot_entries());
        assert_eq!(grown.snapshot_entries(), loaded.snapshot_entries());
    }

    #[test]
    fn duplicate_template_keys_do_not_create_phantom_tombstones() {
        let literals = [h(7)];
        let mut t = Table::with_template_keys(&[0, 0], &literals);
        assert_eq!(t.dead_count, 1);
        t.insert(h(7), Val::Bool(true)).unwrap();
        assert_eq!(t.dead_count, 0);
        t.insert(h(8), Val::Bool(true)).unwrap();
        assert_eq!(t.get(&h(8)), Val::Bool(true));
    }

    #[test]
    fn repeated_template_init_keeps_dead_count_exact() {
        // Forged bytecode can initialize one pinned slot live -> nil -> live.
        let literals = [h(7)];
        let mut inline = Table::default();
        inline.insert(h(7), Val::Bool(true)).unwrap();
        inline.remove(&h(7));
        for mut t in [Table::with_template_keys(&[0], &literals), inline] {
            assert_eq!(t.dead_count, 1);
            assert!(t.init_at_index(0, h(7), Val::Nil));
            assert_eq!(t.dead_count, 1);
            assert!(t.init_at_index(0, h(7), Val::Bool(true)));
            assert_eq!(t.dead_count, 0);
            assert!(t.init_at_index(0, h(7), Val::Nil));
            assert_eq!(t.dead_count, 1);
            assert!(t.init_at_index(0, h(7), Val::Bool(true)));
            assert_eq!(t.dead_count, 0);
            assert!(t.init_at_index(0, h(7), Val::Bool(false)));
            assert_eq!(t.dead_count, 0);
            assert_eq!(t.get(&h(7)), Val::Bool(false));
            assert!(!t.init_at_index(0, h(8), Val::Nil));
            assert_eq!(t.dead_count, 0);
        }
    }

    fn map_slots(t: &Table) -> usize {
        match &t.storage {
            TableStorage::Map(map) => map.len(),
            TableStorage::Inline { .. } => panic!("expected map storage"),
        }
    }

    #[test]
    fn readding_tombstoned_keys_vacates_in_place_and_stays_bounded() {
        // Rotating delete/re-add over every key: each re-add used to
        // shift_remove the dead slot, O(table size) per write. Now the old
        // slot is renamed Vacant in place and the key appends, so the order is
        // still "moved to the back", indices before the vacated slot never
        // move, and compaction keeps the part within twice its live size.
        let count = 64;
        let mut t = Table::default();
        fill_hash(&mut t, 1..=count);
        let mut expected: Vec<Val> = (1..=count).map(h).collect();
        for round in 0..3 {
            for i in 1..=count {
                let (before, _) = t.get_with_index(&h(1)).unwrap();
                t.insert(h(i), Val::Nil).unwrap();
                t.insert(h(i), Val::Num(round as f64)).unwrap();
                expected.retain(|key| *key != h(i));
                expected.push(h(i));
                assert!(map_slots(&t) <= 2 * count + 1);
                assert_eq!(t.hash_live_len(), count);
                if i != 1 {
                    // Key 1 keeps its appended index until compaction, even
                    // when an earlier slot is vacated.
                    let (after, _) = t.get_with_index(&h(1)).unwrap();
                    assert!(after == before || t.dead_count == 0);
                }
            }
            assert_eq!(keys(&t), expected);
        }
        for (pos, key) in expected.iter().enumerate() {
            assert_eq!(t.get(key), Val::Num(2.0), "key at pairs position {pos}");
        }
    }

    #[test]
    fn readded_key_leaves_a_dead_old_index_and_a_valid_new_one() {
        let mut t = Table::default();
        fill_hash(&mut t, 1..=8);
        let (old, _) = t.get_with_index(&h(3)).unwrap();
        t.insert(h(3), Val::Nil).unwrap();
        t.insert(h(5), Val::Nil).unwrap();
        let version = t.version();
        // Two vacated slots at once must not collide.
        t.insert(h(3), Val::Num(3.0)).unwrap();
        t.insert(h(5), Val::Num(5.0)).unwrap();
        assert_eq!(t.version(), version + 2);
        assert_eq!(t.get_index(old), None);
        assert!(!t.set_at_index(old, Val::Bool(false)));
        assert!(matches!(
            t.next_from_matching_index(old, &h(3)),
            TableNextWithIndex::InvalidKey
        ));
        let (new, value) = t.get_with_index(&h(3)).unwrap();
        assert_eq!(value, Val::Num(3.0));
        assert_eq!(t.get_index(new), Some((h(3), Val::Num(3.0))));
        // The cursor walk crosses both vacant slots.
        assert_eq!(
            keys(&t),
            [1, 2, 4, 6, 7, 8, 3, 5]
                .into_iter()
                .map(h)
                .collect::<Vec<_>>()
        );
        // Removing again tombstones the new slot; a third add reuses nothing
        // stale.
        t.insert(h(3), Val::Nil).unwrap();
        assert_eq!(t.get(&h(3)), Val::Nil);
        t.insert(h(3), Val::Bool(true)).unwrap();
        assert_eq!(keys(&t).last(), Some(&h(3)));
    }

    #[test]
    fn dense_tombstones_compact_once_before_append() {
        let mut t = Table::default();
        fill_hash(&mut t, 1..=4);
        t.remove(&h(2));
        t.remove(&h(3));
        assert!(matches!(t.next(&h(1)), TableNext::Pair(Val::Num(4.5), _)));
        t.insert(h(5), Val::Bool(true)).unwrap();
        assert_eq!(t.version(), 1);
        assert!(matches!(t.next(&h(4)), TableNext::Pair(Val::Num(5.5), _)));
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "dead_count must equal the number of nil-valued hash slots")]
    fn invariant_check_detects_dead_count_mismatch() {
        let table = Table {
            dead_count: 1,
            ..Table::default()
        };
        table.debug_assert_invariants();
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "live hash key overlaps the array part or its append slot")]
    fn invariant_check_detects_live_append_key_in_hash_part() {
        let mut table = Table::default();
        table
            .insert(n(2), Val::Bool(true))
            .expect("numeric table key is valid");
        table.array.push(Val::Nil);
        table.vlen += 1;
        table.debug_assert_invariants();
    }
}

#[cfg(test)]
mod key_rule_tests {
    use super::{array_index, integer_index, is_append_key};

    #[test]
    fn numeric_key_boundaries() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 0.5, 4.0] {
            assert_eq!(array_index(value, 3), None);
        }
        assert_eq!(integer_index(-0.0, 3), Some(0));
        assert_eq!(array_index(-0.0, 3), None);
        assert_eq!(array_index(1.0, 3), Some(0));
        assert_eq!(array_index(3.0, 3), Some(2));
        assert_eq!(array_index(1.0, 0), None);
        assert!(is_append_key(1.0, 0));
        assert!(is_append_key(4.0, 3));
        assert!(!is_append_key(3.5, 3));
    }
}
