use std::fmt::{self, Write as _};
use std::sync::Arc;

use crate::LuaType;
use crate::limits::{
    MAX_ASSIGNMENT_OFFSET, MAX_EXPRESSIONS, MAX_FIXED_ARGS, MAX_FIXED_RESULTS, MAX_LITERALS,
    MAX_LOCALS, MAX_NESTED, MAX_UPVALUES,
};

// Types

/// Display adapter for bytes that are usually, but not necessarily, UTF-8:
/// valid UTF-8 renders as itself and each maximal invalid subsequence as one
/// U+FFFD. Rendering only; the bytes themselves stay exact wherever they are
/// stored.
pub(crate) struct LossyBytes<'a>(pub(crate) &'a [u8]);

impl fmt::Display for LossyBytes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.0.utf8_chunks() {
            f.write_str(chunk.valid())?;
            if !chunk.invalid().is_empty() {
                f.write_char(char::REPLACEMENT_CHARACTER)?;
            }
        }
        Ok(())
    }
}

/// A single frame in a stack trace.
#[derive(Debug, Clone)]
pub struct StackFrame {
    /// Function name (if known), shared with the chunk that executed.
    pub function_name: Option<Arc<str>>,
    /// Chunk name exactly as given to `Program::compile`, shared with the
    /// chunk that executed. Byte-exact; `Display` renders it lossily.
    pub source: Option<Arc<[u8]>>,
    /// Line number where the call occurred (1-indexed).
    pub line: u32,
    /// Line where the function was defined (1-indexed), or 0 for a main
    /// chunk. `Display` uses it to label an unnamed function that is not the
    /// main chunk, as `function <source:line>`.
    pub defined_line: u32,
}

/// Instruction context for an internal error escaping Lua dispatch.
///
/// The error kind retains its original detail and display text. Host API and
/// compiler errors have no executing instruction and do not carry this record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmFault {
    /// Zero-based instruction index in the executing chunk.
    pub instruction: usize,
    /// Opcode byte at that instruction.
    pub opcode: u8,
    /// Diagnostic function name, if the chunk has one, shared with the chunk.
    pub function_name: Option<Arc<str>>,
}

/// The reason behind an [`ErrorKind::InternalError`]: a dellingr bug, never a
/// script or host failure. Opaque on purpose - hosts report these, they do
/// not branch on them - and constructible only inside the crate, so an
/// `InternalError` always means dellingr broke one of its own invariants.
/// Host failures use [`ErrorKind::HostError`].
pub struct InternalError(InternalFault);

impl fmt::Debug for InternalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("InternalError").field(&self.0).finish()
    }
}

impl fmt::Display for InternalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[derive(Debug)]
pub(crate) enum InternalFault {
    Compiler(CompilerFault),
    Parser(ParserFault),
    CompilerValidation {
        /// Function name of the chunk that failed verification, if any.
        function: Option<Arc<str>>,
        error: crate::compiler::verify::BytecodeValidationError,
    },
    GsubReplacementTemplateMissing,
    GmatchState(GmatchStateFault),
    #[cfg(feature = "snapshot")]
    SnapshotTable(SnapshotTableFault),
    /// The standard library failed to register one of its own function ids.
    #[cfg(feature = "snapshot")]
    Registration(crate::SaveError),
    /// Injected by unit tests that need an internal error escaping dispatch.
    #[cfg(test)]
    Test,
    CaptureRangeOutsideString,
    MissingPointerIdentity,
    ConstructorTableMissing {
        operation: ConstructorTableOperation,
        actual: LuaType,
    },
    /// A call, open-result or vararg window would cut through registers an
    /// open upvalue still points at. Compiled code always runs these windows
    /// above every live local; only forged bytecode can reach this.
    WindowOverlapsOpenUpvalue,
}

#[derive(Debug)]
pub(crate) enum CompilerFault {
    LineInfoDesynced,
    NestedBytecodeNotUniquelyOwned,
}

#[derive(Debug)]
pub(crate) enum ParserFault {
    InvalidStringToken,
    MissingStringContents,
    BlockExitWithoutEntry,
    MultiValueTailMissingInstruction,
    LoopExitWithoutEntry,
    BreakEmissionLostLoopContext,
    CompletedSourceRetainsFunctionFrames,
    ChunkCompletionWithoutFunctionFrame,
    CompletedFunctionRetainsScopes,
    NamedFunctionTargetNotVariable,
    EmptyReturnList,
    ReturnCountOutOfRange,
    AssignmentPlaceRegistersMissing,
    EmptyComparisonChain,
    /// A folded operand names a number literal that is not in the pool.
    FoldedLiteralOutOfRange,
    /// A resolved local has no declaration in its function.
    LocalWithoutDeclaration,
    /// Identifier and numeral tokens are ASCII by construction in the byte
    /// lexer, and the parser reads them as `&str`.
    TokenTextNotUtf8,
}

#[derive(Debug)]
pub(crate) enum GmatchStateFault {
    StringFieldInvalid(&'static str),
    NumberFieldInvalid(&'static str),
    NumberFieldOutOfRange(&'static str),
    PositionExceedsSubjectLength,
    LastMatchExceedsSubjectLength,
    LastMatchFieldInvalid(&'static str),
    CompileFlagKeyMissing,
    CompileFlagInvalid,
}

#[cfg(feature = "snapshot")]
#[derive(Debug)]
pub(crate) enum SnapshotTableFault {
    ArrayLengthExceedsEntries,
    ArrayLengthExceedsLimit,
    ArraySlotHasWrongKey,
    HashEntryBelongsInArray,
}

#[derive(Debug)]
pub(crate) enum ConstructorTableOperation {
    InitField,
    InitFieldPinned,
    InitIndex,
    SetList,
}

impl fmt::Display for InternalFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compiler(fault) => fmt::Display::fmt(fault, f),
            Self::Parser(fault) => f.write_str(fault.message()),
            Self::CompilerValidation { function, error } => {
                f.write_str("compiler emitted invalid bytecode in ")?;
                match function {
                    Some(name) => write!(f, "function '{name}'")?,
                    None => f.write_str("unnamed chunk")?,
                }
                write!(f, " {error}")
            }
            Self::GsubReplacementTemplateMissing => {
                f.write_str("gsub replacement template was not initialized")
            }
            Self::GmatchState(fault) => write!(f, "invalid gmatch iterator state: {fault}"),
            #[cfg(feature = "snapshot")]
            Self::SnapshotTable(fault) => fmt::Display::fmt(fault, f),
            #[cfg(feature = "snapshot")]
            Self::Registration(error) => fmt::Display::fmt(error, f),
            #[cfg(test)]
            Self::Test => f.write_str("test fault"),
            Self::CaptureRangeOutsideString => {
                f.write_str("capture range outside the source string")
            }
            Self::MissingPointerIdentity => {
                f.write_str("value without a heap rendering has no pointer identity")
            }
            Self::ConstructorTableMissing { operation, actual } => {
                write!(f, "{operation}: expected table, got {actual}")
            }
            Self::WindowOverlapsOpenUpvalue => {
                f.write_str("a register window overlaps an open upvalue")
            }
        }
    }
}

impl fmt::Display for CompilerFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::LineInfoDesynced => "line_info desynced from code",
            Self::NestedBytecodeNotUniquelyOwned => {
                "nested bytecode must be uniquely owned during finalization"
            }
        };
        f.write_str(message)
    }
}

