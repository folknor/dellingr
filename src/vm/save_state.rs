//! Snapshot / restore of a quiescent VM (the `snapshot` feature).
//!
//! [`State::save_state`] serializes the persistent script world - user globals,
//! the reachable table/closure/upvalue/string graph, the RNG stream, and cost
//! counters - to a versioned deterministic binary blob; [`State::load_state`]
//! rebuilds an equivalent [`State`] in the same binary. It is a *data snapshot,
//! not a continuation*: a save is only valid while the VM is quiescent (no Lua
//! or Rust call in flight), so there is no call stack, program counter, open
//! upvalue, or vararg base to capture - the host re-drives its scripts after a
//! load. Anchors, callbacks, and host user-data are host-owned and not saved.
//!
//! Load-bearing decisions:
//!
//! - **Pointers are renumbered.** `ObjectPtr`/`StringPtr` are process-local
//!   slotmap keys, so the walker assigns dense per-arena indices and rewrites
//!   every reference. Bytecode is pooled by `Arc` identity, upvalues live in a
//!   shared arena so sibling closures keep sharing one cell, and load is a
//!   two-pass allocate-then-fill so cyclic graphs resolve. Those allocators
//!   never run GC, which is what keeps the half-built graph alive until the
//!   final collect.
//! - **RustFns need stable ids.** A raw `fn` address is meaningless across
//!   runs, so a reachable `Val::RustFn` is saved as a registered id (see
//!   [`State::register_rust_fn`]) and the save fails fast if one is
//!   unregistered. The stdlib registers itself under dotted paths
//!   (`math.sin`, ...) during `open_libs`.
//! - **Environment objects are rebuilt, not copied.** `math`/`string`/`table`/
//!   `_G`, `_G`'s metatable and the shared string metatable are reconstructed by
//!   `open_libs` on load and referenced by token. The object->token map is
//!   refreshed at each successful `open_libs` (`State.env_tokens`), not read
//!   from the live `builtins`
//!   slots, so shadowing a builtin global (`math = {}`) can't misclassify the
//!   user's table as an environment object. Adding `math.foo` in a later build
//!   therefore doesn't corrupt old saves.
//! - **Globals are persisted whole.** An unshadowed builtin encodes to a cheap
//!   `EnvObj`/`Fn` token; a *shadow* (`print = ...`) is preserved as the user's
//!   value. Skipping builtin names would silently drop shadows.
//! - **In-crate codec.** Little-endian, length-prefixed, `f64` by bit pattern
//!   (NaNs canonicalized, -0.0 exact), explicit enum tags, bounds-checked with
//!   initial vector reservations capped in bytes at remaining input. No
//!   `serde`/`bincode`. Two saves of the same logical state are byte-identical
//!   with the same crate version, because the diagnostic crate-version string
//!   is part of the blob. A host that content-hashes saves across upgrades
//!   must hash the payload after the header, or accept that a version bump
//!   changes the bytes.

use std::sync::Arc;

use std::collections::{BTreeMap, BTreeSet};

use crate::det_hash::{FixedState, IndexMap as DetMap, IndexSet as DetSet, map_with_capacity};

use super::lua_val::RustFunc;
use super::object::{GcHeap, RawObject, StringPtr, Upvalue, UpvalueRef};
use super::rng::VmRng;
use super::table::{
    InvalidTableKey, NormalizedTableKey, TableError, TableKey, TableKeyInput, array_index,
    normalize_table_key,
};
use super::{ObjectPtr, PointerIdKey, State, Val};
use crate::COST_MODEL_VERSION;
use crate::compiler::{Bytecode, UpvalueDesc};
use crate::cost_meter::CostState;
use crate::host::HostCallbacks;
use crate::instr::Instr;
use crate::instr::snapshot_format::FORMAT_VERSION;

mod error;
#[cfg(test)]
mod integrity_tests;
mod verify;

use error::SaveFault;
pub use error::{
    CorruptionReason, DecodeReason, DuplicateEntry, EnvironmentCorruption, GmatchCorruption,
    InvalidBytecodeReason, KeyForm, LoadError, MaterializedPart, SaveCollection, SaveError,
    SaveInternalError, SavedReference, SnapshotError, SnapshotField, TableCorruption,
    TableLoadLocation, TableRestoreFailure, TableRestoreStep,
};

// Token spellings belong to the snapshot schema, not the global-name API.
// Keep the order stable: aliases are classified by the last captured token.
pub(super) const BUILTIN_ENV_TOKENS: &[(crate::instr::Builtin, &str)] = &[
    (crate::instr::Builtin::Math, "math"),
    (crate::instr::Builtin::String, "string"),
    (crate::instr::Builtin::Table, "table"),
    (crate::instr::Builtin::G, "_G"),
];
pub(super) const GLOBAL_METATABLE_TOKEN: &str = "_G.metatable";
pub(super) const STRING_METATABLE_TOKEN: &str = "string.metatable";

const MAGIC: [u8; 4] = *b"DLGS";
use crate::limits::tuning::{
    SAVE_PATH_GLOBAL_BYTES as MAX_PATH_GLOBAL_BYTES,
    SAVE_RENDERED_PATH_BYTES as MAX_RENDERED_PATH_BYTES,
    SAVE_RENDERED_PATH_SEGMENTS as MAX_RENDERED_PATH_SEGMENTS,
};

/// Bytes produced by [`State::save_state`] plus non-fatal save diagnostics.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SaveState {
    /// Versioned deterministic binary save payload.
    pub bytes: Vec<u8>,
    /// Non-fatal information the host may want to surface.
    pub diagnostics: SaveDiagnostics,
}

/// Non-fatal information returned with a save.
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct SaveDiagnostics {
    /// Number of live process-local anchors at save time.
    pub anchor_count: usize,
}

/// A collection longer than the format's u32 length prefix.
fn too_large(collection: SaveCollection, count: usize) -> SaveError {
    SaveError::TooLarge {
        collection,
        count,
        limit: u32::MAX as usize,
    }
}

// Corruption reports share the location vocabulary of table restoration.
type SaveLocation = TableLoadLocation;

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    let mut end = 0usize;
    for character in text.chars() {
        let next = end + character.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    text[..end].to_owned()
}

// `Ord` is derived so verification can detect duplicate keys with an ordered
// set instead of a linear scan. Every payload is attacker-controlled and its
// length is bounded only by the file size, so a quadratic check would make a
// forged save expensive to reject. `Num` already holds raw bits, so there is no
// float ordering hazard.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SavedVal {
    Nil,
    Bool(bool),
    Num(u64),
    Str(u32),
    Obj(u32),
    Fn(String),
    EnvObj(String),
}

#[derive(Clone, Debug, PartialEq)]
enum SavedObject {
    Table {
        /// The logical array length: integer keys `1..=array_len` are array
        /// keys, never hash entries, and `array_len + 1` is the append key.
        array_len: u64,
        /// The first `array_slots` entries are array slots with consecutive
        /// keys, from the first live array key to the last, with `Nil` values
        /// for the empty slots between; every other array key is nil. Zero
        /// slots means an all-nil array. The rest are live hash entries in
        /// insertion order.
        array_slots: u32,
        entries: Vec<(SavedVal, SavedVal)>,
        /// A metatable can be a user object (`Obj`) or an environment object
        /// (`EnvObj`, e.g. `setmetatable(t, math)`), so it goes through the
        /// full value encoder rather than a bare object index.
        metatable: Option<SavedVal>,
    },
    Closure {
        chunk: u32,
        upvalues: Vec<u32>,
    },
}

// This is a wire arena node, not an executable chunk: children are untrusted
// ids (including possible cycles), instructions are unchecked words, and
// numbers retain their encoded bits. Keep it separate until graph verification
// completes so decoding never constructs a recursive executable object graph.
#[derive(Clone, Debug, PartialEq)]
struct SavedBytecode {
    code: Vec<u64>,
    number_literals: Vec<u64>,
    string_literals: Vec<Vec<u8>>,
    table_templates: Vec<Vec<u16>>,
    global_cache_slots: u8,
    field_cache_slots: u8,
    set_field_cache_slots: u8,
    tfor_cursor_slots: u8,
    num_params: u8,
    num_locals: u8,
    max_stack: u16,
    nested: Vec<u32>,
    upvalues: Vec<SavedUpvalueDesc>,
    is_vararg: bool,
    name: Option<String>,
    defined_line: u32,
    source: Option<Arc<[u8]>>,
    line_info: Vec<u32>,
}

type SavedUpvalueDesc = UpvalueDesc;

#[derive(Clone, Debug, PartialEq)]
struct SavePayload {
    has_standard_environment: bool,
    rng_state: u64,
    cost: CostState,
    strings: Vec<Vec<u8>>,
    bytecode: Vec<SavedBytecode>,
    upvalues: Vec<SavedVal>,
    objects: Vec<SavedObject>,
    user_globals: Vec<(String, SavedVal)>,
    /// Whether the saving State had sealed its globals; the loaded one takes
    /// the same phase.
    globals_sealed: bool,
    env_deltas: Vec<SavedEnvDelta>,
    next_format_pointer_id: u64,
    format_pointer_ids: Vec<(SavedVal, u64)>,
}

impl SavePayload {
    /// Rough encoded size: string content, ~16 bytes per value slot, and each
    /// bytecode arena node's own fields (children are separate nodes, so
    /// nothing is counted twice). Used only to seed the encoder's buffer
    /// capacity; every step saturates.
    fn encoded_size_estimate(&self) -> usize {
        let string_bytes = self
            .strings
            .iter()
            .fold(0usize, |sum, s| sum.saturating_add(s.len()));
        let value_count = self
            .objects
            .iter()
            .fold(0usize, |sum, obj| {
                sum.saturating_add(match obj {
                    SavedObject::Table { entries, .. } => {
                        entries.len().saturating_mul(2).saturating_add(1)
                    }
                    SavedObject::Closure { upvalues, .. } => upvalues.len().saturating_add(1),
                })
            })
            .saturating_add(self.upvalues.len())
            .saturating_add(self.user_globals.len().saturating_mul(2));
        let bytecode_bytes = self.bytecode.iter().fold(0usize, |sum, chunk| {
            let literal_bytes = chunk.string_literals.iter().fold(0usize, |sum, literal| {
                sum.saturating_add(literal.len()).saturating_add(4)
            });
            let template_bytes = chunk.table_templates.iter().fold(0usize, |sum, keys| {
                sum.saturating_add(keys.len().saturating_mul(2))
                    .saturating_add(4)
            });
            let word_count = chunk
                .code
                .len()
                .saturating_add(chunk.line_info.len())
                .saturating_add(chunk.nested.len())
                .saturating_add(chunk.upvalues.len());
            sum.saturating_add(word_count.saturating_mul(4))
                .saturating_add(chunk.number_literals.len().saturating_mul(8))
                .saturating_add(literal_bytes)
                .saturating_add(template_bytes)
                .saturating_add(chunk.name.as_ref().map_or(0, String::len))
                .saturating_add(chunk.source.as_ref().map_or(0, |s| s.len()))
                .saturating_add(64)
        });
        string_bytes
            .saturating_add(value_count.saturating_mul(16))
            .saturating_add(bytecode_bytes)
            .saturating_add(1024)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct SavedEnvDelta {
    token: String,
    deleted: Vec<SavedVal>,
    upserts: Vec<(SavedVal, SavedVal)>,
    order: Option<Vec<SavedVal>>,
    /// The table's whole array part, `Nil` for empty slots, when it has one.
    array: Option<Vec<SavedVal>>,
    metatable: SavedMetatableDelta,
}

#[derive(Clone, Debug, PartialEq)]
enum SavedMetatableDelta {
    Unchanged,
    Cleared,
    Set(SavedVal),
}

struct SaveBuilder<'a> {
    state: &'a State,
    env_reverse: BTreeMap<ObjectPtr, String>,
    strings: Vec<Vec<u8>>,
    /// Strings are interned per-State, so `StringPtr` equality is content
    /// equality: keying the dedupe on the pointer avoids a content copy and
    /// byte-wise comparisons per probe. Ids are still assigned in
    /// first-encounter order, so the payload is byte-identical to the
    /// content-keyed map this replaced.
    string_ids: BTreeMap<StringPtr, u32>,
    bytecode: Vec<SavedBytecode>,
    bytecode_ids: BTreeMap<usize, u32>,
    upvalues: Vec<Option<SavedVal>>,
    upvalue_ids: BTreeMap<usize, u32>,
    objects: Vec<Option<PendingObject>>,
    object_ids: BTreeMap<ObjectPtr, u32>,
    roots: Vec<Option<SavedVal>>,
    reachable_pointer_values: BTreeSet<SavedVal>,
    expanded_environment_objects: BTreeSet<ObjectPtr>,
}

#[derive(Clone, Copy)]
enum ValueDestination {
    Root(usize),
    TableKey { object: u32, entry: usize },
    TableValue { object: u32, entry: usize },
    Metatable { object: u32 },
    Upvalue(u32),
    Discard,
}
enum EncodeTask {
    Value {
        val: Val,
        destination: ValueDestination,
    },
    Object {
        ptr: ObjectPtr,
        id: u32,
    },
    Upvalue {
        upvalue: UpvalueRef,
        object: u32,
        index: usize,
    },
}
type PathId = usize;
enum PathSegment {
    Global(String),
    TableKey(usize),
    TableValue(usize),
    Metatable,
    Upvalue(usize),
}

impl PathSegment {
    fn global(name: &str) -> Self {
        Self::Global(truncate_utf8(name, MAX_PATH_GLOBAL_BYTES))
    }
}

struct Breadcrumb {
    parent: Option<PathId>,
    segment: PathSegment,
}

enum PathTask {
    Value { val: Val, path: PathId },
    Object { ptr: ObjectPtr, path: PathId },
    Upvalue { upvalue: UpvalueRef, path: PathId },
}

enum PathObjectChildren {
    Table {
        entries: Vec<(Val, Val)>,
        metatable: Option<ObjectPtr>,
    },
    Closure(std::sync::Arc<[UpvalueRef]>),
}

// Save tasks carry no path data. Rewalk a root only after finding an
// unregistered function, preserving the encoder's LIFO order for diagnostics.
// Earlier roots have encoded successfully, so a shared object cannot hide the
// first unregistered function from this root's fresh visited sets.
struct PathFinder<'a> {
    state: &'a State,
    env_reverse: &'a BTreeMap<ObjectPtr, String>,
    breadcrumbs: Vec<Breadcrumb>,
    objects: BTreeSet<ObjectPtr>,
    environment_objects: BTreeSet<ObjectPtr>,
    upvalues: BTreeSet<usize>,
}

impl<'a> PathFinder<'a> {
    fn new(state: &'a State, env_reverse: &'a BTreeMap<ObjectPtr, String>) -> Self {
        Self {
            state,
            env_reverse,
            breadcrumbs: Vec::new(),
            objects: BTreeSet::new(),
            environment_objects: BTreeSet::new(),
            upvalues: BTreeSet::new(),
        }
    }

    fn find(mut self, root: Val, global: &str, target_function: RustFunc) -> Option<String> {
        let root_path = push_path(&mut self.breadcrumbs, None, PathSegment::global(global));
        let mut tasks = vec![PathTask::Value {
            val: root,
            path: root_path,
        }];
        let target_address = target_function as usize;

        while let Some(task) = tasks.pop() {
            match task {
                PathTask::Value { val, path } => match val {
                    Val::RustFn(function) if function as usize == target_address => {
                        return Some(render_path(&self.breadcrumbs, path));
                    }
                    Val::Obj(ptr) if self.env_reverse.contains_key(&ptr) => {
                        if self.environment_objects.insert(ptr) {
                            let table = self.state.heap.as_table_ref(ptr)?;
                            let metatable = table.get_metatable();
                            let entries = table.entries();
                            if let Some(metatable) = metatable {
                                let child_path = push_path(
                                    &mut self.breadcrumbs,
                                    Some(path),
                                    PathSegment::Metatable,
                                );
                                tasks.push(PathTask::Value {
                                    val: Val::Obj(metatable),
                                    path: child_path,
                                });
                            }
                            for (index, (key, value)) in entries.into_iter().enumerate().rev() {
                                let value_path = push_path(
                                    &mut self.breadcrumbs,
                                    Some(path),
                                    PathSegment::TableValue(index),
                                );
                                tasks.push(PathTask::Value {
                                    val: value,
                                    path: value_path,
                                });
                                let key_path = push_path(
                                    &mut self.breadcrumbs,
                                    Some(path),
                                    PathSegment::TableKey(index),
                                );
                                tasks.push(PathTask::Value {
                                    val: key,
                                    path: key_path,
                                });
                            }
                        }
                    }
                    Val::Obj(ptr) if self.objects.insert(ptr) => {
                        tasks.push(PathTask::Object { ptr, path });
                    }
                    _ => {}
                },
                PathTask::Object { ptr, path } => {
                    let children = match &self.state.heap.get(ptr).raw {
                        RawObject::Table(table) => {
                            let (_, _, entries) = table.snapshot_entries();
                            PathObjectChildren::Table {
                                entries,
                                metatable: table.get_metatable(),
                            }
                        }
                        RawObject::LuaFn(closure) => {
                            PathObjectChildren::Closure(std::sync::Arc::clone(&closure.upvalues))
                        }
                    };
                    match children {
                        PathObjectChildren::Table { entries, metatable } => {
                            if let Some(metatable) = metatable {
                                let child_path = push_path(
                                    &mut self.breadcrumbs,
                                    Some(path),
                                    PathSegment::Metatable,
                                );
                                tasks.push(PathTask::Value {
                                    val: Val::Obj(metatable),
                                    path: child_path,
                                });
                            }
                            for (index, (key, value)) in entries.into_iter().enumerate().rev() {
                                let value_path = push_path(
                                    &mut self.breadcrumbs,
                                    Some(path),
                                    PathSegment::TableValue(index),
                                );
                                tasks.push(PathTask::Value {
                                    val: value,
                                    path: value_path,
                                });
                                let key_path = push_path(
                                    &mut self.breadcrumbs,
                                    Some(path),
                                    PathSegment::TableKey(index),
                                );
                                tasks.push(PathTask::Value {
                                    val: key,
                                    path: key_path,
                                });
                            }
                        }
                        PathObjectChildren::Closure(upvalues) => {
                            for (index, upvalue) in upvalues.iter().enumerate().rev() {
                                let child_path = push_path(
                                    &mut self.breadcrumbs,
                                    Some(path),
                                    PathSegment::Upvalue(index),
                                );
                                tasks.push(PathTask::Upvalue {
                                    upvalue: *upvalue,
                                    path: child_path,
                                });
                            }
                        }
                    }
                }
                PathTask::Upvalue { upvalue, path } => {
                    if self.upvalues.insert(upvalue.index())
                        && let Upvalue::Closed(value) = self.state.upvalue_pool.get(upvalue)
                    {
                        tasks.push(PathTask::Value { val: *value, path });
                    }
                }
            }
        }
        None
    }
}

