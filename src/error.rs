use std::fmt;
use std::sync::Arc;

use crate::LuaType;
use crate::limits::{
    MAX_ASSIGNMENT_OFFSET, MAX_EXPRESSIONS, MAX_FIXED_ARGS, MAX_FIXED_RESULTS, MAX_LITERALS,
    MAX_LOCALS, MAX_NESTED, MAX_UPVALUES,
};

// Types

/// A single frame in a stack trace.
#[derive(Debug, Clone)]
pub struct StackFrame {
    /// Function name (if known).
    pub function_name: Option<String>,
    /// Source file or chunk name.
    pub source: Option<String>,
    /// Line number where the call occurred (1-indexed).
    pub line: u32,
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
    /// Diagnostic function name, if the chunk has one.
    pub function_name: Option<String>,
}

pub(crate) enum InternalFault {
    CallDynamicArguments,
    CallAnchorDynamicArguments,
    Compiler(CompilerFault),
    CompilerValidation {
        chunk: String,
        detail: String,
    },
    GsubReplacementTemplateMissing,
    GmatchPatternNotMemoized,
    GmatchState(GmatchStateFault),
    #[cfg(feature = "snapshot")]
    SnapshotTable(SnapshotTableFault),
    #[cfg(feature = "snapshot")]
    SaveStateFailure(String),
    CaptureRangeOutsideString,
    MissingPointerIdentity,
    ConstructorTableMissing {
        operation: ConstructorTableOperation,
        actual: LuaType,
    },
    SetListBaseNotTable,
}

pub(crate) enum CompilerFault {
    RemappedJumpOffsetOutOfRange,
    MissingLineForInstruction,
    JumpTargetOutOfRange,
    RemappedJumpBeyondCode,
    TransferOpcodeExpected,
    LineInfoDesynced,
    NestedBytecodeNotUniquelyOwned,
}

pub(crate) enum ParserFault {
    InvalidStringToken,
    MissingStringContents,
    BlockExitWithoutEntry,
    ArithmeticFoldRequiresFoldableOpcode,
    InstructionReplacementWithoutInstruction,
    MultiValueTailMissingInstruction,
    LoopExitWithoutEntry,
    BreakEmissionLostLoopContext,
    CompletedSourceRetainsFunctionFrames,
    ChunkCompletionWithoutFunctionFrame,
    CompletedFunctionRetainsScopes,
    NamedFunctionTargetNotVariable,
    EmptyMultiAssignment,
    ConcatChainWithoutOperator,
}

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
    ArraySlotHasWrongKey,
    HashEntryBelongsInArray,
}

pub(crate) enum ConstructorTableOperation {
    InitField,
    InitFieldPinned,
    InitIndex,
    SetList,
}

impl InternalFault {
    fn into_message(self) -> String {
        match self {
            Self::CallDynamicArguments => {
                "call does not support ArgCount::Dynamic; use ArgCount::Fixed".to_owned()
            }
            Self::CallAnchorDynamicArguments => {
                "call_anchor does not support ArgCount::Dynamic; use ArgCount::Fixed".to_owned()
            }
            Self::Compiler(fault) => fault.to_string(),
            Self::CompilerValidation { chunk, detail } => {
                format!("compiler emitted invalid bytecode in {chunk} {detail}")
            }
            Self::GsubReplacementTemplateMissing => {
                "gsub replacement template was not initialized".to_owned()
            }
            Self::GmatchPatternNotMemoized => {
                "gmatch pattern was not memoized before matching".to_owned()
            }
            Self::GmatchState(fault) => format!("invalid gmatch iterator state: {fault}"),
            #[cfg(feature = "snapshot")]
            Self::SnapshotTable(fault) => format!("{fault}"),
            #[cfg(feature = "snapshot")]
            Self::SaveStateFailure(error) => error,
            Self::CaptureRangeOutsideString => "capture range outside the source string".to_owned(),
            Self::MissingPointerIdentity => {
                "value without a heap rendering has no pointer identity".to_owned()
            }
            Self::ConstructorTableMissing { operation, actual } => {
                format!("{operation}: expected table, got {actual}")
            }
            Self::SetListBaseNotTable => "SetList(0): base is not a table".to_owned(),
        }
    }
}