impl ParserFault {
    fn message(&self) -> &'static str {
        match self {
            Self::InvalidStringToken => "parser requested contents for an invalid string token",
            Self::MissingStringContents => "lexer did not retain contents for a string token",
            Self::BlockExitWithoutEntry => "block exit requires a block entry",
            Self::MultiValueTailMissingInstruction => {
                "multi-value tail does not end with its instruction"
            }
            Self::LoopExitWithoutEntry => "loop exit requires a loop entry",
            Self::BreakEmissionLostLoopContext => "break emission lost its loop context",
            Self::CompletedSourceRetainsFunctionFrames => {
                "completed source retains function frames"
            }
            Self::ChunkCompletionWithoutFunctionFrame => {
                "chunk completion requires a function frame"
            }
            Self::CompletedFunctionRetainsScopes => {
                "completed function retains loop or block scopes"
            }
            Self::NamedFunctionTargetNotVariable => "a named function target must be a variable",
            Self::EmptyReturnList => "return emission requires a nonempty expression list",
            Self::ReturnCountOutOfRange => "return count exceeds the parsed expression limit",
            Self::AssignmentPlaceRegistersMissing => "assignment place operands have no registers",
            Self::EmptyComparisonChain => "comparison chain requires an operator",
            Self::FoldedLiteralOutOfRange => "folded operand names a literal outside the pool",
            Self::LocalWithoutDeclaration => "resolved local has no declaration",
            Self::TokenTextNotUtf8 => "lexer produced a non-UTF-8 identifier or numeral token",
        }
    }
}

impl fmt::Display for GmatchStateFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StringFieldInvalid(field) => {
                write!(f, "field '{field}' is missing or is not a string")
            }
            Self::NumberFieldInvalid(field) => {
                write!(f, "field '{field}' is missing or is not a number")
            }
            Self::NumberFieldOutOfRange(field) => {
                write!(f, "field '{field}' is outside its valid range")
            }
            Self::PositionExceedsSubjectLength => {
                f.write_str("position exceeds the stored subject length")
            }
            Self::LastMatchExceedsSubjectLength => {
                f.write_str("last match exceeds the stored subject length")
            }
            Self::LastMatchFieldInvalid(field) => {
                write!(f, "field '{field}' is not a number or nil")
            }
            Self::CompileFlagKeyMissing => {
                f.write_str("compile flag key is missing from integer slot 0")
            }
            Self::CompileFlagInvalid => f.write_str("compile flag is not a boolean or nil"),
        }
    }
}

#[cfg(feature = "snapshot")]
impl fmt::Display for SnapshotTableFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ArrayLengthExceedsEntries => "snapshot array length exceeds its entries",
            Self::ArrayLengthExceedsLimit => "snapshot array length exceeds the array limit",
            Self::ArraySlotHasWrongKey => "snapshot array slot has the wrong key",
            Self::HashEntryBelongsInArray => "snapshot hash entry belongs in the array",
        };
        f.write_str(message)
    }
}

impl fmt::Display for ConstructorTableOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let operation = match self {
            Self::InitField => "InitField",
            Self::InitFieldPinned => "InitFieldPinned",
            Self::InitIndex => "InitIndex",
            Self::SetList => "SetList",
        };
        f.write_str(operation)
    }
}

/// Rarely-set error context, boxed so `Error` stays narrow. Structured
/// attribution lives here so hosts can inspect the operation that failed
/// without the kind carrying it.
struct ErrorContext {
    /// Chunk name, byte-exact, if the error came from named source.
    name: Option<Arc<[u8]>>,
    /// Whether `on_error` has already been notified for this error.
    reported: bool,
    /// String function whose pattern operation produced this error, if any.
    pattern_function: Option<StringPatternFunction>,
    /// Standard-library function responsible for an error whose kind cannot
    /// name it (the default `table.sort` comparison's `TypeError`). Never set
    /// for an `ErrorKind::LibraryError`, which names its function itself.
    library_function: Option<LibraryFunction>,
    vm_fault: Option<VmFault>,
}

// Keep typed metadata behind accessors so Error's existing Debug output stays stable.
impl fmt::Debug for ErrorContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ErrorContext")
            .field("name", &self.name)
            .field("reported", &self.reported)
            .finish()
    }
}

impl ErrorContext {
    fn name_bytes(&self) -> Option<&[u8]> {
        self.name.as_deref()
    }
}

impl fmt::Display for StackFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let source = LossyBytes(self.source.as_deref().unwrap_or(b"[C]"));
        let func_desc = match (&self.function_name, self.defined_line) {
            (Some(name), _) => format!("function '{name}'"),
            (None, 0) => "main chunk".to_string(),
            (None, defined) => match self.source.as_deref() {
                Some(chunk) => format!("function <{}:{defined}>", LossyBytes(chunk)),
                None => format!("function <?:{defined}>"),
            },
        };
        write!(f, "{}:{}: in {}", source, self.line, func_desc)
    }
}

/// An error raised by the parser or VM. Carries an [`ErrorKind`], the source
/// location where it surfaced, and a stack trace.
///
/// One pointer wide. Every fallible VM and parser function returns
/// `Result<_, Error>`, and in unoptimized builds each `?` site reserves its
/// own stack slots for that `Result`; with the 88-byte unboxed layout those
/// slots made one dispatch-loop frame about 40 KB, which decided how much
/// native stack every re-entry and parser level needs (docs/limits.md,
/// "Native stack"). Errors kill the callback, so the allocation is never on a
/// path that continues executing.
pub struct Error(Box<ErrorRepr>);

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug)]
struct ErrorRepr {
    /// What went wrong.
    kind: ErrorKind,
    /// 1-based source line number where the error surfaced (0 if unknown).
    line_num: u32,
    /// 1-based source column (0 if unknown).
    column: u32,
    /// Optional reporting and library context, allocated only when set.
    context: Option<Box<ErrorContext>>,
    /// Stack trace at the point of error (innermost frame first).
    stack_trace: Vec<StackFrame>,
}

/// Which source location an error renders as its `chunk:line:` prefix: the
/// prefix policy of an [`ErrorKind::ScriptError`].
///
/// `error(msg, level)` chooses it for a string value: level 1 (the default)
/// blames the frame that raised the error, level 0 suppresses the prefix, and
/// level N blames N-1 frames further up the traceback. For any other value it
/// records `Suppressed`, as Lua 5.4 adds position information only to
/// strings (Lua 5.2 also adds it to numbers). A host that builds a `ScriptError` itself controls the rendering
/// directly.
///
/// This is a closed set: every level maps to the current frame, no prefix, or
/// a frame selected from the trace. `TraceFrame` represents all non-default
/// positive levels.
#[derive(Debug, Clone, Copy)]
pub enum PrefixLocation {
    /// Blame the frame where the error surfaced. The default.
    Current,
    /// Render no `chunk:line:` prefix at all, as `error(msg, 0)` does.
    Suppressed,
    /// Blame this index into the stack trace, counting from the innermost
    /// frame. An index past the end renders no prefix.
    ///
    /// `u32` rather than `usize` so `PrefixLocation` stays 8 bytes.
    TraceFrame(u32),
}

