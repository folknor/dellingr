//! The typed failures of save and load. No variant carries text that
//! describes what went wrong: every reason is an enum a host can match on,
//! and the strings that remain are data (function ids, environment tokens,
//! rendered value paths). `Display` renders each reason as one sentence.

use std::convert::Infallible;
use std::fmt;

use crate::compiler::verify::ValidationReason;
use crate::error::{FloatEnvironment, SnapshotTableFault};
use crate::limits::tuning::SAVE_LOGGED_USER_TEXT_BYTES as MAX_LOGGED_USER_TEXT_BYTES;
use crate::limits::{
    MAX_CHUNK_NAME_BYTES, MAX_ENGINE_VERSION_BYTES, MAX_ENVIRONMENT_TOKEN_BYTES,
    MAX_SAVE_IDENTIFIER_BYTES, MAX_SOURCE_NAME_BYTES, MAX_STRING_BYTES,
};

use super::super::table::{InvalidTableKey, TableError};

/// Errors produced while saving a [`State`](crate::State).
///
/// Save and load failures are returned directly and do not call
/// [`HostCallbacks::on_error`](crate::HostCallbacks::on_error).
#[non_exhaustive]
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SaveError {
    /// The VM has an active stack/call frame/open upvalue and cannot be snapshotted.
    NotQuiescent,
    /// A panic unwound through the State, so its contents no longer
    /// describe a consistent VM and are not snapshotted.
    StatePoisoned,
    /// The calling thread's floating-point environment is unsupported (see
    /// [`ErrorKind::UnsupportedFloatEnvironment`](crate::error::ErrorKind::UnsupportedFloatEnvironment));
    /// nothing was saved.
    UnsupportedFloatEnvironment(FloatEnvironment),
    /// A reachable Rust function has no stable save/load id.
    UnregisteredFunction {
        /// Best-effort path to the value that made the function reachable.
        reachable_from: String,
    },
    /// A reachable upvalue was still open.
    OpenUpvalueReachable,
    /// A save-time invariant failed inside the codec: a dellingr bug.
    Internal(SaveInternalError),
    /// A saved collection has more entries than its length prefix can hold.
    TooLarge {
        /// The collection that is too long.
        collection: SaveCollection,
        /// Its entry count.
        count: usize,
        /// The largest count the format can encode.
        limit: usize,
    },
    /// A length-limited byte field exceeds its limit.
    FieldSizeExceeded {
        /// The field.
        field: SnapshotField,
        /// Its length in bytes.
        size: usize,
        /// Its maximum length in bytes.
        limit: usize,
    },
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::NotQuiescent => write!(f, "state is not quiescent"),
            SaveError::StatePoisoned => write!(f, "state poisoned by a panic"),
            SaveError::UnsupportedFloatEnvironment(environment) => {
                write!(f, "unsupported floating-point environment ({environment})")
            }
            SaveError::UnregisteredFunction { reachable_from } => {
                f.write_str("unregistered Rust function reachable from ")?;
                write_bounded_user_text(f, reachable_from)
            }
            SaveError::OpenUpvalueReachable => write!(f, "open upvalue reachable during save"),
            SaveError::Internal(reason) => write!(f, "save encode invariant failed: {reason}"),
            SaveError::TooLarge {
                collection, count, ..
            } => {
                write!(
                    f,
                    "save {collection} count {count} exceeds the supported limit"
                )
            }
            SaveError::FieldSizeExceeded { field, size, limit } => {
                write!(f, "save {field} size {size} exceeds limit {limit}")
            }
        }
    }
}

impl std::error::Error for SaveError {}

/// The reason behind a [`SaveError::Internal`]: a dellingr bug, never a
/// property of the saved State. Opaque on purpose, like
/// [`InternalError`](crate::error::InternalError): hosts report these, they
/// do not branch on them.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SaveInternalError(pub(super) SaveFault);

impl fmt::Display for SaveInternalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Encoder invariants. Each slot the encoder reserves is filled before the
/// payload is finished, and each id it records names an entry of the kind
/// the reference needs.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SaveFault {
    UnfilledObjectSlot,
    UnfilledUpvalueSlot,
    UnfilledRootSlot,
    UnfilledTableKeySlot,
    UnfilledTableValueSlot,
    UnfilledMetatableSlot,
    UnfilledClosureUpvalueSlot,
    MissingObjectSlot,
    MissingRootSlot,
    MissingUpvalueSlot,
    MissingClosureUpvalueSlot,
    MissingTableEntrySlot,
    MissingEnvironmentBaseline,
    EnvironmentObjectNotTable,
    UpvalueDestinationNotClosure,
    TableDestinationNotTable,
    MetatableDestinationNotTable,
    UnregisteredFunctionPathNotFound,
}

impl fmt::Display for SaveFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnfilledObjectSlot => "unfilled object slot",
            Self::UnfilledUpvalueSlot => "unfilled upvalue slot",
            Self::UnfilledRootSlot => "unfilled root slot",
            Self::UnfilledTableKeySlot => "unfilled table key slot",
            Self::UnfilledTableValueSlot => "unfilled table value slot",
            Self::UnfilledMetatableSlot => "unfilled metatable slot",
            Self::UnfilledClosureUpvalueSlot => "unfilled closure upvalue slot",
            Self::MissingObjectSlot => "missing object slot",
            Self::MissingRootSlot => "missing root slot",
            Self::MissingUpvalueSlot => "missing upvalue slot",
            Self::MissingClosureUpvalueSlot => "missing closure upvalue slot",
            Self::MissingTableEntrySlot => "missing table entry slot",
            Self::MissingEnvironmentBaseline => "missing environment baseline",
            Self::EnvironmentObjectNotTable => "environment object is not a table",
            Self::UpvalueDestinationNotClosure => "upvalue destination is not a closure",
            Self::TableDestinationNotTable => "table destination is not a table",
            Self::MetatableDestinationNotTable => "metatable destination is not a table",
            Self::UnregisteredFunctionPathNotFound => {
                "unregistered function path was not found from its root"
            }
        })
    }
}

impl From<SaveFault> for SaveError {
    fn from(fault: SaveFault) -> Self {
        Self::Internal(SaveInternalError(fault))
    }
}