fn push_path(
    breadcrumbs: &mut Vec<Breadcrumb>,
    parent: Option<PathId>,
    segment: PathSegment,
) -> PathId {
    let id = breadcrumbs.len();
    breadcrumbs.push(Breadcrumb { parent, segment });
    id
}

fn render_path(breadcrumbs: &[Breadcrumb], path: PathId) -> String {
    let mut ids = Vec::with_capacity(MAX_RENDERED_PATH_SEGMENTS);
    let mut current = Some(path);
    let mut global_name = None;
    let mut skipped = false;
    while let Some(id) = current {
        let breadcrumb = &breadcrumbs[id];
        if let PathSegment::Global(name) = &breadcrumb.segment {
            global_name = Some(name.as_str());
        }
        if ids.len() < MAX_RENDERED_PATH_SEGMENTS {
            ids.push(id);
        } else {
            skipped = true;
        }
        current = breadcrumb.parent;
    }
    let mut text = String::new();
    if let Some(name) = global_name {
        text.push_str("global ");
        text.push_str(name);
    }
    if skipped {
        text.push_str("...");
    }
    for id in ids.into_iter().rev() {
        match &breadcrumbs[id].segment {
            PathSegment::Global(_) => {}
            PathSegment::TableKey(index) => text.push_str(&format!(".key[{index}]")),
            PathSegment::TableValue(index) => text.push_str(&format!("[{index}]")),
            PathSegment::Metatable => text.push_str(".metatable"),
            PathSegment::Upvalue(index) => text.push_str(&format!(".upvalue[{index}]")),
        }
    }
    if text.len() > MAX_RENDERED_PATH_BYTES {
        text = truncate_utf8(&text, MAX_RENDERED_PATH_BYTES.saturating_sub(3));
        text.push_str("...");
    }
    text
}

enum PendingObject {
    Table {
        array_len: u64,
        array_slots: u32,
        entries: Vec<(Option<SavedVal>, Option<SavedVal>)>,
        metatable: Option<Option<SavedVal>>,
    },
    Closure {
        chunk: u32,
        upvalues: Vec<Option<u32>>,
    },
}

impl<'a> SaveBuilder<'a> {
    fn new(state: &'a State) -> Self {
        Self {
            state,
            env_reverse: build_env_reverse(state),
            strings: Vec::new(),
            string_ids: BTreeMap::new(),
            bytecode: Vec::new(),
            bytecode_ids: BTreeMap::new(),
            upvalues: Vec::new(),
            upvalue_ids: BTreeMap::new(),
            objects: Vec::new(),
            object_ids: BTreeMap::new(),
            roots: Vec::new(),
            reachable_pointer_values: BTreeSet::new(),
            expanded_environment_objects: BTreeSet::new(),
        }
    }