/// Top-level error categories. See the type-specific enums (e.g.
/// [`TypeError`], [`SyntaxError`], [`ArgError`], [`PatternErrorDetail`]) for
/// finer-grained variants.
#[non_exhaustive]
#[derive(Debug)]
pub enum ErrorKind {
    /// Operation applied to a value of the wrong Lua type.
    TypeError(TypeError),
    /// Invalid argument type, presence or value for a builtin or host function.
    ArgError(ArgError),
    /// A call exceeded the runtime argument-count limit.
    TooManyArguments {
        /// Maximum supported argument count.
        limit: u8,
    },
    /// A function exceeded the runtime result-count limit.
    TooManyResults {
        /// Maximum supported result count.
        limit: u8,
    },
    /// A Rust function returned `Ok(n)` with fewer than `n` values in its
    /// frame. This is a host bug in the callback; the VM does not pad the
    /// missing results with nil.
    ResultCountExceedsStack {
        /// Result count the callback returned.
        reported: u8,
        /// Values actually in the callback's frame when it returned.
        available: usize,
    },
    /// A panic unwound through this State's execution, leaving its internal
    /// bookkeeping inconsistent. The State refuses every further operation;
    /// drop it and build a new one.
    StatePoisoned,
    /// Parser rejected the source.
    SyntaxError(SyntaxError),
    /// Script exceeded its cost budget.
    BudgetExceeded {
        /// Total cost consumed when the budget was exceeded.
        used: u64,
        /// The budget that was set.
        budget: i64,
    },
    /// Metamethod chain (`__index` / `__newindex`, or a `__call` chain whose
    /// handler is itself a non-function callable) exceeded maximum depth.
    MetamethodDepthExceeded {
        /// Recursion depth reached before bailing out.
        depth: u32,
        /// Maximum permitted by the corresponding `limits` constant.
        limit: u32,
    },
    /// Call stack depth exceeded (too much recursion).
    CallDepthExceeded {
        /// Depth reached at the failing call.
        depth: u32,
        /// Maximum permitted by the corresponding `limits` constant.
        limit: u32,
    },
    /// Too many nested calls made from Rust (host calls, metamethod
    /// handlers, iterator functions, library callbacks such as `table.sort`
    /// comparators): the `limits::MAX_NATIVE_CALL_DEPTH` bound on native
    /// recursion.
    NativeCallDepthExceeded {
        /// Calls from Rust already active at the failing call.
        depth: u32,
        /// Maximum permitted by the corresponding `limits` constant.
        limit: u32,
    },
    /// Stack size exceeded (too many values on stack).
    StackOverflow {
        /// Stack size that triggered the overflow.
        size: usize,
        /// Maximum permitted by the corresponding `limits` constant.
        limit: usize,
    },
    /// A Lua string exceeded the fixed resource limit.
    StringSizeExceeded {
        /// Size requested by the operation.
        size: usize,
        /// Maximum permitted string size.
        limit: usize,
    },
    /// Source text handed to the compiler exceeded the fixed source-size
    /// limit. Raised before tokenising, so no partial chunk is produced.
    SourceSizeExceeded {
        /// Source length in bytes.
        size: usize,
        /// Maximum permitted source length in bytes.
        limit: usize,
    },
    /// A diagnostic source name exceeded its fixed byte limit.
    SourceNameSizeExceeded {
        /// Requested source-name length in bytes.
        size: usize,
        /// Maximum permitted source-name length in bytes.
        limit: usize,
    },
    /// A host passed `ArgCount::Dynamic` to `call` or `call_anchor`. Only an
    /// executing Lua frame owns the call base a dynamic count resolves
    /// against, so a host must use `ArgCount::Fixed`. Rejected before
    /// anything executes.
    DynamicArgumentsUnsupported {
        /// The host API that was called.
        api: HostCallApi,
    },
    /// A host called `State::open_libs` inside `State::with_restricted_env`.
    /// The libraries would land in the temporary environment, which is
    /// discarded on restore, while the shared string metatable and the
    /// snapshot token map they also replace outlive it; the call is refused
    /// before any state changes.
    OpenLibsInRestrictedEnv,
    /// Invalid stack index passed to a host API.
    InvalidStackIndex {
        /// The bad index, as supplied by the caller.
        index: isize,
    },
    /// Lua pattern compilation or matching failed. The string function is
    /// available through [`Error::pattern_function`].
    PatternError(PatternErrorDetail),
    /// An embedder registered one stable id for different Rust functions.
    DuplicateFunctionRegistration {
        /// The conflicting stable function id.
        id: String,
    },
    /// Anchor handle belongs to a different `State`. Operations that need a
    /// live value return this rather than aliasing into the wrong slot.
    InvalidAnchor,
    /// Attempted to anchor `nil`. `nil` carries no GC weight and has no
    /// use case for a stable handle; embedders that need an "absent" value
    /// should use `Option<Anchor>`.
    AnchorNil,
    /// A standard-library operation failed: an invalid `next` key, a bad
    /// `string.format` directive and so on. Pattern failures use
    /// [`ErrorKind::PatternError`]. Distinct from [`ErrorKind::ScriptError`],
    /// which the script raised on purpose, and from
    /// [`ErrorKind::InternalError`], which indicates a dellingr bug.
    LibraryError(LibraryError),
    /// A failure reported by host code: a `RustFunc` returning its own error.
    /// dellingr never constructs this kind. The boxed error is the host's own
    /// value, so a host can carry a typed error through Lua and recover it
    /// with `downcast_ref` (or [`Error::into_kind`] for ownership); it is
    /// also [`std::error::Error::source`] of the enclosing [`Error`].
    /// A plain message converts with `"text".into()`.
    HostError(Box<dyn std::error::Error + Send + Sync>),
    /// An error explicitly raised by script code through `error()`. Hosts can
    /// use this to tell deliberate script termination from a VM-detected fault.
    ///
    /// The prefix policy lives here rather than on [`Error`] because no other
    /// kind has one; every other kind renders the current location.
    ScriptError {
        /// The value the script passed to `error()`, byte-exact.
        value: ErrorValue,
        /// Which frame supplies the rendered `chunk:line:` prefix.
        /// `error()` records [`PrefixLocation::Suppressed`] for a value that
        /// is not a string.
        prefix: PrefixLocation,
    },
    /// Internal error: corrupt bytecode or a dellingr bug; report these as
    /// bugs. Only dellingr constructs it - host failures use
    /// [`ErrorKind::HostError`]. Dispatch attaches structured instruction
    /// context via [`Error::vm_fault`] without widening `Error` on the
    /// recursive evaluation path.
    InternalError(InternalError),
}

/// The host call API named by [`ErrorKind::DynamicArgumentsUnsupported`].
/// `Display` renders the method name.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostCallApi {
    /// `State::call`.
    Call,
    /// `State::call_anchor`.
    CallAnchor,
}

impl fmt::Display for HostCallApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Call => "call",
            Self::CallAnchor => "call_anchor",
        })
    }
}

/// The value a script passed to `error()`.
///
/// Nil, booleans, numbers and strings are carried as values; a string keeps
/// the script's exact bytes, valid UTF-8 or not. Tables and functions cannot
/// outlive the State's garbage collector, so they carry their type and their
/// rendering instead: the `__tostring` result if the value has one, otherwise
/// the State-local identity (`table: 0x1`) that `tostring` would produce.
/// `error()` with no argument raises [`ErrorValue::Nil`], exactly like
/// `error(nil)`.
///
/// `Display` renders numbers with Lua's `tostring` spelling and bytes lossily
/// (each invalid UTF-8 sequence as U+FFFD); read the variants for exact
/// bytes.
#[derive(Debug, Clone, PartialEq)]
pub enum ErrorValue {
    /// `error()` or `error(nil)`.
    Nil,
    /// A boolean error value.
    Boolean(bool),
    /// A number error value.
    Number(f64),
    /// A string error value, byte-exact.
    String(Vec<u8>),
    /// A table or function error value, which does not survive the State.
    Object {
        /// The value's Lua type.
        value_type: LuaType,
        /// Its `tostring` rendering, byte-exact.
        rendering: Vec<u8>,
    },
}

impl ErrorValue {
    /// The Lua type of the value the script raised.
    pub fn value_type(&self) -> LuaType {
        match self {
            Self::Nil => LuaType::Nil,
            Self::Boolean(_) => LuaType::Boolean,
            Self::Number(_) => LuaType::Number,
            Self::String(_) => LuaType::String,
            Self::Object { value_type, .. } => *value_type,
        }
    }
}

impl fmt::Display for ErrorValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nil => f.write_str("nil"),
            Self::Boolean(value) => write!(f, "{value}"),
            Self::Number(number) => f.write_str(&crate::vm::number_to_string(*number)),
            Self::String(bytes)
            | Self::Object {
                rendering: bytes, ..
            } => LossyBytes(bytes).fmt(f),
        }
    }
}

/// Structured reason that Lua pattern compilation or matching failed.
///
/// The display text is reference Lua's pattern diagnostic.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternErrorDetail {
    /// A closing capture delimiter had no matching opening delimiter.
    InvalidPatternCapture,
    /// A back-reference names no valid capture. The optional index is the
    /// zero-based back-reference slot, with `%0` represented as `Some(-1)`;
    /// `None` means the matcher could not provide an index.
    InvalidCaptureIndex(Option<i8>),
    /// A pattern construct was incomplete or malformed.
    MalformedPattern(MalformedPatternDetail),
    /// The pattern declares more captures than the matcher supports.
    TooManyCaptures,
    /// Matching exceeded the bounded pattern recursion depth.
    MatchDepthExceeded,
    /// A capture was still open when matching finished.
    UnfinishedCapture,
}