/// A length-prefixed collection in the save format, named by
/// [`SaveError::TooLarge`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SaveCollection {
    /// The interned string arena.
    Strings,
    /// The bytecode arena.
    Chunks,
    /// The closed upvalue arena.
    Upvalues,
    /// The object arena.
    Objects,
    /// The global table's pins.
    Globals,
    /// The environment deltas.
    EnvironmentDeltas,
    /// The rendered pointer identity records.
    FormatPointerIds,
    /// An environment delta's deleted keys.
    EnvironmentDeletedKeys,
    /// An environment delta's upserts.
    EnvironmentUpserts,
    /// An environment delta's key order.
    EnvironmentOrder,
    /// An environment delta's array slots.
    EnvironmentArray,
    /// A table's stored array slots.
    TableArraySlots,
    /// A table's stored entries.
    TableEntries,
    /// A closure's upvalue references.
    ClosureUpvalues,
    /// A function's instructions.
    Instructions,
    /// A function's number literals.
    NumberLiterals,
    /// A function's string literals.
    StringLiterals,
    /// A function's table templates.
    TableTemplates,
    /// One table template's keys.
    TableTemplateKeys,
    /// A function's nested functions.
    NestedChunks,
    /// A function's upvalue descriptors.
    UpvalueDescriptors,
    /// A function's per-instruction line numbers.
    LineInfo,
}

impl fmt::Display for SaveCollection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Strings => "strings",
            Self::Chunks => "chunks",
            Self::Upvalues => "upvalues",
            Self::Objects => "objects",
            Self::Globals => "globals",
            Self::EnvironmentDeltas => "environment deltas",
            Self::FormatPointerIds => "format pointer ids",
            Self::EnvironmentDeletedKeys => "environment deleted keys",
            Self::EnvironmentUpserts => "environment upserts",
            Self::EnvironmentOrder => "environment order",
            Self::EnvironmentArray => "environment array",
            Self::TableArraySlots => "table array slots",
            Self::TableEntries => "table entries",
            Self::ClosureUpvalues => "closure upvalues",
            Self::Instructions => "instructions",
            Self::NumberLiterals => "number literals",
            Self::StringLiterals => "string literals",
            Self::TableTemplates => "table templates",
            Self::TableTemplateKeys => "table template keys",
            Self::NestedChunks => "nested chunks",
            Self::UpvalueDescriptors => "upvalue descriptors",
            Self::LineInfo => "line info",
        })
    }
}

/// A length-limited byte field of the save format, named by the
/// `FieldSizeExceeded` variants of [`SaveError`] and [`SnapshotError`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SnapshotField {
    /// The diagnostic crate version in the header.
    EngineVersion,
    /// A Lua string. Its load-time limit is reported as
    /// [`SnapshotError::StringSizeExceeded`].
    LuaString,
    /// A registered Rust function id.
    FunctionId,
    /// An environment-object token.
    EnvironmentToken,
    /// A function's diagnostic name.
    ChunkName,
    /// A function's chunk name.
    ChunkSource,
}

impl SnapshotField {
    /// The field's maximum length in bytes.
    pub const fn limit(self) -> usize {
        match self {
            Self::EngineVersion => MAX_ENGINE_VERSION_BYTES,
            Self::LuaString => MAX_STRING_BYTES,
            Self::FunctionId => MAX_SAVE_IDENTIFIER_BYTES,
            Self::EnvironmentToken => MAX_ENVIRONMENT_TOKEN_BYTES,
            Self::ChunkName => MAX_CHUNK_NAME_BYTES,
            Self::ChunkSource => MAX_SOURCE_NAME_BYTES,
        }
    }
}

// Every field length fits the format's u32 length prefix, so a field that
// passed its limit check always encodes.
const _: () = {
    let fields = [
        SnapshotField::EngineVersion,
        SnapshotField::LuaString,
        SnapshotField::FunctionId,
        SnapshotField::EnvironmentToken,
        SnapshotField::ChunkName,
        SnapshotField::ChunkSource,
    ];
    let mut index = 0;
    while index < fields.len() {
        assert!(fields[index].limit() <= u32::MAX as usize);
        index += 1;
    }
};

impl fmt::Display for SnapshotField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EngineVersion => "engine version",
            Self::LuaString => "Lua string",
            Self::FunctionId => "function id",
            Self::EnvironmentToken => "environment token",
            Self::ChunkName => "chunk name",
            Self::ChunkSource => "chunk source",
        })
    }
}

/// Errors produced while loading a saved [`State`](crate::State).
///
/// Decoding, verification and trailing-byte rejection all run before the
/// setup closure, so a [`LoadError::Snapshot`] from those stages means setup
/// never ran.
#[non_exhaustive]
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum LoadError<E = Infallible> {
    /// The host's load-time setup closure failed.
    Setup(E),
    /// The load-time setup closure returned success on a State a panic had
    /// unwound through (it caught the panic), so the snapshot is not loaded
    /// into it.
    StatePoisoned,
    /// The calling thread's floating-point environment is unsupported (see
    /// [`ErrorKind::UnsupportedFloatEnvironment`](crate::error::ErrorKind::UnsupportedFloatEnvironment)),
    /// on entry or after the setup closure returned success; nothing saved
    /// was materialized.
    UnsupportedFloatEnvironment(FloatEnvironment),
    /// The snapshot could not be restored: its bytes are malformed or
    /// unsupported, or they reference something the load-time environment
    /// does not provide.
    Snapshot(SnapshotError),
}

impl<E> From<SnapshotError> for LoadError<E> {
    fn from(error: SnapshotError) -> Self {
        Self::Snapshot(error)
    }
}

impl<E> LoadError<E> {
    /// The snapshot failure, if this is one.
    pub fn snapshot(&self) -> Option<&SnapshotError> {
        match self {
            Self::Snapshot(error) => Some(error),
            Self::Setup(_) | Self::StatePoisoned | Self::UnsupportedFloatEnvironment(_) => None,
        }
    }
}

impl<E: fmt::Display> fmt::Display for LoadError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Setup(error) => write!(f, "load setup failed: {error}"),
            LoadError::StatePoisoned => write!(f, "load setup left the state poisoned by a panic"),
            LoadError::UnsupportedFloatEnvironment(environment) => {
                write!(f, "unsupported floating-point environment ({environment})")
            }
            LoadError::Snapshot(error) => error.fmt(f),
        }
    }
}

impl<E: fmt::Debug + fmt::Display + 'static> std::error::Error for LoadError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Snapshot(error) => Some(error),
            LoadError::Setup(_)
            | LoadError::StatePoisoned
            | LoadError::UnsupportedFloatEnvironment(_) => None,
        }
    }
}

