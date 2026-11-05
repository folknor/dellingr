//! Snapshot / restore of a quiescent VM (the `snapshot` feature).
//!
//! [`State::save_state`] serializes the persistent script world - user globals,
//! the reachable table/closure/upvalue/string graph, the RNG stream, and the
//! costing with its counters - to a versioned deterministic binary blob; [`State::load_state`]
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
//! - **RustFns need stable ids.** A `Val::RustFn` is an entry of the State's
//!   Rust function table, never a code address (see `vm/rust_fn.rs`), so a
//!   reachable one is saved as its entry's registered id (see
//!   [`State::register_rust_fn`]) and the save fails fast if the entry has
//!   none. The stdlib's catalog entries get their ids under dotted paths
//!   (`math.sin`, ...) during `open_libs`. An id names one entry, so no build
//!   difference (optimizer or linker merging of identical functions) can
//!   change which id a value saves as.
//! - **Environment objects keep their identity.** `math`/`string`/`table`,
//!   `_G`'s metatable and the shared string metatable are reconstructed by
//!   `open_libs` on load and referenced by token. The object->token map is
//!   the environment-table record each successful `open_libs` refreshes
//!   (`GlobalEnv::env_tables`), not read from the live builtin bindings, so
//!   shadowing a builtin global (`math = {}`) can't misclassify the user's
//!   table as an environment object.
//! - **Every environment table is persisted whole, by one rule.** The global
//!   table (permanent token `_G`, a raw State has one too) and each
//!   environment table save every slot, tombstones included, their
//!   metatable and the pins sealing recorded; `GlobalEnv::restore` restores
//!   each by the same rule (`restore_environment_table`): the save is
//!   authoritative for every key it holds, and load setup contributes only
//!   string keys it mentions nowhere. Environment tables therefore keep the
//!   global table's slot policy and seal at run time too (`EnvTable`). An
//!   unshadowed builtin or stock library function encodes to a cheap
//!   `EnvObj`/`Fn` token; a *shadow* (`print = ...`, `math.floor = ...`) is
//!   preserved as the user's value. A later build adding `math.foo` does not
//!   corrupt old saves: setup contributes the unmentioned key. A build that
//!   drops a stock function cannot load a save naming it, as for any other
//!   registered function id. An earlier design saved library tables as
//!   deltas against the freshly opened library and let setup values win for
//!   unchanged keys; whether a setup value survived then depended on
//!   unrelated edits to the same table (any slot change wrote every slot),
//!   and a delta against the pristine library cannot tell "never changed"
//!   from "restored to the pristine value after initialization changed it",
//!   so a load could not continue the saving State exactly.
//! - **In-crate codec.** Little-endian, length-prefixed, `f64` by bit pattern
//!   (NaNs canonicalized, -0.0 exact), explicit enum tags, bounds-checked with
//!   initial vector reservations capped in bytes at remaining input. No
//!   `serde`/`bincode`. Two saves of the same logical state written by the
//!   same crate version are byte-identical, whatever profile built it: nothing
//!   in the payload depends on addresses or build options. The header carries
//!   the diagnostic crate-version string, so saves of one state by two crate
//!   versions differ in their header even when the payload is the same. A
//!   host that content-hashes saves across upgrades must hash the payload
//!   after the header, or accept that a version bump changes the bytes.

use std::borrow::Cow;
use std::sync::Arc;

use std::collections::{BTreeMap, BTreeSet};

use crate::det_hash::{IndexMap as DetMap, map_with_capacity};

use super::RustFnId;
use super::global_env::{EnvRole, SavedEnvTable};
use super::inventory::GcRoot;
use super::object::{GcHeap, RawObject, StringPtr, Upvalue, UpvalueRef};
use super::rng::VmRng;
use super::table::{InvalidTableKey, NormalizedTableKey, TableKeyInput, normalize_table_key};
use super::{ObjectPtr, PointerIdKey, State, Val};
use crate::COST_MODEL_VERSION;
use crate::compiler::{Bytecode, UpvalueDesc};
use crate::cost_meter::CostState;
use crate::cost_model::{Charge, CostModel, Costing};
use crate::host::HostCallbacks;
use crate::instr::Instr;
use crate::instr::snapshot_format::FORMAT_VERSION;

mod error;
#[cfg(test)]
mod integrity_tests;
mod verify;

use error::SaveFault;
pub use error::{
    CorruptionReason, DecodeReason, DuplicateEntry, GmatchCorruption, InvalidBytecodeReason,
    KeyForm, LoadError, MaterializedPart, SaveCollection, SaveError, SaveInternalError,
    SavedReference, SnapshotError, SnapshotField, TableCorruption, TableLoadLocation,
    TableRestoreFailure,
};

/// The global table's token. Unlike the environment tables it is permanent
/// (a raw State has a global table too), so it is not in
/// `GlobalEnv::env_tables`: the save walker and loader add it themselves, and
/// its contents travel in the payload's `globals` section.
pub(super) const GLOBALS_TOKEN: &str = "_G";
const GLOBAL_METATABLE_TOKEN: &str = "_G.metatable";
const STRING_METATABLE_TOKEN: &str = "string.metatable";

/// The token an environment table saves under. Token spellings belong to the
/// snapshot schema, not the global-name API.
fn env_token(role: EnvRole) -> &'static str {
    match role {
        EnvRole::Math => "math",
        EnvRole::String => "string",
        EnvRole::Table => "table",
        EnvRole::GlobalMetatable => GLOBAL_METATABLE_TOKEN,
        EnvRole::StringMetatable => STRING_METATABLE_TOKEN,
    }
}

/// Object -> token for the environment tables (not the global table).
pub(super) fn environment_tokens(state: &State) -> BTreeMap<ObjectPtr, &'static str> {
    state
        .global_env
        .env_tables()
        .iter()
        .map(|env| (env.table, env_token(env.role)))
        .collect()
}

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
    /// Number of live process-local anchors at save time. This does not count
    /// omitted values: anchor-only graphs are not saved, and shared graphs may
    /// still be reached through globals or environment objects.
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
        /// keys; only dead controls may also have hash slots for these keys.
        /// `array_len + 1` is the append key.
        array_len: u64,
        /// The first `array_slots` entries are array slots with consecutive
        /// keys, from the first live array key to the last, with `Nil` values
        /// for the empty slots between; every other array key is nil. Zero
        /// slots means an all-nil array. The rest are hash slots, including dead
        /// controls and (Nil, Nil) vacant positions, in insertion order.
        /// A leading (Nil, Bool(true)) marks map storage and is not a slot.
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

#[cfg(test)]
impl SavedObject {
    /// An empty saved table, for payload fixtures with no globals.
    fn empty_table() -> Self {
        Self::Table {
            array_len: 0,
            array_slots: 0,
            entries: Vec::new(),
            metatable: None,
        }
    }
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
    /// The global table, whole: always the `Table` variant (the verifier
    /// rejects anything else). It is not an arena object: `EnvObj("_G")`
    /// names it, and it is restored into the loaded State's own global table.
    globals: SavedObject,
    /// The pin list (`GlobalEnv`): string ids, empty unless sealed.
    global_pins: Vec<SavedVal>,
    /// Whether the saving State had sealed its globals; the loaded one takes
    /// the same phase.
    globals_sealed: bool,
    /// Every environment table, whole, in token order.
    env_tables: Vec<SavedEnvRecord>,
    next_format_pointer_id: u64,
    format_pointer_ids: Vec<(SavedVal, u64)>,
}

