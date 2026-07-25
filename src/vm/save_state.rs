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
//!   `_G` (and `_G`'s metatable) are reconstructed by `open_libs` on load and
//!   referenced by token. The object->token map is captured once at
//!   construction (`State.env_tokens`), not read from the live `builtins`
//!   slots, so shadowing a builtin global (`math = {}`) can't misclassify the
//!   user's table as an environment object. Adding `math.foo` in a later build
//!   therefore doesn't corrupt old saves.
//! - **Globals are persisted whole.** An unshadowed builtin encodes to a cheap
//!   `EnvObj`/`Fn` token; a *shadow* (`print = ...`) is preserved as the user's
//!   value. Skipping builtin names would silently drop shadows.
//! - **In-crate codec.** Little-endian, length-prefixed, `f64` by bit pattern
//!   (NaN / -0.0 exact), explicit enum tags, bounds-checked with reservations
//!   capped at remaining bytes. No `serde`/`bincode`. Two saves of the same
//!   logical state are byte-identical.

use std::fmt;
use std::sync::Arc;

use std::collections::BTreeMap;

use super::lua_val::RustFunc;
use super::object::{RawObject, Upvalue, UpvalueRef};
use super::rng::VmRng;
use super::{ObjectPtr, State, Val};
use crate::compiler::{Bytecode, UpvalueDesc};
use crate::host::HostCallbacks;
use crate::instr::Instr;

mod verify;

const MAGIC: [u8; 4] = *b"DLGS";
const FORMAT_VERSION: u16 = 3;

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

/// Errors produced while saving a [`State`].
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SaveError {
    /// The VM has an active stack/call frame/open upvalue and cannot be snapshotted.
    NotQuiescent,
    /// A reachable Rust function has no stable save/load id.
    UnregisteredFunction {
        /// Best-effort path to the value that made the function reachable.
        reachable_from: String,
    },
    /// One save/load id was registered for two different Rust functions.
    DuplicateFunctionRegistration {
        /// The id that was bound to conflicting function addresses.
        id: String,
    },
    /// A reachable upvalue was still open.
    OpenUpvalueReachable,
    /// Internal codec failure.
    EncodeError(String),
}

/// Errors produced while loading a saved [`State`].
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum LoadError {
    /// Save bytes did not start with the dellingr save magic.
    BadMagic,
    /// Save format version is not supported by this build.
    UnsupportedVersion,
    /// A saved function id was not registered by the load-time environment.
    UnknownFunction(String),
    /// A saved environment-object token was not present at load time.
    UnknownEnvObject(String),
    /// Binary payload could not be decoded.
    DecodeError(String),
    /// Arena indices or object graph contents were corrupt.
    CorruptArena,
    /// A saved bytecode program violates VM structural invariants.
    InvalidBytecode {
        /// Saved bytecode arena id.
        chunk: u32,
        /// Bytecode index when the failure is instruction-specific.
        instruction: Option<u32>,
        /// Deterministic explanation for diagnostics.
        reason: String,
    },
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::NotQuiescent => write!(f, "state is not quiescent"),
            SaveError::UnregisteredFunction { reachable_from } => {
                write!(
                    f,
                    "unregistered Rust function reachable from {reachable_from}"
                )
            }
            SaveError::DuplicateFunctionRegistration { id } => {
                write!(f, "id {id} registered for two different Rust functions")
            }
            SaveError::OpenUpvalueReachable => write!(f, "open upvalue reachable during save"),
            SaveError::EncodeError(err) => write!(f, "save encode error: {err}"),
        }
    }
}

impl std::error::Error for SaveError {}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::BadMagic => write!(f, "bad save magic"),
            LoadError::UnsupportedVersion => write!(f, "unsupported save format version"),
            LoadError::UnknownFunction(id) => write!(f, "unknown saved function id {id}"),
            LoadError::UnknownEnvObject(id) => write!(f, "unknown saved environment object {id}"),
            LoadError::DecodeError(err) => write!(f, "save decode error: {err}"),
            LoadError::CorruptArena => write!(f, "corrupt save arena"),
            LoadError::InvalidBytecode {
                chunk,
                instruction,
                reason,
            } => {
                write!(f, "invalid bytecode chunk {chunk}")?;
                if let Some(instruction) = instruction {
                    write!(f, " at instruction {instruction}")?;
                }
                write!(f, ": {reason}")
            }
        }
    }
}

impl std::error::Error for LoadError {}