/// Why a snapshot could not be restored, independent of the host's setup.
///
/// Not every variant means corrupt bytes: [`SnapshotError::UnknownFunction`]
/// and [`SnapshotError::UnknownEnvObject`] can come from a valid save loaded
/// into an environment that does not register what it references.
#[non_exhaustive]
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SnapshotError {
    /// Save bytes did not start with the dellingr save magic.
    BadMagic,
    /// Save format version is not supported by this build.
    UnsupportedVersion {
        /// Format version in the save.
        found: u16,
        /// Format version supported by this build.
        expected: u16,
    },
    /// Save cost model version is not supported by this build.
    UnsupportedCostModelVersion {
        /// Cost model version in the save.
        found: u16,
        /// Cost model version supported by this build.
        expected: u16,
    },
    /// A saved function id was not registered by the load-time environment.
    UnknownFunction(String),
    /// A saved environment-object token was not present at load time.
    UnknownEnvObject(String),
    /// The binary payload could not be decoded.
    DecodeError {
        /// Byte offset of the failure in the save.
        offset: usize,
        /// What could not be decoded there.
        reason: DecodeReason,
    },
    /// A bounded metadata field exceeded its per-field resource limit.
    FieldSizeExceeded {
        /// Field whose limit was exceeded.
        field: SnapshotField,
        /// Size requested by the save.
        size: usize,
        /// Maximum permitted size.
        limit: usize,
    },
    /// A saved Lua string exceeded the fixed resource limit.
    StringSizeExceeded {
        /// Size requested by the save.
        size: usize,
        /// Maximum permitted string size.
        limit: usize,
    },
    /// Arena indices or object graph contents were corrupt.
    CorruptArena {
        /// Saved arena or collection invariant that failed.
        reason: CorruptionReason,
        /// Index within the collection identified by `location`.
        index: usize,
        /// Arena or nested collection containing the invalid entry.
        location: TableLoadLocation,
    },
    /// A runtime table rejected entries while restoring a save.
    InvalidTable {
        /// Object arena index, or environment delta token and local operation index.
        location: TableLoadLocation,
        /// The restore operation that failed.
        step: TableRestoreStep,
        /// Why the table rejected it.
        failure: TableRestoreFailure,
    },
    /// A saved bytecode program violates VM structural invariants.
    InvalidBytecode {
        /// Saved bytecode arena id.
        chunk: u32,
        /// Bytecode index when the failure is instruction-specific.
        instruction: Option<u32>,
        /// What the verifier rejected.
        reason: InvalidBytecodeReason,
    },
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::BadMagic => write!(f, "bad save magic"),
            SnapshotError::UnsupportedVersion { found, expected } => {
                write!(
                    f,
                    "unsupported save format version {found}; expected {expected}"
                )
            }
            SnapshotError::UnsupportedCostModelVersion { found, expected } => {
                write!(
                    f,
                    "unsupported save cost model version {found}; expected {expected}"
                )
            }
            SnapshotError::UnknownFunction(id) => {
                f.write_str("unknown saved function id ")?;
                write_bounded_user_text(f, id)
            }
            SnapshotError::UnknownEnvObject(id) => {
                f.write_str("unknown saved environment object ")?;
                write_bounded_user_text(f, id)
            }
            SnapshotError::DecodeError { offset, reason } => {
                write!(f, "save decode error: at byte {offset}: {reason}")
            }
            SnapshotError::FieldSizeExceeded { field, size, limit } => {
                write!(f, "save {field} size {size} exceeds limit {limit}")
            }
            SnapshotError::StringSizeExceeded { size, limit } => {
                write!(f, "string size {size} exceeds limit {limit}")
            }
            SnapshotError::CorruptArena {
                reason,
                index,
                location,
            } => {
                write!(f, "corrupt save {location} at index {index}: {reason}")
            }
            SnapshotError::InvalidTable {
                location,
                step,
                failure,
            } => {
                write!(f, "invalid saved table at {location}: ")?;
                if *step != TableRestoreStep::ObjectRebuild {
                    write!(f, "{step}: ")?;
                }
                write!(f, "{failure}")
            }
            SnapshotError::InvalidBytecode {
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

impl std::error::Error for SnapshotError {}

/// What could not be decoded, named by [`SnapshotError::DecodeError`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DecodeReason {
    /// Bytes remained after the payload.
    TrailingBytes,
    /// A length prefix ran past the addressable range.
    LengthOverflow,
    /// The save ended inside a value.
    TruncatedPayload,
    /// A boolean byte was neither 0 nor 1.
    InvalidBool(u8),
    /// A text field that must be UTF-8 was not.
    StringNotUtf8,
    /// The cost counters describe no state the runtime can produce.
    InconsistentCostMeter,
    /// An environment delta's metatable mode byte is unknown.
    InvalidEnvironmentMetatableMode(u8),
    /// A value's tag byte is unknown.
    InvalidValueTag(u8),
    /// An object's tag byte is unknown.
    InvalidObjectTag(u8),
    /// An upvalue descriptor's tag byte is unknown.
    InvalidUpvalueTag(u8),
}

impl fmt::Display for DecodeReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TrailingBytes => "trailing bytes",
            Self::LengthOverflow => "length overflow",
            Self::TruncatedPayload => "truncated payload",
            Self::InvalidBool(_) => "invalid bool",
            Self::StringNotUtf8 => "string is not UTF-8",
            Self::InconsistentCostMeter => "inconsistent cost meter",
            Self::InvalidEnvironmentMetatableMode(_) => "invalid environment metatable mode",
            Self::InvalidValueTag(_) => "invalid value tag",
            Self::InvalidObjectTag(_) => "invalid object tag",
            Self::InvalidUpvalueTag(_) => "invalid upvalue tag",
        })
    }
}

/// Which form of a key a uniqueness or membership check compared.
///
/// Load checks keys twice: the verifier compares the saved representation,
/// and materialization compares the resolved runtime values, which can
/// collide where the saved forms differ (two function ids folded to one
/// address, two environment tokens for one object).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum KeyForm {
    /// The saved representation.
    Saved,
    /// The resolved runtime value.
    Materialized,
}