/// String function whose pattern operation raised an error.
///
/// This closed set covers the pattern-taking functions in the supported Lua
/// 5.2 and 5.4 string libraries: `find`, `match`, `gmatch`, and `gsub`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringPatternFunction {
    /// `string.find`.
    Find,
    /// `string.match`.
    Match,
    /// `string.gmatch`.
    Gmatch,
    /// `string.gsub`.
    Gsub,
}

/// Standard library function named by a typed library error.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryFunction {
    /// `math.random`.
    Random,
    /// `table.insert`.
    Insert,
    /// `table.remove`.
    Remove,
    /// `table.move`.
    Move,
    /// `table.unpack` or global `unpack`.
    Unpack,
    /// `table.concat`.
    Concat,
    /// `table.sort`.
    Sort,
}

impl LibraryFunction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::Insert => "insert",
            Self::Remove => "remove",
            Self::Move => "move",
            Self::Unpack => "unpack",
            Self::Concat => "concat",
            Self::Sort => "sort",
        }
    }
}

/// A failure raised by a standard library operation, carried by
/// [`ErrorKind::LibraryError`].
#[non_exhaustive]
#[derive(Debug)]
pub enum LibraryError {
    /// A protected metatable cannot be replaced.
    ProtectedMetatable,
    /// `next` received a key that does not identify an entry in the table.
    InvalidNextKey,
    /// A table sequence operation received an unsupported position.
    PositionOutOfBounds,
    /// A table sequence is too large to sort.
    ArrayTooBig,
    /// A `__tostring` metamethod returned a value other than a string or number.
    TostringMustReturnString,
    /// The deterministic identity counter used to format a value was exhausted.
    FormatPointerIdentitySpaceExhausted,
    /// A function was called with an unsupported number of arguments.
    WrongNumberOfArguments {
        /// The function whose argument count was rejected.
        function: LibraryFunction,
    },
    /// A `string.format` conversion character is unsupported.
    InvalidFormatConversion {
        /// The conversion directive's bytes, exactly as in the format string.
        directive: Vec<u8>,
    },
    /// A `string.format` conversion directive exceeds the size limit.
    FormatDirectiveTooLong,
    /// A `%q` directive contains flags, width or precision.
    QuotedFormatHasModifiers,
    /// A `string.format` conversion contains invalid flags or precision.
    InvalidFormatSpecification {
        /// The conversion directive's bytes, exactly as in the format string.
        directive: Vec<u8>,
    },
    /// A `table.concat` element has a type other than string or number.
    InvalidConcatValue {
        /// The type of the invalid element.
        value_type: LuaType,
        /// The one-based index of the invalid element.
        index: i64,
    },
    /// An integer key range cannot be represented exactly by Lua numbers.
    IntegerKeyRangeExceeded {
        /// The function whose key range was rejected.
        function: LibraryFunction,
    },
    /// A table length metamethod returned a value without an exact integer.
    ObjectLengthNotInteger {
        /// The function that asked for the length.
        function: LibraryFunction,
    },
    /// A replacement string references a capture that does not exist.
    InvalidGsubCaptureIndex {
        /// The capture number read from the replacement string.
        index: usize,
    },
    /// A `string.gsub` replacement string contains an invalid percent escape.
    InvalidGsubEscape,
    /// A `string.gsub` replacement callback or table returned another type.
    InvalidGsubReplacementValue {
        /// The type returned by the replacement.
        value_type: LuaType,
    },
}

impl LibraryError {
    /// The [`LibraryFunction`] this failure belongs to, if it names one: the
    /// `function` field of the variants that carry it, `Concat` for
    /// [`Self::InvalidConcatValue`] and `Sort` for [`Self::ArrayTooBig`].
    /// Failures of functions outside that enum (`next`, `string.format`,
    /// `string.gsub`, ...) and of host APIs answer `None`.
    pub fn function(&self) -> Option<LibraryFunction> {
        match self {
            Self::WrongNumberOfArguments { function }
            | Self::IntegerKeyRangeExceeded { function }
            | Self::ObjectLengthNotInteger { function } => Some(*function),
            Self::InvalidConcatValue { .. } => Some(LibraryFunction::Concat),
            Self::ArrayTooBig => Some(LibraryFunction::Sort),
            Self::ProtectedMetatable
            | Self::InvalidNextKey
            | Self::PositionOutOfBounds
            | Self::TostringMustReturnString
            | Self::FormatPointerIdentitySpaceExhausted
            | Self::InvalidFormatConversion { .. }
            | Self::FormatDirectiveTooLong
            | Self::QuotedFormatHasModifiers
            | Self::InvalidFormatSpecification { .. }
            | Self::InvalidGsubCaptureIndex { .. }
            | Self::InvalidGsubEscape
            | Self::InvalidGsubReplacementValue { .. } => None,
        }
    }
}

/// The specific malformed Lua pattern construct.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MalformedPatternDetail {
    /// A pattern ended with an unpaired `%`.
    EndsWithPercent,
    /// A `%b` balanced match omitted one or both delimiter arguments.
    MissingBalancedArguments,
    /// A character class did not contain a closing `]`.
    MissingBracket,
    /// A `%f` frontier pattern omitted its character class bracket.
    MissingFrontierBracket,
}

/// Argument error raised by builtins or host functions.
///
/// A missing `func_name` means no name was supplied or resolved. When
/// `State::check_any` or `State::check_type` fails inside a Rust callback, the
/// VM looks the callback up only then: first the first current global bound to
/// the same function pointer, then, with the `snapshot` feature, the callback's
/// registered stable id. Callbacks found by neither remain anonymous. The name
/// is diagnostic text and can differ between builds; see
/// `reference/error-reporting.md`.
#[derive(Debug)]
pub struct ArgError {
    /// 1-based argument index that failed validation. Negative values count
    /// from the end of the argument list.
    arg_number: isize,
    /// Name of the function the argument was passed to (if known).
    func_name: Option<String>,
    /// Reason validation failed.
    detail: ArgDetail,
    /// Expected Lua type for this argument.
    expected: Option<LuaType>,
    /// Lua type the caller actually supplied.
    received: Option<LuaType>,
    /// Whether the failing call was written as a method call (`obj:name()`).
    /// `arg_number` keeps the callee's own position; the rendering counts
    /// from the first argument the script author typed, as reference Lua
    /// does, and calls a bad receiver "bad self".
    method_call: bool,
    /// Set once the innermost Rust callback that returned this error has
    /// decided `method_call`, so an outer call that merely propagates the
    /// error cannot reinterpret it.
    settled: bool,
}

use crate::limits::MAX_RESULTS;

/// Validate a result count against the call ABI, whose cap is
/// [`crate::limits::MAX_RESULTS`]. Applied to Rust callback returns and to
/// every Lua `return` with a dynamic result count. Public so `RustFunc`
/// authors returning a computed count can apply the same check the VM does.
pub fn results_count(count: usize) -> Result<u8, ErrorKind> {
    u8::try_from(count).map_err(|_| ErrorKind::TooManyResults { limit: MAX_RESULTS })
}

/// Structured reason an argument failed validation.
///
/// Equality compares the structured reason by its exact variant.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgDetail {
    /// Type or presence mismatch, described by `expected` and `received`.
    TypeMismatch,
    /// A number cannot be represented as an integer.
    NoInteger,
    /// A random interval contains no values.
    IntervalEmpty,
    /// A required argument was not supplied.
    NoValue,
    /// A string contains a zero byte where `string.format` cannot accept one.
    StringContainsZeros,
    /// A value has no literal form for `string.format`'s `%q` conversion.
    ValueHasNoLiteralForm,
    /// `tonumber` received a base outside its supported range.
    BaseOutOfRange,
    /// `select` received an index below its supported range.
    IndexOutOfRange,
    /// `rawlen` requires a table or string.
    TableOrStringExpected,
    /// `string.gsub` requires a string, function or table replacement.
    StringFunctionOrTableExpected,
    /// A table sequence position is outside its supported range.
    PositionOutOfBounds,
    /// `table.move` was asked to move more elements than it supports; the
    /// argument is the end of the source range.
    TooManyElementsToMove,
    /// `table.move`'s destination range would run past the largest integer;
    /// the argument is the destination start.
    DestinationWrapAround,
}