impl SavePayload {
    /// Minimal fixture payload; production capture fills every field explicitly.
    #[cfg(test)]
    fn empty(has_standard_environment: bool) -> Self {
        Self {
            has_standard_environment,
            rng_state: 0,
            cost: CostState::default(),
            strings: Vec::new(),
            bytecode: Vec::new(),
            upvalues: Vec::new(),
            objects: Vec::new(),
            globals: SavedObject::empty_table(),
            global_pins: Vec::new(),
            globals_sealed: false,
            env_tables: Vec::new(),
            next_format_pointer_id: 1,
            format_pointer_ids: Vec::new(),
        }
    }

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
            .saturating_add(
                std::iter::once((&self.globals, &self.global_pins))
                    .chain(self.env_tables.iter().map(|env| (&env.table, &env.pins)))
                    .fold(0usize, |sum, (table, pins)| {
                        sum.saturating_add(match table {
                            SavedObject::Table { entries, .. } => entries.len().saturating_mul(2),
                            SavedObject::Closure { .. } => 0,
                        })
                        .saturating_add(pins.len())
                    }),
            );
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

/// One environment table, saved whole like the global table.
#[derive(Clone, Debug, PartialEq)]
struct SavedEnvRecord {
    token: String,
    /// Always the `Table` variant (the verifier rejects anything else).
    table: SavedObject,
    /// String ids, empty unless sealed (`EnvTable::pins`).
    pins: Vec<SavedVal>,
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
}

#[derive(Clone, Copy)]
enum ValueDestination {
    Root(usize),
    TableKey { object: u32, entry: usize },
    TableValue { object: u32, entry: usize },
    Metatable { object: u32 },
    Upvalue(u32),
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
    Environment(String),
    GlobalTableEntryKey(usize),
    GlobalTableEntryValue(usize),
    TableKey(usize),
    TableValue(usize),
    Metatable,
    Upvalue(usize),
}

#[derive(Clone, Copy)]
enum DiagnosticRoot<'a> {
    Global(&'a str),
    /// A part of the environment table with this token: an entry's key or
    /// value by physical slot, or its metatable.
    Environment(&'a str, EnvironmentPart),
    GlobalTableEntryKey(usize),
    GlobalTableEntryValue(usize),
}

#[derive(Clone, Copy)]
enum EnvironmentPart {
    Key(usize),
    Value(usize),
    Metatable,
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
// unregistered function, using the encoder's child order for diagnostics.
// Earlier roots have encoded successfully, so a shared object cannot hide the
// first unregistered function from this root's fresh visited sets.
// This finds a reachability witness, not the encoder's exact traversal route:
// the finder expands environment objects, which the encoder writes as tokens
// and saves as records of their own. A path can therefore pass through _G
// or another environment alias to the same function. That is intentional for
// the best-effort diagnostic; reproducing encoder history is unnecessary to
// identify a reachable function that needs a save id.
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

    fn find(
        mut self,
        root: Val,
        diagnostic_root: DiagnosticRoot<'_>,
        target_function: RustFnId,
    ) -> Option<String> {
        let root_path = match diagnostic_root {
            DiagnosticRoot::Global(global) => {
                push_path(&mut self.breadcrumbs, None, PathSegment::global(global))
            }
            DiagnosticRoot::Environment(token, part) => {
                let table = push_path(
                    &mut self.breadcrumbs,
                    None,
                    PathSegment::Environment(truncate_utf8(token, MAX_PATH_GLOBAL_BYTES)),
                );
                let segment = match part {
                    EnvironmentPart::Key(index) => PathSegment::TableKey(index),
                    EnvironmentPart::Value(index) => PathSegment::TableValue(index),
                    EnvironmentPart::Metatable => PathSegment::Metatable,
                };
                push_path(&mut self.breadcrumbs, Some(table), segment)
            }
            DiagnosticRoot::GlobalTableEntryKey(index) => push_path(
                &mut self.breadcrumbs,
                None,
                PathSegment::GlobalTableEntryKey(index),
            ),
            DiagnosticRoot::GlobalTableEntryValue(index) => push_path(
                &mut self.breadcrumbs,
                None,
                PathSegment::GlobalTableEntryValue(index),
            ),
        };
        let mut tasks = vec![PathTask::Value {
            val: root,
            path: root_path,
        }];
        while let Some(task) = tasks.pop() {
            match task {
                PathTask::Value { val, path } => match val {
                    Val::RustFn(function) if function == target_function => {
                        return Some(render_path(&self.breadcrumbs, path));
                    }
                    Val::Obj(ptr) if self.env_reverse.contains_key(&ptr) => {
                        if self.environment_objects.insert(ptr) {
                            let table = self.state.heap.as_table_ref(ptr)?;
                            let metatable = table.get_metatable();
                            let (_, _, entries) = table.snapshot_entries();
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
    let mut environment = false;
    let mut global_table_entry = None;
    let mut skipped = false;
    while let Some(id) = current {
        let breadcrumb = &breadcrumbs[id];
        if let PathSegment::Global(name) = &breadcrumb.segment {
            global_name = Some(name.as_str());
        }
        if let PathSegment::Environment(name) = &breadcrumb.segment {
            global_name = Some(name.as_str());
            environment = true;
        }
        match &breadcrumb.segment {
            PathSegment::GlobalTableEntryKey(index) => {
                global_table_entry = Some((*index, true));
            }
            PathSegment::GlobalTableEntryValue(index) => {
                global_table_entry = Some((*index, false));
            }
            _ => {}
        }
        if ids.len() < MAX_RENDERED_PATH_SEGMENTS {
            ids.push(id);
        } else {
            skipped = true;
        }
        current = breadcrumb.parent;
    }
    let mut text = String::new();
    if let Some((index, is_key)) = global_table_entry {
        text.push_str("global table entry[");
        text.push_str(&index.to_string());
        text.push_str(if is_key { "].key" } else { "].value" });
    } else if let Some(name) = global_name {
        text.push_str(if environment {
            "environment "
        } else {
            "global "
        });
        text.push_str(name);
    }
    if skipped {
        text.push_str("...");
    }
    for id in ids.into_iter().rev() {
        match &breadcrumbs[id].segment {
            PathSegment::Global(_)
            | PathSegment::Environment(_)
            | PathSegment::GlobalTableEntryKey(_)
            | PathSegment::GlobalTableEntryValue(_) => {}
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

/// What `SaveBuilder::walk` encoded besides the arenas it filled.
struct SaveWalk {
    globals: SavedObject,
    global_pins: Vec<SavedVal>,
    env_tables: Vec<SavedEnvRecord>,
    format_pointer_ids: Vec<(SavedVal, u64)>,
}

/// How a snapshot accounts for one kind of GC root. Accounting, not
/// traversal: only `Serialized` roots are walked into the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SaveCoverage {
    /// Walked into the payload, with every object it reaches.
    Serialized,
    /// Recreated by the loaded State itself; at most a `%p` identity is
    /// carried.
    Rebuilt,
    /// Execution-lifetime: `validate_quiescent` refuses a save while it
    /// holds anything.
    RefusedInFlight,
    /// Owned by this process's host and never saved (`SaveDiagnostics`
    /// reports how many).
    ProcessLocal,
}

/// The snapshot's decision for each root kind. Exhaustive, so a new
/// `GcRoot` variant cannot be added without deciding what a save does with
/// it.
pub(super) fn save_coverage(root: &GcRoot<'_>) -> SaveCoverage {
    match root {
        // Globals: the global table and every environment table, each
        // encoded whole: every slot in physical order (tombstones included),
        // its metatable and its seal pins (`encode_globals`,
        // `encode_env_tables`), environment tables under a token for their
        // identity. StringMetatable: the string metatable is an environment
        // table, so it is encoded with them.
        GcRoot::Globals(_) | GcRoot::StringMetatable(_) => SaveCoverage::Serialized,
        // MetaNames: interned again when the loaded State is built.
        // ChunkLiterals: literals of saved bytecode are re-interned by load,
        // carrying their `%p` identities; literals of cache-only chunks need
        // not survive.
        GcRoot::MetaNames(_) | GcRoot::ChunkLiterals(_) => SaveCoverage::Rebuilt,
        GcRoot::Stack(_)
        | GcRoot::RootedValues(_)
        | GcRoot::SuspendedEnv(_)
        | GcRoot::Varargs(_)
        | GcRoot::OpenUpvalue(_)
        | GcRoot::ActiveChunk(_)
        | GcRoot::PendingChunk(_) => SaveCoverage::RefusedInFlight,
        GcRoot::Anchors(_) => SaveCoverage::ProcessLocal,
    }
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
        }
    }

    fn finish(mut self) -> Result<SavePayload, SaveError> {
        let walk = self.walk()?;
        self.into_payload(walk)
    }

    /// Encode everything the snapshot reaches, in encounter order. Every
    /// arena id is assigned here; `into_payload` only finalizes.
    fn walk(&mut self) -> Result<SaveWalk, SaveError> {
        let (globals, global_pins) = self.encode_globals()?;
        let env_tables = self.encode_env_tables()?;
        // Some strings stay observable without the saved graph reaching them
        // as values: metamethod event and builtin names are State roots for
        // its whole lifetime, and a string literal of a saved chunk is
        // re-interned on load to the same string a script reads through that
        // chunk. Carry a `%p` identity on either through the arena; strings
        // without an identity add nothing to the payload.
        let mut identity_strings: Vec<Val> = self.state.meta_names.iter().collect();
        identity_strings.extend(
            (0..crate::instr::Builtin::COUNT)
                .map(|slot| self.state.global_env.builtin_key(slot as u8)),
        );
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
            // A heap value unreachable from saved state or the rebuilt
            // environment cannot survive the load, so its id is dropped. A
            // registered Rust function is different: it is not a heap
            // object, the host can hold it and pass it back in after load,
            // and it resolves by its stable registration id. Keep its rendered identity whether or not the
            // saved graph reaches it (load skips one setup no longer
            // registers, see `materialize_payload`).
            if let Some(saved) = self.saved_reachable_pointer_value(val)
                && (matches!(saved, SavedVal::Fn(_))
                    || self.reachable_pointer_values.contains(&saved))
            {
                format_pointer_ids.push((saved, id));
            }
        }
        // Pointer ids are unique and increase with first rendering in a
        // continuous State. Loading merges setup-kept ids after saved ids, so
        // map insertion order can differ even when the identities match. Sort
        // the records to keep continuous and loaded States byte-identical.
        format_pointer_ids.sort_unstable_by_key(|(_, id)| *id);
        Ok(SaveWalk {
            globals,
            global_pins,
            env_tables,
            format_pointer_ids,
        })
    }

    fn into_payload(self, walk: SaveWalk) -> Result<SavePayload, SaveError> {
        let SaveWalk {
            globals,
            global_pins,
            env_tables,
            format_pointer_ids,
        } = walk;
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
            has_standard_environment: !self.state.global_env.env_tables().is_empty(),
            rng_state: self.state.rng.state(),
            cost: self.state.cost.clone(),
            strings: self.strings,
            bytecode: self.bytecode,
            upvalues,
            objects,
            globals,
            global_pins,
            globals_sealed: self.state.global_env.is_sealed(),
            env_tables,
            next_format_pointer_id: self.state.next_format_pointer_id,
            format_pointer_ids,
        })
    }

    fn saved_reachable_pointer_value(&self, value: Val) -> Option<SavedVal> {
        match value {
            Val::Str(ptr) => self.string_ids.get(&ptr).copied().map(SavedVal::Str),
            Val::RustFn(function) => self
                .state
                .rust_fns
                .save_id(function)
                .map(|id| SavedVal::Fn(id.to_owned())),
            Val::Obj(ptr) => self
                .env_reverse
                .get(&ptr)
                .cloned()
                .map(SavedVal::EnvObj)
                .or_else(|| self.object_ids.get(&ptr).copied().map(SavedVal::Obj)),
            Val::Nil | Val::Bool(_) | Val::Num(_) => None,
        }
    }

    /// The global table, whole, and the pin list.
    ///
    /// Every slot is saved in physical order, tombstones and vacant slots
    /// included: a saved tombstone suppresses the name load setup reinstalls,
    /// and the loaded table compacts exactly when the saving one would have.
    /// Builtins included: an unshadowed one encodes to a cheap `EnvObj`/`Fn`
    /// token, and skipping builtin names would drop user shadows such as
    /// `print = function ... end`. Encoded first, each named value as a root
    /// labelled by its global name, so an unregistered function is reported
    /// by the global that reaches it. Other entries use their physical slot
    /// index and whether the function is in the key or value. A `_G`
    /// self-reference encodes as its token alone, and its rendered identity
    /// survives whether or not any saved value still references it.
    fn encode_globals(&mut self) -> Result<(SavedObject, Vec<SavedVal>), SaveError> {
        let ptr = self.state.global_env.table();
        self.reachable_pointer_values
            .insert(SavedVal::EnvObj(GLOBALS_TOKEN.to_owned()));
        let table = self
            .state
            .heap
            .as_table_ref(ptr)
            .ok_or(SaveError::from(SaveFault::EnvironmentObjectNotTable))?;
        let (array_len, array_slots, entries) = table.snapshot_entries();
        let metatable = table.get_metatable();
        let array_slots = u32::try_from(array_slots)
            .map_err(|_| too_large(SaveCollection::TableArraySlots, array_slots))?;
        let mut saved = Vec::with_capacity(entries.len());
        for (entry_index, (key, value)) in entries.into_iter().enumerate() {
            // Only a UTF-8 string key reads as a global's name.
            let name = match key {
                Val::Str(name) => match str::from_utf8(self.state.heap.get_string(name)) {
                    Ok(name) => Some(name.to_owned()),
                    Err(_) => None,
                },
                _ => None,
            };
            let (key, value) = match name {
                Some(name) => (
                    self.encode_root(key, &name)?,
                    self.encode_root(value, &name)?,
                ),
                None => (
                    self.encode_global_entry_root(key, entry_index, true)?,
                    self.encode_global_entry_root(value, entry_index, false)?,
                ),
            };
            saved.push((key, value));
        }
        let metatable = metatable
            .map(|mt| {
                self.encode_environment_root(
                    Val::Obj(mt),
                    GLOBALS_TOKEN,
                    EnvironmentPart::Metatable,
                )
            })
            .transpose()?;
        let state = self.state;
        let pins = self.encode_pins(state.global_env.pins())?;
        Ok((
            SavedObject::Table {
                // MAX_ARRAY_LEN keeps the logical length below 2^53.
                array_len: array_len as u64,
                array_slots,
                entries: saved,
                metatable,
            },
            pins,
        ))
    }

    /// Every environment table, whole, in token order, by the global table's
    /// rule: every slot in physical order (tombstones and vacant slots
    /// included), the metatable and the pins. Each value is a root labelled
    /// by the table's token. A saved value is authoritative on load, a stock
    /// function included, so a save that leaves a library untouched still
    /// carries it: that is what keeps a setup value for a key the save holds
    /// from silently winning in some saves and losing in others.
    fn encode_env_tables(&mut self) -> Result<Vec<SavedEnvRecord>, SaveError> {
        let canonical: BTreeMap<&'static str, ObjectPtr> = self
            .state
            .global_env
            .env_tables()
            .iter()
            .map(|env| (env_token(env.role), env.table))
            .collect();
        let mut records = Vec::with_capacity(canonical.len());
        for (token, ptr) in canonical {
            // The table's rendered identity survives whether or not any saved
            // value still references it, as for the global table.
            self.reachable_pointer_values
                .insert(SavedVal::EnvObj(token.to_owned()));
            let table = self
                .state
                .heap
                .as_table_ref(ptr)
                .ok_or(SaveError::from(SaveFault::EnvironmentObjectNotTable))?;
            let (array_len, array_slots, entries) = table.snapshot_entries();
            let metatable = table.get_metatable();
            let array_slots = u32::try_from(array_slots)
                .map_err(|_| too_large(SaveCollection::TableArraySlots, array_slots))?;
            let mut saved = Vec::with_capacity(entries.len());
            for (index, (key, value)) in entries.into_iter().enumerate() {
                saved.push((
                    self.encode_environment_root(key, token, EnvironmentPart::Key(index))?,
                    self.encode_environment_root(value, token, EnvironmentPart::Value(index))?,
                ));
            }
            let metatable = metatable
                .map(|mt| {
                    self.encode_environment_root(Val::Obj(mt), token, EnvironmentPart::Metatable)
                })
                .transpose()?;
            let state = self.state;
            let env = state
                .global_env
                .env_tables()
                .iter()
                .find(|env| env.table == ptr)
                .expect("the token map was built from the environment tables");
            let pins = self.encode_pins(&env.pins)?;
            records.push(SavedEnvRecord {
                token: token.to_owned(),
                table: SavedObject::Table {
                    // MAX_ARRAY_LEN keeps the logical length below 2^53.
                    array_len: array_len as u64,
                    array_slots,
                    entries: saved,
                    metatable,
                },
                pins,
            });
        }
        Ok(records)
    }

    /// A pin list as string ids. A pin can outlive every slot naming it, and
    /// stays a root, so its string is reachable.
    fn encode_pins(&mut self, pins: &[StringPtr]) -> Result<Vec<SavedVal>, SaveError> {
        let mut saved = Vec::with_capacity(pins.len());
        for pin in pins {
            let pin = SavedVal::Str(self.string_id(*pin)?);
            self.reachable_pointer_values.insert(pin.clone());
            saved.push(pin);
        }
        Ok(saved)
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
        self.run_tasks(val, DiagnosticRoot::Global(global), root)?;
        self.roots[root]
            .take()
            .ok_or(SaveError::from(SaveFault::UnfilledRootSlot))
    }

    fn encode_environment_root(
        &mut self,
        val: Val,
        token: &str,
        part: EnvironmentPart,
    ) -> Result<SavedVal, SaveError> {
        let root = self.roots.len();
        self.roots.push(None);
        self.run_tasks(val, DiagnosticRoot::Environment(token, part), root)?;
        self.roots[root]
            .take()
            .ok_or(SaveError::from(SaveFault::UnfilledRootSlot))
    }

    fn encode_global_entry_root(
        &mut self,
        val: Val,
        entry_index: usize,
        is_key: bool,
    ) -> Result<SavedVal, SaveError> {
        let root = self.roots.len();
        self.roots.push(None);
        let diagnostic_root = if is_key {
            DiagnosticRoot::GlobalTableEntryKey(entry_index)
        } else {
            DiagnosticRoot::GlobalTableEntryValue(entry_index)
        };
        self.run_tasks(val, diagnostic_root, root)?;
        self.roots[root]
            .take()
            .ok_or(SaveError::from(SaveFault::UnfilledRootSlot))
    }

    fn run_tasks(
        &mut self,
        root: Val,
        diagnostic_root: DiagnosticRoot<'_>,
        root_slot: usize,
    ) -> Result<(), SaveError> {
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
                        Val::RustFn(function) => {
                            let Some(id) = self.state.rust_fns.save_id(function) else {
                                let reachable_from = PathFinder::new(self.state, &self.env_reverse)
                                    .find(root, diagnostic_root, function)
                                    .ok_or(SaveError::from(
                                        SaveFault::UnregisteredFunctionPathNotFound,
                                    ))?;
                                return Err(SaveError::UnregisteredFunction { reachable_from });
                            };
                            Ok(SavedVal::Fn(id.to_owned()))
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
                    // An environment object is a token here; its contents
                    // are a record of their own (`encode_env_tables`).
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
        self.bytecode[id as usize] = saved_bytecode_node(bc, nested);
        Ok(id)
    }
}

/// The wire form of one chunk, given its children's arena ids.
fn saved_bytecode_node(bc: &Bytecode, nested: Vec<u32>) -> SavedBytecode {
    SavedBytecode {
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
    }
}

impl State {
    /// Snapshot persistent script state.
    ///
    /// The VM must be quiescent: no Lua/Rust call can be in flight. Anchors are
    /// process-local and are not serialized, but their count is returned in
    /// [`SaveDiagnostics`]. Values reachable only through anchors are omitted;
    /// anchor presence does not make saving fail.
    ///
    /// A poisoned State (see [`State::is_poisoned`]) is refused with
    /// [`SaveError::StatePoisoned`], and an unsupported floating-point
    /// environment on the calling thread with
    /// [`SaveError::UnsupportedFloatEnvironment`].
    pub fn save_state(&self) -> Result<SaveState, SaveError> {
        self.check_save_usable()?;
        self.validate_quiescent()?;
        if cfg!(debug_assertions) {
            // The two inventories must agree: a root the snapshot refuses as
            // in flight is empty once the lifetime inventory says quiescent.
            // This is not a reachability check: host-owned anchors may keep
            // graphs alive that the snapshot deliberately does not traverse.
            self.for_each_gc_root(|root| {
                debug_assert!(
                    save_coverage(&root) != SaveCoverage::RefusedInFlight || root.is_empty(),
                    "a quiescent State holds an in-flight GC root; \
                     State::rest_state does not cover it"
                );
            });
        }
        let payload = SaveBuilder::new(self).finish()?;
        Ok(SaveState {
            bytes: encode_save(&payload)?,
            diagnostics: SaveDiagnostics {
                anchor_count: self.anchor_count(),
            },
        })
    }

    /// Restore a save into a fresh [`State`].
    ///
    /// Saved costing and counters are trusted input. For untrusted saves,
    /// compare [`State::costing`] with the host's expected mode and weights at
    /// the start of setup and return an error on mismatch, before executing
    /// scripts. After loading, set the host's budget and handle its result;
    /// setting a budget alone cannot enforce weights edited to zero, and a
    /// budget set during setup is replaced by the saved counters.
    ///
    /// The host supplies callbacks and re-runs its environment setup closure so
    /// registered host functions exist before saved values are resolved. A
    /// setup error is returned as [`LoadError::Setup`] without materializing
    /// any saved values. A setup that returns success after a panic unwound
    /// through the State (it caught the panic) gets
    /// [`LoadError::StatePoisoned`].
    ///
    /// The calling thread's floating-point environment is checked on entry
    /// and again after `setup`, which is host code and may change it, before
    /// any saved value is materialized; an unsupported one is
    /// [`LoadError::UnsupportedFloatEnvironment`]. Constructing the State
    /// itself computes nothing that depends on the environment.
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
        if let Some(environment) = crate::fp_env::fault() {
            return Err(LoadError::UnsupportedFloatEnvironment(environment));
        }
        let mut decoder = Decoder::new(bytes);
        decoder.read_magic_and_versions()?;
        // Read and discard the diagnostic crate-version string. Compatibility is
        // enforced by the format and cost-model versions in
        // read_magic_and_versions above; this string is metadata, not a compat
        // gate, so it is deliberately not compared.
        let _engine_version = decoder.read_bytes(SnapshotField::EngineVersion)?;
        let decoded = SavePayload::decode(&mut decoder)?;
        decoder.finish()?;
        // Taken before verification consumes the payload: the State is built
        // under the saved costing, so setup runs under the saved price list.
        // Its counters start fresh with no budget; materialization restores
        // the saved counters last.
        let costing = decoded.cost.costing().clone();
        let payload = verify::verify_payload(decoded)?;

        let builder = State::builder().callbacks(callbacks).costing(costing);
        let mut state = if payload.has_standard_environment() {
            builder.build()
        } else {
            builder.raw().build()
        };
        // Anchors created by setup are deliberately retained and participate
        // in the final collection.
        state.global_env.begin_load_setup();
        setup(&mut state).map_err(LoadError::Setup)?;
        if state.poisoned {
            return Err(LoadError::StatePoisoned);
        }
        if let Some(environment) = crate::fp_env::fault() {
            return Err(LoadError::UnsupportedFloatEnvironment(environment));
        }
        materialize_payload(&mut state, payload)?;
        Ok(state)
    }
}

/// The sole save envelope writer, shared by capture and forged test payloads.
fn encode_save(payload: &SavePayload) -> Result<Vec<u8>, SaveError> {
    let mut encoder = Encoder::with_capacity(payload.encoded_size_estimate());
    encoder.write_magic_and_versions();
    // Diagnostic metadata only; format and cost-model versions gate loading.
    encoder.write_bytes(
        env!("CARGO_PKG_VERSION").as_bytes(),
        SnapshotField::EngineVersion,
    )?;
    payload.encode(&mut encoder)?;
    Ok(encoder.finish())
}

/// Object -> token map for the save-time environment. Sourced from the
/// environment tables `open_libs` recorded, so a shadowed builtin slot
/// (`math = {}`) never causes a user table to be misread as an env object.
fn build_env_reverse(state: &State) -> BTreeMap<ObjectPtr, String> {
    let mut reverse: BTreeMap<ObjectPtr, String> = environment_tokens(state)
        .into_iter()
        .map(|(ptr, token)| (ptr, token.to_owned()))
        .collect();
    reverse.insert(state.global_env.table(), GLOBALS_TOKEN.to_owned());
    reverse
}

/// Token -> object map for the load-time environment, inverting the freshly
/// rebuilt record so saved `EnvObj` tokens resolve to the new objects.
/// Loading does not reconstruct token names from builtin names or literals;
/// capture uses this module's token schema on both sides of the round trip.
fn build_env_forward(state: &State) -> BTreeMap<String, ObjectPtr> {
    let mut forward: BTreeMap<String, ObjectPtr> = environment_tokens(state)
        .into_iter()
        .map(|(ptr, token)| (token.to_owned(), ptr))
        .collect();
    forward.insert(GLOBALS_TOKEN.to_owned(), state.global_env.table());
    forward
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
    let fn_forward = state.rust_fns.ids().clone();

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
                let hash_start = *array_slots as usize;
                let entries = entries
                    .iter()
                    .enumerate()
                    .map(|(entry_index, (key, value))| {
                        let key = decode_val(key, &ctx)?;
                        // As in the verifier, uniqueness covers the hash part
                        // only: array slots carry implied keys, which an older
                        // dead hash control may repeat, and the map-storage
                        // marker and vacant slots have a nil key.
                        if entry_index >= hash_start && !matches!(key, Val::Nil) {
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
                        }
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
                let array_len: usize = crate::saturate::saturating_cast(*array_len);
                table
                    .rebuild_from_snapshot(array_len, *array_slots as usize, entries)
                    .map_err(|error| SnapshotError::InvalidTable {
                        location: TableLoadLocation::Object(idx),
                        failure: error.into(),
                    })?;
                if metatable.is_some() {
                    table.set_metatable(metatable);
                }
            }
            SavedObject::Closure { .. } => {}
        }
    }

    restore_environment(state, &payload, &ctx)?;
    state.rng = VmRng::from_state(payload.rng_state);
    // Discards whatever setup charged or budgeted. The saved costing equals
    // the one the State was built with, so only the counters change.
    debug_assert_eq!(payload.cost.costing(), state.cost.costing());
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
            && !ctx.fn_forward.contains_key(name.as_str())
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
        // A duplicate value is corruption: the verifier already rejected
        // saved duplicates, and distinct function ids resolve to distinct
        // function table entries; this rechecks the resolved values.
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
    // Saved identities take precedence over the ones setup rendered: a value
    // the save records keeps its saved id, which is the replay contract.
    let setup_pointer_ids = std::mem::replace(&mut state.format_pointer_ids, format_pointer_ids);
    state.reset_in_flight();
    state.gc_collect();
    merge_setup_pointer_ids(state, setup_pointer_ids, &seen_pointer_ids);
    Ok(())
}

/// Decode the saved global table and every environment table with their
/// pins, and restore them into the loaded State (`GlobalEnv::restore` owns
/// the one rule that merges each with setup).
fn restore_environment(
    state: &mut State,
    payload: &SavePayload,
    ctx: &DecodeCtx<'_>,
) -> Result<(), SnapshotError> {
    let globals = decode_env_table(
        state,
        &payload.globals,
        &payload.global_pins,
        SaveLocation::Globals,
        ctx,
    )?;
    let mut env = Vec::with_capacity(payload.env_tables.len());
    for (index, record) in payload.env_tables.iter().enumerate() {
        let ptr = *ctx
            .env_forward
            .get(&record.token)
            .filter(|ptr| **ptr != state.global_env.table())
            .ok_or_else(|| SnapshotError::UnknownEnvObject(record.token.clone()))?;
        env.push((
            ptr,
            decode_env_table(
                state,
                &record.table,
                &record.pins,
                SaveLocation::EnvironmentTable(index),
                ctx,
            )?,
        ));
    }
    // Two tokens can resolve to one object only through a corrupt save; the
    // verifier has rejected duplicate tokens already.
    let mut seen = BTreeSet::new();
    for (index, (ptr, _)) in env.iter().enumerate() {
        if !seen.insert(*ptr) {
            return Err(corrupt_arena(
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::EnvironmentToken,
                    form: KeyForm::Materialized,
                },
                index,
                SaveLocation::EnvironmentTables,
            ));
        }
    }
    let tokens = environment_tokens(state);
    state
        .global_env
        .restore(&mut state.heap, globals, env, payload.globals_sealed)
        .map_err(|(table, error)| SnapshotError::InvalidTable {
            location: match table.and_then(|ptr| tokens.get(&ptr)) {
                Some(token) => TableLoadLocation::Environment {
                    token: (*token).to_owned(),
                },
                None => TableLoadLocation::Globals,
            },
            failure: error.into(),
        })
}

/// Decode one saved environment table: its entries, with resolved hash keys
/// checked for uniqueness (the verifier can only check them in saved form),
/// its metatable and its pins.
fn decode_env_table(
    state: &State,
    table: &SavedObject,
    pins: &[SavedVal],
    location: SaveLocation,
    ctx: &DecodeCtx<'_>,
) -> Result<SavedEnvTable, SnapshotError> {
    let SavedObject::Table {
        array_len,
        array_slots,
        entries,
        metatable,
    } = table
    else {
        return Err(corrupt_arena(
            CorruptionReason::Table(TableCorruption::MaterializedWrongType),
            0,
            location,
        ));
    };
    let mut runtime_keys = BTreeSet::new();
    let hash_start = *array_slots as usize;
    let mut decoded = Vec::with_capacity(entries.len());
    for (entry_index, (key, value)) in entries.iter().enumerate() {
        let key = decode_val(key, ctx)?;
        if entry_index >= hash_start && !matches!(key, Val::Nil) {
            insert_runtime_key(
                &mut runtime_keys,
                key,
                &state.heap,
                entry_index,
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::TableKey,
                    form: KeyForm::Materialized,
                },
                location.clone(),
            )?;
        }
        decoded.push((key, decode_val(value, ctx)?));
    }
    let metatable = match metatable {
        None => None,
        Some(mt) => match decode_val(mt, ctx)? {
            Val::Obj(mt_ptr) if state.heap.as_table_ref(mt_ptr).is_some() => Some(mt_ptr),
            _ => {
                return Err(corrupt_arena(
                    CorruptionReason::Table(TableCorruption::MetatableNotTable),
                    0,
                    location,
                ));
            }
        },
    };
    let mut saved_pins = Vec::with_capacity(pins.len());
    for (index, pin) in pins.iter().enumerate() {
        match decode_val(pin, ctx)? {
            Val::Str(ptr) => saved_pins.push(ptr),
            _ => {
                return Err(corrupt_arena(CorruptionReason::InvalidPin, index, location));
            }
        }
    }
    // The verifier bounds `array_len` by MAX_ARRAY_LEN, so it fits.
    let array_len: usize = crate::saturate::saturating_cast(*array_len);
    Ok(SavedEnvTable {
        entries: (array_len, hash_start, decoded),
        metatable,
        pins: saved_pins,
    })
}

/// Merge the identities load setup rendered into the restored ones.
///
/// Only values that survive the load and that the save does not record are
/// considered: an anchor, a value reachable only from one, a setup-only
/// global, a function setup newly registered. A setup that replays the
/// saving State's initialization renders such a value with the id its
/// counterpart held in the saving State, which no saved entry uses, so the
/// value keeps that id and the loaded State continues exactly as the
/// continuous run would. An id that a saved entry (or a retired saved
/// registration) already holds can come only from a setup that differs; that
/// value is re-keyed above the counter instead, so no two live values share
/// an identity.
///
/// Kept ids are placed first, then the re-keyed ones in setup's rendering
/// order, so a fresh id can never equal a kept one. Liveness is read straight
/// after the final collection, before any allocation, so a swept handle
/// cannot alias a new object. An exhausted counter leaves a value unrendered;
/// its next rendering raises the ordinary exhaustion error.
fn merge_setup_pointer_ids(
    state: &mut State,
    setup_pointer_ids: DetMap<PointerIdKey, u64>,
    saved_ids: &BTreeSet<u64>,
) {
    let mut rekey = Vec::new();
    for (key, id) in setup_pointer_ids {
        if state.format_pointer_ids.contains_key(&key) || !state.heap.val_is_live(&key.as_val()) {
            continue;
        }
        if saved_ids.contains(&id) {
            rekey.push(key);
            continue;
        }
        // Setup's own counter kept `id` below u64::MAX.
        if let Some(next) = id.checked_add(1) {
            state.next_format_pointer_id = state.next_format_pointer_id.max(next);
        }
        state.format_pointer_ids.insert(key, id);
    }
    for key in rekey {
        let id = state.next_format_pointer_id;
        let Some(next) = id.checked_add(1) else {
            break;
        };
        state.next_format_pointer_id = next;
        state.format_pointer_ids.insert(key, id);
    }
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
    fn_forward: &'a BTreeMap<Cow<'static, str>, RustFnId>,
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

type RuntimeTableKey = NormalizedTableKey<Vec<u8>, ObjectPtr, RustFnId>;

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
            .get(id.as_str())
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
    // A gmatch iterator chunk becomes this process's shared iterator Arc, the
    // identity error reporting uses to locate a lazy pattern failure at the
    // script caller. Only the chunks of closures capturing a gmatch iterator
    // function qualify, which no script-defined closure can, so an identical
    // or same-named user chunk keeps its own Arc. A chunk that differs from
    // this build's wrapper (a save from another build) loads as saved and
    // its errors stay located in the synthetic chunk.
    let iterator_chunks = payload.gmatch_iterator_chunks();
    if !iterator_chunks.is_empty() {
        let shared = crate::lua_std::gmatch_iterator_bytecode();
        let shared_form = saved_bytecode_node(&shared, Vec::new());
        for idx in iterator_chunks {
            if saved.get(idx) == Some(&shared_form) {
                out[idx] = Some(Arc::clone(&shared));
            }
        }
    }
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

/// Costing mode tags. For `Metered`, one u32 weight per charge follows, in
/// `Charge` discriminant order; the count is fixed by `COST_MODEL_VERSION`,
/// which the header checks for an exact match.
const COSTING_METERED: u8 = 0;
const COSTING_OFF: u8 = 1;

// All weights, including zero, are valid host choices. Decoding checks the
// schema, not host policy: load setup can reject the saved costing before
// running scripts or materializing values by comparing State::costing().
fn read_costing(input: &mut Decoder<'_>) -> Result<Costing, SnapshotError> {
    let offset = input.pos;
    match input.read_u8()? {
        COSTING_METERED => {
            let mut weights = [0u32; Charge::COUNT];
            for weight in &mut weights {
                *weight = input.read_u32()?;
            }
            Ok(Costing::Metered(CostModel::from_weights(weights)))
        }
        COSTING_OFF => Ok(Costing::Off),
        tag => Err(input.decode_error_at(offset, DecodeReason::InvalidCostingTag(tag))),
    }
}

impl SavePayload {
    fn encode(&self, out: &mut Encoder) -> Result<(), SaveError> {
        out.write_u8(u8::from(self.has_standard_environment));
        out.write_u64(self.rng_state);
        match self.cost.costing() {
            Costing::Metered(model) => {
                out.write_u8(COSTING_METERED);
                for (_, weight) in model.iter() {
                    out.write_u32(weight);
                }
            }
            Costing::Off => out.write_u8(COSTING_OFF),
        }
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
        self.globals.encode(out)?;
        write_vec(
            out,
            SaveCollection::Globals,
            &self.global_pins,
            |out, pin| pin.encode(out),
        )?;
        out.write_bool(self.globals_sealed);
        write_vec(
            out,
            SaveCollection::EnvironmentTables,
            &self.env_tables,
            |out, record| record.encode(out),
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
                let costing = read_costing(input)?;
                let offset = input.pos;
                CostState::from_snapshot(
                    costing,
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
            globals: SavedObject::decode(input)?,
            global_pins: read_vec(input, SavedVal::decode)?,
            globals_sealed: input.read_bool()?,
            env_tables: read_vec(input, SavedEnvRecord::decode)?,
            next_format_pointer_id: input.read_u64()?,
            format_pointer_ids: read_vec(input, |input| {
                Ok((SavedVal::decode(input)?, input.read_u64()?))
            })?,
        })
    }
}

impl SavedEnvRecord {
    fn encode(&self, out: &mut Encoder) -> Result<(), SaveError> {
        out.write_string(&self.token, SnapshotField::EnvironmentToken)?;
        self.table.encode(out)?;
        write_vec(
            out,
            SaveCollection::EnvironmentPins,
            &self.pins,
            |out, pin| pin.encode(out),
        )
    }

    fn decode(input: &mut Decoder<'_>) -> Result<Self, SnapshotError> {
        Ok(Self {
            token: input.read_string(SnapshotField::EnvironmentToken)?,
            table: SavedObject::decode(input)?,
            pins: read_vec(input, SavedVal::decode)?,
        })
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
        // Encode in descending address order, then ascending, so neither an
        // ascending nor a descending address sort can match both by accident.
        for pass in 0..2 {
            if pass == 1 {
                chunks.reverse();
            }
            let expected: Vec<_> = chunks
                .iter()
                .map(|chunk| {
                    chunk
                        .string_literals
                        .iter()
                        .find(|literal| {
                            literal.as_slice() == b"identity-a"
                                || literal.as_slice() == b"identity-b"
                        })
                        .unwrap()
                        .clone()
                })
                .collect();
            let mut builder = SaveBuilder::new(&state);
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
            assert_eq!(actual, expected, "encode pass {pass}");
        }
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

    /// The environment flag, the RNG word and a metered costing with every
    /// weight 1, ready for counter fields.
    fn metered_head() -> Encoder {
        let mut encoder = Encoder::new();
        encoder.write_bool(false);
        encoder.write_u64(0);
        encoder.write_u8(COSTING_METERED);
        for _ in 0..Charge::COUNT {
            encoder.write_u32(1);
        }
        encoder
    }

    /// Offset of the counter fields after `metered_head`.
    const METERED_COUNTERS_OFFSET: usize = 9 + 1 + 4 * Charge::COUNT;

    #[test]
    fn decode_rejects_inconsistent_legacy_meter_fields() {
        for (remaining, limit, configured, used) in [
            (0, 0, false, 0),
            (11, 10, true, 0),
            (9, 10, true, 0),
            (-1, 0, true, 1),
        ] {
            let mut encoder = metered_head();
            encoder.write_i64(remaining);
            encoder.write_i64(limit);
            encoder.write_bool(configured);
            encoder.write_u64(used);
            let mut decoder = Decoder::new(&encoder.bytes);
            match SavePayload::decode(&mut decoder) {
                Err(error @ SnapshotError::DecodeError { offset, reason }) => {
                    assert_eq!(
                        (offset, reason),
                        (METERED_COUNTERS_OFFSET, DecodeReason::InconsistentCostMeter)
                    );
                    assert_eq!(
                        error.to_string(),
                        format!(
                            "save decode error: at byte {METERED_COUNTERS_OFFSET}: \
                             inconsistent cost meter"
                        )
                    );
                }
                _ => panic!("inconsistent meter must fail before decoding the arena"),
            }
        }
    }

    #[test]
    fn decode_rejects_unknown_costing_tag() {
        let mut encoder = Encoder::new();
        encoder.write_bool(false);
        encoder.write_u64(0);
        encoder.write_u8(2);
        let mut decoder = Decoder::new(&encoder.bytes);
        match SavePayload::decode(&mut decoder) {
            Err(SnapshotError::DecodeError { offset, reason }) => {
                assert_eq!((offset, reason), (9, DecodeReason::InvalidCostingTag(2)));
            }
            _ => panic!("an unknown costing tag must be rejected"),
        }
    }

    #[test]
    fn decode_rejects_truncated_weight_list() {
        let mut encoder = Encoder::new();
        encoder.write_bool(false);
        encoder.write_u64(0);
        encoder.write_u8(COSTING_METERED);
        for _ in 0..Charge::COUNT - 1 {
            encoder.write_u32(1);
        }
        let end = encoder.bytes.len();
        let mut decoder = Decoder::new(&encoder.bytes);
        match SavePayload::decode(&mut decoder) {
            Err(SnapshotError::DecodeError { offset, reason }) => {
                assert_eq!((offset, reason), (end, DecodeReason::TruncatedPayload));
            }
            _ => panic!("a short weight list must be rejected"),
        }
    }

    #[test]
    fn decode_rejects_off_costing_with_use_or_budget() {
        for (remaining, limit, configured, used) in
            [(i64::MAX, i64::MAX, false, 1), (10, 10, true, 0)]
        {
            let mut encoder = Encoder::new();
            encoder.write_bool(false);
            encoder.write_u64(0);
            encoder.write_u8(COSTING_OFF);
            encoder.write_i64(remaining);
            encoder.write_i64(limit);
            encoder.write_bool(configured);
            encoder.write_u64(used);
            let mut decoder = Decoder::new(&encoder.bytes);
            match SavePayload::decode(&mut decoder) {
                Err(SnapshotError::DecodeError { offset, reason }) => {
                    assert_eq!((offset, reason), (10, DecodeReason::InconsistentCostMeter));
                }
                _ => panic!("an Off save with use or a budget must be rejected"),
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
        // The names are the string arena; the globals table binds them.
        let strings = user_globals
            .iter()
            .map(|(name, _)| name.as_bytes().to_vec())
            .collect();
        let entries = std::iter::once((SavedVal::Nil, SavedVal::Bool(true)))
            .chain(
                user_globals
                    .into_iter()
                    .enumerate()
                    .map(|(id, (_, value))| (SavedVal::Str(id as u32), value)),
            )
            .collect();
        let payload = SavePayload {
            strings,
            bytecode,
            objects,
            globals: SavedObject::Table {
                array_len: 0,
                array_slots: 0,
                entries,
                metatable: None,
            },
            ..SavePayload::empty(false)
        };
        let Ok(bytes) = encode_save(&payload) else {
            panic!("fixture payload must encode");
        };
        State::load_state(&bytes, Box::new(crate::DefaultCallbacks), no_setup).map_err(|error| {
            match error {
                LoadError::Snapshot(error) => error,
                other => panic!("fixture load failed outside the snapshot: {other:?}"),
            }
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

    /// A global name need not be UTF-8: `_G` is keyed by the interned
    /// literal, as a script's `_G["\255"]` is. Forged bytecode that writes a
    /// binary-named global, reads it back through a cached read and through
    /// an uncached one (a plain `_G` table lookup), and publishes both, loads,
    /// runs cold and warm, and survives a save round trip.
    #[test]
    fn a_binary_global_name_loads_runs_and_round_trips() {
        let mut bytecode = valid_saved_bytecode();
        bytecode.number_literals = vec![7.0f64.to_bits()];
        bytecode.string_literals = vec![vec![255], b"seen".to_vec(), b"seen_raw".to_vec()];
        bytecode.code = vec![
            Instr::load_num(0, 0).raw(),
            Instr::set_global(0, 0, Some(0)).raw(),
            Instr::get_global(1, 0, Some(1)).raw(),
            Instr::set_global(1, 1, Some(2)).raw(),
            Instr::get_global(1, 0, None).raw(),
            Instr::set_global(1, 2, Some(3)).raw(),
            Instr::ret(0, RetCount::Fixed(0)).raw(),
        ];
        bytecode.global_cache_slots = 4;
        bytecode.line_info = vec![1; bytecode.code.len()];
        let mut state = load_bytecode_with_globals(
            vec![bytecode],
            vec![SavedObject::Closure {
                chunk: 0,
                upvalues: Vec::new(),
            }],
            vec![("writer".to_string(), SavedVal::Obj(0))],
        )
        .expect("a binary global name is admitted");

        let check = |state: &mut State| {
            for _ in 0..2 {
                state.get_global("writer").expect("writer exists");
                state
                    .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                    .expect("the forged chunk runs");
            }
            for name in ["seen", "seen_raw"] {
                state.get_global(name).expect("the global is readable");
                assert_eq!(state.to_number(-1).expect("a number"), 7.0, "{name}");
                state.pop(1).expect("the global was pushed");
            }
        };
        check(&mut state);
        let save = state.save_state().expect("the State saves");
        let mut reloaded =
            State::load_state(&save.bytes, Box::new(crate::DefaultCallbacks), no_setup)
                .expect("the save loads");
        check(&mut reloaded);
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
            bytecode: vec![bytecode],
            ..SavePayload::empty(false)
        };
        let bytes = encode_save(&payload).expect("marker payload encodes");
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
    fn saved_environment_table_round_trips() {
        roundtrip!(
            SavedEnvRecord,
            SavedEnvRecord {
                token: "math".to_string(),
                table: SavedObject::Table {
                    array_len: 2,
                    array_slots: 1,
                    entries: vec![
                        (SavedVal::Num(2.0f64.to_bits()), SavedVal::Bool(true)),
                        (SavedVal::Str(2), SavedVal::Nil),
                        (SavedVal::Str(1), SavedVal::Obj(3)),
                    ],
                    metatable: Some(SavedVal::EnvObj("table".to_string())),
                },
                pins: vec![SavedVal::Str(1), SavedVal::Str(2)],
            }
        );
    }

    #[test]
    fn saved_main_chunk_keeps_zero_defined_line() {
        fn assert_main_chunk_frame(state: &mut State) {
            state.get_global("main").unwrap();
            let err = state
                .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                .unwrap_err();
            let frame = &err.stack_trace()[0];
            assert_eq!(frame.defined_line, 0);
            assert_eq!(frame.to_string(), "main:1: in main chunk");
        }

        let mut state = State::new();
        let program = crate::Program::compile("error('x')", Some(b"main".as_slice())).unwrap();
        state.load(&program).unwrap();
        state.set_global("main").unwrap();
        assert_main_chunk_frame(&mut state);

        let save = state.save_state().unwrap();
        let mut loaded =
            State::load_state(&save.bytes, Box::new(crate::DefaultCallbacks), no_setup).unwrap();
        assert_main_chunk_frame(&mut loaded);
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

    /// The objects reachable from the roots `save_coverage` calls
    /// `Serialized` are exactly the objects the save walker encoded or
    /// tokenized. Every string reachable from those roots is in the string
    /// arena or in a saved bytecode literal pool. This checks the walker
    /// against the coverage decision for every root kind the fixture
    /// populates; it cannot check a kind the fixture leaves empty.
    #[test]
    fn save_walker_encodes_exactly_what_serialized_roots_reach() {
        let mut state = State::new();
        crate::Program::compile(
            r#"
                a = { 1, 2, 3 }
                a.b = {}
                a.b.back = a
                a.note = "serialized-only-string"
                local captured = { inner = {} }
                keep = function() return a, captured, "closure-literal" end
                string.format("%p", "closure-literal")
                string.format("%p", "__call")
                string.format("%p", "print")
                m = math
                junk = { x = {} }
                junk = nil
                shared = {}
                orphan = { child = {}, note = "anchor-only-string" }
                string.extra = {}
            "#,
            None,
        )
        .and_then(|program| state.load(&program))
        .expect("compile");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("run");
        // Anchors are `ProcessLocal`: one on a table the globals also reach
        // (still encoded), one on a table nothing else reaches (live, but
        // neither reached nor encoded).
        state.get_global("shared").expect("global read");
        let _shared = state.anchor().expect("anchor shared");
        state.get_global("orphan").expect("global read");
        let _orphan = state.anchor().expect("anchor orphan");
        state.push_nil().expect("push");
        state.set_global("orphan").expect("clear orphan");
        state.seal_globals();
        state.push_nil().expect("push");
        state
            .set_global("print")
            .expect("delete pinned builtin name");
        state.gc_collect();

        let mut builder = SaveBuilder::new(&state);
        let walk = builder.walk().expect("walk");
        let mut saved: BTreeSet<ObjectPtr> = builder.object_ids.keys().copied().collect();
        saved.extend(builder.env_reverse.keys().copied());
        let saved_strings: BTreeSet<_> = builder.string_ids.keys().copied().collect();
        let mut encoded_strings = saved_strings.clone();
        for literal in builder
            .bytecode
            .iter()
            .flat_map(|chunk| chunk.string_literals.iter())
        {
            encoded_strings.insert(
                state
                    .heap
                    .find_string(literal)
                    .expect("a saved bytecode literal is interned in its State"),
            );
        }
        builder.into_payload(walk).expect("finish");

        let serialized_only = state
            .heap
            .find_string(b"serialized-only-string")
            .expect("the serialized graph keeps its string interned");
        let anchor_only = state
            .heap
            .find_string(b"anchor-only-string")
            .expect("the process-local anchor keeps its string interned");
        let builtin_name = state
            .heap
            .find_string(b"print")
            .expect("the sealed global pin keeps the builtin name interned");
        let event_name = state
            .heap
            .find_string(b"__call")
            .expect("the event-name root keeps its rendered identity interned");
        let closure_literal = state
            .heap
            .find_string(b"closure-literal")
            .expect("the saved closure keeps its literal interned");
        assert!(saved_strings.contains(&serialized_only));
        assert!(saved_strings.contains(&builtin_name));
        assert!(saved_strings.contains(&event_name));
        assert!(saved_strings.contains(&closure_literal));
        assert!(state.global_env.pins().contains(&builtin_name));
        assert!(!saved_strings.contains(&anchor_only));
        assert!(!encoded_strings.contains(&anchor_only));
        assert!(state.heap.val_is_live(&Val::Str(anchor_only)));

        // After the collection every colour is Unmarked, so this mark phase
        // starts clean; its marks are undone before anything can sweep.
        let mut worklist = Vec::new();
        let mut live_chunks = crate::vm::object::LiveChunks::default();
        state.mark_roots(&mut worklist, &mut live_chunks, |root| {
            save_coverage(root) == SaveCoverage::Serialized
        });
        state
            .heap
            .drain_mark_worklist(&mut worklist, &state.upvalue_pool);
        let (reached, reached_strings) = state.heap.take_marked_objects_and_strings();
        state.upvalue_pool.reset_marks();

        assert_eq!(reached, saved);
        assert!(
            reached_strings.is_subset(&encoded_strings),
            "a string reachable from a serialized root is absent from the save arenas"
        );
        // The orphan and its child are the only live objects left out.
        assert_eq!(state.object_count(), reached.len() + 2);
    }

    /// The math table's value for `key`, through its environment token.
    fn math_field(state: &State, key: &[u8]) -> Val {
        let (math, _) = environment_tokens(state)
            .into_iter()
            .find(|(_, token)| *token == "math")
            .expect("math is an environment table");
        let key = Val::Str(state.heap.find_string(key).expect("the key is interned"));
        state
            .heap
            .as_table_ref(math)
            .expect("math is a table")
            .get(&key)
    }

    #[test]
    fn setup_library_addition_survives_load_and_resave() {
        let state = State::new();
        let save = state.save_state().expect("fresh state saves");
        let loaded = State::load_state(
            &save.bytes,
            Box::new(crate::DefaultCallbacks),
            |state| -> crate::Result<()> {
                state.get_global("math")?;
                state.push_string("setup_only")?;
                state.push_boolean(true)?;
                state.set_table_raw(-3)?;
                Ok(())
            },
        )
        .expect("setup addition loads");
        // A key the save mentions nowhere is setup's to contribute.
        assert_eq!(math_field(&loaded, b"setup_only"), Val::Bool(true));
        let resave = loaded.save_state().expect("setup addition resaves");
        let restored =
            State::load_state(&resave.bytes, Box::new(crate::DefaultCallbacks), no_setup)
                .expect("setup addition survives a round trip");
        assert_eq!(math_field(&restored, b"setup_only"), Val::Bool(true));
    }

    #[test]
    fn library_signed_zero_survives_load() {
        let mut state = State::new();
        crate::Program::compile("math.zero = -0.0", None)
            .and_then(|program| state.load(&program))
            .expect("source compiles");
        state
            .call(crate::ArgCount::Fixed(0), crate::RetCount::Fixed(0))
            .expect("source runs");
        let save = state.save_state().expect("state saves");
        let loaded = State::load_state(&save.bytes, Box::new(crate::DefaultCallbacks), no_setup)
            .expect("state loads");
        let Val::Num(zero) = math_field(&loaded, b"zero") else {
            panic!("math.zero is a number");
        };
        assert_eq!(zero.to_bits(), (-0.0f64).to_bits());
    }
}