/// Why a snapshot's arenas or object graph are corrupt, named by
/// [`SnapshotError::CorruptArena`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CorruptionReason {
    /// A reference names an arena entry that does not exist.
    IdOutOfRange(SavedReference),
    /// A collection that must hold distinct entries repeats one.
    Duplicate {
        /// The collection.
        entry: DuplicateEntry,
        /// The key form that collided.
        form: KeyForm,
    },
    /// A table key is nil.
    NilKey,
    /// A table key is NaN.
    NanKey,
    /// A rendered pointer identity names a value that has none.
    InvalidFormatPointerValue,
    /// A rendered pointer identity is not below the next identity to issue.
    FormatPointerIdNotBelowNext,
    /// The nested-function graph has a cycle.
    NestedChunkCycle,
    /// Part of the bytecode arena was left unbuilt.
    NotMaterialized(MaterializedPart),
    /// A saved table object is malformed.
    Table(TableCorruption),
    /// An environment delta is malformed.
    Environment(EnvironmentCorruption),
    /// A saved `string.gmatch` iterator is malformed.
    Gmatch(GmatchCorruption),
    /// A global pin is not a string, or an unsealed save carries pins.
    InvalidGlobalPin,
}

impl fmt::Display for CorruptionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdOutOfRange(reference) => write!(f, "{reference} is out of range"),
            Self::Duplicate { entry, form } => duplicate_text(f, *entry, *form),
            Self::NilKey => f.write_str("nil is not a table key"),
            Self::NanKey => f.write_str("NaN is not a table key"),
            Self::InvalidFormatPointerValue => f.write_str("invalid format pointer value"),
            Self::FormatPointerIdNotBelowNext => f.write_str("format pointer id not below next id"),
            Self::NestedChunkCycle => f.write_str("nested chunk graph contains a cycle"),
            Self::NotMaterialized(part) => write!(f, "{part} was not materialized"),
            Self::Table(reason) => fmt::Display::fmt(reason, f),
            Self::Environment(reason) => fmt::Display::fmt(reason, f),
            Self::Gmatch(reason) => fmt::Display::fmt(reason, f),
            Self::InvalidGlobalPin => f.write_str("invalid global pin"),
        }
    }
}

fn duplicate_text(f: &mut fmt::Formatter<'_>, entry: DuplicateEntry, form: KeyForm) -> fmt::Result {
    use DuplicateEntry as E;
    use KeyForm::{Materialized, Saved};
    f.write_str(match (entry, form) {
        (E::TableKey, Saved) => "duplicate saved table key",
        (E::TableKey, Materialized) => "duplicate materialized table key",
        (E::DeletedEnvironmentKey, Saved) => "duplicate deleted environment key",
        (E::DeletedEnvironmentKey, Materialized) => "duplicate materialized deleted key",
        (E::EnvironmentUpsertKey, Saved) => "duplicate environment upsert key",
        (E::EnvironmentUpsertKey, Materialized) => "duplicate materialized upsert key",
        (E::EnvironmentOrderKey, Saved) => "duplicate environment order key",
        (E::EnvironmentOrderKey, Materialized) => "duplicate materialized environment order key",
        (E::EnvironmentToken, Saved) => "duplicate environment token",
        (E::EnvironmentToken, Materialized) => "duplicate materialized environment token",
        (E::GlobalPin, Saved) => "duplicate saved global pin",
        (E::GlobalPin, Materialized) => "duplicate materialized global pin",
        (E::FormatPointerValue, Saved) => "duplicate format pointer value",
        (E::FormatPointerValue, Materialized) => "duplicate materialized format pointer value",
        (E::FormatPointerId, Saved) => "duplicate format pointer id",
        (E::FormatPointerId, Materialized) => "duplicate materialized format pointer id",
    })
}

/// A reference into a snapshot arena, named by
/// [`CorruptionReason::IdOutOfRange`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SavedReference {
    /// A closure's function, in the bytecode arena.
    ClosureChunk,
    /// A closure's captured upvalue, in the upvalue arena.
    ClosureUpvalue,
    /// An upvalue reference, in the upvalue arena.
    Upvalue,
    /// An object reference, in the object arena.
    Object,
    /// A string value, in the string arena.
    String,
    /// A table key string, in the string arena.
    TableKeyString,
    /// A function, in the bytecode arena.
    BytecodeChunk,
    /// A nested function, in the bytecode arena.
    NestedChunk,
}

impl fmt::Display for SavedReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ClosureChunk => "closure chunk id",
            Self::ClosureUpvalue => "closure upvalue id",
            Self::Upvalue => "upvalue id",
            Self::Object => "object id",
            Self::String => "string id",
            Self::TableKeyString => "table key string id",
            Self::BytecodeChunk => "bytecode chunk id",
            Self::NestedChunk => "nested chunk id",
        })
    }
}

/// A collection whose entries must be distinct, named by
/// [`CorruptionReason::Duplicate`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DuplicateEntry {
    /// A saved table's keys.
    TableKey,
    /// An environment delta's deleted keys.
    DeletedEnvironmentKey,
    /// An environment delta's upsert keys.
    EnvironmentUpsertKey,
    /// An environment delta's key order.
    EnvironmentOrderKey,
    /// The environment deltas' tokens.
    EnvironmentToken,
    /// The sealed global table's pinned names.
    GlobalPin,
    /// The values given rendered pointer identities.
    FormatPointerValue,
    /// The rendered pointer identities.
    FormatPointerId,
}

/// Part of the bytecode arena, named by [`CorruptionReason::NotMaterialized`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MaterializedPart {
    /// A function.
    Chunk,
    /// A nested function.
    NestedChunk,
    /// A function's chunk name.
    SourceName,
}

impl fmt::Display for MaterializedPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Chunk => "bytecode chunk",
            Self::NestedChunk => "nested bytecode chunk",
            Self::SourceName => "bytecode source name",
        })
    }
}

/// Why a saved table object is malformed, named by
/// [`CorruptionReason::Table`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TableCorruption {
    /// More stored array slots than entries.
    ArraySlotsExceedEntries,
    /// The logical array length is beyond the array limit.
    ArrayLengthExceedsLimit,
    /// A stored array slot's key is not its position.
    ArraySlotWrongKey,
    /// The stored array span does not start and end at live slots.
    ArraySpanEndpointsNotLive,
    /// A hash entry's key belongs to the array part.
    HashEntryBelongsInArray,
    /// A hash entry stores nil.
    EntryStoresNil,
    /// The metatable is not a table.
    MetatableNotTable,
    /// The object built for a saved table is not a table.
    MaterializedWrongType,
}