impl fmt::Display for CompilerFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::RemappedJumpOffsetOutOfRange => {
                "instruction stripping produced an out-of-range jump"
            }
            Self::MissingLineForInstruction => {
                "instruction stripping requires one line per instruction"
            }
            Self::JumpTargetOutOfRange => "instruction stripping found an out-of-range jump",
            Self::RemappedJumpBeyondCode => {
                "instruction stripping mapped a jump beyond retained code"
            }
            Self::TransferOpcodeExpected => "instruction stripping requires a transfer opcode",
            Self::LineInfoDesynced => "line_info desynced from code",
            Self::NestedBytecodeNotUniquelyOwned => {
                "nested bytecode must be uniquely owned during finalization"
            }
        };
        f.write_str(message)
    }
}

impl ParserFault {
    pub(crate) fn into_message(self) -> &'static str {
        match self {
            Self::InvalidStringToken => "parser requested contents for an invalid string token",
            Self::MissingStringContents => "lexer did not retain contents for a string token",
            Self::BlockExitWithoutEntry => "block exit requires a block entry",
            Self::ArithmeticFoldRequiresFoldableOpcode => {
                "arithmetic folding requires a foldable opcode"
            }
            Self::InstructionReplacementWithoutInstruction => {
                "instruction replacement requires emitted code"
            }
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
            Self::EmptyMultiAssignment => "multi-assignment requires a nonempty expression list",
            Self::ConcatChainWithoutOperator => "concat chain requires an operator",
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

/// Rarely-set error context, boxed so `Error` stays narrow. Structured library
/// details live here so existing rendered messages and `ErrorKind` matches
/// remain stable while hosts can inspect the operation that failed.
struct ErrorContext {
    /// Source file or chunk name, if the error came from named source.
    name: Option<Arc<str>>,
    /// Whether `on_error` has already been notified for this error.
    reported: bool,
    /// String function whose pattern operation produced this error, if any.
    pattern_function: Option<StringPatternFunction>,
    /// Typed standard-library reason behind a rendered runtime error, if any.
    library_error: Option<LibraryError>,
    /// Standard-library function responsible for a non-library error, if any.
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
    fn as_str(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

impl fmt::Display for StackFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let source = self.source.as_deref().unwrap_or("[C]");
        let func_desc = match &self.function_name {
            Some(name) => format!("function '{name}'"),
            None => "main chunk".to_string(),
        };
        write!(f, "{}:{}: in {}", source, self.line, func_desc)
    }
}

/// An error raised by the parser or VM. Carries an [`ErrorKind`], the source
/// location where it surfaced, and a stack trace.
#[derive(Debug)]
pub struct Error {
    /// What went wrong.
    kind: ErrorKind,
    /// 1-based source line number where the error surfaced (0 if unknown).
    line_num: u32,
    /// 1-based source column (0 if unknown).
    column: u32,
    /// The box keeps this optional context to one pointer in each `Error`, which
    /// is carried across recursive VM calls.
    context: Option<Box<ErrorContext>>,
    /// Stack trace at the point of error (innermost frame first).
    stack_trace: Vec<StackFrame>,
}

/// Which source location an error renders as its `chunk:line:` prefix.
///
/// Set by `error(msg, level)`: level 1 (the default) blames the frame that
/// raised the error, level 0 suppresses the prefix, and level N blames N-1
/// frames further up the traceback.
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
    /// `u32` rather than `usize` so `PrefixLocation` stays 8 bytes and
    /// `ScriptError` stays within `ErrorKind`'s existing largest variant.
    /// Widening `ErrorKind` widens every `Result<_, Error>` held across the
    /// Lua call recursion, which runs to `limits::MAX_CALL_DEPTH` and overflows the
    /// debug-build stack - `call_depth_exceeded_error` catches exactly that.
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
    /// Anchor handle is stale (released, generation mismatch) or belongs
    /// to a different `State`. Operations that need a live value return
    /// this rather than aliasing into the wrong slot.
    InvalidAnchor,
    /// Attempted to anchor `nil`. `nil` carries no GC weight and has no
    /// use case for a stable handle; embedders that need an "absent" value
    /// should use `Option<Anchor>`.
    AnchorNil,
    /// A runtime fault detected by the VM or by a library operation - an
    /// invalid `next` key and so on. Typed standard-library details are
    /// available through [`Error::library_error`]. Pattern failures use
    /// [`ErrorKind::PatternError`]. Distinct
    /// from [`ErrorKind::ScriptError`], which the script raised on purpose,
    /// and from [`ErrorKind::InternalError`], which indicates a VM bug.
    RuntimeError(String),
    /// An error explicitly raised by script code through `error()`. Hosts can
    /// use this to tell deliberate script termination from a VM-detected fault.
    ///
    /// The prefix location lives here rather than on [`Error`] deliberately:
    /// only `error()` can set it, and `Error` is returned by value through
    /// every level of the Lua call recursion, so widening it costs stack on a
    /// path that already runs to `MAX_CALL_DEPTH`. `ErrorKind` is sized by its
    /// largest variant, which this is not.
    ScriptError {
        /// The message the script passed to `error()`.
        message: String,
        /// Which frame supplies the rendered `chunk:line:` prefix.
        prefix: PrefixLocation,
    },
    /// Internal error (corrupt bytecode or VM bug). The string is a
    /// human-readable description; report these as bugs. Hosts may use this
    /// legacy kind for their own errors. Library-created internal failures use
    /// typed private reasons while preserving their rendered messages.
    /// Dispatch attaches structured instruction context via [`Error::vm_fault`]
    /// without widening `Error` on the recursive evaluation path.
    InternalError(String),
}

/// Structured reason that Lua pattern compilation or matching failed.
///
/// The display text matches the Lua pattern diagnostics previously returned
/// as [`ErrorKind::RuntimeError`].
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// `table.sort`'s default comparator.
    Sort,
    /// Another library function name, for an internal caller not yet listed.
    Other(String),
}