    fn finish(mut self) -> Result<SavePayload, SaveError> {
        // Persist every global, builtins included. An unshadowed builtin
        // encodes to a cheap EnvObj/Fn token (resolved against the rebuilt
        // environment on load), so we pay almost nothing for it. Skipping
        // builtin *names* would silently drop user shadows like
        // `print = function ... end` or `math = {}`, which live in `globals`
        // under a builtin name but no longer hold the canonical value.
        let mut user_globals = Vec::new();
        // Tombstones are saved too, in slot order: they suppress names load
        // setup reinstalls, and they let the loaded environment compact
        // exactly when the saving one would have.
        for (name, val) in self.state.global_env.iter() {
            let saved = self.encode_root(val, name)?;
            user_globals.push((name.clone(), saved));
        }
        let env_deltas = self.encode_env_deltas()?;
        // Some strings stay observable without the saved graph reaching them
        // as values: metamethod event names are State roots for its whole
        // lifetime, and a string literal of a saved chunk is re-interned on
        // load to the same string a script reads through that chunk. Carry a
        // `%p` identity on either through the arena; strings without an
        // identity add nothing to the payload.
        let mut identity_strings: Vec<Val> = self.state.meta_names.iter().collect();
        // Address keys are lookup-only: arena ids carry graph traversal order.
        // Sorting by address would make identity-only strings allocator-dependent.
        let mut chunks: Vec<_> = self.bytecode_ids.iter().collect();
        chunks.sort_unstable_by_key(|(_, id)| **id);
        for (key, _) in chunks {
            if let Some(entry) = self.state.bytecode_caches.get(key) {
                identity_strings.extend(entry.runtime.literals.iter().copied());
            }
        }
        for value in identity_strings {
            if let Val::Str(ptr) = value
                && let Some(key) = PointerIdKey::from_val(value)
                && self.state.format_pointer_ids.contains_key(&key)
            {
                let id = self.string_id(ptr)?;
                self.reachable_pointer_values.insert(SavedVal::Str(id));
            }
        }
        let pointer_ids = self.state.format_pointer_ids.clone();
        let mut format_pointer_ids = Vec::new();
        for (key, id) in pointer_ids {
            let val = key.as_val();
            // A heap value unreachable from saved state cannot survive the
            // load, so its id is dropped. A registered Rust function is
            // different: it is not a heap object, the host can hold it and
            // pass it back in after load, and it resolves by its stable
            // registration id. Keep its rendered identity whether or not the
            // saved graph reaches it (load skips one setup no longer
            // registers, see `materialize_payload`).
            if let Some(saved) = self.saved_reachable_pointer_value(val)
                && (matches!(saved, SavedVal::Fn(_))
                    || self.reachable_pointer_values.contains(&saved))
            {
                format_pointer_ids.push((saved, id));
            }
        }

        let objects = self
            .objects
            .into_iter()
            .map(|obj| {
                Self::finish_object(obj.ok_or(SaveError::from(SaveFault::UnfilledObjectSlot))?)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let upvalues = self
            .upvalues
            .into_iter()
            .map(|value| value.ok_or(SaveError::from(SaveFault::UnfilledUpvalueSlot)))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(SavePayload {
            has_standard_environment: !self.state.env_tokens.is_empty(),
            rng_state: self.state.rng.state(),
            cost: self.state.cost,
            strings: self.strings,
            bytecode: self.bytecode,
            upvalues,
            objects,
            user_globals,
            globals_sealed: self.state.global_env.is_sealed(),
            env_deltas,
            next_format_pointer_id: self.state.next_format_pointer_id,
            format_pointer_ids,
        })
    }

    fn saved_reachable_pointer_value(&self, value: Val) -> Option<SavedVal> {
        match value {
            Val::Str(ptr) => self.string_ids.get(&ptr).copied().map(SavedVal::Str),
            Val::RustFn(func) => self
                .state
                .rust_fn_ids_by_addr
                .get(&(func as usize))
                .cloned()
                .map(SavedVal::Fn),
            Val::Obj(ptr) => self
                .env_reverse
                .get(&ptr)
                .cloned()
                .map(SavedVal::EnvObj)
                .or_else(|| self.object_ids.get(&ptr).copied().map(SavedVal::Obj)),
            Val::Nil | Val::Bool(_) | Val::Num(_) => None,
        }
    }

    fn encode_env_deltas(&mut self) -> Result<Vec<SavedEnvDelta>, SaveError> {
        let mut canonical: BTreeMap<String, ObjectPtr> = BTreeMap::new();
        for (ptr, token) in &self.state.env_tokens {
            canonical.insert(token.clone(), *ptr);
        }
        let mut deltas = Vec::new();
        for (token, ptr) in canonical {
            let table = self
                .state
                .heap
                .as_table_ref(ptr)
                .ok_or(SaveError::from(SaveFault::EnvironmentObjectNotTable))?;
            let baseline = self
                .state
                .env_baselines
                .get(&ptr)
                .ok_or(SaveError::from(SaveFault::MissingEnvironmentBaseline))?;
            let live = table.entries();
            // One membership map per side, built once per table. `TableKey`
            // hashes and compares with runtime key equality (-0.0 equals
            // +0.0), and both sides come from table entries, so every key is
            // already a valid table key. `or_insert` keeps the first entry for
            // a repeated key. Values are still compared with `Val::eq`
            // directly so a NaN value stays unequal to itself.
            let mut baseline_values: DetMap<TableKey, Val> =
                map_with_capacity(baseline.entries.len());
            for (key, value) in &baseline.entries {
                baseline_values
                    .entry(TableKey::from_prevalidated(*key))
                    .or_insert(*value);
            }
            let mut live_keys: DetSet<TableKey> =
                DetSet::with_capacity_and_hasher(live.len(), FixedState::default());
            for (key, _) in &live {
                live_keys.insert(TableKey::from_prevalidated(*key));
            }
            let mut deleted = Vec::new();
            for (key, _) in &baseline.entries {
                if !live_keys.contains(&TableKey::from_prevalidated(*key)) {
                    deleted.push(self.encode_root(*key, &token)?);
                }
            }
            let mut upserts = Vec::new();
            for (key, value) in &live {
                match baseline_values.get(&TableKey::from_prevalidated(*key)) {
                    Some(base_value) if base_value == value => {}
                    _ => upserts.push((
                        self.encode_root(*key, &token)?,
                        self.encode_root(*value, &token)?,
                    )),
                }
            }
            let replayed_order: Vec<Val> = baseline
                .entries
                .iter()
                .filter(|(key, _)| live_keys.contains(&TableKey::from_prevalidated(*key)))
                .map(|(key, _)| *key)
                .chain(live.iter().filter_map(|(key, _)| {
                    (!baseline_values.contains_key(&TableKey::from_prevalidated(*key)))
                        .then_some(*key)
                }))
                .collect();
            let order = (replayed_order != live.iter().map(|(key, _)| *key).collect::<Vec<_>>())
                .then(|| {
                    live.iter()
                        .map(|(key, _)| self.encode_root(*key, &token))
                        .collect::<Result<Vec<_>, SaveError>>()
                })
                .transpose()?;
            let metatable = match (baseline.metatable, table.get_metatable().map(Val::Obj)) {
                (before, after) if before == after => SavedMetatableDelta::Unchanged,
                (_, None) => SavedMetatableDelta::Cleared,
                (_, Some(value)) => SavedMetatableDelta::Set(self.encode_root(value, &token)?),
            };
            // The array part's shape (its nil slots included) decides where
            // later integer writes land, so it must round-trip; replaying the
            // upserts alone would rebuild only its live prefix.
            let virtual_array = table.virtual_array();
            let array = if virtual_array.is_empty() {
                None
            } else {
                Some(
                    virtual_array
                        .into_iter()
                        .map(|value| self.encode_root(value, &token))
                        .collect::<Result<Vec<_>, SaveError>>()?,
                )
            };
            if !deleted.is_empty()
                || !upserts.is_empty()
                || order.is_some()
                || array.is_some()
                || !matches!(metatable, SavedMetatableDelta::Unchanged)
            {
                deltas.push(SavedEnvDelta {
                    token,
                    deleted,
                    upserts,
                    order,
                    array,
                    metatable,
                });
            }
        }
        Ok(deltas)
    }

    /// The string-arena id of `ptr`, adding its single content copy on first use.
    fn string_id(&mut self, ptr: StringPtr) -> Result<u32, SaveError> {
        if let Some(id) = self.string_ids.get(&ptr) {
            return Ok(*id);
        }
        let id = u32::try_from(self.strings.len())
            .map_err(|_| too_large(SaveCollection::Strings, self.strings.len()))?;
        self.strings.push(self.state.heap.get_string(ptr).to_vec());
        self.string_ids.insert(ptr, id);
        Ok(id)
    }

    fn encode_root(&mut self, val: Val, global: &str) -> Result<SavedVal, SaveError> {
        let root = self.roots.len();
        self.roots.push(None);
        self.run_tasks(val, global, root)?;
        self.roots[root]
            .take()
            .ok_or(SaveError::from(SaveFault::UnfilledRootSlot))
    }

    fn run_tasks(&mut self, root: Val, global: &str, root_slot: usize) -> Result<(), SaveError> {
        let mut tasks = vec![EncodeTask::Value {
            val: root,
            destination: ValueDestination::Root(root_slot),
        }];
        while let Some(task) = tasks.pop() {
            match task {
                EncodeTask::Value { val, destination } => {
                    let saved = match val {
                        Val::Nil => Ok(SavedVal::Nil),
                        Val::Bool(b) => Ok(SavedVal::Bool(b)),
                        Val::Num(n) => Ok(SavedVal::Num(saved_num_bits(n))),
                        Val::Str(ptr) => self.string_id(ptr).map(SavedVal::Str),
                        Val::RustFn(func) => {
                            let addr = func as usize;
                            let Some(id) = self.state.rust_fn_ids_by_addr.get(&addr) else {
                                let reachable_from = PathFinder::new(self.state, &self.env_reverse)
                                    .find(root, global, func)
                                    .ok_or(SaveError::from(
                                        SaveFault::UnregisteredFunctionPathNotFound,
                                    ))?;
                                return Err(SaveError::UnregisteredFunction { reachable_from });
                            };
                            Ok(SavedVal::Fn(id.clone()))
                        }
                        Val::Obj(ptr) => {
                            if let Some(token) = self.env_reverse.get(&ptr) {
                                Ok(SavedVal::EnvObj(token.clone()))
                            } else {
                                let id = self.object_id(ptr, &mut tasks)?;
                                Ok(SavedVal::Obj(id))
                            }
                        }
                    }?;
                    if matches!(val, Val::Str(_) | Val::RustFn(_) | Val::Obj(_)) {
                        self.reachable_pointer_values.insert(saved.clone());
                    }
                    if let Val::Obj(ptr) = val
                        && self.env_reverse.contains_key(&ptr)
                        && self.expanded_environment_objects.insert(ptr)
                    {
                        let table = self
                            .state
                            .heap
                            .as_table_ref(ptr)
                            .ok_or(SaveError::from(SaveFault::EnvironmentObjectNotTable))?;
                        if let Some(metatable) = table.get_metatable() {
                            tasks.push(EncodeTask::Value {
                                val: Val::Obj(metatable),
                                destination: ValueDestination::Discard,
                            });
                        }
                        for (key, value) in table.entries().into_iter().rev() {
                            tasks.push(EncodeTask::Value {
                                val: value,
                                destination: ValueDestination::Discard,
                            });
                            tasks.push(EncodeTask::Value {
                                val: key,
                                destination: ValueDestination::Discard,
                            });
                        }
                    }
                    self.write_value(destination, saved)?;
                }
                EncodeTask::Object { ptr, id } => {
                    self.expand_object(ptr, id, &mut tasks)?;
                }
                EncodeTask::Upvalue {
                    upvalue,
                    object,
                    index,
                } => {
                    let key = upvalue.index();
                    let id = if let Some(id) = self.upvalue_ids.get(&key) {
                        *id
                    } else {
                        let id = u32::try_from(self.upvalues.len()).map_err(|_| {
                            too_large(SaveCollection::Upvalues, self.upvalues.len())
                        })?;
                        self.upvalue_ids.insert(key, id);
                        self.upvalues.push(None);
                        match self.state.upvalue_pool.get(upvalue) {
                            Upvalue::Closed(val) => tasks.push(EncodeTask::Value {
                                val: *val,
                                destination: ValueDestination::Upvalue(id),
                            }),
                            Upvalue::Open(_) => return Err(SaveError::OpenUpvalueReachable),
                        }
                        id
                    };
                    let pending = self
                        .objects
                        .get_mut(object as usize)
                        .ok_or(SaveError::from(SaveFault::MissingObjectSlot))?;
                    let Some(PendingObject::Closure { upvalues, .. }) = pending else {
                        return Err(SaveError::from(SaveFault::UpvalueDestinationNotClosure));
                    };
                    let slot = upvalues
                        .get_mut(index)
                        .ok_or(SaveError::from(SaveFault::MissingClosureUpvalueSlot))?;
                    *slot = Some(id);
                }
            }
        }
        Ok(())
    }

    fn object_id(&mut self, ptr: ObjectPtr, tasks: &mut Vec<EncodeTask>) -> Result<u32, SaveError> {
        if let Some(id) = self.object_ids.get(&ptr) {
            return Ok(*id);
        }
        let id = u32::try_from(self.objects.len())
            .map_err(|_| too_large(SaveCollection::Objects, self.objects.len()))?;
        self.object_ids.insert(ptr, id);
        self.objects.push(None);
        tasks.push(EncodeTask::Object { ptr, id });
        Ok(id)
    }

    fn expand_object(
        &mut self,
        ptr: ObjectPtr,
        id: u32,
        tasks: &mut Vec<EncodeTask>,
    ) -> Result<(), SaveError> {
        let pending = match &self.state.heap.get(ptr).raw {
            RawObject::Table(table) => {
                let (array_len, array_slots, entries) = table.snapshot_entries();
                // MAX_ARRAY_LEN keeps the logical length below 2^53.
                let array_len = array_len as u64;
                let array_slots = u32::try_from(array_slots)
                    .map_err(|_| too_large(SaveCollection::TableArraySlots, array_slots))?;
                let entry_count = entries.len();
                let metatable = table.get_metatable();
                if let Some(mt) = metatable {
                    tasks.push(EncodeTask::Value {
                        val: Val::Obj(mt),
                        destination: ValueDestination::Metatable { object: id },
                    });
                }
                for (idx, (key, value)) in entries.into_iter().enumerate().rev() {
                    tasks.push(EncodeTask::Value {
                        val: value,
                        destination: ValueDestination::TableValue {
                            object: id,
                            entry: idx,
                        },
                    });
                    tasks.push(EncodeTask::Value {
                        val: key,
                        destination: ValueDestination::TableKey {
                            object: id,
                            entry: idx,
                        },
                    });
                }
                PendingObject::Table {
                    array_len,
                    array_slots,
                    entries: vec![(None, None); entry_count],
                    metatable: metatable.map(|_| None),
                }
            }
            RawObject::LuaFn(closure) => {
                let chunk = self.encode_bytecode(&closure.bytecode)?;
                for (idx, uv_ref) in closure.upvalues.iter().copied().enumerate().rev() {
                    tasks.push(EncodeTask::Upvalue {
                        upvalue: uv_ref,
                        object: id,
                        index: idx,
                    });
                }
                PendingObject::Closure {
                    chunk,
                    upvalues: vec![None; closure.upvalues.len()],
                }
            }
        };
        let slot = self
            .objects
            .get_mut(id as usize)
            .ok_or(SaveError::from(SaveFault::MissingObjectSlot))?;
        *slot = Some(pending);
        Ok(())
    }

    fn write_value(
        &mut self,
        destination: ValueDestination,
        value: SavedVal,
    ) -> Result<(), SaveError> {
        match destination {
            ValueDestination::Root(index) => {
                *self
                    .roots
                    .get_mut(index)
                    .ok_or(SaveError::from(SaveFault::MissingRootSlot))? = Some(value);
            }
            ValueDestination::Upvalue(id) => {
                *self
                    .upvalues
                    .get_mut(id as usize)
                    .ok_or(SaveError::from(SaveFault::MissingUpvalueSlot))? = Some(value);
            }
            ValueDestination::TableKey { object, entry }
            | ValueDestination::TableValue { object, entry } => {
                let pending = self
                    .objects
                    .get_mut(object as usize)
                    .ok_or(SaveError::from(SaveFault::MissingObjectSlot))?;
                let Some(PendingObject::Table { entries, .. }) = pending else {
                    return Err(SaveError::from(SaveFault::TableDestinationNotTable));
                };
                let slot = entries
                    .get_mut(entry)
                    .ok_or(SaveError::from(SaveFault::MissingTableEntrySlot))?;
                if matches!(destination, ValueDestination::TableKey { .. }) {
                    slot.0 = Some(value);
                } else {
                    slot.1 = Some(value);
                }
            }
            ValueDestination::Metatable { object } => {
                let pending = self
                    .objects
                    .get_mut(object as usize)
                    .ok_or(SaveError::from(SaveFault::MissingObjectSlot))?;
                let Some(PendingObject::Table { metatable, .. }) = pending else {
                    return Err(SaveError::from(SaveFault::MetatableDestinationNotTable));
                };
                *metatable = Some(Some(value));
            }
            ValueDestination::Discard => {}
        }
        Ok(())
    }

    fn finish_object(object: PendingObject) -> Result<SavedObject, SaveError> {
        match object {
            PendingObject::Table {
                array_len,
                array_slots,
                entries,
                metatable,
            } => Ok(SavedObject::Table {
                array_len,
                array_slots,
                entries: entries
                    .into_iter()
                    .map(|(key, value)| {
                        Ok((
                            key.ok_or(SaveError::from(SaveFault::UnfilledTableKeySlot))?,
                            value.ok_or(SaveError::from(SaveFault::UnfilledTableValueSlot))?,
                        ))
                    })
                    .collect::<Result<Vec<_>, SaveError>>()?,
                metatable: match metatable {
                    None => None,
                    Some(Some(value)) => Some(value),
                    Some(None) => {
                        return Err(SaveError::from(SaveFault::UnfilledMetatableSlot));
                    }
                },
            }),
            PendingObject::Closure { chunk, upvalues } => Ok(SavedObject::Closure {
                chunk,
                upvalues: upvalues
                    .into_iter()
                    .map(|value| {
                        value.ok_or(SaveError::from(SaveFault::UnfilledClosureUpvalueSlot))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            }),
        }
    }
    fn encode_bytecode(&mut self, bc: &Arc<Bytecode>) -> Result<u32, SaveError> {
        let key = Arc::as_ptr(bc) as usize;
        if let Some(id) = self.bytecode_ids.get(&key) {
            return Ok(*id);
        }
        let id = u32::try_from(self.bytecode.len())
            .map_err(|_| too_large(SaveCollection::Chunks, self.bytecode.len()))?;
        self.bytecode_ids.insert(key, id);

        // Reserve the arena slot before recursively encoding children. Nested
        // ids are arena indices, so post-order insertion would alias ids when
        // a parent and child are first seen in the same walk.
        self.bytecode.push(SavedBytecode {
            code: Vec::new(),
            number_literals: Vec::new(),
            string_literals: Vec::new(),
            table_templates: Vec::new(),
            global_cache_slots: 0,
            field_cache_slots: 0,
            set_field_cache_slots: 0,
            tfor_cursor_slots: 0,
            num_params: 0,
            num_locals: 0,
            max_stack: 0,
            nested: Vec::new(),
            upvalues: Vec::new(),
            is_vararg: false,
            name: None,
            defined_line: 0,
            source: None,
            line_info: Vec::new(),
        });

        let mut nested = Vec::with_capacity(bc.nested.len());
        for child in &bc.nested {
            nested.push(self.encode_bytecode(child)?);
        }
        self.bytecode[id as usize] = SavedBytecode {
            code: bc.code.iter().map(|inst| inst.raw()).collect(),
            number_literals: bc
                .number_literals
                .iter()
                .map(|n| saved_num_bits(*n))
                .collect(),
            string_literals: bc.string_literals.clone(),
            table_templates: bc.table_templates.clone(),
            global_cache_slots: bc.global_cache_slots,
            field_cache_slots: bc.field_cache_slots,
            set_field_cache_slots: bc.set_field_cache_slots,
            tfor_cursor_slots: bc.tfor_cursor_slots,
            num_params: bc.num_params,
            num_locals: bc.num_locals,
            max_stack: bc.max_stack,
            nested,
            upvalues: bc.upvalues.clone(),
            is_vararg: bc.is_vararg,
            name: bc.name.as_deref().map(str::to_owned),
            defined_line: bc.defined_line,
            source: bc.source.clone(),
            line_info: bc.line_info.clone(),
        };
        Ok(id)
    }
}

impl State {
    /// Snapshot persistent script state.
    ///
    /// The VM must be quiescent: no Lua/Rust call can be in flight. Anchors are
    /// process-local and are not serialized, but their count is returned in
    /// [`SaveDiagnostics`].
    ///
    /// A poisoned State (see [`State::is_poisoned`]) is refused with
    /// [`SaveError::StatePoisoned`].
    pub fn save_state(&self) -> Result<SaveState, SaveError> {
        if self.poisoned {
            return Err(SaveError::StatePoisoned);
        }
        self.validate_quiescent()?;
        let payload = SaveBuilder::new(self).finish()?;
        let mut encoder = Encoder::with_capacity(payload.encoded_size_estimate());
        encoder.write_magic_and_versions();
        // Diagnostic metadata only. The format and cost-model versions in the
        // magic block are the hard compatibility gates; this human-readable
        // crate version records which build produced the snapshot for debugging
        // and is intentionally not compared on load.
        encoder.write_bytes(
            env!("CARGO_PKG_VERSION").as_bytes(),
            SnapshotField::EngineVersion,
        )?;
        payload.encode(&mut encoder)?;
        Ok(SaveState {
            bytes: encoder.finish(),
            diagnostics: SaveDiagnostics {
                anchor_count: self.anchor_count(),
            },
        })
    }

    /// Restore a save into a fresh [`State`].
    ///
    /// The host supplies callbacks and re-runs its environment setup closure so
    /// registered host functions exist before saved values are resolved. A
    /// setup error is returned as [`LoadError::Setup`] without materializing
    /// any saved values. A setup that returns success after a panic unwound
    /// through the State (it caught the panic) gets
    /// [`LoadError::StatePoisoned`].
    ///
    /// A load is all-or-nothing by construction: the State is built here and
    /// only returned on success. Any error drops it, so a failure partway
    /// through materialization (for example after some environment deltas were
    /// applied to the fresh library tables) is never observable. Rollback of
    /// partial work is therefore deliberately absent; keep the State private
    /// to this function until materialization has fully succeeded.
    ///
    /// Whether the standard library is installed is decided by the save itself,
    /// not by the caller: a save taken from a standard-library State loads into
    /// one, and a raw save ([`State::raw`]) loads into a raw State. There is
    /// deliberately no override, so a load can never disagree with the
    /// environment tokens recorded in the save.
    pub fn load_state<E>(
        bytes: &[u8],
        callbacks: Box<dyn HostCallbacks>,
        setup: impl FnOnce(&mut State) -> std::result::Result<(), E>,
    ) -> std::result::Result<State, LoadError<E>> {
        let mut decoder = Decoder::new(bytes);
        decoder.read_magic_and_versions()?;
        // Read and discard the diagnostic crate-version string. Compatibility is
        // enforced by the format and cost-model versions in
        // read_magic_and_versions above; this string is metadata, not a compat
        // gate, so it is deliberately not compared.
        let _engine_version = decoder.read_bytes(SnapshotField::EngineVersion)?;
        let decoded = SavePayload::decode(&mut decoder)?;
        let payload = verify::verify_payload(decoded)?;
        decoder.finish()?;

        let mut state = if payload.has_standard_environment() {
            State::with_callbacks(callbacks)
        } else {
            State::raw_with_callbacks(callbacks)
        };
        // Anchors created by setup are deliberately retained and participate
        // in the final collection.
        state.global_env.begin_load_setup();
        setup(&mut state).map_err(LoadError::Setup)?;
        if state.poisoned {
            return Err(LoadError::StatePoisoned);
        }
        materialize_payload(&mut state, payload)?;
        Ok(state)
    }
}

/// Object -> token map for the save-time environment. Sourced from the
/// canonical snapshot captured at construction, so a shadowed builtin slot
/// (`math = {}`) never causes a user table to be misread as an env object.
fn build_env_reverse(state: &State) -> BTreeMap<ObjectPtr, String> {
    state.env_tokens.clone()
}

/// Token -> object map for the load-time environment, inverting the freshly
/// rebuilt snapshot so saved `EnvObj` tokens resolve to the new objects.
/// Loading does not reconstruct token names from builtin names or literals;
/// capture uses this module's token schema on both sides of the round trip.
fn build_env_forward(state: &State) -> BTreeMap<String, ObjectPtr> {
    state
        .env_tokens
        .iter()
        .map(|(ptr, token)| (token.clone(), *ptr))
        .collect()
}

/// Bit pattern a number is saved as. Every NaN is written as the one canonical
/// quiet NaN: hardware default NaNs differ by host (sign bit set on x86-64,
/// clear on aarch64), and those differences must not affect snapshot bytes.
/// No script can observe the difference - NaN prints unsigned,
/// never compares equal and cannot be a table key - so canonicalising loses
/// nothing. Every other value, `-0.0` included, keeps its exact bits.
fn saved_num_bits(n: f64) -> u64 {
    if n.is_nan() {
        crate::numeral::CANONICAL_NAN_BITS
    } else {
        n.to_bits()
    }
}

fn corrupt_arena(reason: CorruptionReason, index: usize, location: SaveLocation) -> SnapshotError {
    SnapshotError::CorruptArena {
        reason,
        index,
        location,
    }
}

// Retain the table's precise failure as well as the environment token. The
// optional entry index belongs to the deletion or upsert collection.
fn invalid_environment_table(
    token: &str,
    entry: Option<usize>,
    step: TableRestoreStep,
    error: TableError,
) -> SnapshotError {
    SnapshotError::InvalidTable {
        location: TableLoadLocation::Environment {
            token: token.to_owned(),
            entry,
        },
        step,
        failure: error.into(),
    }
}

fn runtime_array_slot(key: Val, array_len: usize) -> Option<usize> {
    match key {
        Val::Num(n) => array_index(n, array_len),
        _ => None,
    }
}

fn materialize_payload(
    state: &mut State,
    payload: verify::VerifiedSavePayload,
) -> Result<(), SnapshotError> {
    // All graph allocation below uses the heap's no-collection allocator and
    // UpvaluePool directly. They do not consult thresholds or collect; the one
    // runtime resolver used here is explicitly resolve_bytecode_runtime_no_gc.
    // Do not replace these with `State::allocator`: it may collect
    // before the temporary strings/objects/upvalues have become State roots.
    // The only collection is after all globals, deltas and setup anchors are
    // rooted, at the end of this function.
    let env_forward = build_env_forward(state);
    let fn_forward = state.rust_fns_by_id.clone();

    let bytecode = materialize_bytecode(&payload)?;
    let payload = payload.into_inner();

    let mut strings = Vec::with_capacity(payload.strings.len());
    for bytes in &payload.strings {
        strings.push(state.heap.no_gc_allocator().alloc_string(bytes));
    }

    let mut upvalues = Vec::with_capacity(payload.upvalues.len());
    for _ in &payload.upvalues {
        upvalues.push(state.upvalue_pool.alloc_closed_nil());
    }

    let mut objects = Vec::with_capacity(payload.objects.len());
    for obj in &payload.objects {
        let ptr = match obj {
            // Tables are filled (and sized) in the second pass via
            // rebuild_from_snapshot, so allocate an empty shell here. None of
            // these allocators run GC, which is what keeps the half-built graph
            // alive until the final collect.
            SavedObject::Table { .. } => state.heap.no_gc_allocator().alloc_table(),
            SavedObject::Closure {
                chunk,
                upvalues: saved_upvalues,
            } => {
                let bc = Arc::clone(bytecode.get(*chunk as usize).ok_or_else(|| {
                    corrupt_arena(
                        CorruptionReason::IdOutOfRange(SavedReference::ClosureChunk),
                        *chunk as usize,
                        SaveLocation::Bytecode,
                    )
                })?);
                let mut refs = Vec::with_capacity(saved_upvalues.len());
                for uv_id in saved_upvalues {
                    refs.push(*upvalues_ref(&upvalues, *uv_id)?);
                }
                let runtime = state.resolve_bytecode_runtime_no_gc(&bc);
                state.heap.no_gc_allocator().alloc_lua_fn(bc, runtime, refs)
            }
        };
        objects.push(ptr);
    }

    let ctx = DecodeCtx {
        strings: &strings,
        objects: &objects,
        env_forward: &env_forward,
        fn_forward: &fn_forward,
    };

    for (idx, saved) in payload.upvalues.iter().enumerate() {
        let val = decode_val(saved, &ctx)?;
        state.upvalue_pool.set_closed(upvalues[idx], val);
    }

    for (idx, saved_obj) in payload.objects.iter().enumerate() {
        match saved_obj {
            SavedObject::Table {
                array_len,
                array_slots,
                entries,
                metatable,
            } => {
                let mut runtime_keys = BTreeSet::new();
                let entries = entries
                    .iter()
                    .enumerate()
                    .map(|(entry_index, (key, value))| {
                        let key = decode_val(key, &ctx)?;
                        insert_runtime_key(
                            &mut runtime_keys,
                            key,
                            &state.heap,
                            entry_index,
                            CorruptionReason::Duplicate {
                                entry: DuplicateEntry::TableKey,
                                form: KeyForm::Materialized,
                            },
                            SaveLocation::TableEntries { object: idx },
                        )?;
                        Ok((key, decode_val(value, &ctx)?))
                    })
                    .collect::<Result<Vec<_>, SnapshotError>>()?;
                // A metatable must be a table; the verifier checks saved `Obj`
                // ids, and this also covers `EnvObj` tokens, whose runtime
                // object is only known here.
                let metatable = match metatable {
                    None => None,
                    Some(mt) => match decode_val(mt, &ctx)? {
                        Val::Obj(mt_ptr) if state.heap.as_table_ref(mt_ptr).is_some() => {
                            Some(mt_ptr)
                        }
                        _ => {
                            return Err(corrupt_arena(
                                CorruptionReason::Table(TableCorruption::MetatableNotTable),
                                idx,
                                SaveLocation::Objects,
                            ));
                        }
                    },
                };
                let ptr = objects[idx];
                let table = state.heap.as_table(ptr).ok_or_else(|| {
                    corrupt_arena(
                        CorruptionReason::Table(TableCorruption::MaterializedWrongType),
                        idx,
                        SaveLocation::Objects,
                    )
                })?;
                // The verifier bounds `array_len` by MAX_ARRAY_LEN, so it fits.
                let array_len = match usize::try_from(*array_len) {
                    Ok(len) => len,
                    Err(_) => usize::MAX,
                };
                table
                    .rebuild_from_snapshot(array_len, *array_slots as usize, entries)
                    .map_err(|error| SnapshotError::InvalidTable {
                        location: TableLoadLocation::Object(idx),
                        step: TableRestoreStep::ObjectRebuild,
                        failure: error.into(),
                    })?;
                if metatable.is_some() {
                    table.set_metatable(metatable);
                }
            }
            SavedObject::Closure { .. } => {}
        }
    }

    for (delta_index, delta) in payload.env_deltas.iter().enumerate() {
        let ptr = *env_forward
            .get(&delta.token)
            .ok_or_else(|| SnapshotError::UnknownEnvObject(delta.token.clone()))?;
        // Deliberately repeats the verifier's duplicate and disjointness
        // checks, now on resolved runtime values. The verifier compares saved
        // keys, which can differ while naming the same runtime key: two `Fn`
        // ids that resolve to one address (identical-code folding) or two
        // environment tokens for one object. The reason strings differ because
        // this pass reports the materialized form.
        let mut deleted_keys = BTreeSet::new();
        for (key_index, key) in delta.deleted.iter().enumerate() {
            insert_runtime_key(
                &mut deleted_keys,
                decode_val(key, &ctx)?,
                &state.heap,
                key_index,
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::DeletedEnvironmentKey,
                    form: KeyForm::Materialized,
                },
                SaveLocation::EnvironmentDeleted(delta_index),
            )?;
        }
        let mut upsert_keys = BTreeSet::new();
        for (entry_index, (key, _)) in delta.upserts.iter().enumerate() {
            insert_runtime_key(
                &mut upsert_keys,
                decode_val(key, &ctx)?,
                &state.heap,
                entry_index,
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::EnvironmentUpsertKey,
                    form: KeyForm::Materialized,
                },
                SaveLocation::EnvironmentUpserts(delta_index),
            )?;
        }
        if !deleted_keys.is_disjoint(&upsert_keys) {
            return Err(corrupt_arena(
                CorruptionReason::Environment(EnvironmentCorruption::KeyDeletedAndUpserted(
                    KeyForm::Materialized,
                )),
                delta_index,
                SaveLocation::EnvironmentDeltas,
            ));
        }
        if let Some(order) = &delta.order {
            let mut order_keys = BTreeSet::new();
            for (key_index, key) in order.iter().enumerate() {
                insert_runtime_key(
                    &mut order_keys,
                    decode_val(key, &ctx)?,
                    &state.heap,
                    key_index,
                    CorruptionReason::Duplicate {
                        entry: DuplicateEntry::EnvironmentOrderKey,
                        form: KeyForm::Materialized,
                    },
                    SaveLocation::EnvironmentOrder(delta_index),
                )?;
            }
            if !deleted_keys.is_disjoint(&order_keys) {
                return Err(corrupt_arena(
                    CorruptionReason::Environment(EnvironmentCorruption::OrderContainsDeletedKey(
                        KeyForm::Materialized,
                    )),
                    delta_index,
                    SaveLocation::EnvironmentDeltas,
                ));
            }
            if !upsert_keys.is_subset(&order_keys) {
                return Err(corrupt_arena(
                    CorruptionReason::Environment(EnvironmentCorruption::OrderOmitsUpsertedKey(
                        KeyForm::Materialized,
                    )),
                    delta_index,
                    SaveLocation::EnvironmentDeltas,
                ));
            }
        }
        {
            let table = state.heap.as_table(ptr).ok_or_else(|| {
                corrupt_arena(
                    CorruptionReason::Environment(EnvironmentCorruption::TokenNotTable),
                    delta_index,
                    SaveLocation::EnvironmentDeltas,
                )
            })?;
            for (key_index, key) in delta.deleted.iter().enumerate() {
                table
                    .insert(decode_val(key, &ctx)?, Val::Nil)
                    .map_err(|error| {
                        invalid_environment_table(
                            &delta.token,
                            Some(key_index),
                            TableRestoreStep::DeleteEnvironmentKey,
                            error,
                        )
                    })?;
            }
            for (entry_index, (key, value)) in delta.upserts.iter().enumerate() {
                table
                    .insert(decode_val(key, &ctx)?, decode_val(value, &ctx)?)
                    .map_err(|error| {
                        invalid_environment_table(
                            &delta.token,
                            Some(entry_index),
                            TableRestoreStep::UpsertEnvironmentEntry,
                            error,
                        )
                    })?;
            }
        }
        if let Some(order) = &delta.order {
            let (current, array, metatable) = {
                let table = state.heap.as_table(ptr).ok_or_else(|| {
                    corrupt_arena(
                        CorruptionReason::Environment(EnvironmentCorruption::TokenNotTable),
                        delta_index,
                        SaveLocation::EnvironmentDeltas,
                    )
                })?;
                (
                    table.entries(),
                    table.virtual_array(),
                    table.get_metatable(),
                )
            };
            let mut current_by_key = BTreeMap::new();
            for (key_index, (key, value)) in current.iter().copied().enumerate() {
                current_by_key.insert(
                    runtime_table_key(
                        key,
                        &state.heap,
                        key_index,
                        SaveLocation::EnvironmentOrder(delta_index),
                    )?,
                    (key, value),
                );
            }
            let mut ordered = Vec::with_capacity(current.len());
            for (key_index, key) in order.iter().enumerate() {
                let key = decode_val(key, &ctx)?;
                let Some((current_key, value)) = current_by_key.remove(&runtime_table_key(
                    key,
                    &state.heap,
                    key_index,
                    SaveLocation::EnvironmentOrder(delta_index),
                )?) else {
                    return Err(corrupt_arena(
                        CorruptionReason::Environment(EnvironmentCorruption::OrderKeyAbsent),
                        key_index,
                        SaveLocation::EnvironmentOrder(delta_index),
                    ));
                };
                ordered.push((current_key, value));
            }
            // A newer library or load setup may have added entries unknown to
            // the save. Preserve them after the saved order in their current
            // insertion order.
            for (key_index, (key, value)) in current.into_iter().enumerate() {
                if current_by_key
                    .remove(&runtime_table_key(
                        key,
                        &state.heap,
                        key_index,
                        SaveLocation::EnvironmentOrder(delta_index),
                    )?)
                    .is_some()
                {
                    ordered.push((key, value));
                }
            }
            let table = state.heap.as_table(ptr).ok_or_else(|| {
                corrupt_arena(
                    CorruptionReason::Environment(EnvironmentCorruption::TokenNotTable),
                    delta_index,
                    SaveLocation::EnvironmentDeltas,
                )
            })?;
            // Reordering live hash entries must retain setup's array slots,
            // including holes that are absent from `current`. With an empty
            // array this is just `ordered`: it permutes the table's live hash
            // keys, none of which can be the append key 1, so the exact-shape
            // rebuild accepts it and re-derives no array part.
            let mut entries: Vec<_> = array
                .iter()
                .enumerate()
                .map(|(slot, value)| (Val::Num((slot + 1) as f64), *value))
                .collect();
            entries.extend(
                ordered
                    .into_iter()
                    .filter(|(key, _)| runtime_array_slot(*key, array.len()).is_none()),
            );
            table
                .rebuild_from_snapshot(array.len(), array.len(), entries)
                .map_err(|error| {
                    invalid_environment_table(
                        &delta.token,
                        None,
                        TableRestoreStep::RestoreEnvironmentOrder,
                        error,
                    )
                })?;
            // Rebuilding entries replaces the table shell. An unchanged delta
            // must retain a metatable that setup or a later library installed.
            table.set_metatable(metatable);
        }
        if let Some(array) = &delta.array {
            // Restore the saved array shape: the saved slots (nil ones
            // included) first, then every other live entry in its current
            // order. A live entry for a slot key is superseded by the slot.
            let mut array = array
                .iter()
                .map(|value| decode_val(value, &ctx))
                .collect::<Result<Vec<_>, SnapshotError>>()?;
            let table = state.heap.as_table(ptr).ok_or_else(|| {
                corrupt_arena(
                    CorruptionReason::Environment(EnvironmentCorruption::TokenNotTable),
                    delta_index,
                    SaveLocation::EnvironmentDeltas,
                )
            })?;
            let metatable = table.get_metatable();
            // Setup can have added later slots. Retain their shape and values
            // beyond the saved range, just like other setup-only entries.
            let saved_len = array.len();
            array.extend(table.virtual_array().into_iter().skip(saved_len));
            let append_key = Val::Num((array.len() + 1) as f64);
            let append_value = table.get(&append_key);
            let mut entries: Vec<(Val, Val)> = array
                .iter()
                .enumerate()
                .map(|(slot, value)| (Val::Num((slot + 1) as f64), *value))
                .collect();
            entries.extend(table.entries().into_iter().filter(|(key, _)| {
                runtime_array_slot(*key, array.len()).is_none() && *key != append_key
            }));
            table
                .rebuild_from_snapshot(array.len(), array.len(), entries)
                .map_err(|error| {
                    invalid_environment_table(
                        &delta.token,
                        None,
                        TableRestoreStep::RestoreEnvironmentArray,
                        error,
                    )
                })?;
            // A saved append key is rejected by the verifier. A setup-only
            // append key is legal and extends the restored shape normally.
            if !matches!(append_value, Val::Nil) {
                table.insert(append_key, append_value).map_err(|error| {
                    invalid_environment_table(
                        &delta.token,
                        None,
                        TableRestoreStep::SetupEnvironmentAppend,
                        error,
                    )
                })?;
            }
            table.set_metatable(metatable);
        }
        let new_metatable = match &delta.metatable {
            SavedMetatableDelta::Unchanged => None,
            SavedMetatableDelta::Cleared => Some(None),
            SavedMetatableDelta::Set(saved) => match decode_val(saved, &ctx)? {
                Val::Obj(mt_ptr) if state.heap.as_table_ref(mt_ptr).is_some() => Some(Some(mt_ptr)),
                _ => {
                    return Err(corrupt_arena(
                        CorruptionReason::Environment(EnvironmentCorruption::MetatableNotTable(
                            KeyForm::Materialized,
                        )),
                        delta_index,
                        SaveLocation::EnvironmentDeltas,
                    ));
                }
            },
        };
        let table = state.heap.as_table(ptr).ok_or_else(|| {
            corrupt_arena(
                CorruptionReason::Environment(EnvironmentCorruption::TokenNotTable),
                delta_index,
                SaveLocation::EnvironmentDeltas,
            )
        })?;
        if let Some(metatable) = new_metatable {
            table.set_metatable(metatable);
        }
    }

    let mut user_globals = Vec::with_capacity(payload.user_globals.len());
    for (name, saved) in payload.user_globals {
        user_globals.push((name, decode_val(&saved, &ctx)?));
    }
    state
        .global_env
        .restore(user_globals, payload.globals_sealed);
    state.rng = VmRng::from_state(payload.rng_state);
    state.cost = payload.cost;
    state.next_format_pointer_id = payload.next_format_pointer_id;
    let mut seen_pointer_values = BTreeSet::new();
    let mut seen_pointer_ids = BTreeSet::new();
    let mut format_pointer_ids = map_with_capacity(payload.format_pointer_ids.len());
    for (index, (saved, id)) in payload.format_pointer_ids.iter().enumerate() {
        // Save records the id of every registered Rust function that was
        // rendered, reachable or not. One the load-time setup no longer
        // registers cannot be reached by any script or host value, so its
        // id is simply retired (it stays below next_format_pointer_id and is
        // never reissued) instead of failing the load.
        if let SavedVal::Fn(name) = saved
            && !ctx.fn_forward.contains_key(name)
        {
            if !seen_pointer_ids.insert(*id) {
                return Err(corrupt_arena(
                    CorruptionReason::Duplicate {
                        entry: DuplicateEntry::FormatPointerId,
                        form: KeyForm::Saved,
                    },
                    index,
                    SaveLocation::PointerIds,
                ));
            }
            continue;
        }
        let value = decode_val(saved, &ctx)?;
        insert_runtime_key(
            &mut seen_pointer_values,
            value,
            &state.heap,
            index,
            CorruptionReason::Duplicate {
                entry: DuplicateEntry::FormatPointerValue,
                form: KeyForm::Materialized,
            },
            SaveLocation::PointerIds,
        )?;
        if !seen_pointer_ids.insert(*id) {
            return Err(corrupt_arena(
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::FormatPointerId,
                    form: KeyForm::Saved,
                },
                index,
                SaveLocation::PointerIds,
            ));
        }
        // The verifier admits only pointer-like saved values here.
        let key = PointerIdKey::from_val(value).ok_or_else(|| {
            corrupt_arena(
                CorruptionReason::InvalidFormatPointerValue,
                index,
                SaveLocation::PointerIds,
            )
        })?;
        format_pointer_ids.insert(key, *id);
    }
    state.format_pointer_ids = format_pointer_ids;
    state.reset_in_flight();
    state.gc_collect();
    Ok(())
}

fn upvalues_ref(upvalues: &[UpvalueRef], id: u32) -> Result<&UpvalueRef, SnapshotError> {
    upvalues.get(id as usize).ok_or_else(|| {
        corrupt_arena(
            CorruptionReason::IdOutOfRange(SavedReference::Upvalue),
            id as usize,
            SaveLocation::Upvalues,
        )
    })
}

fn object_ref(objects: &[ObjectPtr], id: u32) -> Result<&ObjectPtr, SnapshotError> {
    objects.get(id as usize).ok_or_else(|| {
        corrupt_arena(
            CorruptionReason::IdOutOfRange(SavedReference::Object),
            id as usize,
            SaveLocation::Objects,
        )
    })
}

struct DecodeCtx<'a> {
    strings: &'a [super::object::StringPtr],
    objects: &'a [ObjectPtr],
    env_forward: &'a BTreeMap<String, ObjectPtr>,
    fn_forward: &'a BTreeMap<String, RustFunc>,
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
enum SavedObjectKey {
    Object(u32),
    EnvironmentObject(String),
}

type ResolvedKey = NormalizedTableKey<Vec<u8>, SavedObjectKey, String>;

impl SavedVal {
    /// Snapshot counterpart of `Val::table_key`. Reference payloads differ, but
    /// numeric equality and nil/NaN rejection come from the same normalizer.
    fn table_key(
        &self,
        strings: &[Vec<u8>],
        index: usize,
        location: SaveLocation,
    ) -> Result<ResolvedKey, SnapshotError> {
        let key = match self {
            SavedVal::Nil => TableKeyInput::Nil,
            SavedVal::Bool(value) => TableKeyInput::Bool(*value),
            SavedVal::Num(bits) => TableKeyInput::Number(f64::from_bits(*bits)),
            SavedVal::Str(id) => {
                TableKeyInput::String(strings.get(*id as usize).cloned().ok_or_else(|| {
                    corrupt_arena(
                        CorruptionReason::IdOutOfRange(SavedReference::TableKeyString),
                        *id as usize,
                        SaveLocation::Strings,
                    )
                })?)
            }
            SavedVal::Obj(id) => TableKeyInput::Object(SavedObjectKey::Object(*id)),
            SavedVal::Fn(id) => TableKeyInput::RustFn(id.clone()),
            SavedVal::EnvObj(id) => {
                TableKeyInput::Object(SavedObjectKey::EnvironmentObject(id.clone()))
            }
        };
        normalize_table_key(key).map_err(|error| invalid_key_error(error, index, location))
    }
}

/// The corruption error for a key the shared normalizer rejected.
fn invalid_key_error(
    error: InvalidTableKey,
    index: usize,
    location: SaveLocation,
) -> SnapshotError {
    match error {
        InvalidTableKey::Nil => corrupt_arena(CorruptionReason::NilKey, index, location),
        InvalidTableKey::Nan => corrupt_arena(CorruptionReason::NanKey, index, location),
    }
}

type RuntimeTableKey = NormalizedTableKey<Vec<u8>, ObjectPtr, usize>;

fn runtime_table_key(
    value: Val,
    heap: &GcHeap,
    index: usize,
    location: SaveLocation,
) -> Result<RuntimeTableKey, SnapshotError> {
    let key = value
        .table_key()
        .map_err(|error| invalid_key_error(error, index, location))?;
    Ok(match key {
        NormalizedTableKey::Bool(value) => NormalizedTableKey::Bool(value),
        NormalizedTableKey::Number(value) => NormalizedTableKey::Number(value),
        NormalizedTableKey::String(ptr) => {
            NormalizedTableKey::String(heap.get_string(ptr).to_vec())
        }
        NormalizedTableKey::Object(ptr) => NormalizedTableKey::Object(ptr),
        NormalizedTableKey::RustFn(func) => NormalizedTableKey::RustFn(func),
    })
}

fn insert_runtime_key(
    keys: &mut BTreeSet<RuntimeTableKey>,
    value: Val,
    heap: &GcHeap,
    index: usize,
    reason: CorruptionReason,
    location: SaveLocation,
) -> Result<(), SnapshotError> {
    if keys.insert(runtime_table_key(value, heap, index, location.clone())?) {
        Ok(())
    } else {
        Err(corrupt_arena(reason, index, location))
    }
}

fn decode_val(saved: &SavedVal, ctx: &DecodeCtx<'_>) -> Result<Val, SnapshotError> {
    match saved {
        SavedVal::Nil => Ok(Val::Nil),
        SavedVal::Bool(b) => Ok(Val::Bool(*b)),
        SavedVal::Num(bits) => Ok(Val::Num(f64::from_bits(*bits))),
        SavedVal::Str(id) => Ok(Val::Str(*ctx.strings.get(*id as usize).ok_or_else(
            || {
                corrupt_arena(
                    CorruptionReason::IdOutOfRange(SavedReference::String),
                    *id as usize,
                    SaveLocation::Strings,
                )
            },
        )?)),
        SavedVal::Obj(id) => Ok(Val::Obj(*object_ref(ctx.objects, *id)?)),
        SavedVal::Fn(id) => ctx
            .fn_forward
            .get(id)
            .copied()
            .map(Val::RustFn)
            .ok_or_else(|| SnapshotError::UnknownFunction(id.clone())),
        SavedVal::EnvObj(token) => ctx
            .env_forward
            .get(token)
            .copied()
            .map(Val::Obj)
            .ok_or_else(|| SnapshotError::UnknownEnvObject(token.clone())),
    }
}

fn materialize_bytecode(
    payload: &verify::VerifiedSavePayload,
) -> Result<Vec<Arc<Bytecode>>, SnapshotError> {
    let saved = payload.bytecode();
    let source_names = intern_bytecode_sources(saved);
    let mut out: Vec<Option<Arc<Bytecode>>> = vec![None; saved.len()];
    let mut visiting = vec![false; saved.len()];
    for idx in 0..saved.len() {
        build_bytecode_tree(idx, saved, &source_names, &mut out, &mut visiting)?;
    }
    out.into_iter()
        .enumerate()
        .map(|(index, bc)| {
            bc.ok_or_else(|| {
                corrupt_arena(
                    CorruptionReason::NotMaterialized(MaterializedPart::Chunk),
                    index,
                    SaveLocation::Bytecode,
                )
            })
        })
        .collect()
}

fn intern_bytecode_sources(saved: &[SavedBytecode]) -> Vec<Option<Arc<[u8]>>> {
    // The save format stores each chunk's source field; share equal names again after loading.
    let mut source_pool = BTreeMap::<&[u8], Arc<[u8]>>::new();
    saved
        .iter()
        .map(|bytecode| {
            bytecode.source.as_deref().map(|source| {
                Arc::clone(
                    source_pool
                        .entry(source)
                        .or_insert_with(|| Arc::<[u8]>::from(source)),
                )
            })
        })
        .collect()
}

/// Materialize the chunk at `root` and everything nested under it.
///
/// Iterative depth-first walk on an explicit stack of `(chunk, next_child)`
/// frames: nesting depth in hostile or deep save data costs heap, not Rust
/// stack, so the load path cannot abort the host by recursion. Traversal
/// order, memoization through `out`, and cycle detection through `visiting`
/// all match the recursive version this replaced.
fn build_bytecode_tree(
    root: usize,
    saved: &[SavedBytecode],
    source_names: &[Option<Arc<[u8]>>],
    out: &mut [Option<Arc<Bytecode>>],
    visiting: &mut [bool],
) -> Result<(), SnapshotError> {
    if out[root].is_some() {
        return Ok(());
    }
    visiting[root] = true;
    let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
    while let Some((idx, cursor)) = stack.last_mut() {
        let idx = *idx;
        let src = saved.get(idx).ok_or_else(|| {
            corrupt_arena(
                CorruptionReason::IdOutOfRange(SavedReference::BytecodeChunk),
                idx,
                SaveLocation::Bytecode,
            )
        })?;
        if let Some(child) = src.nested.get(*cursor) {
            *cursor += 1;
            let child = *child as usize;
            if out
                .get(child)
                .ok_or_else(|| {
                    corrupt_arena(
                        CorruptionReason::IdOutOfRange(SavedReference::NestedChunk),
                        child,
                        SaveLocation::Bytecode,
                    )
                })?
                .is_some()
            {
                continue;
            }
            if visiting[child] {
                return Err(corrupt_arena(
                    CorruptionReason::NestedChunkCycle,
                    child,
                    SaveLocation::Bytecode,
                ));
            }
            visiting[child] = true;
            stack.push((child, 0));
            continue;
        }
        let nested = src
            .nested
            .iter()
            .map(|child| {
                out.get(*child as usize)
                    .and_then(|chunk| chunk.as_ref())
                    .map(Arc::clone)
                    .ok_or_else(|| {
                        corrupt_arena(
                            CorruptionReason::NotMaterialized(MaterializedPart::NestedChunk),
                            *child as usize,
                            SaveLocation::Bytecode,
                        )
                    })
            })
            .collect::<Result<Vec<_>, SnapshotError>>()?;
        let source = source_names.get(idx).cloned().ok_or_else(|| {
            corrupt_arena(
                CorruptionReason::NotMaterialized(MaterializedPart::SourceName),
                idx,
                SaveLocation::Bytecode,
            )
        })?;
        out[idx] = Some(build_bytecode_node(src, nested, source));
        visiting[idx] = false;
        stack.pop();
    }
    Ok(())
}

fn build_bytecode_node(
    src: &SavedBytecode,
    nested: Vec<Arc<Bytecode>>,
    source: Option<Arc<[u8]>>,
) -> Arc<Bytecode> {
    Arc::new(Bytecode {
        code: src.code.iter().map(|raw| Instr::from_raw(*raw)).collect(),
        number_literals: src
            .number_literals
            .iter()
            .map(|bits| f64::from_bits(*bits))
            .collect(),
        string_literals: src.string_literals.clone(),
        table_templates: src.table_templates.clone(),
        global_cache_slots: src.global_cache_slots,
        field_cache_slots: src.field_cache_slots,
        set_field_cache_slots: src.set_field_cache_slots,
        tfor_cursor_slots: src.tfor_cursor_slots,
        num_params: src.num_params,
        num_locals: src.num_locals,
        max_stack: src.max_stack,
        nested,
        upvalues: src.upvalues.clone(),
        is_vararg: src.is_vararg,
        name: src.name.as_deref().map(Arc::from),
        defined_line: src.defined_line,
        source,
        line_info: src.line_info.clone(),
    })
}

struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    #[cfg(test)]
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Seed the output buffer so a large save does not reallocate it
    /// log-many times. The estimate is cheap and deliberately rough; the
    /// buffer still grows normally if it undershoots.
    fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn write_magic_and_versions(&mut self) {
        self.bytes.extend_from_slice(&MAGIC);
        self.write_u16(FORMAT_VERSION);
        self.write_u16(COST_MODEL_VERSION);
    }