impl fmt::Display for TableCorruption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ArraySlotsExceedEntries => "saved table array slots exceed its entries",
            Self::ArrayLengthExceedsLimit => "saved table array length exceeds the array limit",
            Self::ArraySlotWrongKey => "saved table array slot has the wrong key",
            Self::ArraySpanEndpointsNotLive => "saved table array span does not end at live slots",
            Self::HashEntryBelongsInArray => "saved table hash entry belongs in the array",
            Self::EntryStoresNil => "saved table entry stores nil",
            Self::MetatableNotTable => "saved table metatable is not a table",
            Self::MaterializedWrongType => "materialized table object has wrong type",
        })
    }
}

/// Why an environment delta is malformed, named by
/// [`CorruptionReason::Environment`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EnvironmentCorruption {
    /// An upsert stores nil.
    UpsertStoresNil,
    /// A key is both deleted and upserted.
    KeyDeletedAndUpserted(KeyForm),
    /// The key order contains a deleted key.
    OrderContainsDeletedKey(KeyForm),
    /// The key order omits an upserted key.
    OrderOmitsUpsertedKey(KeyForm),
    /// An upsert's key belongs to the array part.
    HashEntryBelongsInArray,
    /// The array slots disagree with the upserts.
    ArrayDisagreesWithUpserts,
    /// The key order disagrees with the array slots.
    OrderDisagreesWithArray,
    /// A hash key in the order belongs to the array part.
    HashOrderBelongsInArray,
    /// The metatable is not an object.
    MetatableNotObject,
    /// The metatable is not a table.
    MetatableNotTable(KeyForm),
    /// The token names an environment object that is not a table.
    TokenNotTable,
    /// A key in the saved order is not in the table.
    OrderKeyAbsent,
}

impl fmt::Display for EnvironmentCorruption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use KeyForm::{Materialized, Saved};
        f.write_str(match self {
            Self::UpsertStoresNil => "environment upsert stores nil",
            Self::KeyDeletedAndUpserted(Saved) => "environment key is both deleted and upserted",
            Self::KeyDeletedAndUpserted(Materialized) => {
                "materialized key is both deleted and upserted"
            }
            Self::OrderContainsDeletedKey(Saved) => "environment order contains a deleted key",
            Self::OrderContainsDeletedKey(Materialized) => {
                "materialized environment order contains a deleted key"
            }
            Self::OrderOmitsUpsertedKey(Saved) => "environment order omits an upserted key",
            Self::OrderOmitsUpsertedKey(Materialized) => {
                "materialized environment order omits an upserted key"
            }
            Self::HashEntryBelongsInArray => "environment hash entry belongs in the array",
            Self::ArrayDisagreesWithUpserts => "environment array disagrees with upserts",
            Self::OrderDisagreesWithArray => "environment order disagrees with array",
            Self::HashOrderBelongsInArray => "environment hash order belongs in the array",
            Self::MetatableNotObject => "environment metatable is not an object",
            Self::MetatableNotTable(Saved) => "environment metatable is not a table",
            Self::MetatableNotTable(Materialized) => {
                "materialized environment metatable is not a table"
            }
            Self::TokenNotTable => "environment token does not resolve to a table",
            Self::OrderKeyAbsent => "environment order key is absent from the table",
        })
    }
}

/// Why a saved `string.gmatch` iterator is malformed, named by
/// [`CorruptionReason::Gmatch`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum GmatchCorruption {
    /// The iterator closure's upvalue reference is out of range.
    ClosureUpvalueOutOfRange,
    /// The closure captures more than one iterator function.
    MultipleIterators,
    /// The closure captures more than one object.
    MultipleStateObjects,
    /// The closure captures no state table.
    StateTableMissing,
    /// The captured state is not a table.
    StateNotTable,
    /// The subject is missing or not a string.
    SubjectInvalid,
    /// The subject's string id is out of range.
    SubjectStringOutOfRange,
    /// The pattern is missing or not a string.
    PatternInvalid,
    /// The position is missing or not a valid index.
    PositionInvalid,
    /// The position is past the end of the subject.
    PositionExceedsSubject,
    /// An empty-pattern iterator's length is missing or invalid.
    EmptyLengthInvalid,
    /// An empty-pattern iterator's position is missing or invalid.
    EmptyPositionInvalid,
    /// An empty-pattern iterator's position is past its length.
    EmptyPositionExceedsLength,
    /// The last match position is invalid.
    LastMatchInvalid,
    /// The compile flag's key is missing.
    CompileFlagKeyMissing,
    /// The compile flag's key string id is out of range.
    CompileFlagKeyOutOfRange,
    /// The compile flag's key is not the expected field name.
    CompileFlagKeyInvalid,
    /// The compile flag is neither absent nor `true`.
    CompileFlagInvalid,
}

impl fmt::Display for GmatchCorruption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ClosureUpvalueOutOfRange => "gmatch closure upvalue is out of range",
            Self::MultipleIterators => "gmatch closure captures multiple iterators",
            Self::MultipleStateObjects => "gmatch closure captures multiple state objects",
            Self::StateTableMissing => "gmatch closure state table is missing",
            Self::StateNotTable => "gmatch closure state is not a table",
            Self::SubjectInvalid => "gmatch subject is missing or invalid",
            Self::SubjectStringOutOfRange => "gmatch subject string id is out of range",
            Self::PatternInvalid => "gmatch pattern is missing or invalid",
            Self::PositionInvalid => "gmatch position is missing or invalid",
            Self::PositionExceedsSubject => "gmatch position exceeds its subject",
            Self::EmptyLengthInvalid => "empty gmatch length is missing or invalid",
            Self::EmptyPositionInvalid => "empty gmatch position is missing or invalid",
            Self::EmptyPositionExceedsLength => "empty gmatch position exceeds its length",
            Self::LastMatchInvalid => "gmatch last match is invalid",
            Self::CompileFlagKeyMissing => "gmatch compile flag key is missing",
            Self::CompileFlagKeyOutOfRange => "gmatch compile flag key is out of range",
            Self::CompileFlagKeyInvalid => "gmatch compile flag key is invalid",
            Self::CompileFlagInvalid => "gmatch compile flag is invalid",
        })
    }
}