impl ArgDetail {
    fn rendered_detail(&self) -> Option<&str> {
        match self {
            Self::NoInteger => Some("number has no integer representation"),
            Self::IntervalEmpty => Some("interval is empty"),
            Self::NoValue => Some("no value"),
            Self::StringContainsZeros => Some("string contains zeros"),
            Self::ValueHasNoLiteralForm => Some("value has no literal form"),
            Self::BaseOutOfRange => Some("base out of range"),
            Self::IndexOutOfRange => Some("index out of range"),
            Self::TableOrStringExpected => Some("table or string expected"),
            Self::StringFunctionOrTableExpected => Some("string/function/table expected"),
            Self::PositionOutOfBounds => Some("position out of bounds"),
            Self::TooManyElementsToMove => Some("too many elements to move"),
            Self::DestinationWrapAround => Some("destination wrap around"),
            Self::TypeMismatch => None,
        }
    }
}

/// Syntax-level error categories raised by the parser.
#[non_exhaustive]
#[derive(Debug)]
pub enum SyntaxError {
    /// Numeric literal could not be parsed as a number.
    BadNumber,
    /// `break` used outside any enclosing loop.
    BreakOutsideLoop,
    /// Character not valid as part of any token.
    InvalidCharacter(char),
    /// A byte that is neither valid as part of any token nor the start of a
    /// valid UTF-8 character.
    InvalidByte(u8),
    /// Escape sequence is not supported by Lua string literals.
    InvalidEscapeSequence,
    /// A hexadecimal string escape did not contain two hexadecimal digits.
    HexadecimalDigitExpected,
    /// A decimal string escape exceeds the byte range.
    DecimalEscapeTooLarge,
    /// Long bracketed strings are deliberately unsupported.
    LongStringUnsupported,
    /// More expressions in a single source construct than the VM accepts.
    TooManyExpressions,
    /// An assignment has more targets than the compiler accepts.
    TooManyAssignmentTargets {
        /// Target count at the first rejected target.
        count: usize,
    },
    /// A function needs more temporary registers than bytecode can encode.
    TooManyRegisters {
        /// Number of registers requested.
        count: usize,
    },
    /// An assignment target stack offset exceeds the bytecode operand width.
    AssignmentOffsetTooLarge,
    /// More fixed assignment results than a result-count operand can encode.
    TooManyFixedResults,
    /// Function body has more locals than the VM accepts.
    TooManyLocals,
    /// Function has more upvalues than the bytecode can encode.
    TooManyUpvalues,
    /// Source exceeds the parser's bounded syntax nesting depth.
    TooManySyntaxLevels,
    /// A fixed-arity call uses the dynamic-argument sentinel.
    TooManyArguments,
    /// A control-flow target cannot be represented by a jump instruction.
    JumpTooFar,
    /// A chunk has more direct child functions than the bytecode can index.
    TooManyNestedFunctions,
    /// More numeric literals in a chunk than the literal pool accepts.
    TooManyNumbers,
    /// More string literals in a chunk than the literal pool accepts.
    TooManyStrings,
    /// More fields in a single table constructor than the VM accepts.
    TooManyTableFields,
    /// String literal not closed before EOF or end of line.
    UnclosedString,
    /// Source ended while a construct was still open.
    UnexpectedEof,
    /// A long comment (`--[[` / `--[==[`) reached EOF before its closing
    /// delimiter. Reported at the comment's opener.
    UnfinishedLongComment,
    /// A required token was missing before another token.
    ExpectedToken {
        /// The token the grammar required.
        expected: TokenKind,
        /// The source bytes of the token actually found, exactly as written.
        found: Vec<u8>,
    },
    /// A token that cannot start an expression or statement where one was
    /// required.
    UnexpectedSymbol {
        /// The source bytes of the offending token, exactly as written.
        found: Vec<u8>,
    },
    /// An expression statement that is neither a call nor an assignment to a
    /// variable, such as `x + 1`, `(x) = 1` or `f() = 1`.
    MalformedStatement {
        /// The source bytes of the token after the expression, exactly as
        /// written.
        found: Vec<u8>,
    },
    /// A `for` loop's first name was followed by neither `=` nor `in`.
    ForSeparatorExpected {
        /// The source bytes of the token actually found, exactly as written.
        found: Vec<u8>,
    },
    /// A method call's `:name` was not followed by call arguments.
    FunctionArgumentsExpected {
        /// The source bytes of the token actually found, exactly as written.
        found: Vec<u8>,
    },
    /// `...` was used outside a vararg function.
    VarargOutsideVarargFunction,
}

/// A kind of Lua token, as named by [`SyntaxError::ExpectedToken`].
///
/// `Display` renders it as reference Lua does: keywords and symbols quoted
/// (`'end'`, `'='`), names, literals and end of input as their unquoted
/// category (`<name>`, `<number>`, `<string>`, `<eof>`).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// `and`.
    And,
    /// `break`.
    Break,
    /// `do`.
    Do,
    /// `else`.
    Else,
    /// `elseif`.
    ElseIf,
    /// `end`.
    End,
    /// `false`.
    False,
    /// `for`.
    For,
    /// `function`.
    Function,
    /// `if`.
    If,
    /// `in`.
    In,
    /// `local`.
    Local,
    /// `nil`.
    Nil,
    /// `not`.
    Not,
    /// `or`.
    Or,
    /// `repeat`.
    Repeat,
    /// `return`.
    Return,
    /// `then`.
    Then,
    /// `true`.
    True,
    /// `until`.
    Until,
    /// `while`.
    While,
    /// `+`.
    Plus,
    /// `-`.
    Minus,
    /// `*`.
    Star,
    /// `/`.
    Slash,
    /// `%`.
    Mod,
    /// `^`.
    Caret,
    /// `#`.
    Hash,
    /// `==`.
    Equal,
    /// `~=`.
    NotEqual,
    /// `<=`.
    LessEqual,
    /// `>=`.
    GreaterEqual,
    /// `<`.
    Less,
    /// `>`.
    Greater,
    /// `(`.
    LParen,
    /// `)`.
    RParen,
    /// `{`.
    LCurly,
    /// `}`.
    RCurly,
    /// `[`.
    LSquare,
    /// `]`.
    RSquare,
    /// `;`.
    Semi,
    /// `:`.
    Colon,
    /// `,`.
    Comma,
    /// `.`.
    Dot,
    /// `..`.
    DotDot,
    /// `...`.
    DotDotDot,
    /// `=`.
    Assign,
    /// A name.
    Identifier,
    /// A numeric literal.
    LiteralNumber,
    /// A string literal.
    LiteralString,
    /// End of input.
    EndOfFile,
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Identifier | Self::LiteralNumber | Self::LiteralString | Self::EndOfFile => {
                f.write_str(self.spelling())
            }
            _ => write!(f, "'{}'", self.spelling()),
        }
    }
}

/// Type errors raised by arithmetic, numeric for loops, comparison, indexing,
/// and concat.
#[non_exhaustive]
#[derive(Debug)]
pub enum TypeError {
    /// Arithmetic op applied to a non-numeric value of the given type.
    Arithmetic(LuaType),
    /// The initial value of a numeric `for` loop was not numeric.
    NumericForInitialValue(LuaType),
    /// The limit of a numeric `for` loop was not numeric.
    NumericForLimit(LuaType),
    /// The step of a numeric `for` loop was not numeric.
    NumericForStep(LuaType),
    /// Comparison applied to incompatible types.
    Comparison(LuaType, LuaType),
    /// `..` (concat) applied to a non-string non-number of the given type.
    Concat(LuaType),
    /// Attempted to call a value that is not a function.
    FunctionCall(LuaType),
    /// `#` applied to a value that is neither a string nor a table.
    Length(LuaType),
    /// Indexed (`t[k]`, `t.k`, `t:m`) into a non-table value.
    TableIndex(LuaType),
    /// Used `NaN` as a table key.
    TableKeyNan,
    /// Used `nil` as a table key.
    TableKeyNil,
}

// main impls

impl Error {
    /// Construct an error with explicit source location.
    pub fn new(kind: impl Into<ErrorKind>, line_num: u32, column: u32) -> Self {
        Self::boxed(kind.into(), line_num, column)
    }