#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Debug, PartialEq)]
struct SavedBytecode {
    code: Vec<u32>,
    number_literals: Vec<u64>,
    string_literals: Vec<Vec<u8>>,
    table_templates: Vec<Vec<u8>>,
    global_cache_slots: u16,
    field_cache_slots: u16,
    set_field_cache_slots: u8,
    num_params: u8,
    num_locals: u8,
    nested: Vec<u32>,
    upvalues: Vec<SavedUpvalueDesc>,
    is_vararg: bool,
    name: Option<String>,
    source: Option<String>,
    line_info: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SavedUpvalueDesc {
    Local(u8),
    Upvalue(u8),
}

#[derive(Clone, Debug, PartialEq)]
struct SavePayload {
    has_standard_environment: bool,
    rng_state: u64,
    cost_remaining: i64,
    cost_budget: i64,
    cost_budget_configured: bool,
    cost_used: u64,
    strings: Vec<Vec<u8>>,
    bytecode: Vec<SavedBytecode>,
    upvalues: Vec<SavedVal>,
    objects: Vec<SavedObject>,
    user_globals: Vec<(Vec<u8>, SavedVal)>,
}

struct SaveBuilder<'a> {
    state: &'a State,
    env_reverse: BTreeMap<ObjectPtr, String>,
    strings: Vec<Vec<u8>>,
    string_ids: BTreeMap<Vec<u8>, u32>,
    bytecode: Vec<SavedBytecode>,
    bytecode_ids: BTreeMap<usize, u32>,
    upvalues: Vec<Option<SavedVal>>,
    upvalue_ids: BTreeMap<usize, u32>,
    objects: Vec<Option<PendingObject>>,
    object_ids: BTreeMap<ObjectPtr, u32>,
    roots: Vec<Option<SavedVal>>,
    breadcrumbs: Vec<Breadcrumb>,
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
        path: PathId,
        destination: ValueDestination,
    },
    Object {
        ptr: ObjectPtr,
        id: u32,
        path: PathId,
    },
    Upvalue {
        upvalue: UpvalueRef,
        path: PathId,
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
struct Breadcrumb {
    parent: Option<PathId>,
    segment: PathSegment,
}
enum PendingObject {
    Table {
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
            breadcrumbs: Vec::new(),
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
        for (name, val) in &self.state.globals {
            let saved = self.encode_root(*val, PathSegment::Global(name.clone()))?;
            user_globals.push((name.as_bytes().to_vec(), saved));
        }

        let objects = self
            .objects
            .into_iter()
            .map(|obj| {
                Self::finish_object(
                    obj.ok_or_else(|| SaveError::EncodeError("unfilled object slot".to_string()))?,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let upvalues = self
            .upvalues
            .into_iter()
            .map(|value| {
                value.ok_or_else(|| SaveError::EncodeError("unfilled upvalue slot".to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(SavePayload {
            has_standard_environment: !self.state.env_tokens.is_empty(),
            rng_state: self.state.rng.state(),
            cost_remaining: self.state.cost_remaining,
            cost_budget: self.state.cost_budget,
            cost_budget_configured: self.state.cost_budget_configured,
            cost_used: self.state.cost_used,
            strings: self.strings,
            bytecode: self.bytecode,
            upvalues,
            objects,
            user_globals,
        })
    }

    fn encode_root(&mut self, val: Val, segment: PathSegment) -> Result<SavedVal, SaveError> {
        let root = self.roots.len();
        self.roots.push(None);
        let path = self.push_path(None, segment);
        self.run_tasks(vec![EncodeTask::Value {
            val,
            path,
            destination: ValueDestination::Root(root),
        }])?;
        self.roots[root]
            .take()
            .ok_or_else(|| SaveError::EncodeError("unfilled root slot".to_string()))
    }

    fn run_tasks(&mut self, mut tasks: Vec<EncodeTask>) -> Result<(), SaveError> {
        while let Some(task) = tasks.pop() {
            match task {
                EncodeTask::Value {
                    val,
                    path,
                    destination,
                } => {
                    let saved = match val {
                        Val::Nil => Ok(SavedVal::Nil),
                        Val::Bool(b) => Ok(SavedVal::Bool(b)),
                        Val::Num(n) => Ok(SavedVal::Num(n.to_bits())),
                        Val::Str(ptr) => {
                            let bytes = self.state.heap.get_string(ptr).to_vec();
                            let id = if let Some(id) = self.string_ids.get(&bytes) {
                                *id
                            } else {
                                let id = u32::try_from(self.strings.len()).map_err(|_| {
                                    SaveError::EncodeError("too many strings".to_string())
                                })?;
                                self.strings.push(bytes.clone());
                                self.string_ids.insert(bytes, id);
                                id
                            };
                            Ok(SavedVal::Str(id))
                        }
                        Val::RustFn(func) => {
                            let addr = func as usize;
                            let id =
                                self.state.rust_fn_ids_by_addr.get(&addr).ok_or_else(|| {
                                    SaveError::UnregisteredFunction {
                                        reachable_from: self.render_path(path),
                                    }
                                })?;
                            Ok(SavedVal::Fn(id.clone()))
                        }
                        Val::Obj(ptr) => {
                            if let Some(token) = self.env_reverse.get(&ptr) {
                                Ok(SavedVal::EnvObj(token.clone()))
                            } else {
                                let id = self.object_id(ptr, path, &mut tasks)?;
                                Ok(SavedVal::Obj(id))
                            }
                        }
                    }?;
                    self.write_value(destination, saved)?;
                }
                EncodeTask::Object { ptr, id, path } => {
                    self.expand_object(ptr, id, path, &mut tasks)?;
                }
                EncodeTask::Upvalue {
                    upvalue,
                    path,
                    object,
                    index,
                } => {
                    let key = upvalue.index();
                    let id = if let Some(id) = self.upvalue_ids.get(&key) {
                        *id
                    } else {
                        let id = u32::try_from(self.upvalues.len())
                            .map_err(|_| SaveError::EncodeError("too many upvalues".to_string()))?;
                        self.upvalue_ids.insert(key, id);
                        self.upvalues.push(None);
                        match self.state.upvalue_pool.get(upvalue) {
                            Upvalue::Closed(val) => tasks.push(EncodeTask::Value {
                                val: *val,
                                path,
                                destination: ValueDestination::Upvalue(id),
                            }),
                            Upvalue::Open(_) => return Err(SaveError::OpenUpvalueReachable),
                        }
                        id
                    };
                    let pending = self
                        .objects
                        .get_mut(object as usize)
                        .ok_or_else(|| SaveError::EncodeError("missing object slot".to_string()))?;
                    let Some(PendingObject::Closure { upvalues, .. }) = pending else {
                        return Err(SaveError::EncodeError(
                            "upvalue destination is not a closure".to_string(),
                        ));
                    };
                    let slot = upvalues.get_mut(index).ok_or_else(|| {
                        SaveError::EncodeError("missing closure upvalue slot".to_string())
                    })?;
                    *slot = Some(id);
                }
            }
        }
        Ok(())
    }

    fn object_id(
        &mut self,
        ptr: ObjectPtr,
        path: PathId,
        tasks: &mut Vec<EncodeTask>,
    ) -> Result<u32, SaveError> {
        if let Some(id) = self.object_ids.get(&ptr) {
            return Ok(*id);
        }
        let id = u32::try_from(self.objects.len())
            .map_err(|_| SaveError::EncodeError("too many objects".to_string()))?;
        self.object_ids.insert(ptr, id);
        self.objects.push(None);
        tasks.push(EncodeTask::Object { ptr, id, path });
        Ok(id)
    }

    fn expand_object(
        &mut self,
        ptr: ObjectPtr,
        id: u32,
        path: PathId,
        tasks: &mut Vec<EncodeTask>,
    ) -> Result<(), SaveError> {
        let pending = match &self.state.heap.get(ptr).raw {
            RawObject::Table(table) => {
                let entries = table.entries();
                let metatable = table.get_metatable();
                if let Some(mt) = metatable {
                    tasks.push(EncodeTask::Value {
                        val: Val::Obj(mt),
                        path: self.push_path(Some(path), PathSegment::Metatable),
                        destination: ValueDestination::Metatable { object: id },
                    });
                }
                for (idx, (key, value)) in entries.into_iter().enumerate().rev() {
                    tasks.push(EncodeTask::Value {
                        val: value,
                        path: self.push_path(Some(path), PathSegment::TableValue(idx)),
                        destination: ValueDestination::TableValue {
                            object: id,
                            entry: idx,
                        },
                    });
                    tasks.push(EncodeTask::Value {
                        val: key,
                        path: self.push_path(Some(path), PathSegment::TableKey(idx)),
                        destination: ValueDestination::TableKey {
                            object: id,
                            entry: idx,
                        },
                    });
                }
                PendingObject::Table {
                    entries: vec![(None, None); table.entries().len()],
                    metatable: metatable.map(|_| None),
                }
            }
            RawObject::LuaFn(closure) => {
                let chunk = self.encode_bytecode(&closure.bytecode)?;
                for (idx, uv_ref) in closure.upvalues.iter().copied().enumerate().rev() {
                    tasks.push(EncodeTask::Upvalue {
                        upvalue: uv_ref,
                        path: self.push_path(Some(path), PathSegment::Upvalue(idx)),
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
            .ok_or_else(|| SaveError::EncodeError("missing object slot".to_string()))?;
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
                    .ok_or_else(|| SaveError::EncodeError("missing root slot".to_string()))? =
                    Some(value);
            }
            ValueDestination::Upvalue(id) => {
                *self
                    .upvalues
                    .get_mut(id as usize)
                    .ok_or_else(|| SaveError::EncodeError("missing upvalue slot".to_string()))? =
                    Some(value);
            }
            ValueDestination::TableKey { object, entry }
            | ValueDestination::TableValue { object, entry } => {
                let pending = self
                    .objects
                    .get_mut(object as usize)
                    .ok_or_else(|| SaveError::EncodeError("missing object slot".to_string()))?;
                let Some(PendingObject::Table { entries, .. }) = pending else {
                    return Err(SaveError::EncodeError(
                        "table destination is not a table".to_string(),
                    ));
                };
                let slot = entries.get_mut(entry).ok_or_else(|| {
                    SaveError::EncodeError("missing table entry slot".to_string())
                })?;
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
                    .ok_or_else(|| SaveError::EncodeError("missing object slot".to_string()))?;
                let Some(PendingObject::Table { metatable, .. }) = pending else {
                    return Err(SaveError::EncodeError(
                        "metatable destination is not a table".to_string(),
                    ));
                };
                *metatable = Some(Some(value));
            }
        }
        Ok(())
    }

    fn push_path(&mut self, parent: Option<PathId>, segment: PathSegment) -> PathId {
        let id = self.breadcrumbs.len();
        self.breadcrumbs.push(Breadcrumb { parent, segment });
        id
    }
    fn render_path(&self, path: PathId) -> String {
        let mut ids = Vec::new();
        let mut current = Some(path);
        while let Some(id) = current {
            ids.push(id);
            current = self.breadcrumbs[id].parent;
        }
        let mut text = String::new();
        for id in ids.into_iter().rev() {
            match &self.breadcrumbs[id].segment {
                PathSegment::Global(name) => {
                    text.push_str("global ");
                    text.push_str(name);
                }
                PathSegment::TableKey(index) => text.push_str(&format!(".key[{index}]")),
                PathSegment::TableValue(index) => text.push_str(&format!("[{index}]")),
                PathSegment::Metatable => text.push_str(".metatable"),
                PathSegment::Upvalue(index) => text.push_str(&format!(".upvalue[{index}]")),
            }
        }
        text
    }
    fn finish_object(object: PendingObject) -> Result<SavedObject, SaveError> {
        match object {
            PendingObject::Table { entries, metatable } => Ok(SavedObject::Table {
                entries: entries
                    .into_iter()
                    .map(|(key, value)| {
                        Ok((
                            key.ok_or_else(|| {
                                SaveError::EncodeError("unfilled table key slot".to_string())
                            })?,
                            value.ok_or_else(|| {
                                SaveError::EncodeError("unfilled table value slot".to_string())
                            })?,
                        ))
                    })
                    .collect::<Result<Vec<_>, SaveError>>()?,
                metatable: match metatable {
                    None => None,
                    Some(Some(value)) => Some(value),
                    Some(None) => {
                        return Err(SaveError::EncodeError(
                            "unfilled metatable slot".to_string(),
                        ));
                    }
                },
            }),
            PendingObject::Closure { chunk, upvalues } => Ok(SavedObject::Closure {
                chunk,
                upvalues: upvalues
                    .into_iter()
                    .map(|value| {
                        value.ok_or_else(|| {
                            SaveError::EncodeError("unfilled closure upvalue slot".to_string())
                        })
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
            .map_err(|_| SaveError::EncodeError("too many chunks".to_string()))?;
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
            num_params: 0,
            num_locals: 0,
            nested: Vec::new(),
            upvalues: Vec::new(),
            is_vararg: false,
            name: None,
            source: None,
            line_info: Vec::new(),
        });

        let mut nested = Vec::with_capacity(bc.nested.len());
        for child in &bc.nested {
            nested.push(self.encode_bytecode(child)?);
        }
        self.bytecode[id as usize] = SavedBytecode {
            code: bc.code.iter().map(|inst| inst.raw()).collect(),
            number_literals: bc.number_literals.iter().map(|n| n.to_bits()).collect(),
            string_literals: bc.string_literals.clone(),
            table_templates: bc.table_templates.clone(),
            global_cache_slots: bc.global_cache_slots,
            field_cache_slots: bc.field_cache_slots,
            set_field_cache_slots: bc.set_field_cache_slots,
            num_params: bc.num_params,
            num_locals: bc.num_locals,
            nested,
            upvalues: bc
                .upvalues
                .iter()
                .map(|uv| match uv {
                    UpvalueDesc::Local(idx) => SavedUpvalueDesc::Local(*idx),
                    UpvalueDesc::Upvalue(idx) => SavedUpvalueDesc::Upvalue(*idx),
                })
                .collect(),
            is_vararg: bc.is_vararg,
            name: bc.name.clone(),
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
    pub fn save_state(&self) -> Result<SaveState, SaveError> {
        self.validate_quiescent()?;
        let payload = SaveBuilder::new(self).finish()?;
        let mut encoder = Encoder::new();
        encoder.write_magic_and_version();
        // Diagnostic metadata only. FORMAT_VERSION (in the magic block) is the
        // hard compatibility gate; this human-readable crate version records
        // which build produced the snapshot for debugging and is intentionally
        // not compared on load (L10).
        encoder.write_bytes(env!("CARGO_PKG_VERSION").as_bytes())?;
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
    /// registered host functions exist before saved values are resolved.
    pub fn load_state(
        bytes: &[u8],
        callbacks: Box<dyn HostCallbacks + Send>,
        setup: impl FnOnce(&mut State),
    ) -> Result<State, LoadError> {
        let mut decoder = Decoder::new(bytes);
        decoder.read_magic_and_version()?;
        // Read and discard the diagnostic crate-version string. Compatibility is
        // enforced solely by FORMAT_VERSION in read_magic_and_version above; this
        // string is metadata, not a compat gate, so it is deliberately not
        // compared (L10).
        let _engine_version = decoder.read_bytes()?;
        let payload = verify::verify_payload(SavePayload::decode(&mut decoder)?)?;
        decoder.finish()?;

        let mut state = if payload.has_standard_environment() {
            State::with_callbacks(callbacks)
        } else {
            State::empty_with_callbacks(callbacks)
        };
        setup(&mut state);
        materialize_payload(&mut state, payload)?;
        Ok(state)
    }

    fn validate_quiescent(&self) -> Result<(), SaveError> {
        if self.stack.is_empty()
            && self.stack_bottom == 0
            && self.string_literals.is_empty()
            && self.transient_roots.is_empty()
            && self.open_upvalues.is_empty()
            && self.vararg_call_bases.is_empty()
            && self.table_constructor_bases.is_empty()
            && self.call_stack.is_empty()
            && self.metamethod_depth == 0
            && self.call_depth == 0
        {
            Ok(())
        } else {
            Err(SaveError::NotQuiescent)
        }
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
fn build_env_forward(state: &State) -> BTreeMap<String, ObjectPtr> {
    state
        .env_tokens
        .iter()
        .map(|(ptr, token)| (token.clone(), *ptr))
        .collect()
}

fn materialize_payload(
    state: &mut State,
    payload: verify::VerifiedSavePayload,
) -> Result<(), LoadError> {
    let env_forward = build_env_forward(state);
    let fn_forward = state.rust_fns_by_id.clone();

    let bytecode = materialize_bytecode(&payload)?;
    let payload = payload.into_inner();

    let mut strings = Vec::with_capacity(payload.strings.len());
    for bytes in &payload.strings {
        strings.push(state.heap.alloc_string(bytes));
    }

    let mut upvalues = Vec::with_capacity(payload.upvalues.len());
    for _ in &payload.upvalues {
        upvalues.push(state.upvalue_pool.alloc_closed_nil());
    }

    let mut objects = Vec::with_capacity(payload.objects.len());
    for obj in &payload.objects {
        let ptr = match obj {
            // Tables are filled (and sized) in the second pass via
            // clear_and_insert_entries, so allocate an empty shell here. None of
            // these allocators run GC, which is what keeps the half-built graph
            // alive until the final collect.
            SavedObject::Table { .. } => state.heap.alloc_table(),
            SavedObject::Closure {
                chunk,
                upvalues: saved_upvalues,
            } => {
                let bc = Arc::clone(
                    bytecode
                        .get(*chunk as usize)
                        .ok_or(LoadError::CorruptArena)?,
                );
                let mut refs = Vec::with_capacity(saved_upvalues.len());
                for uv_id in saved_upvalues {
                    refs.push(*upvalues_ref(&upvalues, *uv_id)?);
                }
                state.heap.alloc_lua_fn(bc, refs)
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
            SavedObject::Table { entries, metatable } => {
                let entries = entries
                    .iter()
                    .map(|(key, value)| Ok((decode_val(key, &ctx)?, decode_val(value, &ctx)?)))
                    .collect::<Result<Vec<_>, LoadError>>()?;
                let ptr = objects[idx];
                let table = state.heap.as_table(ptr).ok_or(LoadError::CorruptArena)?;
                table
                    .clear_and_insert_entries(entries)
                    .map_err(|_| LoadError::CorruptArena)?;
                if let Some(mt) = metatable {
                    match decode_val(mt, &ctx)? {
                        Val::Obj(ptr) => table.set_metatable(Some(ptr)),
                        _ => return Err(LoadError::CorruptArena),
                    }
                }
            }
            SavedObject::Closure { .. } => {}
        }
    }

    for (name, saved) in payload.user_globals {
        let name = String::from_utf8(name)
            .map_err(|_| LoadError::DecodeError("global name is not UTF-8".to_string()))?;
        let val = decode_val(&saved, &ctx)?;
        state.set_global_value_owned(name, val);
    }
    state.rng = VmRng::from_state(payload.rng_state);
    state.cost_remaining = payload.cost_remaining;
    state.cost_budget = payload.cost_budget;
    state.cost_budget_configured = payload.cost_budget_configured;
    state.cost_used = payload.cost_used;
    state.stack.clear();
    state.stack_bottom = 0;
    state.string_literals.clear();
    state.transient_roots.values.clear();
    state.transient_roots.suspended_envs.clear();
    state.open_upvalues.clear();
    state.vararg_call_bases.clear();
    state.table_constructor_bases.clear();
    state.call_stack.clear();
    state.metamethod_depth = 0;
    state.call_depth = 0;
    state.current_source = None;
    state.registry.clear();
    state.gc_collect();
    Ok(())
}

fn upvalues_ref(upvalues: &[UpvalueRef], id: u32) -> Result<&UpvalueRef, LoadError> {
    upvalues.get(id as usize).ok_or(LoadError::CorruptArena)
}

fn object_ref(objects: &[ObjectPtr], id: u32) -> Result<&ObjectPtr, LoadError> {
    objects.get(id as usize).ok_or(LoadError::CorruptArena)
}

struct DecodeCtx<'a> {
    strings: &'a [super::object::StringPtr],
    objects: &'a [ObjectPtr],
    env_forward: &'a BTreeMap<String, ObjectPtr>,
    fn_forward: &'a BTreeMap<String, RustFunc>,
}

fn decode_val(saved: &SavedVal, ctx: &DecodeCtx<'_>) -> Result<Val, LoadError> {
    match saved {
        SavedVal::Nil => Ok(Val::Nil),
        SavedVal::Bool(b) => Ok(Val::Bool(*b)),
        SavedVal::Num(bits) => Ok(Val::Num(f64::from_bits(*bits))),
        SavedVal::Str(id) => Ok(Val::Str(
            *ctx.strings
                .get(*id as usize)
                .ok_or(LoadError::CorruptArena)?,
        )),
        SavedVal::Obj(id) => Ok(Val::Obj(*object_ref(ctx.objects, *id)?)),
        SavedVal::Fn(id) => ctx
            .fn_forward
            .get(id)
            .copied()
            .map(Val::RustFn)
            .ok_or_else(|| LoadError::UnknownFunction(id.clone())),
        SavedVal::EnvObj(token) => ctx
            .env_forward
            .get(token)
            .copied()
            .map(Val::Obj)
            .ok_or_else(|| LoadError::UnknownEnvObject(token.clone())),
    }
}

fn materialize_bytecode(
    payload: &verify::VerifiedSavePayload,
) -> Result<Vec<Arc<Bytecode>>, LoadError> {
    let saved = payload.bytecode();
    let mut out: Vec<Option<Arc<Bytecode>>> = vec![None; saved.len()];
    let mut visiting = vec![false; saved.len()];
    for idx in 0..saved.len() {
        let bc = build_bytecode(idx, saved, &mut out, &mut visiting)?;
        out[idx] = Some(bc);
    }
    out.into_iter()
        .map(|bc| bc.ok_or(LoadError::CorruptArena))
        .collect()
}

fn build_bytecode(
    idx: usize,
    saved: &[SavedBytecode],
    out: &mut [Option<Arc<Bytecode>>],
    visiting: &mut [bool],
) -> Result<Arc<Bytecode>, LoadError> {
    if let Some(bc) = &out[idx] {
        return Ok(Arc::clone(bc));
    }
    let Some(is_visiting) = visiting.get_mut(idx) else {
        return Err(LoadError::CorruptArena);
    };
    if *is_visiting {
        return Err(LoadError::CorruptArena);
    }
    *is_visiting = true;
    let src = saved.get(idx).ok_or(LoadError::CorruptArena)?;
    let mut nested = Vec::with_capacity(src.nested.len());
    for child in &src.nested {
        nested.push(build_bytecode(*child as usize, saved, out, visiting)?);
    }
    let bc = Arc::new(Bytecode {
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
        num_params: src.num_params,
        num_locals: src.num_locals,
        nested,
        upvalues: src
            .upvalues
            .iter()
            .map(|uv| match uv {
                SavedUpvalueDesc::Local(idx) => UpvalueDesc::Local(*idx),
                SavedUpvalueDesc::Upvalue(idx) => UpvalueDesc::Upvalue(*idx),
            })
            .collect(),
        is_vararg: src.is_vararg,
        name: src.name.clone(),
        source: src.source.clone(),
        line_info: src.line_info.clone(),
    });
    out[idx] = Some(Arc::clone(&bc));
    visiting[idx] = false;
    Ok(bc)
}

struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn write_magic_and_version(&mut self) {
        self.bytes.extend_from_slice(&MAGIC);
        self.write_u16(FORMAT_VERSION);
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

    fn write_len(&mut self, len: usize) -> Result<(), SaveError> {
        let len = u32::try_from(len)
            .map_err(|_| SaveError::EncodeError("vector too long".to_string()))?;
        self.write_u32(len);
        Ok(())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), SaveError> {
        self.write_len(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn write_string(&mut self, s: &str) -> Result<(), SaveError> {
        self.write_bytes(s.as_bytes())
    }

    fn write_option_string(&mut self, s: &Option<String>) -> Result<(), SaveError> {
        match s {
            Some(s) => {
                self.write_bool(true);
                self.write_string(s)?;
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

    fn finish(&self) -> Result<(), LoadError> {
        if self.pos == self.bytes.len() {
            Ok(())
        } else {
            Err(LoadError::DecodeError("trailing bytes".to_string()))
        }
    }

    /// Bytes not yet consumed. Used to bound speculative allocations so a
    /// corrupt length prefix cannot trigger a huge reservation.
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    fn read_magic_and_version(&mut self) -> Result<(), LoadError> {
        let magic = self.read_exact(4)?;
        if magic != MAGIC {
            return Err(LoadError::BadMagic);
        }
        if self.read_u16()? != FORMAT_VERSION {
            return Err(LoadError::UnsupportedVersion);
        }
        Ok(())
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8], LoadError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| LoadError::DecodeError("length overflow".to_string()))?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| LoadError::DecodeError("truncated payload".to_string()))?;
        self.pos = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, LoadError> {
        Ok(self.read_exact(1)?[0])
    }

    fn read_bool(&mut self) -> Result<bool, LoadError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(LoadError::DecodeError("invalid bool".to_string())),
        }
    }

    fn read_u16(&mut self) -> Result<u16, LoadError> {
        let bytes = self.read_exact(2)?;
        let mut out = [0; 2];
        out.copy_from_slice(bytes);
        Ok(u16::from_le_bytes(out))
    }

    fn read_u32(&mut self) -> Result<u32, LoadError> {
        let bytes = self.read_exact(4)?;
        let mut out = [0; 4];
        out.copy_from_slice(bytes);
        Ok(u32::from_le_bytes(out))
    }

    fn read_u64(&mut self) -> Result<u64, LoadError> {
        let bytes = self.read_exact(8)?;
        let mut out = [0; 8];
        out.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(out))
    }

    fn read_i64(&mut self) -> Result<i64, LoadError> {
        let bytes = self.read_exact(8)?;
        let mut out = [0; 8];
        out.copy_from_slice(bytes);
        Ok(i64::from_le_bytes(out))
    }

    fn read_len(&mut self) -> Result<usize, LoadError> {
        Ok(self.read_u32()? as usize)
    }

    fn read_bytes(&mut self) -> Result<Vec<u8>, LoadError> {
        let len = self.read_len()?;
        Ok(self.read_exact(len)?.to_vec())
    }

    fn read_string(&mut self) -> Result<String, LoadError> {
        String::from_utf8(self.read_bytes()?)
            .map_err(|_| LoadError::DecodeError("string is not UTF-8".to_string()))
    }

    fn read_option_string(&mut self) -> Result<Option<String>, LoadError> {
        if self.read_bool()? {
            Ok(Some(self.read_string()?))
        } else {
            Ok(None)
        }
    }
}

impl SavePayload {
    fn encode(&self, out: &mut Encoder) -> Result<(), SaveError> {
        out.write_u8(u8::from(self.has_standard_environment));
        out.write_u64(self.rng_state);
        out.write_i64(self.cost_remaining);
        out.write_i64(self.cost_budget);
        out.write_bool(self.cost_budget_configured);
        out.write_u64(self.cost_used);
        write_vec(out, &self.strings, |out, bytes| out.write_bytes(bytes))?;
        write_vec(out, &self.bytecode, |out, item| item.encode(out))?;
        write_vec(out, &self.upvalues, |out, item| item.encode(out))?;
        write_vec(out, &self.objects, |out, item| item.encode(out))?;
        write_vec(out, &self.user_globals, |out, (name, val)| {
            out.write_bytes(name)?;
            val.encode(out)
        })?;
        Ok(())
    }

    fn decode(input: &mut Decoder<'_>) -> Result<Self, LoadError> {
        Ok(Self {
            has_standard_environment: input.read_u8()? != 0,
            rng_state: input.read_u64()?,
            cost_remaining: input.read_i64()?,
            cost_budget: input.read_i64()?,
            cost_budget_configured: input.read_bool()?,
            cost_used: input.read_u64()?,
            strings: read_vec(input, Decoder::read_bytes)?,
            bytecode: read_vec(input, SavedBytecode::decode)?,
            upvalues: read_vec(input, SavedVal::decode)?,
            objects: read_vec(input, SavedObject::decode)?,
            user_globals: read_vec(input, |input| {
                Ok((input.read_bytes()?, SavedVal::decode(input)?))
            })?,
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
                out.write_string(id)?;
            }
            SavedVal::EnvObj(token) => {
                out.write_u8(7);
                out.write_string(token)?;
            }
        }
        Ok(())
    }

    fn decode(input: &mut Decoder<'_>) -> Result<Self, LoadError> {
        match input.read_u8()? {
            0 => Ok(SavedVal::Nil),
            1 => Ok(SavedVal::Bool(false)),
            2 => Ok(SavedVal::Bool(true)),
            3 => Ok(SavedVal::Num(input.read_u64()?)),
            4 => Ok(SavedVal::Str(input.read_u32()?)),
            5 => Ok(SavedVal::Obj(input.read_u32()?)),
            6 => Ok(SavedVal::Fn(input.read_string()?)),
            7 => Ok(SavedVal::EnvObj(input.read_string()?)),
            _ => Err(LoadError::DecodeError("invalid value tag".to_string())),
        }
    }
}

impl SavedObject {
    fn encode(&self, out: &mut Encoder) -> Result<(), SaveError> {
        match self {
            SavedObject::Table { entries, metatable } => {
                out.write_u8(0);
                write_vec(out, entries, |out, (key, value)| {
                    key.encode(out)?;
                    value.encode(out)
                })?;
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
                write_vec(out, upvalues, |out, id| {
                    out.write_u32(*id);
                    Ok(())
                })?;
            }
        }
        Ok(())
    }

    fn decode(input: &mut Decoder<'_>) -> Result<Self, LoadError> {
        match input.read_u8()? {
            0 => {
                let entries = read_vec(input, |input| {
                    Ok((SavedVal::decode(input)?, SavedVal::decode(input)?))
                })?;
                let metatable = if input.read_bool()? {
                    Some(SavedVal::decode(input)?)
                } else {
                    None
                };
                Ok(SavedObject::Table { entries, metatable })
            }
            1 => Ok(SavedObject::Closure {
                chunk: input.read_u32()?,
                upvalues: read_vec(input, Decoder::read_u32)?,
            }),
            _ => Err(LoadError::DecodeError("invalid object tag".to_string())),
        }
    }
}

impl SavedBytecode {
    fn encode(&self, out: &mut Encoder) -> Result<(), SaveError> {
        write_vec(out, &self.code, |out, raw| {
            out.write_u32(*raw);
            Ok(())
        })?;
        write_vec(out, &self.number_literals, |out, bits| {
            out.write_u64(*bits);
            Ok(())
        })?;
        write_vec(out, &self.string_literals, |out, bytes| {
            out.write_bytes(bytes)
        })?;
        write_vec(out, &self.table_templates, |out, bytes| {
            out.write_bytes(bytes)
        })?;
        out.write_u16(self.global_cache_slots);
        out.write_u16(self.field_cache_slots);
        out.write_u8(self.set_field_cache_slots);
        out.write_u8(self.num_params);
        out.write_u8(self.num_locals);
        write_vec(out, &self.nested, |out, id| {
            out.write_u32(*id);
            Ok(())
        })?;
        write_vec(out, &self.upvalues, |out, item| item.encode(out))?;
        out.write_bool(self.is_vararg);
        out.write_option_string(&self.name)?;
        out.write_option_string(&self.source)?;
        write_vec(out, &self.line_info, |out, line| {
            out.write_u32(*line);
            Ok(())
        })?;
        Ok(())
    }

    fn decode(input: &mut Decoder<'_>) -> Result<Self, LoadError> {
        Ok(Self {
            code: read_vec(input, Decoder::read_u32)?,
            number_literals: read_vec(input, Decoder::read_u64)?,
            string_literals: read_vec(input, Decoder::read_bytes)?,
            table_templates: read_vec(input, Decoder::read_bytes)?,
            global_cache_slots: input.read_u16()?,
            field_cache_slots: input.read_u16()?,
            set_field_cache_slots: input.read_u8()?,
            num_params: input.read_u8()?,
            num_locals: input.read_u8()?,
            nested: read_vec(input, Decoder::read_u32)?,
            upvalues: read_vec(input, SavedUpvalueDesc::decode)?,
            is_vararg: input.read_bool()?,
            name: input.read_option_string()?,
            source: input.read_option_string()?,
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

    fn decode(input: &mut Decoder<'_>) -> Result<Self, LoadError> {
        match input.read_u8()? {
            0 => Ok(SavedUpvalueDesc::Local(input.read_u8()?)),
            1 => Ok(SavedUpvalueDesc::Upvalue(input.read_u8()?)),
            _ => Err(LoadError::DecodeError("invalid upvalue tag".to_string())),
        }
    }
}

fn write_vec<T>(
    out: &mut Encoder,
    items: &[T],
    mut write_item: impl FnMut(&mut Encoder, &T) -> Result<(), SaveError>,
) -> Result<(), SaveError> {
    out.write_len(items.len())?;
    for item in items {
        write_item(out, item)?;
    }
    Ok(())
}

fn read_vec<'a, T>(
    input: &mut Decoder<'a>,
    mut read_item: impl FnMut(&mut Decoder<'a>) -> Result<T, LoadError>,
) -> Result<Vec<T>, LoadError> {
    let len = input.read_len()?;
    // Each element consumes at least one byte, so the true count cannot exceed
    // the bytes left. Cap the reservation to defang a forged length prefix.
    let mut out = Vec::with_capacity(len.min(input.remaining()));
    for _ in 0..len {
        out.push(read_item(input)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instr::{ArgCount, RetCount};

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

    fn valid_saved_bytecode() -> SavedBytecode {
        SavedBytecode {
            code: vec![Instr::ret(RetCount::Fixed(0)).raw()],
            number_literals: Vec::new(),
            string_literals: Vec::new(),
            table_templates: Vec::new(),
            global_cache_slots: 0,
            field_cache_slots: 0,
            set_field_cache_slots: 0,
            num_params: 0,
            num_locals: 0,
            nested: Vec::new(),
            upvalues: Vec::new(),
            is_vararg: false,
            name: None,
            source: None,
            line_info: vec![1],
        }
    }

    fn load_bytecode(
        bytecode: Vec<SavedBytecode>,
        objects: Vec<SavedObject>,
    ) -> Result<State, LoadError> {
        let payload = SavePayload {
            has_standard_environment: false,
            rng_state: 0,
            cost_remaining: 0,
            cost_budget: 0,
            cost_budget_configured: false,
            cost_used: 0,
            strings: Vec::new(),
            bytecode,
            upvalues: Vec::new(),
            objects,
            user_globals: Vec::new(),
        };
        let mut encoder = Encoder::new();
        encoder.write_magic_and_version();
        encoder
            .write_bytes(env!("CARGO_PKG_VERSION").as_bytes())
            .map_err(|error| LoadError::DecodeError(error.to_string()))?;
        payload
            .encode(&mut encoder)
            .map_err(|error| LoadError::DecodeError(error.to_string()))?;
        State::load_state(&encoder.finish(), Box::new(crate::DefaultCallbacks), |_| {})
    }

    fn rejected_bytecode(bytecode: Vec<SavedBytecode>, objects: Vec<SavedObject>) -> LoadError {
        match load_bytecode(bytecode, objects) {
            Ok(_) => panic!("fixture must be rejected"),
            Err(error) => error,
        }
    }

    fn assert_invalid(error: LoadError, chunk: u32, instruction: Option<u32>) {
        let LoadError::InvalidBytecode {
            chunk: actual_chunk,
            instruction: actual_instruction,
            reason,
        } = error
        else {
            panic!("fixture must return InvalidBytecode");
        };
        assert_eq!(actual_chunk, chunk);
        assert_eq!(actual_instruction, instruction);
        drop(reason);
    }

    #[test]
    fn verifier_rejects_instruction_and_cache_corruption() {
        let mut empty = valid_saved_bytecode();
        empty.code.clear();
        assert!(matches!(
            rejected_bytecode(vec![empty], Vec::new()),
            LoadError::InvalidBytecode { .. }
        ));

        let mut no_return = valid_saved_bytecode();
        no_return.code[0] = Instr::push_nil().raw();
        assert!(matches!(
            rejected_bytecode(vec![no_return], Vec::new()),
            LoadError::InvalidBytecode { .. }
        ));

        let mut bad_jump = valid_saved_bytecode();
        bad_jump.code.insert(0, Instr::jump(1).raw());
        bad_jump.line_info.insert(0, 1);
        assert!(matches!(
            rejected_bytecode(vec![bad_jump], Vec::new()),
            LoadError::InvalidBytecode { .. }
        ));

        let mut bad_literal = valid_saved_bytecode();
        bad_literal.code.insert(0, Instr::push_num(0).raw());
        bad_literal.line_info.insert(0, 1);
        assert!(matches!(
            rejected_bytecode(vec![bad_literal], Vec::new()),
            LoadError::InvalidBytecode { .. }
        ));

        let mut bad_cache = valid_saved_bytecode();
        bad_cache.string_literals.push(b"x".to_vec());
        bad_cache
            .code
            .insert(0, Instr::get_global_cached(0, 1).raw());
        bad_cache.line_info.insert(0, 1);
        assert!(matches!(
            rejected_bytecode(vec![bad_cache], Vec::new()),
            LoadError::InvalidBytecode { .. }
        ));
    }

    #[test]
    fn verifier_rejects_bad_graphs_and_closure_captures() {
        let mut missing_child = valid_saved_bytecode();
        missing_child.nested.push(1);
        assert_eq!(
            rejected_bytecode(vec![missing_child], Vec::new()),
            LoadError::CorruptArena
        );

        let mut cycle = valid_saved_bytecode();
        cycle.nested.push(0);
        assert!(matches!(
            rejected_bytecode(vec![cycle], Vec::new()),
            LoadError::InvalidBytecode { .. }
        ));

        let mut chunks = vec![valid_saved_bytecode(); 201];
        for (idx, chunk) in chunks.iter_mut().take(200).enumerate() {
            chunk.nested.push((idx + 1) as u32);
        }
        assert!(matches!(
            rejected_bytecode(chunks, Vec::new()),
            LoadError::InvalidBytecode { .. }
        ));

        let chunk = valid_saved_bytecode();
        let object = SavedObject::Closure {
            chunk: 0,
            upvalues: vec![0],
        };
        assert!(matches!(
            rejected_bytecode(vec![chunk], vec![object]),
            LoadError::InvalidBytecode { .. }
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
        binary_literal.code.insert(0, Instr::push_string(0).raw());
        binary_literal.line_info.insert(0, 1);
        assert!(load_bytecode(vec![binary_literal], Vec::new()).is_ok());

        let mut bad_global_name = valid_saved_bytecode();
        bad_global_name.string_literals.push(vec![255]);
        bad_global_name
            .code
            .insert(0, Instr::get_global_cached(0, 0).raw());
        bad_global_name.global_cache_slots = 1;
        bad_global_name.line_info.insert(0, 1);
        assert_invalid(
            rejected_bytecode(vec![bad_global_name], Vec::new()),
            0,
            Some(0),
        );

        let mut bad_builtin = valid_saved_bytecode();
        bad_builtin
            .code
            .insert(0, Instr::op_a(Instr::OP_GET_BUILTIN, 255).raw());
        bad_builtin.line_info.insert(0, 1);
        assert_invalid(rejected_bytecode(vec![bad_builtin], Vec::new()), 0, Some(0));

        let mut bad_template = valid_saved_bytecode();
        bad_template
            .code
            .insert(0, Instr::new_table_template(0).raw());
        bad_template.line_info.insert(0, 1);
        assert_invalid(
            rejected_bytecode(vec![bad_template], Vec::new()),
            0,
            Some(0),
        );

        let mut bad_template_key = valid_saved_bytecode();
        bad_template_key.table_templates.push(vec![0]);
        assert_invalid(
            rejected_bytecode(vec![bad_template_key], Vec::new()),
            0,
            None,
        );

        let mut bad_local = valid_saved_bytecode();
        bad_local.code.insert(0, Instr::get_local(0).raw());
        bad_local.line_info.insert(0, 1);
        assert_invalid(rejected_bytecode(vec![bad_local], Vec::new()), 0, Some(0));

        let mut bad_upvalue = valid_saved_bytecode();
        bad_upvalue.code.insert(0, Instr::get_upvalue(0).raw());
        bad_upvalue.line_info.insert(0, 1);
        assert_invalid(rejected_bytecode(vec![bad_upvalue], Vec::new()), 0, Some(0));

        let mut bad_reserved = valid_saved_bytecode();
        bad_reserved.code[0] = Instr::op_a(Instr::OP_PUSH_NIL, 1).raw();
        assert_invalid(
            rejected_bytecode(vec![bad_reserved], Vec::new()),
            0,
            Some(0),
        );

        let mut unknown = valid_saved_bytecode();
        unknown.code[0] = Instr::op(255).raw();
        assert_invalid(rejected_bytecode(vec![unknown], Vec::new()), 0, Some(0));
    }

    #[test]
    fn verifier_covers_cache_metadata_nested_chunks_and_graph_cycles() {
        let mut bad_cache_count = valid_saved_bytecode();
        bad_cache_count.string_literals.push(b"x".to_vec());
        bad_cache_count
            .code
            .insert(0, Instr::get_global_cached(0, 0).raw());
        bad_cache_count.line_info.insert(0, 1);
        assert_invalid(
            rejected_bytecode(vec![bad_cache_count], Vec::new()),
            0,
            None,
        );

        let mut bad_line_info = valid_saved_bytecode();
        bad_line_info.code.insert(0, Instr::push_nil().raw());
        assert_invalid(rejected_bytecode(vec![bad_line_info], Vec::new()), 0, None);

        let mut negative_jump = valid_saved_bytecode();
        negative_jump.code.insert(0, Instr::jump(-2).raw());
        negative_jump.line_info.insert(0, 1);
        assert_invalid(
            rejected_bytecode(vec![negative_jump], Vec::new()),
            0,
            Some(0),
        );

        let mut parent = valid_saved_bytecode();
        parent.nested.push(1);
        let mut child = valid_saved_bytecode();
        child.upvalues.push(SavedUpvalueDesc::Local(0));
        assert_invalid(rejected_bytecode(vec![parent, child], Vec::new()), 0, None);

        let mut first = valid_saved_bytecode();
        first.nested.push(1);
        let mut second = valid_saved_bytecode();
        second.nested.push(0);
        assert_invalid(rejected_bytecode(vec![first, second], Vec::new()), 0, None);

        let mut nested_bad_opcode = valid_saved_bytecode();
        nested_bad_opcode.code[0] = Instr::op(255).raw();
        assert_invalid(
            rejected_bytecode(vec![valid_saved_bytecode(), nested_bad_opcode], Vec::new()),
            1,
            Some(0),
        );
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
                entries: vec![
                    (SavedVal::Str(0), SavedVal::Num(1.0f64.to_bits())),
                    (SavedVal::Obj(1), SavedVal::Bool(true)),
                ],
                metatable: None,
            }
        );
        roundtrip!(
            SavedObject,
            SavedObject::Table {
                entries: vec![],
                metatable: Some(SavedVal::Obj(4)),
            }
        );
        roundtrip!(
            SavedObject,
            SavedObject::Table {
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
    fn saved_bytecode_round_trips() {
        roundtrip!(
            SavedBytecode,
            SavedBytecode {
                code: vec![1, 2, 3, 0xdead_beef],
                number_literals: vec![1.5f64.to_bits(), f64::NAN.to_bits()],
                string_literals: vec![b"hi".to_vec(), vec![0xff, 0x00, 0xfe]],
                table_templates: vec![vec![0, 1], vec![]],
                global_cache_slots: 7,
                field_cache_slots: 9,
                set_field_cache_slots: 3,
                num_params: 2,
                num_locals: 5,
                nested: vec![0, 1, 2],
                upvalues: vec![SavedUpvalueDesc::Local(1), SavedUpvalueDesc::Upvalue(2)],
                is_vararg: true,
                name: Some("f".to_string()),
                source: None,
                line_info: vec![10, 11, 12],
            }
        );
    }

    #[test]
    fn save_walker_object_count_matches_gc_live_minus_env() {
        let mut state = State::new();
        state
            .load_string(
                r#"
                a = { 1, 2, 3 }
                a.b = {}
                a.b.back = a
                keep = function() return a end
                m = math
                junk = { x = {} }
                junk = nil
            "#,
            )
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