impl LibraryFunction {
    /// Resolve the stable names used by standard library helpers.
    pub(crate) fn from_name(name: &str) -> Self {
        match name {
            "random" => Self::Random,
            "insert" => Self::Insert,
            "remove" => Self::Remove,
            "move" => Self::Move,
            "unpack" => Self::Unpack,
            "concat" => Self::Concat,
            "sort" => Self::Sort,
            other => Self::Other(other.to_owned()),
        }
    }

    fn as_str(&self) -> &str {
        match self {
            Self::Random => "random",
            Self::Insert => "insert",
            Self::Remove => "remove",
            Self::Move => "move",
            Self::Unpack => "unpack",
            Self::Concat => "concat",
            Self::Sort => "sort",
            Self::Other(name) => name,
        }
    }
}

/// A failure raised by a standard library operation. Library runtime errors
/// retain this value through [`Error::library_error`] while rendering through
/// [`ErrorKind::RuntimeError`].
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
        /// The rendered conversion directive.
        directive: String,
    },
    /// A `string.format` conversion directive exceeds the size limit.
    FormatDirectiveTooLong,
    /// A `%q` directive contains flags, width or precision.
    QuotedFormatHasModifiers,
    /// A `string.format` conversion contains invalid flags or precision.
    InvalidFormatSpecification {
        /// The rendered conversion directive.
        directive: String,
    },
    /// A `table.concat` element has a type other than string or number.
    InvalidConcatValue {
        /// The type of the invalid element.
        value_type: LuaType,
        /// The one-based index of the invalid element.
        index: i64,
    },
    /// A `table.move` operation would exceed the supported element count.
    TooManyMoveElements,
    /// A `table.move` destination calculation would wrap around.
    MoveDestinationWrapAround,
    /// An integer key range cannot be represented exactly by Lua numbers.
    IntegerKeyRangeExceeded {
        /// The function whose key range was rejected.
        function: LibraryFunction,
    },
    /// A table length metamethod returned a value without an exact integer.
    ObjectLengthNotInteger,
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
    /// Unexpected token. The String contains a token description.
    UnexpectedTok(String),
    /// A required token was missing before another token.
    ExpectedToken {
        /// Spelling of the token the grammar required.
        expected: String,
        /// Description of the token actually found.
        found: String,
    },
    /// `...` was used outside a vararg function.
    VarargOutsideVarargFunction,
}