    /// The one allocation site. Cold and out of line so the allocation is
    /// not inlined into every fallible fast path, the dispatch loop included,
    /// where it would cost code size and registers on paths that never fail.
    #[cold]
    #[inline(never)]
    fn boxed(kind: ErrorKind, line_num: u32, column: u32) -> Self {
        Error(Box::new(ErrorRepr {
            kind,
            line_num,
            column,
            context: None,
            stack_trace: Vec::new(),
        }))
    }

    /// Construct an error without source-location information. Used for
    /// errors raised before any chunk has been parsed (e.g. host API misuse).
    pub fn without_location(kind: ErrorKind) -> Self {
        Self::boxed(kind, 0, 0)
    }

    pub(crate) fn internal_fault(fault: InternalFault) -> Self {
        Self::without_location(ErrorKind::InternalError(InternalError(fault)))
    }

    /// Attach a stack trace to this error. The VM attaches the real trace when
    /// an error unwinds out of a call; a host-supplied trace is reported to
    /// `on_error` as given. Public chiefly so hosts and tests can build errors
    /// with known traces.
    pub fn with_stack_trace(mut self, trace: Vec<StackFrame>) -> Self {
        self.0.stack_trace = trace;
        self
    }

    pub(crate) fn with_location(mut self, line_num: u32, column: u32) -> Self {
        self.0.line_num = line_num;
        self.0.column = column;
        self
    }

    pub(crate) fn with_source_name(mut self, source_name: Option<Arc<[u8]>>) -> Self {
        if source_name.is_some() || self.0.context.is_some() {
            self.context_mut().name = source_name;
        }
        self
    }

    /// Attach the reporting frame's source name unless the error already
    /// carries one, so `on_error` sees where the error was first reported.
    pub(crate) fn attach_source_name_if_absent(&mut self, source_name: Option<&Arc<[u8]>>) {
        if self.source_name().is_none()
            && let Some(name) = source_name
        {
            self.context_mut().name = Some(Arc::clone(name));
        }
    }

    fn context_mut(&mut self) -> &mut ErrorContext {
        self.0.context.get_or_insert_with(|| {
            Box::new(ErrorContext {
                name: None,
                reported: false,
                pattern_function: None,
                library_function: None,
                vm_fault: None,
            })
        })
    }

    pub(crate) fn mark_reported(&mut self) -> bool {
        let context = self.context_mut();
        let first_report = !context.reported;
        context.reported = true;
        first_report
    }

    /// Chunk name attached to this error, byte-exact, if it came from named
    /// source.
    pub fn source_name(&self) -> Option<&[u8]> {
        self.0.context.as_deref().and_then(ErrorContext::name_bytes)
    }

    /// Error category and details.
    pub fn kind(&self) -> &ErrorKind {
        &self.0.kind
    }

    /// Take the error category, discarding location and context; the owned
    /// way to recover an [`ErrorKind::HostError`] payload or an
    /// [`ErrorValue`].
    pub fn into_kind(self) -> ErrorKind {
        self.0.kind
    }

    /// Stack frames at the point the error was raised, innermost first.
    pub fn stack_trace(&self) -> &[StackFrame] {
        &self.0.stack_trace
    }

    /// Record whether the call whose Rust callback produced this error was a
    /// method call. Only the first (innermost) callback to return an argument
    /// error decides; outer calls propagating it leave it alone.
    pub(crate) fn settle_method_call(mut self, method: bool) -> Self {
        if let ErrorKind::ArgError(argument) = &mut self.0.kind
            && !argument.settled
        {
            argument.settled = true;
            argument.method_call = method;
        }
        self
    }

    /// Give an argument error the explicit standard-library function name.
    /// Updating the kind in place keeps this error's source and reporting
    /// state attached.
    pub(crate) fn with_argument_function_name(mut self, function_name: &str) -> Self {
        if let ErrorKind::ArgError(argument) = &mut self.0.kind {
            argument.func_name = Some(function_name.to_owned());
        }
        self
    }

    /// String function whose pattern operation produced this error, if any.
    pub fn pattern_function(&self) -> Option<StringPatternFunction> {
        self.0
            .context
            .as_deref()
            .and_then(|context| context.pattern_function)
    }

    /// Standard-library function responsible for this error, if known.
    ///
    /// For an [`ErrorKind::LibraryError`] this is [`LibraryError::function`].
    /// The one other attributed error is the [`TypeError::Comparison`] that
    /// `table.sort`'s default comparison raises, which answers `Sort`. Argument
    /// errors name their function in [`ArgError::func_name`] instead, and an
    /// error propagated out of a metamethod handler, comparator or other
    /// callback the library called answers `None`.
    pub fn library_function(&self) -> Option<LibraryFunction> {
        match &self.0.kind {
            ErrorKind::LibraryError(error) => error.function(),
            _ => self
                .0
                .context
                .as_deref()
                .and_then(|context| context.library_function),
        }
    }

    pub(crate) fn with_pattern_function(mut self, function: StringPatternFunction) -> Self {
        self.context_mut().pattern_function = Some(function);
        self
    }

    /// Attribute an error whose kind cannot name its library function. A
    /// `LibraryError` names its own, so attributing one here would create a
    /// second source that `library_function` ignores.
    pub(crate) fn with_library_function(mut self, function: LibraryFunction) -> Self {
        debug_assert!(
            !matches!(self.0.kind, ErrorKind::LibraryError(_)),
            "a LibraryError carries its function in its variant"
        );
        self.context_mut().library_function = Some(function);
        self
    }

    /// Instruction context shared by internal dispatch failures.
    pub fn vm_fault(&self) -> Option<&VmFault> {
        self.0
            .context
            .as_deref()
            .and_then(|context| context.vm_fault.as_ref())
    }

    pub(crate) fn with_vm_fault(
        mut self,
        instruction: usize,
        opcode: u8,
        function_name: Option<&Arc<str>>,
    ) -> Self {
        if matches!(self.0.kind, ErrorKind::InternalError(_)) && self.vm_fault().is_none() {
            self.context_mut().vm_fault = Some(VmFault {
                instruction,
                opcode,
                function_name: function_name.cloned(),
            });
        }
        self
    }

    /// 1-based column number where the error surfaced (0 if unknown).
    pub fn column(&self) -> u32 {
        self.0.column
    }

    /// 1-based line number where the error surfaced (0 if unknown).
    pub fn line_num(&self) -> u32 {
        self.0.line_num
    }

    /// Whether the host can plausibly retry the operation after handling the
    /// error. Currently only some [`SyntaxError`] variants are recoverable:
    /// those that appending more source text can fix. Intended for host REPLs
    /// that keep reading lines until a chunk is complete; the crate's own CLI
    /// has no REPL and does not call it.
    pub fn is_recoverable(&self) -> bool {
        self.0.kind.is_recoverable()
    }
}

impl ErrorKind {
    /// Build a named argument error from a structured validation reason.
    /// Public for `RustFunc` authors who want their argument failures to render
    /// and compare like the standard library's.
    pub fn argument_detail(arg_number: isize, function_name: &str, detail: ArgDetail) -> Self {
        Self::ArgError(ArgError {
            arg_number,
            func_name: Some(function_name.to_owned()),
            detail,
            expected: None,
            received: None,
            method_call: false,
            settled: false,
        })
    }

    /// Whether the underlying error category is recoverable - see
    /// [`Error::is_recoverable`].
    pub fn is_recoverable(&self) -> bool {
        if let Self::SyntaxError(e) = self {
            e.is_recoverable()
        } else {
            false
        }
    }
}

impl ArgError {
    /// One-based argument index that failed validation. Negative values count
    /// from the end of the argument list.
    pub fn arg_number(&self) -> isize {
        self.arg_number
    }

    /// Name of the function the argument was passed to, if known.
    pub fn func_name(&self) -> Option<&str> {
        self.func_name.as_deref()
    }

    /// Structured reason validation failed.
    pub fn detail(&self) -> &ArgDetail {
        &self.detail
    }

    /// Expected Lua type for this argument.
    pub fn expected(&self) -> Option<LuaType> {
        self.expected
    }