    fn write_u8(&mut self, n: u8) {
        self.bytes.push(n);
    }

    fn write_bool(&mut self, b: bool) {
        self.write_u8(u8::from(b));
    }

    fn write_u16(&mut self, n: u16) {
        self.bytes.extend_from_slice(&n.to_le_bytes());
    }

    fn write_u32(&mut self, n: u32) {
        self.bytes.extend_from_slice(&n.to_le_bytes());
    }

    fn write_u64(&mut self, n: u64) {
        self.bytes.extend_from_slice(&n.to_le_bytes());
    }

    fn write_i64(&mut self, n: i64) {
        self.bytes.extend_from_slice(&n.to_le_bytes());
    }

    fn write_len(&mut self, len: usize, collection: SaveCollection) -> Result<(), SaveError> {
        let len = u32::try_from(len).map_err(|_| too_large(collection, len))?;
        self.write_u32(len);
        Ok(())
    }

    fn write_bytes(&mut self, bytes: &[u8], field: SnapshotField) -> Result<(), SaveError> {
        // Every field limit fits the u32 length prefix (asserted where
        // `SnapshotField` is defined), so the limit is the only rejection.
        let len = match u32::try_from(bytes.len()) {
            Ok(len) if bytes.len() <= field.limit() => len,
            _ => {
                return Err(SaveError::FieldSizeExceeded {
                    field,
                    size: bytes.len(),
                    limit: field.limit(),
                });
            }
        };
        self.write_u32(len);
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn write_string(&mut self, s: &str, field: SnapshotField) -> Result<(), SaveError> {
        self.write_bytes(s.as_bytes(), field)
    }

    fn write_option_string(
        &mut self,
        s: Option<&str>,
        field: SnapshotField,
    ) -> Result<(), SaveError> {
        self.write_option_bytes(s.map(str::as_bytes), field)
    }

    /// The same encoding as `write_option_string`: a presence flag, then a
    /// length-prefixed byte field.
    fn write_option_bytes(
        &mut self,
        s: Option<&[u8]>,
        field: SnapshotField,
    ) -> Result<(), SaveError> {
        match s {
            Some(s) => {
                self.write_bool(true);
                self.write_bytes(s, field)?;
            }
            None => self.write_bool(false),
        }
        Ok(())
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn finish(&self) -> Result<(), SnapshotError> {
        if self.pos == self.bytes.len() {
            Ok(())
        } else {
            Err(self.decode_error_at(self.pos, DecodeReason::TrailingBytes))
        }
    }

    fn decode_error_at(&self, offset: usize, reason: DecodeReason) -> SnapshotError {
        SnapshotError::DecodeError { offset, reason }
    }

    /// Bytes not yet consumed. Used to bound speculative allocations so a
    /// corrupt length prefix cannot trigger a huge reservation.
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    fn read_magic_and_versions(&mut self) -> Result<(), SnapshotError> {
        let magic = self.read_exact(4)?;
        if magic != MAGIC {
            return Err(SnapshotError::BadMagic);
        }
        let found = self.read_u16()?;
        if found != FORMAT_VERSION {
            return Err(SnapshotError::UnsupportedVersion {
                found,
                expected: FORMAT_VERSION,
            });
        }
        let found = self.read_u16()?;
        if found != COST_MODEL_VERSION {
            return Err(SnapshotError::UnsupportedCostModelVersion {
                found,
                expected: COST_MODEL_VERSION,
            });
        }
        Ok(())
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8], SnapshotError> {
        let start = self.pos;
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| self.decode_error_at(start, DecodeReason::LengthOverflow))?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| self.decode_error_at(start, DecodeReason::TruncatedPayload))?;
        self.pos = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, SnapshotError> {
        Ok(self.read_exact(1)?[0])
    }

    fn read_bool(&mut self) -> Result<bool, SnapshotError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            byte => {
                Err(self
                    .decode_error_at(self.pos.saturating_sub(1), DecodeReason::InvalidBool(byte)))
            }
        }
    }

    fn read_u16(&mut self) -> Result<u16, SnapshotError> {
        let bytes = self.read_exact(2)?;
        let mut out = [0; 2];
        out.copy_from_slice(bytes);
        Ok(u16::from_le_bytes(out))
    }

    fn read_u32(&mut self) -> Result<u32, SnapshotError> {
        let bytes = self.read_exact(4)?;
        let mut out = [0; 4];
        out.copy_from_slice(bytes);
        Ok(u32::from_le_bytes(out))
    }

    fn read_u64(&mut self) -> Result<u64, SnapshotError> {
        let bytes = self.read_exact(8)?;
        let mut out = [0; 8];
        out.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(out))
    }

    fn read_i64(&mut self) -> Result<i64, SnapshotError> {
        let bytes = self.read_exact(8)?;
        let mut out = [0; 8];
        out.copy_from_slice(bytes);
        Ok(i64::from_le_bytes(out))
    }

    fn read_len(&mut self) -> Result<usize, SnapshotError> {
        Ok(self.read_u32()? as usize)
    }

    fn read_bytes(&mut self, field: SnapshotField) -> Result<Vec<u8>, SnapshotError> {
        self.read_bounded(field)
    }

    fn read_bounded(&mut self, field: SnapshotField) -> Result<Vec<u8>, SnapshotError> {
        let len = self.read_len()?;
        if len > field.limit() {
            if matches!(field, SnapshotField::LuaString) {
                return Err(SnapshotError::StringSizeExceeded {
                    size: len,
                    limit: field.limit(),
                });
            }
            return Err(SnapshotError::FieldSizeExceeded {
                field,
                size: len,
                limit: field.limit(),
            });
        }
        Ok(self.read_exact(len)?.to_vec())
    }

    fn read_lua_string_bytes(&mut self) -> Result<Vec<u8>, SnapshotError> {
        self.read_bounded(SnapshotField::LuaString)
    }

    fn read_string(&mut self, field: SnapshotField) -> Result<String, SnapshotError> {
        let start = self.pos;
        String::from_utf8(self.read_bounded(field)?)
            .map_err(|_| self.decode_error_at(start, DecodeReason::StringNotUtf8))
    }

    fn read_option_string(
        &mut self,
        field: SnapshotField,
    ) -> Result<Option<String>, SnapshotError> {
        if self.read_bool()? {
            Ok(Some(self.read_string(field)?))
        } else {
            Ok(None)
        }
    }

    fn read_option_bytes(
        &mut self,
        field: SnapshotField,
    ) -> Result<Option<Vec<u8>>, SnapshotError> {
        if self.read_bool()? {
            Ok(Some(self.read_bytes(field)?))
        } else {
            Ok(None)
        }
    }
}