/// The table-restore operation named by [`SnapshotError::InvalidTable`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TableRestoreStep {
    /// Rebuilding a saved table object from its entries.
    ObjectRebuild,
    /// Deleting a key from an environment table.
    DeleteEnvironmentKey,
    /// Writing an environment upsert.
    UpsertEnvironmentEntry,
    /// Restoring an environment table's key order.
    RestoreEnvironmentOrder,
    /// Restoring an environment table's array shape.
    RestoreEnvironmentArray,
    /// Re-adding an entry the load setup put at the array's append key.
    SetupEnvironmentAppend,
}

impl fmt::Display for TableRestoreStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ObjectRebuild => "saved table is invalid",
            Self::DeleteEnvironmentKey => "deleted environment key is invalid",
            Self::UpsertEnvironmentEntry => "environment upsert is invalid",
            Self::RestoreEnvironmentOrder => "saved environment order is invalid",
            Self::RestoreEnvironmentArray => "saved environment array is invalid",
            Self::SetupEnvironmentAppend => "setup environment append is invalid",
        })
    }
}

/// Why a runtime table rejected a restore, named by
/// [`SnapshotError::InvalidTable`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TableRestoreFailure {
    /// A key is nil.
    NilKey,
    /// A key is NaN.
    NanKey,
    /// More array slots than entries; the verifier rejects this first, so it
    /// reaching the table is a dellingr bug.
    ArrayLengthExceedsEntries,
    /// The array length is beyond the array limit; a dellingr bug, as above.
    ArrayLengthExceedsLimit,
    /// An array slot's key is not its position; a dellingr bug, as above.
    ArraySlotHasWrongKey,
    /// A hash entry's key belongs to the array part; a dellingr bug, as above.
    HashEntryBelongsInArray,
    /// Growing the table's array part failed: the saved logical length left
    /// no room for the restored keys, or the allocator refused the storage.
    GrowthFailed,
}

impl From<TableError> for TableRestoreFailure {
    fn from(error: TableError) -> Self {
        match error {
            TableError::InvalidKey(InvalidTableKey::Nil) => Self::NilKey,
            TableError::InvalidKey(InvalidTableKey::Nan) => Self::NanKey,
            TableError::GrowthFailed => Self::GrowthFailed,
            TableError::InvalidSnapshot(fault) => match fault {
                SnapshotTableFault::ArrayLengthExceedsEntries => Self::ArrayLengthExceedsEntries,
                SnapshotTableFault::ArrayLengthExceedsLimit => Self::ArrayLengthExceedsLimit,
                SnapshotTableFault::ArraySlotHasWrongKey => Self::ArraySlotHasWrongKey,
                SnapshotTableFault::HashEntryBelongsInArray => Self::HashEntryBelongsInArray,
            },
        }
    }
}

impl fmt::Display for TableRestoreFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let fault = match self {
            Self::NilKey => return f.write_str("table index was nil"),
            Self::NanKey => return f.write_str("table index was NaN"),
            Self::GrowthFailed => return f.write_str("table growth failed"),
            Self::ArrayLengthExceedsEntries => SnapshotTableFault::ArrayLengthExceedsEntries,
            Self::ArrayLengthExceedsLimit => SnapshotTableFault::ArrayLengthExceedsLimit,
            Self::ArraySlotHasWrongKey => SnapshotTableFault::ArraySlotHasWrongKey,
            Self::HashEntryBelongsInArray => SnapshotTableFault::HashEntryBelongsInArray,
        };
        write!(f, "internal error: {fault}")
    }
}

/// Why saved bytecode was rejected, named by [`SnapshotError::InvalidBytecode`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InvalidBytecodeReason {
    /// The bytecode verifier rejected the function.
    Validation(ValidationReason),
    /// More references to one nested function than the verifier can count.
    NestedGraphTooLarge,
    /// The nested-function graph has a cycle.
    NestedGraphCycle,
    /// A closure captures a different number of upvalues than its function
    /// declares.
    ClosureUpvalueCountMismatch,
}

impl fmt::Display for InvalidBytecodeReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(reason) => fmt::Display::fmt(reason, f),
            Self::NestedGraphTooLarge => f.write_str("nested graph is too large"),
            Self::NestedGraphCycle => f.write_str("nested chunk graph contains a cycle"),
            Self::ClosureUpvalueCountMismatch => {
                f.write_str("closure upvalue count does not match bytecode")
            }
        }
    }
}

/// Location of a table restoration failure or corrupt snapshot collection.
///
/// `Object` and `Environment` identify materialized tables; the other variants
/// qualify the separate entry index in [`SnapshotError::CorruptArena`].
#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum TableLoadLocation {
    /// Interned string arena.
    Strings,
    /// Saved object arena.
    Objects,
    /// Saved bytecode arena.
    Bytecode,
    /// Closed upvalue arena.
    Upvalues,
    /// The saved global table and its pins.
    Globals,
    /// Rendered pointer identity records.
    PointerIds,
    /// Entries of a saved table object.
    TableEntries {
        /// Owner in the object arena.
        object: usize,
    },
    /// Ordered environment deltas.
    EnvironmentDeltas,
    /// Deleted keys of the environment delta at this index.
    EnvironmentDeleted(usize),
    /// Upserts of the environment delta at this index.
    EnvironmentUpserts(usize),
    /// Key order of the environment delta at this index.
    EnvironmentOrder(usize),
    /// Array slots of the environment delta at this index.
    EnvironmentArray(usize),
    /// Physical hash slots of the environment delta at this index.
    EnvironmentHashSlots(usize),
    /// Metatable of the environment delta at this index.
    EnvironmentMetatable(usize),
    /// A table in the saved object arena.
    Object(usize),
    /// A library or host environment table.
    Environment {
        /// Stable environment-object token.
        token: String,
        /// Entry index for a deletion or upsert; absent for a whole-table rebuild.
        entry: Option<usize>,
    },
}