    /// Lua type the caller actually supplied.
    pub fn received(&self) -> Option<LuaType> {
        self.received
    }

    /// Whether the failing call was a method call (`obj:name()`). The
    /// rendered message then counts arguments without the receiver, and
    /// reports a bad first argument as a bad `self`; [`Self::arg_number`] is
    /// still the callee's own position.
    pub fn is_method_call(&self) -> bool {
        self.method_call
    }

    /// Build an error for a required argument without an expected type.
    pub(crate) fn required(arg_number: isize, func_name: Option<&str>) -> Self {
        Self {
            arg_number,
            func_name: func_name.map(str::to_owned),
            detail: ArgDetail::TypeMismatch,
            expected: None,
            received: None,
            method_call: false,
            settled: false,
        }
    }

    /// Build a typed argument type mismatch.
    pub(crate) fn type_mismatch(
        arg_number: isize,
        func_name: Option<&str>,
        expected: LuaType,
        received: Option<LuaType>,
    ) -> Self {
        Self {
            arg_number,
            func_name: func_name.map(str::to_owned),
            detail: ArgDetail::TypeMismatch,
            expected: Some(expected),
            received,
            method_call: false,
            settled: false,
        }
    }
}

impl SyntaxError {
    /// Returns true if this is a SyntaxError that can be fixed by appending
    /// more text to the source code.
    pub fn is_recoverable(&self) -> bool {
        // Incomplete EOF constructs are recoverable. UnclosedString also
        // covers unescaped line breaks, which appending text cannot fix, so
        // short-string errors remain nonrecoverable.
        // An unfinished long comment is the comment analogue of EOF: appending
        // its closing delimiter completes the source.
        matches!(self, Self::UnexpectedEof | Self::UnfinishedLongComment)
    }
}