impl SavePayload {
    fn encode(&self, out: &mut Encoder) -> Result<(), SaveError> {
        out.write_u8(u8::from(self.has_standard_environment));
        out.write_u64(self.rng_state);
        let (remaining, limit, configured, used) = self.cost.snapshot_fields();
        out.write_i64(remaining);
        out.write_i64(limit);
        out.write_bool(configured);
        out.write_u64(used);
        write_vec(out, SaveCollection::Strings, &self.strings, |out, bytes| {
            out.write_bytes(bytes, SnapshotField::LuaString)
        })?;
        write_vec(out, SaveCollection::Chunks, &self.bytecode, |out, item| {
            item.encode(out)
        })?;
        write_vec(
            out,
            SaveCollection::Upvalues,
            &self.upvalues,
            |out, item| item.encode(out),
        )?;
        write_vec(out, SaveCollection::Objects, &self.objects, |out, item| {
            item.encode(out)
        })?;
        write_vec(
            out,
            SaveCollection::Globals,
            &self.user_globals,
            |out, (name, val)| {
                out.write_string(name, SnapshotField::GlobalName)?;
                val.encode(out)
            },
        )?;
        out.write_bool(self.globals_sealed);
        write_vec(
            out,
            SaveCollection::EnvironmentDeltas,
            &self.env_deltas,
            |out, delta| delta.encode(out),
        )?;
        out.write_u64(self.next_format_pointer_id);
        write_vec(
            out,
            SaveCollection::FormatPointerIds,
            &self.format_pointer_ids,
            |out, (value, id)| {
                value.encode(out)?;
                out.write_u64(*id);
                Ok(())
            },
        )?;
        Ok(())
    }

    fn decode(input: &mut Decoder<'_>) -> Result<Self, SnapshotError> {
        Ok(Self {
            has_standard_environment: input.read_bool()?,
            rng_state: input.read_u64()?,
            cost: {
                let offset = input.pos;
                CostState::from_snapshot(
                    input.read_i64()?,
                    input.read_i64()?,
                    input.read_bool()?,
                    input.read_u64()?,
                )
                .ok_or_else(|| input.decode_error_at(offset, DecodeReason::InconsistentCostMeter))?
            },
            strings: read_vec(input, Decoder::read_lua_string_bytes)?,
            bytecode: read_vec(input, SavedBytecode::decode)?,
            upvalues: read_vec(input, SavedVal::decode)?,
            objects: read_vec(input, SavedObject::decode)?,
            user_globals: read_vec(input, |input| {
                Ok((
                    input.read_string(SnapshotField::GlobalName)?,
                    SavedVal::decode(input)?,
                ))
            })?,
            globals_sealed: input.read_bool()?,
            env_deltas: read_vec(input, SavedEnvDelta::decode)?,
            next_format_pointer_id: input.read_u64()?,
            format_pointer_ids: read_vec(input, |input| {
                Ok((SavedVal::decode(input)?, input.read_u64()?))
            })?,
        })
    }
}

impl SavedEnvDelta {
    fn encode(&self, out: &mut Encoder) -> Result<(), SaveError> {
        out.write_string(&self.token, SnapshotField::EnvironmentToken)?;
        write_vec(
            out,
            SaveCollection::EnvironmentDeletedKeys,
            &self.deleted,
            |out, key| key.encode(out),
        )?;
        write_vec(
            out,
            SaveCollection::EnvironmentUpserts,
            &self.upserts,
            |out, (key, value)| {
                key.encode(out)?;
                value.encode(out)
            },
        )?;
        match &self.order {
            Some(order) => {
                out.write_bool(true);
                write_vec(out, SaveCollection::EnvironmentOrder, order, |out, key| {
                    key.encode(out)
                })?;
            }
            None => out.write_bool(false),
        }
        match &self.array {
            Some(array) => {
                out.write_bool(true);
                write_vec(
                    out,
                    SaveCollection::EnvironmentArray,
                    array,
                    |out, value| value.encode(out),
                )?;
            }
            None => out.write_bool(false),
        }
        self.metatable.encode(out)
    }

    fn decode(input: &mut Decoder<'_>) -> Result<Self, SnapshotError> {
        Ok(Self {
            token: input.read_string(SnapshotField::EnvironmentToken)?,
            deleted: read_vec(input, SavedVal::decode)?,
            upserts: read_vec(input, |input| {
                Ok((SavedVal::decode(input)?, SavedVal::decode(input)?))
            })?,
            order: input
                .read_bool()?
                .then(|| read_vec(input, SavedVal::decode))
                .transpose()?,
            array: input
                .read_bool()?
                .then(|| read_vec(input, SavedVal::decode))
                .transpose()?,
            metatable: SavedMetatableDelta::decode(input)?,
        })
    }
}

impl SavedMetatableDelta {
    fn encode(&self, out: &mut Encoder) -> Result<(), SaveError> {
        match self {
            Self::Unchanged => out.write_u8(0),
            Self::Cleared => out.write_u8(1),
            Self::Set(value) => {
                out.write_u8(2);
                value.encode(out)?;
            }
        }
        Ok(())
    }

    fn decode(input: &mut Decoder<'_>) -> Result<Self, SnapshotError> {
        let tag_offset = input.pos;
        match input.read_u8()? {
            0 => Ok(Self::Unchanged),
            1 => Ok(Self::Cleared),
            2 => Ok(Self::Set(SavedVal::decode(input)?)),
            tag => Err(input.decode_error_at(
                tag_offset,
                DecodeReason::InvalidEnvironmentMetatableMode(tag),
            )),
        }
    }
}

impl SavedVal {
    fn encode(&self, out: &mut Encoder) -> Result<(), SaveError> {
        match self {
            SavedVal::Nil => out.write_u8(0),
            SavedVal::Bool(false) => out.write_u8(1),
            SavedVal::Bool(true) => out.write_u8(2),
            SavedVal::Num(bits) => {
                out.write_u8(3);
                out.write_u64(*bits);
            }
            SavedVal::Str(id) => {
                out.write_u8(4);
                out.write_u32(*id);
            }
            SavedVal::Obj(id) => {
                out.write_u8(5);
                out.write_u32(*id);
            }
            SavedVal::Fn(id) => {
                out.write_u8(6);
                out.write_string(id, SnapshotField::FunctionId)?;
            }
            SavedVal::EnvObj(token) => {
                out.write_u8(7);
                out.write_string(token, SnapshotField::EnvironmentToken)?;
            }
        }
        Ok(())
    }

    fn decode(input: &mut Decoder<'_>) -> Result<Self, SnapshotError> {
        let tag_offset = input.pos;
        match input.read_u8()? {
            0 => Ok(SavedVal::Nil),
            1 => Ok(SavedVal::Bool(false)),
            2 => Ok(SavedVal::Bool(true)),
            3 => Ok(SavedVal::Num(input.read_u64()?)),
            4 => Ok(SavedVal::Str(input.read_u32()?)),
            5 => Ok(SavedVal::Obj(input.read_u32()?)),
            6 => Ok(SavedVal::Fn(input.read_string(SnapshotField::FunctionId)?)),
            7 => Ok(SavedVal::EnvObj(
                input.read_string(SnapshotField::EnvironmentToken)?,
            )),
            tag => Err(input.decode_error_at(tag_offset, DecodeReason::InvalidValueTag(tag))),
        }
    }
}