impl fmt::Display for TableLoadLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Object(index) => write!(f, "object[{index}]"),
            Self::Environment { token, entry } => {
                f.write_str("environment ")?;
                write_bounded_user_text(f, token)?;
                if let Some(entry) = entry {
                    write!(f, " entry[{entry}]")?;
                }
                Ok(())
            }
            Self::Strings => f.write_str("string arena"),
            Self::Objects => f.write_str("object arena"),
            Self::Bytecode => f.write_str("bytecode arena"),
            Self::Upvalues => f.write_str("upvalue arena"),
            Self::Globals => f.write_str("globals"),
            Self::PointerIds => f.write_str("format pointer ids"),
            Self::TableEntries { object } => write!(f, "object[{object}] entries"),
            Self::EnvironmentDeltas => f.write_str("environment deltas"),
            Self::EnvironmentDeleted(delta) => write!(f, "environment delta[{delta}] deleted keys"),
            Self::EnvironmentUpserts(delta) => write!(f, "environment delta[{delta}] upserts"),
            Self::EnvironmentOrder(delta) => write!(f, "environment delta[{delta}] order"),
            Self::EnvironmentArray(delta) => write!(f, "environment delta[{delta}] array"),
            Self::EnvironmentHashSlots(delta) => write!(f, "environment delta[{delta}] hash slots"),
            Self::EnvironmentMetatable(delta) => {
                write!(f, "environment delta[{delta}] metatable")
            }
        }
    }
}