/// Type errors raised by arithmetic, comparison, indexing, and concat.
#[non_exhaustive]
#[derive(Debug)]
pub enum TypeError {
    /// Arithmetic op applied to a non-numeric value of the given type.
    Arithmetic(LuaType),
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
        Error {
            kind: kind.into(),
            line_num,
            column,
            context: None,
            stack_trace: Vec::new(),
        }
    }

    /// Construct an error without source-location information. Used for
    /// errors raised before any chunk has been parsed (e.g. host API misuse).
    pub fn without_location(kind: ErrorKind) -> Self {
        Error::new(kind, 0, 0)
    }

    pub(crate) fn internal_fault(fault: InternalFault) -> Self {
        Self::without_location(ErrorKind::InternalError(fault.into_message()))
    }

    /// Attach a stack trace to this error. The VM attaches the real trace when
    /// an error unwinds out of a call; a host-supplied trace is reported to
    /// `on_error` as given. Public chiefly so hosts and tests can build errors
    /// with known traces.
    pub fn with_stack_trace(mut self, trace: Vec<StackFrame>) -> Self {
        self.stack_trace = trace;
        self
    }

    pub(crate) fn with_location(mut self, line_num: u32, column: u32) -> Self {
        self.line_num = line_num;
        self.column = column;
        self
    }

    pub(crate) fn with_source_name(mut self, source_name: Option<Arc<str>>) -> Self {
        if source_name.is_some() || self.context.is_some() {
            self.context_mut().name = source_name;
        }
        self
    }

    /// Attach the reporting frame's source name unless the error already
    /// carries one, so `on_error` sees where the error was first reported.
    pub(crate) fn attach_source_name_if_absent(&mut self, source_name: Option<&Arc<str>>) {
        if self.source_name().is_none()
            && let Some(name) = source_name
        {
            self.context_mut().name = Some(Arc::clone(name));
        }
    }

    // Keep reporting state in the existing optional box: widening Error adds
    // stack storage at every level of the bounded recursive Lua call path.
    fn context_mut(&mut self) -> &mut ErrorContext {
        self.context.get_or_insert_with(|| {
            Box::new(ErrorContext {
                name: None,
                reported: false,
                pattern_function: None,
                library_error: None,
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

    /// Source file or chunk name attached to this error, if it came from named source.
    pub fn source_name(&self) -> Option<&str> {
        self.context.as_deref().and_then(ErrorContext::as_str)
    }

    /// Error category and details.
    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    /// Take the error category, discarding location and context.
    pub(crate) fn into_kind(self) -> ErrorKind {
        self.kind
    }

    /// Stack frames at the point the error was raised, innermost first.
    pub fn stack_trace(&self) -> &[StackFrame] {
        &self.stack_trace
    }

    /// Give an argument error the explicit standard-library function name.
    /// Updating the kind in place keeps this error's source and reporting
    /// state attached.
    pub(crate) fn with_argument_function_name(mut self, function_name: &str) -> Self {
        if let ErrorKind::ArgError(argument) = &mut self.kind {
            argument.func_name = Some(function_name.to_owned());
        }
        self
    }

    /// String function whose pattern operation produced this error, if any.
    pub fn pattern_function(&self) -> Option<StringPatternFunction> {
        self.context
            .as_deref()
            .and_then(|context| context.pattern_function)
    }

    /// Typed standard-library reason behind this runtime error, if any.
    pub fn library_error(&self) -> Option<&LibraryError> {
        self.context
            .as_deref()
            .and_then(|context| context.library_error.as_ref())
    }

    /// Standard-library function responsible for this error, if known.
    pub fn library_function(&self) -> Option<&LibraryFunction> {
        self.context
            .as_deref()
            .and_then(|context| context.library_function.as_ref())
    }

    pub(crate) fn with_pattern_function(mut self, function: StringPatternFunction) -> Self {
        self.context_mut().pattern_function = Some(function);
        self
    }

    pub(crate) fn with_library_error(mut self, detail: LibraryError) -> Self {
        self.context_mut().library_error = Some(detail);
        self
    }

    pub(crate) fn with_library_function(mut self, function: LibraryFunction) -> Self {
        self.context_mut().library_function = Some(function);
        self
    }

    /// Instruction context shared by internal dispatch failures.
    pub fn vm_fault(&self) -> Option<&VmFault> {
        self.context
            .as_deref()
            .and_then(|context| context.vm_fault.as_ref())
    }

    pub(crate) fn with_vm_fault(
        mut self,
        instruction: usize,
        opcode: u8,
        function_name: Option<&str>,
    ) -> Self {
        if matches!(self.kind, ErrorKind::InternalError(_)) && self.vm_fault().is_none() {
            self.context_mut().vm_fault = Some(VmFault {
                instruction,
                opcode,
                function_name: function_name.map(str::to_owned),
            });
        }
        self
    }

    /// 1-based column number where the error surfaced (0 if unknown).
    pub fn column(&self) -> u32 {
        self.column
    }

    /// 1-based line number where the error surfaced (0 if unknown).
    pub fn line_num(&self) -> u32 {
        self.line_num
    }

    /// Whether the host can plausibly retry the operation after handling the
    /// error. Currently only some [`SyntaxError`] variants are recoverable:
    /// those that appending more source text can fix. Intended for host REPLs
    /// that keep reading lines until a chunk is complete; the crate's own CLI
    /// has no REPL and does not call it.
    pub fn is_recoverable(&self) -> bool {
        self.kind.is_recoverable()
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

    /// Build an error for a required argument without an expected type.
    pub(crate) fn required(arg_number: isize, func_name: Option<&str>) -> Self {
        Self {
            arg_number,
            func_name: func_name.map(str::to_owned),
            detail: ArgDetail::TypeMismatch,
            expected: None,
            received: None,
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
        }
    }
}

impl SyntaxError {
    pub(crate) fn expected_token(expected: &str, got: String) -> Self {
        Self::ExpectedToken {
            expected: expected.to_string(),
            found: got,
        }
    }

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
        // from the innermost frame but the line from `self.line_num` can splice
        // together two unrelated locations: a host `RustFunc` may return an
        // `Error` carrying its own line, which says nothing about the Lua chunk
        // whose name is about to be printed next to it.
        let prefix_location = match &self.kind {
            ErrorKind::ScriptError { prefix, .. } => *prefix,
            _ => PrefixLocation::Current,
        };
        let prefix_frame = match prefix_location {
            PrefixLocation::Current => self.stack_trace.first(),
            PrefixLocation::Suppressed => None,
            PrefixLocation::TraceFrame(index) => self.stack_trace.get(index as usize),
        };
        let suppress_prefix = matches!(prefix_location, PrefixLocation::Suppressed)
            || matches!(prefix_location, PrefixLocation::TraceFrame(_)) && prefix_frame.is_none();
        // The line always comes from the selected frame, whether or not that
        // frame knows its source name. Falling back to `self.line_num` when the
        // source is absent would report the innermost line under an
        // `error(msg, 2)` that deliberately selected a caller.
        if suppress_prefix {
            write!(f, "{}", self.kind)?;
        } else {
            match prefix_frame {
                Some(frame) => match frame.source.as_deref() {
                    Some(source) => write!(f, "{source}:{}: {}", frame.line, self.kind)?,
                    None => {
                        // `self.column` belongs to the innermost error site.
                        // Pair it with the frame's line only when that frame
                        // IS the innermost site; otherwise the column is
                        // unknown (0) for the selected frame.
                        let same_site = matches!(
                            prefix_location,
                            PrefixLocation::Current | PrefixLocation::TraceFrame(0)
                        ) && frame.line != 0
                            && frame.line == self.line_num;
                        let column = if same_site { self.column } else { 0 };
                        write!(f, "{}:{}: {}", frame.line, column, self.kind)?;
                    }
                },
                None => match self.source_name() {
                    Some(source) if self.line_num != 0 && self.column != 0 => write!(
                        f,
                        "{source}:{}:{}: {}",
                        self.line_num, self.column, self.kind
                    )?,
                    Some(source) if self.line_num != 0 => {
                        write!(f, "{source}:{}: {}", self.line_num, self.kind)?;
                    }
                    Some(source) => write!(f, "{source}: {}", self.kind)?,
                    None => write!(f, "{}:{}: {}", self.line_num, self.column, self.kind)?,
                },
            }
        }
        if !self.stack_trace.is_empty() {
            write!(f, "\nstack traceback:")?;
            for frame in &self.stack_trace {
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
            InvalidStackIndex { index } => {
                write!(f, "invalid stack index ({index})")
            }
            PatternError(detail) => fmt::Display::fmt(detail, f),
            DuplicateFunctionRegistration { id } => {
                write!(f, "duplicate Rust function registration: {id}")
            }
            InvalidAnchor => {
                write!(
                    f,
                    "invalid anchor (released, stale generation, or wrong State)"
                )
            }
            AnchorNil => {
                write!(f, "cannot anchor nil")
            }
            // A script's `error()` message and a library runtime error render
            // identically, exactly as reference does. The variants are distinct
            // so hosts can tell deliberate script termination from a VM-detected
            // fault, which is not a rendering difference.
            RuntimeError(msg) | ScriptError { message: msg, .. } | InternalError(msg) => {
                write!(f, "{msg}")
            }
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
                write!(f, "invalid conversion '{directive}' to 'format'")
            }
            Self::FormatDirectiveTooLong => f.write_str("invalid format (too long)"),
            Self::QuotedFormatHasModifiers => f.write_str("specifier '%q' cannot have modifiers"),
            Self::InvalidFormatSpecification { directive } => {
                write!(f, "invalid conversion specification: '{directive}'")
            }
            Self::InvalidConcatValue { value_type, index } => write!(
                f,
                "invalid value ({value_type}) at index {index} in table for 'concat'"
            ),
            Self::TooManyMoveElements => f.write_str("too many elements to move"),
            Self::MoveDestinationWrapAround => f.write_str("destination wrap around"),
            Self::IntegerKeyRangeExceeded { function } => write!(
                f,
                "{} range exceeds the exactly representable integer keys",
                function.as_str()
            ),
            Self::ObjectLengthNotInteger => f.write_str("object length is not an integer"),
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
            InvalidEscapeSequence => write!(f, "invalid escape sequence"),
            HexadecimalDigitExpected => write!(f, "hexadecimal digit expected"),
            DecimalEscapeTooLarge => write!(f, "decimal escape too large"),
            LongStringUnsupported => write!(f, "long strings are not supported"),
            TooManyExpressions => write!(f, "too many expressions (limit {MAX_EXPRESSIONS})"),
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
                "control-flow jump is too far (exceeds signed 16-bit range)"
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
            UnexpectedTok(msg) => write!(f, "{msg}"),
            ExpectedToken { expected, found } => write!(f, "'{expected}' expected near {found}"),
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