impl SavedObject {
    fn encode(&self, out: &mut Encoder) -> Result<(), SaveError> {
        match self {
            SavedObject::Table {
                array_len,
                array_slots,
                entries,
                metatable,
            } => {
                out.write_u8(0);
                out.write_u64(*array_len);
                out.write_u32(*array_slots);
                write_vec(
                    out,
                    SaveCollection::TableEntries,
                    entries,
                    |out, (key, value)| {
                        key.encode(out)?;
                        value.encode(out)
                    },
                )?;
                match metatable {
                    Some(mt) => {
                        out.write_bool(true);
                        mt.encode(out)?;
                    }
                    None => out.write_bool(false),
                }
            }
            SavedObject::Closure { chunk, upvalues } => {
                out.write_u8(1);
                out.write_u32(*chunk);
                write_vec(out, SaveCollection::ClosureUpvalues, upvalues, |out, id| {
                    out.write_u32(*id);
                    Ok(())
                })?;
            }
        }
        Ok(())
    }

    fn decode(input: &mut Decoder<'_>) -> Result<Self, SnapshotError> {
        let tag_offset = input.pos;
        match input.read_u8()? {
            0 => {
                let array_len = input.read_u64()?;
                let array_slots = input.read_u32()?;
                let entries = read_vec(input, |input| {
                    Ok((SavedVal::decode(input)?, SavedVal::decode(input)?))
                })?;
                let metatable = if input.read_bool()? {
                    Some(SavedVal::decode(input)?)
                } else {
                    None
                };
                Ok(SavedObject::Table {
                    array_len,
                    array_slots,
                    entries,
                    metatable,
                })
            }
            1 => Ok(SavedObject::Closure {
                chunk: input.read_u32()?,
                upvalues: read_vec(input, Decoder::read_u32)?,
            }),
            tag => Err(input.decode_error_at(tag_offset, DecodeReason::InvalidObjectTag(tag))),
        }
    }
}

impl SavedBytecode {
    fn encode(&self, out: &mut Encoder) -> Result<(), SaveError> {
        write_vec(out, SaveCollection::Instructions, &self.code, |out, raw| {
            out.write_u64(*raw);
            Ok(())
        })?;
        write_vec(
            out,
            SaveCollection::NumberLiterals,
            &self.number_literals,
            |out, bits| {
                out.write_u64(*bits);
                Ok(())
            },
        )?;
        write_vec(
            out,
            SaveCollection::StringLiterals,
            &self.string_literals,
            |out, bytes| out.write_bytes(bytes, SnapshotField::LuaString),
        )?;
        write_vec(
            out,
            SaveCollection::TableTemplates,
            &self.table_templates,
            |out, keys| {
                write_vec(out, SaveCollection::TableTemplateKeys, keys, |out, key| {
                    out.write_u16(*key);
                    Ok(())
                })
            },
        )?;
        out.write_u8(self.global_cache_slots);
        out.write_u8(self.field_cache_slots);
        out.write_u8(self.set_field_cache_slots);
        out.write_u8(self.tfor_cursor_slots);
        out.write_u8(self.num_params);
        out.write_u8(self.num_locals);
        out.write_u16(self.max_stack);
        write_vec(
            out,
            SaveCollection::NestedChunks,
            &self.nested,
            |out, id| {
                out.write_u32(*id);
                Ok(())
            },
        )?;
        write_vec(
            out,
            SaveCollection::UpvalueDescriptors,
            &self.upvalues,
            |out, item| item.encode(out),
        )?;
        out.write_bool(self.is_vararg);
        out.write_option_string(self.name.as_deref(), SnapshotField::ChunkName)?;
        out.write_u32(self.defined_line);
        out.write_option_bytes(self.source.as_deref(), SnapshotField::ChunkSource)?;
        write_vec(
            out,
            SaveCollection::LineInfo,
            &self.line_info,
            |out, line| {
                out.write_u32(*line);
                Ok(())
            },
        )?;
        Ok(())
    }

    fn decode(input: &mut Decoder<'_>) -> Result<Self, SnapshotError> {
        Ok(Self {
            code: read_vec(input, Decoder::read_u64)?,
            number_literals: read_vec(input, Decoder::read_u64)?,
            string_literals: read_vec(input, Decoder::read_lua_string_bytes)?,
            table_templates: read_vec(input, |input| read_vec(input, Decoder::read_u16))?,
            global_cache_slots: input.read_u8()?,
            field_cache_slots: input.read_u8()?,
            set_field_cache_slots: input.read_u8()?,
            tfor_cursor_slots: input.read_u8()?,
            num_params: input.read_u8()?,
            num_locals: input.read_u8()?,
            max_stack: input.read_u16()?,
            nested: read_vec(input, Decoder::read_u32)?,
            upvalues: read_vec(input, SavedUpvalueDesc::decode)?,
            is_vararg: input.read_bool()?,
            name: input.read_option_string(SnapshotField::ChunkName)?,
            defined_line: input.read_u32()?,
            // Chunk names are byte-exact, so unlike function names they are
            // not required to be UTF-8.
            source: input
                .read_option_bytes(SnapshotField::ChunkSource)?
                .map(Arc::<[u8]>::from),
            line_info: read_vec(input, Decoder::read_u32)?,
        })
    }
}

impl SavedUpvalueDesc {
    fn encode(&self, out: &mut Encoder) -> Result<(), SaveError> {
        match self {
            SavedUpvalueDesc::Local(idx) => {
                out.write_u8(0);
                out.write_u8(*idx);
            }
            SavedUpvalueDesc::Upvalue(idx) => {
                out.write_u8(1);
                out.write_u8(*idx);
            }
        }
        Ok(())
    }

    fn decode(input: &mut Decoder<'_>) -> Result<Self, SnapshotError> {
        let tag_offset = input.pos;
        match input.read_u8()? {
            0 => Ok(SavedUpvalueDesc::Local(input.read_u8()?)),
            1 => Ok(SavedUpvalueDesc::Upvalue(input.read_u8()?)),
            tag => Err(input.decode_error_at(tag_offset, DecodeReason::InvalidUpvalueTag(tag))),
        }
    }
}

fn write_vec<T>(
    out: &mut Encoder,
    collection: SaveCollection,
    items: &[T],
    mut write_item: impl FnMut(&mut Encoder, &T) -> Result<(), SaveError>,
) -> Result<(), SaveError> {
    out.write_len(items.len(), collection)?;
    for item in items {
        write_item(out, item)?;
    }
    Ok(())
}

fn decoded_vector_capacity<T>(len: usize, remaining: usize) -> usize {
    len.min(remaining / std::mem::size_of::<T>().max(1))
}