pub(super) fn write_bounded_user_text(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    let mut written = 0usize;
    for character in text.chars() {
        let escaped = character.escape_debug().to_string();
        if written.saturating_add(escaped.len()) > MAX_LOGGED_USER_TEXT_BYTES {
            f.write_str("...")?;
            return Ok(());
        }
        f.write_str(&escaped)?;
        written += escaped.len();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each reason renders the sentence the corruption report carried as a
    /// string before reasons were typed.
    #[test]
    fn corruption_reasons_keep_their_sentences() {
        for (reason, text) in [
            (
                CorruptionReason::IdOutOfRange(SavedReference::ClosureChunk),
                "closure chunk id is out of range",
            ),
            (
                CorruptionReason::IdOutOfRange(SavedReference::ClosureUpvalue),
                "closure upvalue id is out of range",
            ),
            (
                CorruptionReason::IdOutOfRange(SavedReference::Upvalue),
                "upvalue id is out of range",
            ),
            (
                CorruptionReason::IdOutOfRange(SavedReference::Object),
                "object id is out of range",
            ),
            (
                CorruptionReason::IdOutOfRange(SavedReference::String),
                "string id is out of range",
            ),
            (
                CorruptionReason::IdOutOfRange(SavedReference::TableKeyString),
                "table key string id is out of range",
            ),
            (
                CorruptionReason::IdOutOfRange(SavedReference::BytecodeChunk),
                "bytecode chunk id is out of range",
            ),
            (
                CorruptionReason::IdOutOfRange(SavedReference::NestedChunk),
                "nested chunk id is out of range",
            ),
            (
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::TableKey,
                    form: KeyForm::Saved,
                },
                "duplicate saved table key",
            ),
            (
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::TableKey,
                    form: KeyForm::Materialized,
                },
                "duplicate materialized table key",
            ),
            (
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::DeletedEnvironmentKey,
                    form: KeyForm::Saved,
                },
                "duplicate deleted environment key",
            ),
            (
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::DeletedEnvironmentKey,
                    form: KeyForm::Materialized,
                },
                "duplicate materialized deleted key",
            ),
            (
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::EnvironmentUpsertKey,
                    form: KeyForm::Saved,
                },
                "duplicate environment upsert key",
            ),
            (
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::EnvironmentUpsertKey,
                    form: KeyForm::Materialized,
                },
                "duplicate materialized upsert key",
            ),
            (
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::EnvironmentOrderKey,
                    form: KeyForm::Saved,
                },
                "duplicate environment order key",
            ),
            (
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::EnvironmentOrderKey,
                    form: KeyForm::Materialized,
                },
                "duplicate materialized environment order key",
            ),
            (
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::EnvironmentToken,
                    form: KeyForm::Saved,
                },
                "duplicate environment token",
            ),
            (
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::GlobalPin,
                    form: KeyForm::Saved,
                },
                "duplicate saved global pin",
            ),
            (
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::FormatPointerValue,
                    form: KeyForm::Saved,
                },
                "duplicate format pointer value",
            ),
            (
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::FormatPointerValue,
                    form: KeyForm::Materialized,
                },
                "duplicate materialized format pointer value",
            ),
            (
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::FormatPointerId,
                    form: KeyForm::Saved,
                },
                "duplicate format pointer id",
            ),
            (CorruptionReason::NilKey, "nil is not a table key"),
            (CorruptionReason::NanKey, "NaN is not a table key"),
            (
                CorruptionReason::InvalidFormatPointerValue,
                "invalid format pointer value",
            ),
            (
                CorruptionReason::FormatPointerIdNotBelowNext,
                "format pointer id not below next id",
            ),
            (
                CorruptionReason::NestedChunkCycle,
                "nested chunk graph contains a cycle",
            ),
            (
                CorruptionReason::NotMaterialized(MaterializedPart::Chunk),
                "bytecode chunk was not materialized",
            ),
            (
                CorruptionReason::NotMaterialized(MaterializedPart::NestedChunk),
                "nested bytecode chunk was not materialized",
            ),
            (
                CorruptionReason::NotMaterialized(MaterializedPart::SourceName),
                "bytecode source name was not materialized",
            ),
            (
                CorruptionReason::Table(TableCorruption::ArraySlotsExceedEntries),
                "saved table array slots exceed its entries",
            ),
            (
                CorruptionReason::Table(TableCorruption::ArrayLengthExceedsLimit),
                "saved table array length exceeds the array limit",
            ),
            (
                CorruptionReason::Table(TableCorruption::ArraySlotWrongKey),
                "saved table array slot has the wrong key",
            ),
            (
                CorruptionReason::Table(TableCorruption::ArraySpanEndpointsNotLive),
                "saved table array span does not end at live slots",
            ),
            (
                CorruptionReason::Table(TableCorruption::HashEntryBelongsInArray),
                "saved table hash entry belongs in the array",
            ),
            (
                CorruptionReason::Table(TableCorruption::EntryStoresNil),
                "saved table entry stores nil",
            ),
            (
                CorruptionReason::Table(TableCorruption::MetatableNotTable),
                "saved table metatable is not a table",
            ),
            (
                CorruptionReason::Table(TableCorruption::MaterializedWrongType),
                "materialized table object has wrong type",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::UpsertStoresNil),
                "environment upsert stores nil",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::KeyDeletedAndUpserted(
                    KeyForm::Saved,
                )),
                "environment key is both deleted and upserted",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::KeyDeletedAndUpserted(
                    KeyForm::Materialized,
                )),
                "materialized key is both deleted and upserted",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::OrderContainsDeletedKey(
                    KeyForm::Saved,
                )),
                "environment order contains a deleted key",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::OrderContainsDeletedKey(
                    KeyForm::Materialized,
                )),
                "materialized environment order contains a deleted key",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::OrderOmitsUpsertedKey(
                    KeyForm::Saved,
                )),
                "environment order omits an upserted key",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::OrderOmitsUpsertedKey(
                    KeyForm::Materialized,
                )),
                "materialized environment order omits an upserted key",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::HashEntryBelongsInArray),
                "environment hash entry belongs in the array",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::ArrayDisagreesWithUpserts),
                "environment array disagrees with upserts",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::OrderDisagreesWithArray),
                "environment order disagrees with array",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::HashOrderBelongsInArray),
                "environment hash order belongs in the array",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::MetatableNotObject),
                "environment metatable is not an object",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::MetatableNotTable(
                    KeyForm::Saved,
                )),
                "environment metatable is not a table",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::MetatableNotTable(
                    KeyForm::Materialized,
                )),
                "materialized environment metatable is not a table",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::TokenNotTable),
                "environment token does not resolve to a table",
            ),
            (
                CorruptionReason::Environment(EnvironmentCorruption::OrderKeyAbsent),
                "environment order key is absent from the table",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::ClosureUpvalueOutOfRange),
                "gmatch closure upvalue is out of range",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::MultipleIterators),
                "gmatch closure captures multiple iterators",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::MultipleStateObjects),
                "gmatch closure captures multiple state objects",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::StateTableMissing),
                "gmatch closure state table is missing",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::StateNotTable),
                "gmatch closure state is not a table",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::SubjectInvalid),
                "gmatch subject is missing or invalid",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::SubjectStringOutOfRange),
                "gmatch subject string id is out of range",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::PatternInvalid),
                "gmatch pattern is missing or invalid",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::PositionInvalid),
                "gmatch position is missing or invalid",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::PositionExceedsSubject),
                "gmatch position exceeds its subject",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::EmptyLengthInvalid),
                "empty gmatch length is missing or invalid",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::EmptyPositionInvalid),
                "empty gmatch position is missing or invalid",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::EmptyPositionExceedsLength),
                "empty gmatch position exceeds its length",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::LastMatchInvalid),
                "gmatch last match is invalid",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::CompileFlagKeyMissing),
                "gmatch compile flag key is missing",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::CompileFlagKeyOutOfRange),
                "gmatch compile flag key is out of range",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::CompileFlagKeyInvalid),
                "gmatch compile flag key is invalid",
            ),
            (
                CorruptionReason::Gmatch(GmatchCorruption::CompileFlagInvalid),
                "gmatch compile flag is invalid",
            ),
        ] {
            assert_eq!(reason.to_string(), text);
        }
    }

    #[test]
    fn decode_reasons_keep_their_sentences() {
        for (reason, text) in [
            (DecodeReason::TrailingBytes, "trailing bytes"),
            (DecodeReason::LengthOverflow, "length overflow"),
            (DecodeReason::TruncatedPayload, "truncated payload"),
            (DecodeReason::InvalidBool(7), "invalid bool"),
            (DecodeReason::StringNotUtf8, "string is not UTF-8"),
            (
                DecodeReason::InconsistentCostMeter,
                "inconsistent cost meter",
            ),
            (
                DecodeReason::InvalidEnvironmentMetatableMode(9),
                "invalid environment metatable mode",
            ),
            (DecodeReason::InvalidValueTag(9), "invalid value tag"),
            (DecodeReason::InvalidObjectTag(9), "invalid object tag"),
            (DecodeReason::InvalidUpvalueTag(9), "invalid upvalue tag"),
        ] {
            assert_eq!(reason.to_string(), text);
        }
        assert_eq!(
            SnapshotError::DecodeError {
                offset: 12,
                reason: DecodeReason::InvalidValueTag(9),
            }
            .to_string(),
            "save decode error: at byte 12: invalid value tag"
        );
    }

    #[test]
    fn invalid_table_renders_step_and_failure() {
        let environment = |step| SnapshotError::InvalidTable {
            location: TableLoadLocation::Environment {
                token: "math".to_owned(),
                entry: Some(2),
            },
            step,
            failure: TableRestoreFailure::NilKey,
        };
        assert_eq!(
            environment(TableRestoreStep::UpsertEnvironmentEntry).to_string(),
            "invalid saved table at environment math entry[2]: environment upsert is invalid: table index was nil"
        );
        assert_eq!(
            SnapshotError::InvalidTable {
                location: TableLoadLocation::Object(3),
                step: TableRestoreStep::ObjectRebuild,
                failure: TableRestoreFailure::HashEntryBelongsInArray,
            }
            .to_string(),
            "invalid saved table at object[3]: internal error: snapshot hash entry belongs in the array"
        );
        for (error, failure) in [
            (
                TableError::InvalidKey(InvalidTableKey::Nan),
                TableRestoreFailure::NanKey,
            ),
            (
                TableError::InvalidSnapshot(SnapshotTableFault::ArraySlotHasWrongKey),
                TableRestoreFailure::ArraySlotHasWrongKey,
            ),
            (TableError::GrowthFailed, TableRestoreFailure::GrowthFailed),
        ] {
            assert_eq!(TableRestoreFailure::from(error), failure);
        }
        assert_eq!(
            TableRestoreFailure::GrowthFailed.to_string(),
            "table growth failed"
        );
    }

    #[test]
    fn save_errors_name_their_collection_and_field() {
        assert_eq!(
            SaveError::TooLarge {
                collection: SaveCollection::TableEntries,
                count: 5,
                limit: 4,
            }
            .to_string(),
            "save table entries count 5 exceeds the supported limit"
        );
        assert_eq!(
            SaveError::FieldSizeExceeded {
                field: SnapshotField::FunctionId,
                size: 5000,
                limit: 4096,
            }
            .to_string(),
            "save function id size 5000 exceeds limit 4096"
        );
        assert_eq!(
            SaveError::from(SaveFault::UnfilledRootSlot).to_string(),
            "save encode invariant failed: unfilled root slot"
        );
        assert_eq!(
            SnapshotError::InvalidBytecode {
                chunk: 1,
                instruction: None,
                reason: InvalidBytecodeReason::ClosureUpvalueCountMismatch,
            }
            .to_string(),
            "invalid bytecode chunk 1: closure upvalue count does not match bytecode"
        );
    }
}