// `Display` impls

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Take the source and the line from the SAME record. Reading the source
        // from the innermost frame but the line from `self.0.line_num` can splice
        // together two unrelated locations: a host `RustFunc` may return an
        // `Error` carrying its own line, which says nothing about the Lua chunk
        // whose name is about to be printed next to it.
        let prefix_location = match &self.0.kind {
            ErrorKind::ScriptError { prefix, .. } => *prefix,
            _ => PrefixLocation::Current,
        };
        let prefix_frame = match prefix_location {
            PrefixLocation::Current => self.0.stack_trace.first(),
            PrefixLocation::Suppressed => None,
            PrefixLocation::TraceFrame(index) => self.0.stack_trace.get(index as usize),
        };
        let suppress_prefix = matches!(prefix_location, PrefixLocation::Suppressed)
            || matches!(prefix_location, PrefixLocation::TraceFrame(_)) && prefix_frame.is_none();
        // The line always comes from the selected frame, whether or not that
        // frame knows its source name. Falling back to `self.0.line_num` when the
        // source is absent would report the innermost line under an
        // `error(msg, 2)` that deliberately selected a caller.
        if suppress_prefix {
            write!(f, "{}", self.0.kind)?;
        } else {
            match prefix_frame {
                Some(frame) => match frame.source.as_deref() {
                    Some(source) => {
                        write!(f, "{}:{}: {}", LossyBytes(source), frame.line, self.0.kind)?;
                    }
                    None => {
                        // `self.0.column` belongs to the innermost error site.
                        // Pair it with the frame's line only when that frame
                        // IS the innermost site; otherwise the column is
                        // unknown (0) for the selected frame.
                        let same_site = matches!(
                            prefix_location,
                            PrefixLocation::Current | PrefixLocation::TraceFrame(0)
                        ) && frame.line != 0
                            && frame.line == self.0.line_num;
                        let column = if same_site { self.0.column } else { 0 };
                        write!(f, "{}:{}: {}", frame.line, column, self.0.kind)?;
                    }
                },
                None => match self.source_name().map(LossyBytes) {
                    Some(source) if self.0.line_num != 0 && self.0.column != 0 => write!(
                        f,
                        "{source}:{}:{}: {}",
                        self.0.line_num, self.0.column, self.0.kind
                    )?,
                    Some(source) if self.0.line_num != 0 => {
                        write!(f, "{source}:{}: {}", self.0.line_num, self.0.kind)?;
                    }
                    Some(source) => write!(f, "{source}: {}", self.0.kind)?,
                    None => write!(f, "{}:{}: {}", self.0.line_num, self.0.column, self.0.kind)?,
                },
            }
        }
        if !self.0.stack_trace.is_empty() {
            write!(f, "\nstack traceback:")?;
            for frame in &self.0.stack_trace {
                write!(f, "\n\t{frame}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for PatternErrorDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use PatternErrorDetail::*;
        match self {
            InvalidPatternCapture => f.write_str("invalid pattern capture"),
            InvalidCaptureIndex(None) => f.write_str("invalid capture index"),
            InvalidCaptureIndex(Some(idx)) => {
                write!(f, "invalid capture index %{}", i16::from(*idx) + 1)
            }
            MalformedPattern(reason) => write!(f, "malformed pattern ({reason})"),
            TooManyCaptures => f.write_str("too many captures"),
            MatchDepthExceeded => f.write_str("pattern too complex"),
            UnfinishedCapture => f.write_str("unfinished capture"),
        }
    }
}

impl fmt::Display for MalformedPatternDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EndsWithPercent => "ends with '%'",
            Self::MissingBalancedArguments => "missing arguments to '%b'",
            Self::MissingBracket => "missing ']'",
            Self::MissingFrontierBracket => "missing '[' after '%f' in pattern",
        };
        f.write_str(message)
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ErrorKind::*;
        if matches!(self, InternalError(_)) {
            f.write_str("internal error: ")?;
        }
        match self {
            ArgError(e) => e.fmt(f),
            TooManyArguments { limit } => write!(f, "too many arguments (limit {limit})"),
            TooManyResults { limit } => write!(f, "too many results (limit {limit})"),
            ResultCountExceedsStack {
                reported,
                available,
            } => write!(
                f,
                "Rust function reported {reported} results but its frame holds {available} values"
            ),
            StatePoisoned => f.write_str(
                "state poisoned: a panic unwound through it, so it can no longer be used",
            ),
            SyntaxError(e) => e.fmt(f),
            TypeError(e) => e.fmt(f),
            BudgetExceeded { used, budget } => {
                write!(
                    f,
                    "budget exceeded: used {used} cost with budget of {budget}"
                )
            }
            MetamethodDepthExceeded { depth, limit } => {
                write!(
                    f,
                    "metamethod chain too deep (depth {depth}, limit {limit})"
                )
            }
            CallDepthExceeded { depth, limit } => {
                write!(f, "call stack overflow (depth {depth}, limit {limit})")
            }
            NativeCallDepthExceeded { depth, limit } => {
                write!(
                    f,
                    "native call stack overflow (depth {depth}, limit {limit})"
                )
            }
            StackOverflow { size, limit } => {
                write!(f, "stack overflow ({size} values, limit {limit})")
            }
            StringSizeExceeded { size, limit } => {
                write!(f, "string size {size} exceeds limit {limit}")
            }
            SourceSizeExceeded { size, limit } => {
                write!(f, "source size {size} exceeds limit {limit}")
            }
            SourceNameSizeExceeded { size, limit } => {
                write!(f, "source name size {size} exceeds limit {limit}")
            }
            DynamicArgumentsUnsupported { api } => {
                write!(
                    f,
                    "{api} does not support ArgCount::Dynamic; use ArgCount::Fixed"
                )
            }
            OpenLibsInRestrictedEnv => {
                f.write_str("cannot open libraries inside a restricted environment")
            }
            InvalidStackIndex { index } => {
                write!(f, "invalid stack index ({index})")
            }
            PatternError(detail) => fmt::Display::fmt(detail, f),
            DuplicateFunctionRegistration { id } => {
                write!(f, "duplicate Rust function registration: {id}")
            }
            InvalidAnchor => write!(f, "invalid anchor (issued by a different State)"),
            AnchorNil => {
                write!(f, "cannot anchor nil")
            }
            // A script's `error()` value and a library error render the same
            // way, exactly as reference does. The variants are distinct so
            // hosts can tell deliberate script termination from a VM-detected
            // fault, which is not a rendering difference.
            LibraryError(error) => fmt::Display::fmt(error, f),
            HostError(error) => fmt::Display::fmt(error, f),
            ScriptError { value, .. } => fmt::Display::fmt(value, f),
            InternalError(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for Error {
    /// The host's own error behind an [`ErrorKind::HostError`], itself rather
    /// than its source, so it can be downcast.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.0.kind {
            ErrorKind::HostError(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let func_name = self.func_name.as_deref().unwrap_or("<anonymous>");
        let extra = match &self.detail {
            ArgDetail::TypeMismatch => match (&self.expected, &self.received) {
                (Some(expected), Some(got)) => format!("{expected} expected, got {got}"),
                (Some(expected), None) => format!("{expected} expected, got no value"),
                (None, _) => "value expected".into(),
            },
            detail => detail
                .rendered_detail()
                .expect("every non-type argument detail has rendered wording")
                .to_owned(),
        };

        if self.method_call && self.arg_number > 0 {
            return match self.arg_number - 1 {
                0 => write!(f, "calling '{func_name}' on bad self ({extra})"),
                number => write!(f, "bad argument #{number} to '{func_name}' ({extra})"),
            };
        }
        write!(
            f,
            "bad argument #{} to '{}' ({})",
            self.arg_number, func_name, extra
        )
    }
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProtectedMetatable => f.write_str("cannot change a protected metatable"),
            Self::InvalidNextKey => f.write_str("invalid key to 'next'"),
            Self::PositionOutOfBounds => f.write_str("position out of bounds"),
            Self::ArrayTooBig => f.write_str("array too big"),
            Self::TostringMustReturnString => f.write_str("'__tostring' must return a string"),
            Self::FormatPointerIdentitySpaceExhausted => {
                f.write_str("format pointer identity space exhausted")
            }
            Self::WrongNumberOfArguments { function } => {
                write!(f, "wrong number of arguments to '{}'", function.as_str())
            }
            Self::InvalidFormatConversion { directive } => {
                write!(
                    f,
                    "invalid conversion '{}' to 'format'",
                    LossyBytes(directive)
                )
            }
            Self::FormatDirectiveTooLong => f.write_str("invalid format (too long)"),
            Self::QuotedFormatHasModifiers => f.write_str("specifier '%q' cannot have modifiers"),
            Self::InvalidFormatSpecification { directive } => {
                write!(
                    f,
                    "invalid conversion specification: '{}'",
                    LossyBytes(directive)
                )
            }
            Self::InvalidConcatValue { value_type, index } => write!(
                f,
                "invalid value ({value_type}) at index {index} in table for 'concat'"
            ),
            Self::IntegerKeyRangeExceeded { function } => write!(
                f,
                "{} range exceeds the exactly representable integer keys",
                function.as_str()
            ),
            Self::ObjectLengthNotInteger { .. } => f.write_str("object length is not an integer"),
            Self::InvalidGsubCaptureIndex { .. } => f.write_str("invalid capture index"),
            Self::InvalidGsubEscape => f.write_str("invalid use of '%' in replacement string"),
            Self::InvalidGsubReplacementValue { value_type } => {
                write!(f, "invalid replacement value (a {value_type})")
            }
        }
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use SyntaxError::*;
        match self {
            BadNumber => write!(f, "malformed number"),
            // The source location belongs to the enclosing Error, which
            // formats the actual line and column for this syntax error.
            BreakOutsideLoop => write!(f, "<break> not inside a loop"),
            InvalidCharacter(c) => write!(f, "invalid character '{c}'"),
            InvalidByte(byte) => write!(f, "invalid character '<\\{byte}>'"),
            InvalidEscapeSequence => write!(f, "invalid escape sequence"),
            HexadecimalDigitExpected => write!(f, "hexadecimal digit expected"),
            DecimalEscapeTooLarge => write!(f, "decimal escape too large"),
            LongStringUnsupported => write!(f, "long strings are not supported"),
            TooManyExpressions => write!(f, "too many expressions (limit {MAX_EXPRESSIONS})"),
            TooManyAssignmentTargets { count } => write!(
                f,
                "too many assignment targets ({count}, limit {})",
                crate::limits::MAX_ASSIGNMENT_TARGETS
            ),
            TooManyRegisters { count } => write!(
                f,
                "too many registers ({count}, limit {})",
                crate::limits::MAX_REGISTERS
            ),
            AssignmentOffsetTooLarge => write!(
                f,
                "assignment receiver offset exceeds {MAX_ASSIGNMENT_OFFSET}"
            ),
            TooManyFixedResults => write!(
                f,
                "too many fixed results in multiple assignment (limit {MAX_FIXED_RESULTS})"
            ),
            TooManyLocals => write!(f, "too many local variables (limit {MAX_LOCALS})"),
            TooManyUpvalues => write!(f, "too many upvalues (limit {MAX_UPVALUES})"),
            TooManySyntaxLevels => write!(f, "chunk has too many syntax levels"),
            TooManyArguments => write!(
                f,
                "too many arguments in a fixed call (limit {MAX_FIXED_ARGS})"
            ),
            JumpTooFar => write!(
                f,
                "control-flow jump is too far (exceeds signed 24-bit range)"
            ),
            TooManyNestedFunctions => {
                write!(f, "too many direct child functions (limit {MAX_NESTED})")
            }
            TooManyNumbers => {
                write!(f, "too many literal numbers (limit {MAX_LITERALS})")
            }
            TooManyStrings => {
                write!(f, "too many literal strings (limit {MAX_LITERALS})")
            }
            TooManyTableFields => write!(f, "too many fields in table constructor"),
            UnclosedString => write!(f, "unfinished string"),
            UnexpectedEof => write!(f, "unexpected <eof>"),
            UnfinishedLongComment => write!(f, "unfinished long comment"),
            ExpectedToken { expected, found } => {
                write!(f, "{expected} expected near '{}'", LossyBytes(found))
            }
            UnexpectedSymbol { found } => {
                write!(f, "unexpected symbol near '{}'", LossyBytes(found))
            }
            MalformedStatement { found } => {
                write!(f, "syntax error near '{}'", LossyBytes(found))
            }
            ForSeparatorExpected { found } => {
                write!(f, "'=' or 'in' expected near '{}'", LossyBytes(found))
            }
            FunctionArgumentsExpected { found } => {
                write!(
                    f,
                    "function arguments expected near '{}'",
                    LossyBytes(found)
                )
            }
            VarargOutsideVarargFunction => {
                write!(f, "cannot use '...' outside a vararg function")
            }
        }
    }
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use TypeError::*;
        match self {
            Arithmetic(typ) => write!(f, "attempt to perform arithmetic on a {typ} value"),
            NumericForInitialValue(typ) => {
                write!(
                    f,
                    "numeric for loop initial value must be a number, got {typ}"
                )
            }
            NumericForLimit(typ) => {
                write!(f, "numeric for loop limit must be a number, got {typ}")
            }
            NumericForStep(typ) => {
                write!(f, "numeric for loop step must be a number, got {typ}")
            }
            Comparison(type1, type2) if type1 == type2 => {
                write!(f, "attempt to compare two {type1} values")
            }
            Comparison(type1, type2) => write!(f, "attempt to compare {type1} with {type2}"),
            Concat(typ) => write!(f, "attempt to concatenate a {typ} value"),
            FunctionCall(typ) => write!(f, "attempt to call a {typ} value"),
            Length(typ) => write!(f, "attempt to get length of a {typ} value"),
            TableIndex(typ) => write!(f, "attempt to index a {typ} value"),
            TableKeyNan => write!(f, "table index was NaN"),
            TableKeyNil => write!(f, "table index was nil"),
        }
    }
}

// `From` impls

impl From<ArgError> for ErrorKind {
    fn from(e: ArgError) -> Self {
        Self::ArgError(e)
    }
}

impl From<SyntaxError> for ErrorKind {
    fn from(e: SyntaxError) -> Self {
        Self::SyntaxError(e)
    }
}

impl From<TypeError> for ErrorKind {
    fn from(e: TypeError) -> Self {
        Self::TypeError(e)
    }
}