fn read_vec<'a, T>(
    input: &mut Decoder<'a>,
    mut read_item: impl FnMut(&mut Decoder<'a>) -> Result<T, SnapshotError>,
) -> Result<Vec<T>, SnapshotError> {
    let len = input.read_len()?;
    // Cap the initial allocation in bytes, not elements: arena records can
    // contain many Vec headers even when no element has been decoded yet.
    let capacity = decoded_vector_capacity::<T>(len, input.remaining());
    let mut out = Vec::with_capacity(capacity);
    for _ in 0..len {
        out.push(read_item(input)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instr::{ArgCount, RetCount};
    use std::convert::Infallible;

    fn no_setup(_: &mut State) -> std::result::Result<(), Infallible> {
        Ok(())
    }

    #[test]
    fn decoded_vector_reservation_is_bounded_in_bytes() {
        for remaining in [0, 1, 255, 1024, 1 << 20] {
            let capacity = decoded_vector_capacity::<SavedBytecode>(u32::MAX as usize, remaining);
            assert!(capacity * std::mem::size_of::<SavedBytecode>() <= remaining);
        }
        assert_eq!(decoded_vector_capacity::<SavedBytecode>(1, 1 << 20), 1);
        assert_eq!(decoded_vector_capacity::<()>(4, 8), 4);
    }

    #[test]
    fn literal_identity_strings_follow_chunk_ids_instead_of_addresses() {
        let mut state = State::new();
        let program = crate::Program::compile(
            "f = function() return string.format('%p', 'identity-a') end; \
             g = function() return string.format('%p', 'identity-b') end; f(); g()",
            None,
        )
        .unwrap();
        state.load(&program).unwrap();
        state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
        let mut chunks: Vec<_> = state
            .bytecode_caches
            .values()
            .filter(|entry| {
                entry.bytecode.string_literals.iter().any(|literal| {
                    literal.as_slice() == b"identity-a" || literal.as_slice() == b"identity-b"
                })
            })
            .map(|entry| Arc::clone(&entry.bytecode))
            .collect();
        assert_eq!(chunks.len(), 2);
        chunks.sort_unstable_by_key(|chunk| std::cmp::Reverse(Arc::as_ptr(chunk) as usize));
        let expected: Vec<_> = chunks
            .iter()
            .map(|chunk| {
                chunk
                    .string_literals
                    .iter()
                    .find(|literal| {
                        literal.as_slice() == b"identity-a" || literal.as_slice() == b"identity-b"
                    })
                    .unwrap()
                    .clone()
            })
            .collect();
        let mut builder = SaveBuilder::new(&state);
        // Force arena order opposite to address order on every allocator.
        for chunk in &chunks {
            builder.encode_bytecode(chunk).unwrap();
        }
        let payload = builder.finish().unwrap();
        let actual: Vec<_> = payload
            .strings
            .into_iter()
            .filter(|literal| {
                literal.as_slice() == b"identity-a" || literal.as_slice() == b"identity-b"
            })
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn bytecode_nan_literals_are_saved_with_fixed_canonical_bits() {
        let mut canonical = None;
        for bits in [
            0x7ff8_0000_0000_0000,
            0xfff8_0000_0000_0000,
            0x7ff0_0000_0000_0001,
        ] {
            let mut bytecode = valid_saved_bytecode();
            bytecode.code = vec![
                Instr::load_num(0, 0).raw(),
                Instr::ret(0, RetCount::Fixed(1)).raw(),
            ];
            bytecode.line_info = vec![1; bytecode.code.len()];
            bytecode.number_literals = vec![bits];
            let state = load_bytecode_with_globals(
                vec![bytecode],
                vec![SavedObject::Closure {
                    chunk: 0,
                    upvalues: Vec::new(),
                }],
                vec![("f".to_string(), SavedVal::Obj(0))],
            )
            .unwrap();
            let save = state.save_state().unwrap();
            match &canonical {
                Some(bytes) => assert_eq!(&save.bytes, bytes),
                None => canonical = Some(save.bytes.clone()),
            }
            let mut loaded =
                State::load_state(&save.bytes, Box::new(crate::DefaultCallbacks), no_setup)
                    .unwrap();
            loaded.get_global("f").unwrap();
            loaded.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
            assert_eq!(
                loaded.to_number(-1).unwrap().to_bits(),
                0x7ff8_0000_0000_0000
            );
        }
    }

    /// Encode a value, decode it back, and assert round-trip identity with no
    /// trailing bytes left over.
    macro_rules! roundtrip {
        ($ty:ty, $val:expr) => {{
            let value: $ty = $val;
            let mut enc = Encoder::new();
            value.encode(&mut enc).expect("encode");
            let bytes = enc.finish();
            let mut dec = Decoder::new(&bytes);
            let decoded = <$ty>::decode(&mut dec).expect("decode");
            assert_eq!(decoded, value);
            dec.finish().expect("no trailing bytes");
        }};
    }

    #[test]
    fn decode_rejects_inconsistent_legacy_meter_fields() {
        for (remaining, limit, configured, used) in [
            (0, 0, false, 0),
            (11, 10, true, 0),
            (9, 10, true, 0),
            (-1, 0, true, 1),
        ] {
            let mut encoder = Encoder::new();
            encoder.write_bool(false);
            encoder.write_u64(0);
            encoder.write_i64(remaining);
            encoder.write_i64(limit);
            encoder.write_bool(configured);
            encoder.write_u64(used);
            let mut decoder = Decoder::new(&encoder.bytes);
            match SavePayload::decode(&mut decoder) {
                Err(error @ SnapshotError::DecodeError { offset, reason }) => {
                    // The meter fields start after the environment flag and RNG.
                    assert_eq!((offset, reason), (9, DecodeReason::InconsistentCostMeter));
                    assert_eq!(
                        error.to_string(),
                        "save decode error: at byte 9: inconsistent cost meter"
                    );
                }
                _ => panic!("inconsistent meter must fail before decoding the arena"),
            }
        }
    }

    fn valid_saved_bytecode() -> SavedBytecode {
        SavedBytecode {
            code: vec![Instr::ret(0, RetCount::Fixed(0)).raw()],
            number_literals: Vec::new(),
            string_literals: Vec::new(),
            table_templates: Vec::new(),
            global_cache_slots: 0,
            field_cache_slots: 0,
            set_field_cache_slots: 0,
            tfor_cursor_slots: 0,
            num_params: 0,
            num_locals: 0,
            max_stack: 2,
            nested: Vec::new(),
            upvalues: Vec::new(),
            is_vararg: false,
            name: None,
            defined_line: 0,
            source: None,
            line_info: vec![1],
        }
    }

    fn load_bytecode(
        bytecode: Vec<SavedBytecode>,
        objects: Vec<SavedObject>,
    ) -> Result<State, SnapshotError> {
        load_bytecode_with_globals(bytecode, objects, Vec::new())
    }

    fn load_bytecode_with_globals(
        bytecode: Vec<SavedBytecode>,
        objects: Vec<SavedObject>,
        user_globals: Vec<(String, SavedVal)>,
    ) -> Result<State, SnapshotError> {
        let payload = SavePayload {
            has_standard_environment: false,
            rng_state: 0,
            cost: CostState::default(),
            strings: Vec::new(),
            bytecode,
            upvalues: Vec::new(),
            objects,
            user_globals,
            globals_sealed: false,
            env_deltas: Vec::new(),
            next_format_pointer_id: 1,
            format_pointer_ids: Vec::new(),
        };
        let mut encoder = Encoder::new();
        encoder.write_magic_and_versions();
        // The encoder enforces production field limits, so a fixture that
        // fails to encode is a broken test, never a decoder rejection.
        let encoded = encoder
            .write_bytes(
                env!("CARGO_PKG_VERSION").as_bytes(),
                SnapshotField::EngineVersion,
            )
            .and_then(|()| payload.encode(&mut encoder));
        if let Err(error) = encoded {
            panic!("fixture payload must encode: {error}");
        }
        State::load_state(
            &encoder.finish(),
            Box::new(crate::DefaultCallbacks),
            no_setup,
        )
        .map_err(|error| match error {
            LoadError::Snapshot(error) => error,
            other => panic!("fixture load failed outside the snapshot: {other:?}"),
        })
    }

    fn rejected_bytecode(bytecode: Vec<SavedBytecode>, objects: Vec<SavedObject>) -> SnapshotError {
        match load_bytecode(bytecode, objects) {
            Ok(_) => panic!("fixture must be rejected"),
            Err(error) => error,
        }
    }

    fn assert_invalid(error: &SnapshotError, chunk: u32, instruction: Option<u32>) {
        let SnapshotError::InvalidBytecode {
            chunk: actual_chunk,
            instruction: actual_instruction,
            ..
        } = error
        else {
            panic!("fixture must return InvalidBytecode");
        };
        assert_eq!(*actual_chunk, chunk);
        assert_eq!(*actual_instruction, instruction);
    }

    /// A two-register frame running `code`, rejected at `instruction`.
    fn register_rejection(code: Vec<Instr>, instruction: u32) {
        let mut bytecode = valid_saved_bytecode();
        bytecode.code = code.into_iter().map(Instr::raw).collect();
        bytecode.line_info = vec![1; bytecode.code.len()];
        bytecode.string_literals.push(b"field".to_vec());
        bytecode.is_vararg = true;
        assert_invalid(
            &rejected_bytecode(vec![bytecode], Vec::new()),
            0,
            Some(instruction),
        );
    }

    #[test]
    fn verifier_rejects_forged_register_and_open_result_discipline() {
        let ret = Instr::ret(0, RetCount::Fixed(0));
        let call_all = Instr::call(0, ArgCount::Fixed(0), RetCount::All, false);
        // Registers past the frame, singly and as ranges.
        register_rejection(vec![Instr::mov(2, 0), ret], 0);
        register_rejection(vec![Instr::mov(0, 2), ret], 0);
        register_rejection(vec![Instr::ret(1, RetCount::Fixed(2))], 0);
        register_rejection(vec![Instr::load_nil(1, 2), ret], 0);
        register_rejection(vec![Instr::concat(0, 1, 2), ret], 0);
        register_rejection(vec![Instr::self_(1, 0, 0, None), ret], 0);
        register_rejection(vec![Instr::for_prep(0, 0), ret], 0);
        register_rejection(vec![Instr::tfor_call(0, 1, None), ret], 0);
        register_rejection(vec![Instr::vararg(1, RetCount::Fixed(2)), ret], 0);
        register_rejection(
            vec![
                Instr::call(1, ArgCount::Fixed(1), RetCount::Fixed(0), false),
                ret,
            ],
            0,
        );
        register_rejection(vec![Instr::set_list(1, Some(1), 0), ret], 0);
        register_rejection(vec![Instr::set_table(0, 1, 2), ret], 0);
        register_rejection(vec![Instr::init_index(0, 2, 1), ret], 0);
        register_rejection(vec![Instr::close(3), ret], 0);
        // Malformed counts.
        register_rejection(vec![Instr::load_nil(0, 0), ret], 0);
        register_rejection(vec![Instr::concat(0, 0, 1), ret], 0);
        register_rejection(vec![Instr::tfor_call(0, 0, None), ret], 0);
        register_rejection(
            vec![
                Instr::call(0, ArgCount::Fixed(0), RetCount::Fixed(0), false).with_c(300),
                ret,
            ],
            0,
        );
        // Open results: a consumer without its producer, a producer without
        // its consumer, a jump into the consumer, and a consumer reading
        // above where the producer left its values.
        register_rejection(
            vec![
                Instr::call(0, ArgCount::Dynamic, RetCount::Fixed(1), false),
                ret,
            ],
            0,
        );
        register_rejection(vec![Instr::set_list(0, None, 0), ret], 0);
        register_rejection(vec![Instr::ret(0, RetCount::All)], 0);
        register_rejection(vec![call_all, Instr::test_false(0, 0), ret], 0);
        register_rejection(vec![call_all, ret], 0);
        register_rejection(
            vec![Instr::jump(1), call_all, Instr::ret(0, RetCount::All)],
            1,
        );
        register_rejection(vec![call_all, Instr::ret(1, RetCount::All)], 0);
        register_rejection(
            vec![
                Instr::vararg(1, RetCount::All),
                Instr::set_list(1, None, 0),
                ret,
            ],
            0,
        );

        // The frame must hold the parameters and locals, and the open sentinel
        // is never a frame size.
        for (num_locals, max_stack) in [(3, 2), (0, crate::instr::OPEN_COUNT)] {
            let mut bytecode = valid_saved_bytecode();
            bytecode.num_locals = num_locals;
            bytecode.max_stack = max_stack;
            assert_invalid(&rejected_bytecode(vec![bytecode], Vec::new()), 0, None);
        }

        // The shapes the compiler emits are accepted.
        let mut ok = valid_saved_bytecode();
        ok.is_vararg = true;
        ok.code = vec![
            Instr::vararg(1, RetCount::All).raw(),
            Instr::call(0, ArgCount::Dynamic, RetCount::All, false).raw(),
            Instr::ret(0, RetCount::All).raw(),
        ];
        ok.line_info = vec![1; ok.code.len()];
        assert!(load_bytecode(vec![ok], Vec::new()).is_ok());
    }

    #[test]
    fn verifier_rejects_instruction_and_cache_corruption() {
        let mut empty = valid_saved_bytecode();
        empty.code.clear();
        assert!(matches!(
            rejected_bytecode(vec![empty], Vec::new()),
            SnapshotError::InvalidBytecode { .. }
        ));

        let mut no_return = valid_saved_bytecode();
        no_return.code[0] = Instr::load_nil(0, 1).raw();
        assert!(matches!(
            rejected_bytecode(vec![no_return], Vec::new()),
            SnapshotError::InvalidBytecode { .. }
        ));

        let mut bad_jump = valid_saved_bytecode();
        bad_jump.code.insert(0, Instr::jump(1).raw());
        bad_jump.line_info.insert(0, 1);
        assert!(matches!(
            rejected_bytecode(vec![bad_jump], Vec::new()),
            SnapshotError::InvalidBytecode { .. }
        ));

        let mut bad_literal = valid_saved_bytecode();
        bad_literal.code.insert(0, Instr::load_num(0, 0).raw());
        bad_literal.line_info.insert(0, 1);
        assert!(matches!(
            rejected_bytecode(vec![bad_literal], Vec::new()),
            SnapshotError::InvalidBytecode { .. }
        ));

        let mut bad_cache = valid_saved_bytecode();
        bad_cache.string_literals.push(b"x".to_vec());
        bad_cache
            .code
            .insert(0, Instr::get_global(0, 0, Some(1)).raw());
        bad_cache.line_info.insert(0, 1);
        assert!(matches!(
            rejected_bytecode(vec![bad_cache], Vec::new()),
            SnapshotError::InvalidBytecode { .. }
        ));

        let mut uncached_set = valid_saved_bytecode();
        uncached_set.string_literals.push(b"x".to_vec());
        uncached_set
            .code
            .insert(0, Instr::set_global(0, 0, None).raw());
        uncached_set.code.insert(0, Instr::load_nil(0, 1).raw());
        uncached_set.line_info = vec![1; uncached_set.code.len()];
        assert!(load_bytecode(vec![uncached_set], Vec::new()).is_ok());

        let mut valid_set = valid_saved_bytecode();
        valid_set.string_literals.push(b"x".to_vec());
        valid_set.global_cache_slots = 1;
        valid_set
            .code
            .insert(0, Instr::set_global(0, 0, Some(0)).raw());
        valid_set.code.insert(0, Instr::load_nil(0, 1).raw());
        valid_set.line_info = vec![1; valid_set.code.len()];
        assert!(load_bytecode(vec![valid_set], Vec::new()).is_ok());

        let mut out_of_range_set = valid_saved_bytecode();
        out_of_range_set.string_literals.push(b"x".to_vec());
        out_of_range_set.global_cache_slots = 1;
        out_of_range_set
            .code
            .insert(0, Instr::set_global(0, 0, Some(1)).raw());
        out_of_range_set.code.insert(0, Instr::load_nil(0, 1).raw());
        out_of_range_set.line_info = vec![1; out_of_range_set.code.len()];
        assert_invalid(
            &rejected_bytecode(vec![out_of_range_set], Vec::new()),
            0,
            Some(1),
        );

        let mut wrong_order_set = valid_saved_bytecode();
        wrong_order_set.string_literals.push(b"x".to_vec());
        wrong_order_set.global_cache_slots = 2;
        wrong_order_set.code = vec![
            Instr::load_nil(0, 1).raw(),
            Instr::set_global(0, 0, Some(1)).raw(),
            Instr::load_nil(0, 1).raw(),
            Instr::set_global(0, 0, Some(0)).raw(),
            Instr::ret(0, RetCount::Fixed(0)).raw(),
        ];
        wrong_order_set.line_info = vec![1; wrong_order_set.code.len()];
        assert_invalid(
            &rejected_bytecode(vec![wrong_order_set], Vec::new()),
            0,
            Some(1),
        );

        // Forge an out-of-range string id through every opcode that names
        // one, so a future edit cannot drop one of them from its check
        // unnoticed.
        for forged in [
            Instr::load_str(0, 0),
            Instr::get_global(0, 0, None),
            Instr::set_global(0, 0, None),
            Instr::get_field(0, 0, 0, None),
            Instr::self_(0, 0, 0, None),
            Instr::set_field(0, 0, 0, None),
            Instr::init_field_pinned(0, 0, 0, 0),
            Instr::init_field(0, 0, 0),
        ] {
            let mut bad_string = valid_saved_bytecode();
            bad_string.string_literals.clear();
            bad_string.code.insert(0, forged.raw());
            bad_string.line_info.insert(0, 1);
            assert!(
                matches!(
                    rejected_bytecode(vec![bad_string], Vec::new()),
                    SnapshotError::InvalidBytecode { .. }
                ),
                "{forged:?} with an out-of-range string id must be rejected"
            );
        }
    }

    #[test]
    fn forged_closure_free_close_loads_and_executes() {
        let mut bytecode = valid_saved_bytecode();
        bytecode.code = vec![
            Instr::close(0).raw(),
            Instr::ret(0, RetCount::Fixed(0)).raw(),
        ];
        bytecode.line_info = vec![1; bytecode.code.len()];
        let mut state = load_bytecode_with_globals(
            vec![bytecode],
            vec![SavedObject::Closure {
                chunk: 0,
                upvalues: Vec::new(),
            }],
            vec![("writer".to_string(), SavedVal::Obj(0))],
        )
        .expect("forged free-op fixture loads");
        state.get_global("writer").expect("writer exists");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("forged free-op fixture runs");
        assert_eq!(state.cost_used(), 0);

        // Insert before the RETURN rather than replacing it: overwriting the
        // terminal RETURN trips the ends-in-return check first and the
        // reserved-operand arm would go unexercised.
        let mut bad_close = valid_saved_bytecode();
        bad_close.code.insert(0, Instr::close(0).with_b(1).raw());
        bad_close.line_info.insert(0, 1);
        let error = rejected_bytecode(vec![bad_close], Vec::new());
        let SnapshotError::InvalidBytecode { reason, .. } = &error else {
            panic!("malformed close must be InvalidBytecode, got {error:?}");
        };
        assert!(
            matches!(
                reason,
                InvalidBytecodeReason::Validation(
                    crate::compiler::verify::ValidationReason::ReservedBitsNonZero {
                        opcode: Instr::OP_CLOSE,
                        ..
                    }
                )
            ),
            "{reason}"
        );
    }

    #[test]
    fn compiler_saves_contain_no_closure_free_closes() {
        let mut state = State::new();
        crate::Program::compile("writer = function() local x = 0; for i = 1, 3 do do local y = i; x = x + y end end; fixed(1) end", None).and_then(|program| state.load(&program))
            .expect("compiler fixture loads");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("compiler fixture installs writer");
        state.get_global("writer").expect("writer exists");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("undefined fixed call is not relevant after compilation");
        let payload = SaveBuilder::new(&state)
            .finish()
            .expect("failed callback leaves state saveable");
        assert!(payload.bytecode.iter().all(|chunk| {
            chunk
                .code
                .iter()
                .all(|raw| Instr::from_raw(*raw).opcode() != Instr::OP_CLOSE)
        }));
    }

    #[test]
    fn uncached_set_cannot_alias_a_warmed_get_slot() {
        let mut bytecode = valid_saved_bytecode();
        bytecode.code = vec![
            Instr::get_global(0, 0, Some(0)).raw(),
            Instr::load_num(0, 0).raw(),
            // Operand 0 is uncached, never global slot zero.
            Instr::set_global(0, 1, None).raw(),
            Instr::ret(0, RetCount::Fixed(0)).raw(),
        ];
        bytecode.number_literals.push(42.0f64.to_bits());
        bytecode.string_literals = vec![b"x".to_vec(), b"y".to_vec()];
        bytecode.global_cache_slots = 1;
        bytecode.line_info = vec![1; bytecode.code.len()];
        let mut state = load_bytecode_with_globals(
            vec![bytecode],
            vec![SavedObject::Closure {
                chunk: 0,
                upvalues: Vec::new(),
            }],
            vec![
                ("x".to_string(), SavedVal::Num(1.0f64.to_bits())),
                ("y".to_string(), SavedVal::Num(2.0f64.to_bits())),
                ("writer".to_string(), SavedVal::Obj(0)),
            ],
        )
        .expect("uncached-set fixture loads");

        for _ in 0..2 {
            state.get_global("writer").expect("writer exists");
            state
                .call(crate::ArgCount::Fixed(0), crate::RetCount::Fixed(0))
                .expect("writer runs");
        }
        state.get_global("x").expect("x exists");
        assert_eq!(state.to_number(-1).expect("x is numeric"), 1.0);
        state.pop(1).expect("x pops");
        state.get_global("y").expect("y exists");
        assert_eq!(state.to_number(-1).expect("y is numeric"), 42.0);
    }

    #[test]
    fn forged_tfor_cursor_operands_validate_against_the_declared_count() {
        let mut uncached = valid_saved_bytecode();
        uncached.code = vec![
            Instr::tfor_call(0, 1, None).raw(),
            Instr::ret(0, RetCount::Fixed(0)).raw(),
        ];
        uncached.num_locals = 4;
        uncached.max_stack = 4;
        uncached.line_info = vec![1; uncached.code.len()];
        assert!(load_bytecode(vec![uncached.clone()], Vec::new()).is_ok());

        let mut sequential = uncached.clone();
        sequential.code = vec![
            Instr::tfor_call(0, 1, Some(0)).raw(),
            Instr::tfor_call(0, 1, Some(1)).raw(),
            Instr::ret(0, RetCount::Fixed(0)).raw(),
        ];
        sequential.tfor_cursor_slots = 2;
        sequential.line_info = vec![1; sequential.code.len()];
        assert!(load_bytecode(vec![sequential.clone()], Vec::new()).is_ok());
        let mut restored = load_bytecode_with_globals(
            vec![sequential.clone()],
            vec![SavedObject::Closure {
                chunk: 0,
                upvalues: Vec::new(),
            }],
            vec![("writer".to_string(), SavedVal::Obj(0))],
        )
        .expect("cached cursor fixture loads");
        restored.get_global("writer").expect("writer exists");
        let closure = restored
            .pop_val()
            .as_lua_function(&restored.heap)
            .expect("writer is a closure");
        assert_eq!(closure.runtime.caches.tfor_cursor.len(), 2);
        assert!(
            closure
                .runtime
                .caches
                .tfor_cursor
                .iter()
                .all(|slot| slot.get().is_none())
        );
        roundtrip!(SavedBytecode, sequential.clone());

        // A declared count that disagrees with the operands, either way.
        for declared in [0, 1, 3] {
            let mut miscounted = sequential.clone();
            miscounted.tfor_cursor_slots = declared;
            assert_invalid(&rejected_bytecode(vec![miscounted], Vec::new()), 0, None);
        }

        for (code, declared) in [
            (vec![Some(1)], 1),
            (vec![Some(254)], 1),
            (vec![Some(0), Some(0)], 1),
            (vec![Some(1), Some(0)], 2),
        ] {
            let mut forged = uncached.clone();
            forged.code = code
                .into_iter()
                .map(|slot| Instr::tfor_call(0, 1, slot).raw())
                .chain([Instr::ret(0, RetCount::Fixed(0)).raw()])
                .collect();
            forged.tfor_cursor_slots = declared;
            forged.line_info = vec![1; forged.code.len()];
            assert!(matches!(
                rejected_bytecode(vec![forged], Vec::new()),
                SnapshotError::InvalidBytecode { .. }
            ));
        }
    }

    #[test]
    fn forged_builtin_set_global_never_populates_a_cache_slot() {
        let mut bytecode = valid_saved_bytecode();
        bytecode.code = vec![
            Instr::load_num(0, 0).raw(),
            Instr::set_global(0, 0, Some(0)).raw(),
            Instr::ret(0, RetCount::Fixed(0)).raw(),
        ];
        bytecode.number_literals.push(7.0f64.to_bits());
        bytecode.string_literals.push(b"table".to_vec());
        bytecode.global_cache_slots = 1;
        bytecode.line_info = vec![1; bytecode.code.len()];
        let mut state = load_bytecode_with_globals(
            vec![bytecode],
            vec![SavedObject::Closure {
                chunk: 0,
                upvalues: Vec::new(),
            }],
            vec![("writer".to_string(), SavedVal::Obj(0))],
        )
        .expect("forged builtin fixture loads");

        state.get_global("writer").expect("writer exists");
        let writer = state.pop_val();
        let closure = writer
            .as_lua_function(&state.heap)
            .expect("writer is a closure");
        let runtime = Arc::clone(&closure.runtime);
        state.push_val(writer).expect("writer fits on stack");
        state
            .call(crate::ArgCount::Fixed(0), crate::RetCount::Fixed(0))
            .expect("writer runs");
        assert!(runtime.caches.global_lookup[0].get().is_none());
    }

    #[test]
    fn restored_same_bytecode_closures_share_one_runtime() {
        // The no-GC resolver must give every restored closure shell of one
        // materialized Bytecode the same runtime bundle - not a cold copy
        // each - or a loaded State pays per-closure caches all over again.
        let mut original = State::new();
        crate::Program::compile(
            "function make(n) return function() return n end end \
                 a = make(1) b = make(2)",
            None,
        )
        .and_then(|program| original.load(&program))
        .expect("factory source compiles");
        original
            .call(crate::ArgCount::Fixed(0), crate::RetCount::Fixed(0))
            .expect("factory source runs");
        let save = original.save_state().expect("state saves");
        let mut loaded =
            State::load_state(&save.bytes, Box::new(crate::DefaultCallbacks), no_setup)
                .expect("state loads");
        loaded.get_global("a").expect("a exists");
        let a = loaded
            .pop_val()
            .as_lua_function(&loaded.heap)
            .expect("a is a closure");
        loaded.get_global("b").expect("b exists");
        let b = loaded
            .pop_val()
            .as_lua_function(&loaded.heap)
            .expect("b is a closure");
        assert!(Arc::ptr_eq(&a.runtime, &b.runtime));
    }

    #[test]
    fn decoder_rejects_oversized_string_literal() {
        // Bytecode literals intern during materialization now that the
        // per-Bytecode literal cache exists (through the no-GC resolver,
        // which cannot surface a size error), and the per-call size
        // backstop is gone with it. The decoder's read_lua_string_bytes
        // cap is therefore the load path's ONLY guard against a forged
        // oversized literal - this pins it so it cannot be relaxed without
        // reintroducing the unbounded-allocation hole. (Compile-time literals
        // are capped by the parser; raw test bytecode is prevalidated by
        // resolve_bytecode_runtime.)
        //
        // The encoder refuses oversized fields, so the forged save is built by
        // encoding a marker literal and splicing raw bytes over it.
        let limit = crate::limits::MAX_STRING_BYTES;
        let oversized = vec![b'a'; limit + 1];
        let mut with_body = forged_literal_save(&oversized, true);
        assert!(matches!(
            State::load_state(&with_body, Box::new(crate::DefaultCallbacks), no_setup),
            Err(LoadError::Snapshot(SnapshotError::StringSizeExceeded { size, limit: reported }))
                if size == limit + 1 && reported == limit
        ));
        // The cap is checked before the body is read: a forged prefix with no
        // following bytes still reports the size, not truncation.
        with_body = forged_literal_save(&oversized, false);
        assert!(matches!(
            State::load_state(&with_body, Box::new(crate::DefaultCallbacks), no_setup),
            Err(LoadError::Snapshot(
                SnapshotError::StringSizeExceeded { .. }
            ))
        ));
    }

    /// A valid one-chunk save whose single string literal is replaced, in the
    /// raw bytes, by a length prefix for `literal` and (if `body`) its bytes.
    fn forged_literal_save(literal: &[u8], body: bool) -> Vec<u8> {
        const MARKER: &[u8] = b"forged-literal-marker";
        let mut bytecode = valid_saved_bytecode();
        bytecode.string_literals.push(MARKER.to_vec());
        let payload = SavePayload {
            has_standard_environment: false,
            rng_state: 0,
            cost: CostState::default(),
            strings: Vec::new(),
            bytecode: vec![bytecode],
            upvalues: Vec::new(),
            objects: Vec::new(),
            user_globals: Vec::new(),
            globals_sealed: false,
            env_deltas: Vec::new(),
            next_format_pointer_id: 1,
            format_pointer_ids: Vec::new(),
        };
        let mut encoder = Encoder::new();
        encoder.write_magic_and_versions();
        encoder
            .write_bytes(
                env!("CARGO_PKG_VERSION").as_bytes(),
                SnapshotField::EngineVersion,
            )
            .expect("engine version fits");
        payload
            .encode(&mut encoder)
            .expect("marker payload encodes");
        let bytes = encoder.finish();
        let hits: Vec<usize> = bytes
            .windows(MARKER.len())
            .enumerate()
            .filter(|(_, window)| *window == MARKER)
            .map(|(at, _)| at)
            .collect();
        assert_eq!(hits.len(), 1, "marker must occur exactly once");
        let at = hits[0];
        let prefix = at - 4;
        let marker_len = u32::try_from(MARKER.len()).expect("marker length fits u32");
        assert_eq!(bytes[prefix..at], marker_len.to_le_bytes());
        let mut forged = bytes[..prefix].to_vec();
        let literal_len = u32::try_from(literal.len()).expect("literal length fits u32");
        forged.extend_from_slice(&literal_len.to_le_bytes());
        if body {
            forged.extend_from_slice(literal);
            forged.extend_from_slice(&bytes[at + MARKER.len()..]);
        }
        forged
    }

    #[test]
    fn encoder_enforces_field_limits() {
        let limit = SnapshotField::ChunkSource.limit();
        let mut encoder = Encoder::new();
        encoder
            .write_bytes(&vec![b's'; limit], SnapshotField::ChunkSource)
            .expect("a field at its limit encodes");
        assert_eq!(
            encoder.write_bytes(&vec![b's'; limit + 1], SnapshotField::ChunkSource),
            Err(SaveError::FieldSizeExceeded {
                field: SnapshotField::ChunkSource,
                size: limit + 1,
                limit,
            })
        );
    }

    #[test]
    fn verifier_rejects_bad_graphs_and_closure_captures() {
        let mut missing_child = valid_saved_bytecode();
        missing_child.nested.push(1);
        assert_eq!(
            rejected_bytecode(vec![missing_child], Vec::new()),
            SnapshotError::CorruptArena {
                reason: CorruptionReason::IdOutOfRange(SavedReference::NestedChunk),
                index: 1,
                location: SaveLocation::Bytecode,
            }
        );

        let mut cycle = valid_saved_bytecode();
        cycle.nested.push(0);
        assert!(matches!(
            rejected_bytecode(vec![cycle], Vec::new()),
            SnapshotError::InvalidBytecode { .. }
        ));

        let mut chunks = vec![valid_saved_bytecode(); 201];
        for (idx, chunk) in chunks.iter_mut().take(200).enumerate() {
            chunk.nested.push((idx + 1) as u32);
        }
        assert!(matches!(
            rejected_bytecode(chunks, Vec::new()),
            SnapshotError::InvalidBytecode { .. }
        ));

        let chunk = valid_saved_bytecode();
        let object = SavedObject::Closure {
            chunk: 0,
            upvalues: vec![0],
        };
        assert!(matches!(
            rejected_bytecode(vec![chunk], vec![object]),
            SnapshotError::InvalidBytecode { .. }
        ));

        let mut boundary = vec![valid_saved_bytecode(); 200];
        for (idx, chunk) in boundary.iter_mut().take(199).enumerate() {
            chunk
                .nested
                .push(u32::try_from(idx + 1).expect("fixture id fits"));
        }
        assert!(load_bytecode(boundary, Vec::new()).is_ok());
    }

    #[test]
    fn verifier_covers_each_operand_class_and_reports_its_location() {
        let mut binary_literal = valid_saved_bytecode();
        binary_literal.string_literals.push(vec![255]);
        binary_literal.code.insert(0, Instr::load_str(0, 0).raw());
        binary_literal.line_info.insert(0, 1);
        assert!(load_bytecode(vec![binary_literal], Vec::new()).is_ok());

        let mut bad_global_name = valid_saved_bytecode();
        bad_global_name.string_literals.push(vec![255]);
        bad_global_name
            .code
            .insert(0, Instr::get_global(0, 0, Some(0)).raw());
        bad_global_name.global_cache_slots = 1;
        bad_global_name.line_info.insert(0, 1);
        assert_invalid(
            &rejected_bytecode(vec![bad_global_name], Vec::new()),
            0,
            Some(0),
        );

        let mut bad_builtin = valid_saved_bytecode();
        bad_builtin.code.insert(
            0,
            Instr::get_builtin(0, crate::instr::Builtin::Print)
                .with_b(255)
                .raw(),
        );
        bad_builtin.line_info.insert(0, 1);
        assert_invalid(
            &rejected_bytecode(vec![bad_builtin], Vec::new()),
            0,
            Some(0),
        );

        let mut bad_template = valid_saved_bytecode();
        bad_template
            .code
            .insert(0, Instr::new_table_template(0, 0).raw());
        bad_template.line_info.insert(0, 1);
        assert_invalid(
            &rejected_bytecode(vec![bad_template], Vec::new()),
            0,
            Some(0),
        );

        let mut bad_template_key = valid_saved_bytecode();
        bad_template_key.table_templates.push(vec![u16::MAX]);
        assert_invalid(
            &rejected_bytecode(vec![bad_template_key], Vec::new()),
            0,
            None,
        );

        // Template keys must be distinct by content: a repeated id and two
        // equal-content literals both collapse to one runtime key.
        for duplicate in [[0u16, 0], [0, 1]] {
            let mut duplicate_template_key = valid_saved_bytecode();
            duplicate_template_key.string_literals = vec![b"k".to_vec(), b"k".to_vec()];
            duplicate_template_key
                .table_templates
                .push(duplicate.to_vec());
            assert_invalid(
                &rejected_bytecode(vec![duplicate_template_key], Vec::new()),
                0,
                None,
            );
        }

        let mut bad_register = valid_saved_bytecode();
        bad_register.code.insert(0, Instr::mov(0, 2).raw());
        bad_register.line_info.insert(0, 1);
        assert_invalid(
            &rejected_bytecode(vec![bad_register], Vec::new()),
            0,
            Some(0),
        );

        let mut bad_upvalue = valid_saved_bytecode();
        bad_upvalue.code.insert(0, Instr::get_upval(0, 0).raw());
        bad_upvalue.line_info.insert(0, 1);
        assert_invalid(
            &rejected_bytecode(vec![bad_upvalue], Vec::new()),
            0,
            Some(0),
        );

        let mut bad_reserved = valid_saved_bytecode();
        bad_reserved.code[0] = Instr::ret(0, RetCount::Fixed(0)).with_c(1).raw();
        assert_invalid(
            &rejected_bytecode(vec![bad_reserved], Vec::new()),
            0,
            Some(0),
        );

        let mut unknown = valid_saved_bytecode();
        unknown.code[0] = u64::MAX;
        assert_invalid(&rejected_bytecode(vec![unknown], Vec::new()), 0, Some(0));
    }

    #[test]
    fn verifier_covers_cache_metadata_nested_chunks_and_graph_cycles() {
        let mut bad_cache_count = valid_saved_bytecode();
        bad_cache_count.string_literals.push(b"x".to_vec());
        bad_cache_count
            .code
            .insert(0, Instr::get_global(0, 0, Some(0)).raw());
        bad_cache_count.line_info.insert(0, 1);
        assert_invalid(
            &rejected_bytecode(vec![bad_cache_count], Vec::new()),
            0,
            None,
        );

        let mut bad_line_info = valid_saved_bytecode();
        bad_line_info.code.insert(0, Instr::load_nil(0, 1).raw());
        assert_invalid(&rejected_bytecode(vec![bad_line_info], Vec::new()), 0, None);

        let mut negative_jump = valid_saved_bytecode();
        negative_jump.code.insert(0, Instr::jump(-2).raw());
        negative_jump.line_info.insert(0, 1);
        assert_invalid(
            &rejected_bytecode(vec![negative_jump], Vec::new()),
            0,
            Some(0),
        );

        let mut parent = valid_saved_bytecode();
        parent.nested.push(1);
        let mut child = valid_saved_bytecode();
        child.upvalues.push(SavedUpvalueDesc::Local(0));
        assert_invalid(&rejected_bytecode(vec![parent, child], Vec::new()), 0, None);

        let mut first = valid_saved_bytecode();
        first.nested.push(1);
        let mut second = valid_saved_bytecode();
        second.nested.push(0);
        assert_invalid(&rejected_bytecode(vec![first, second], Vec::new()), 0, None);

        let mut deep_self = valid_saved_bytecode();
        deep_self.nested.push(0);
        assert_invalid(&rejected_bytecode(vec![deep_self], Vec::new()), 0, None);

        let mut nested_bad_opcode = valid_saved_bytecode();
        nested_bad_opcode.code[0] = u64::MAX;
        assert_invalid(
            &rejected_bytecode(vec![valid_saved_bytecode(), nested_bad_opcode], Vec::new()),
            1,
            Some(0),
        );
    }

    fn chunk_chain(depth: u32) -> Vec<SavedBytecode> {
        let mut chain = Vec::with_capacity(depth as usize);
        for idx in 0..depth {
            let mut chunk = valid_saved_bytecode();
            if idx + 1 < depth {
                chunk.nested.push(idx + 1);
            }
            chain.push(chunk);
        }
        chain
    }

    #[test]
    fn nested_chunk_depth_is_capped_at_the_parser_limit() {
        let max_depth = u32::try_from(crate::limits::MAX_CHUNK_NESTING)
            .expect("the chunk nesting limit fits a saved chunk id");

        // The deepest chain the verifier admits materializes fine; one deeper
        // is rejected before materialization. Materialization itself walks an
        // explicit stack, so it does not depend on this cap for Rust-stack
        // safety - the two layers are independent.
        load_bytecode(
            chunk_chain(max_depth),
            vec![SavedObject::Closure {
                chunk: 0,
                upvalues: Vec::new(),
            }],
        )
        .expect("a chain at the parser depth limit is valid save data");

        assert_invalid(
            &rejected_bytecode(chunk_chain(max_depth + 1), Vec::new()),
            max_depth,
            None,
        );
    }

    #[test]
    fn fused_and_folded_closures_round_trip_through_save_load() {
        // A parser-emitted closure carrying fused compare-and-branch opcodes
        // and folded literals must pass the load-side verifier and run.
        let mut state = State::new();
        crate::Program::compile("function count(n)\n  local i = 0\n  while i < n do i = i + 1 end\n  if i == n then return i + 60 * 1000 end\n  return -1\nend", None).and_then(|program| state.load(&program))
            .expect("script compiles");
        state
            .call(crate::ArgCount::Fixed(0), crate::RetCount::Fixed(0))
            .expect("chunk runs");
        let save = state.save_state().expect("state saves");
        let mut loaded =
            State::load_state(&save.bytes, Box::new(crate::DefaultCallbacks), no_setup)
                .expect("save containing fused opcodes loads");
        loaded.get_global("count").expect("count exists");
        loaded.push_number(5.0).expect("argument fits");
        loaded
            .call(crate::ArgCount::Fixed(1), crate::RetCount::Fixed(1))
            .expect("loaded count runs");
        assert_eq!(loaded.to_number(-1).expect("numeric result"), 60005.0);
    }

    #[test]
    fn reused_upvalue_pool_slots_round_trip_through_save_load() {
        // Force the pool sweep to free slots, then allocate closures that
        // reuse them, and prove the save/load identity mapping (keyed on
        // slot index) still round-trips the reused slots' values.
        let mut state = State::new();
        crate::Program::compile("do\n  local dead = 1\n  local f = function() dead = dead + 1 end\nend\ncollect_marker = 1", None).and_then(|program| state.load(&program))
            .expect("dead-closure script compiles");
        state
            .call(crate::ArgCount::Fixed(0), crate::RetCount::Fixed(0))
            .expect("dead-closure chunk runs");
        state.gc_collect();
        crate::Program::compile(
            "local n = 41\nbump = function() n = n + 1 return n end\nbump()",
            None,
        )
        .and_then(|program| state.load(&program))
        .expect("live-closure script compiles");
        state
            .call(crate::ArgCount::Fixed(0), crate::RetCount::Fixed(0))
            .expect("live-closure chunk runs");
        let save = state.save_state().expect("state saves after slot reuse");
        let mut loaded =
            State::load_state(&save.bytes, Box::new(crate::DefaultCallbacks), no_setup)
                .expect("save loads");
        loaded.get_global("bump").expect("bump exists");
        loaded
            .call(crate::ArgCount::Fixed(0), crate::RetCount::Fixed(1))
            .expect("loaded bump runs");
        assert_eq!(loaded.to_number(-1).expect("numeric result"), 43.0);
    }

    #[test]
    fn forged_compare_branches_validate_registers_and_targets() {
        use crate::instr::CompareOp;
        let ret = Instr::ret(0, RetCount::Fixed(0)).raw();

        // An operand register past the frame.
        let mut beyond = valid_saved_bytecode();
        beyond.code = vec![Instr::branch_compare(CompareOp::Lt, 0, 2, 0).raw(), ret];
        beyond.line_info = vec![1; beyond.code.len()];
        assert_invalid(&rejected_bytecode(vec![beyond], Vec::new()), 0, Some(0));

        // Jump target past the end of the code.
        let mut oob = valid_saved_bytecode();
        oob.code = vec![
            Instr::load_nil(0, 2).raw(),
            Instr::branch_compare(CompareOp::Eq, 0, 1, 5).raw(),
            ret,
        ];
        oob.line_info = vec![1; oob.code.len()];
        assert_invalid(&rejected_bytecode(vec![oob], Vec::new()), 0, Some(1));

        // The well-formed shape is accepted.
        let mut ok = valid_saved_bytecode();
        ok.code = vec![
            Instr::load_nil(0, 2).raw(),
            Instr::branch_compare(CompareOp::Ne, 0, 1, 0).raw(),
            ret,
        ];
        ok.line_info = vec![1; ok.code.len()];
        assert!(load_bytecode(vec![ok], Vec::new()).is_ok());
    }

    #[test]
    fn saved_val_variants_round_trip() {
        roundtrip!(SavedVal, SavedVal::Nil);
        roundtrip!(SavedVal, SavedVal::Bool(false));
        roundtrip!(SavedVal, SavedVal::Bool(true));
        roundtrip!(SavedVal, SavedVal::Num(0.0f64.to_bits()));
        roundtrip!(SavedVal, SavedVal::Num((-0.0f64).to_bits()));
        roundtrip!(SavedVal, SavedVal::Num(f64::NAN.to_bits()));
        roundtrip!(SavedVal, SavedVal::Str(7));
        roundtrip!(SavedVal, SavedVal::Obj(42));
        roundtrip!(SavedVal, SavedVal::Fn("math.sin".to_string()));
        roundtrip!(SavedVal, SavedVal::EnvObj("math".to_string()));
    }

    #[test]
    fn saved_upvalue_desc_round_trips() {
        roundtrip!(SavedUpvalueDesc, SavedUpvalueDesc::Local(3));
        roundtrip!(SavedUpvalueDesc, SavedUpvalueDesc::Upvalue(9));
    }

    #[test]
    fn saved_object_variants_round_trip() {
        roundtrip!(
            SavedObject,
            SavedObject::Table {
                array_len: 1 << 40,
                array_slots: 1,
                entries: vec![
                    (SavedVal::Num(1.0f64.to_bits()), SavedVal::Nil),
                    (SavedVal::Str(0), SavedVal::Num(1.0f64.to_bits())),
                    (SavedVal::Obj(1), SavedVal::Bool(true)),
                ],
                metatable: None,
            }
        );
        roundtrip!(
            SavedObject,
            SavedObject::Table {
                array_len: 0,
                array_slots: 0,
                entries: vec![],
                metatable: Some(SavedVal::Obj(4)),
            }
        );
        roundtrip!(
            SavedObject,
            SavedObject::Table {
                array_len: 0,
                array_slots: 0,
                entries: vec![(SavedVal::Str(2), SavedVal::Nil)],
                metatable: Some(SavedVal::EnvObj("string".to_string())),
            }
        );
        roundtrip!(
            SavedObject,
            SavedObject::Closure {
                chunk: 5,
                upvalues: vec![0, 1, 2],
            }
        );
    }

    #[test]
    fn saved_environment_delta_and_pointer_ids_round_trip() {
        roundtrip!(
            SavedEnvDelta,
            SavedEnvDelta {
                token: "math".to_string(),
                deleted: vec![SavedVal::Str(1)],
                upserts: vec![(SavedVal::Str(2), SavedVal::Obj(3))],
                order: Some(vec![SavedVal::Str(2)]),
                array: Some(vec![SavedVal::Nil, SavedVal::Bool(true)]),
                metatable: SavedMetatableDelta::Set(SavedVal::EnvObj("table".to_string())),
            }
        );
        roundtrip!(SavedMetatableDelta, SavedMetatableDelta::Cleared);
    }

    #[test]
    fn saved_main_chunk_keeps_zero_defined_line() {
        roundtrip!(SavedBytecode, valid_saved_bytecode());
    }

    #[test]
    fn saved_bytecode_round_trips() {
        roundtrip!(
            SavedBytecode,
            SavedBytecode {
                code: vec![1, 2, 3, 0xdead_beef_0bad_f00d],
                number_literals: vec![1.5f64.to_bits(), f64::NAN.to_bits()],
                string_literals: vec![b"hi".to_vec(), vec![0xff, 0x00, 0xfe]],
                table_templates: vec![vec![0, 1], vec![]],
                global_cache_slots: 7,
                field_cache_slots: 9,
                set_field_cache_slots: 3,
                tfor_cursor_slots: 4,
                num_params: 2,
                num_locals: 5,
                max_stack: 300,
                nested: vec![0, 1, 2],
                upvalues: vec![SavedUpvalueDesc::Local(1), SavedUpvalueDesc::Upvalue(2)],
                is_vararg: true,
                name: Some("f".to_string()),
                defined_line: 9,
                source: None,
                line_info: vec![10, 11, 12],
            }
        );
    }

    #[test]
    fn save_walker_object_count_matches_gc_live_minus_env() {
        let mut state = State::new();
        crate::Program::compile(
            r#"
                a = { 1, 2, 3 }
                a.b = {}
                a.b.back = a
                keep = function() return a end
                m = math
                junk = { x = {} }
                junk = nil
            "#,
            None,
        )
        .and_then(|program| state.load(&program))
        .expect("compile");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("run");
        state.gc_collect();

        let payload = SaveBuilder::new(&state).finish().expect("walk");
        let env_count = build_env_reverse(&state).len();

        // Every live object is either an env object (tokenized, excluded from
        // the payload) or a user object the walker materialized. Adding a GC
        // root without teaching the save walker would break this identity.
        assert_eq!(payload.objects.len() + env_count, state.object_count());
    }
}
