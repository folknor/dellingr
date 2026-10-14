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

/// Rarely-set error context, boxed so `Error` stays narrow.
#[derive(Debug)]
struct ErrorContext {
    /// Source file or chunk name, if the error came from named source.
    name: Option<Arc<str>>,
    /// Whether `on_error` has already been notified for this error.
    reported: bool,
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
    pub kind: ErrorKind,
    /// 1-based source line number where the error surfaced (0 if unknown).
    pub line_num: usize,
    /// 1-based source column (0 if unknown).
    pub column: usize,
    /// The box keeps this optional context to one pointer in each `Error`, which
    /// is carried across recursive VM calls.
    context: Option<Box<ErrorContext>>,
    /// Stack trace at the point of error (innermost frame first).
    pub stack_trace: Vec<StackFrame>,
}

/// Which source location an error renders as its `chunk:line:` prefix.
///
/// Set by `error(msg, level)`: level 1 (the default) blames the frame that
/// raised the error, level 0 suppresses the prefix, and level N blames N-1
/// frames further up the traceback.
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
    /// Invalid jump target (compiler bug or corrupt bytecode).
    InvalidJump {
        /// Bytecode index where the bad jump originated.
        ip: usize,
        /// Signed jump offset that pointed outside the chunk.
        offset: isize,
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
    /// Invalid stack index passed to a host API.
    InvalidStackIndex {
        /// The bad index, as supplied by the caller.
        index: isize,
    },
    /// Lua pattern compilation or matching failed.
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
    /// invalid `next` key and so on. Pattern failures use
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
    /// human-readable description; report these as bugs.
    InternalError(String),
}

/// Structured reason that Lua pattern compilation or matching failed.
///
/// The display text matches the Lua pattern diagnostics previously returned
/// as [`ErrorKind::RuntimeError`].
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

/// The specific malformed Lua pattern construct.
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
    pub arg_number: isize,
    /// Name of the function the argument was passed to (if known).
    pub func_name: Option<String>,
    /// Reason validation failed.
    pub detail: ArgDetail,
    /// Expected Lua type for this argument.
    pub expected: Option<LuaType>,
    /// Lua type the caller actually supplied.
    pub received: Option<LuaType>,
}

/// Maximum number of values a Rust callback can return.
pub use crate::limits::MAX_RESULTS;

/// Validate a result count against the Rust callback ABI.
pub fn results_count(count: usize) -> Result<u8, ErrorKind> {
    u8::try_from(count).map_err(|_| ErrorKind::TooManyResults { limit: MAX_RESULTS })
}

/// Structured reason an argument failed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgDetail {
    /// Type or presence mismatch, described by `expected` and `received`.
    TypeMismatch,
    /// A number cannot be represented as an integer.
    NoInteger,
    /// A value lies outside the permitted range, with its precise diagnostic.
    OutOfBounds(String),
    /// A random interval contains no values.
    IntervalEmpty,
    /// Another validation failure, including a union of expected types.
    Custom(String),
}

/// Syntax-level error categories raised by the parser.
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
    /// A line started with an open parenthesis, which is ambiguous in Lua
    /// (could continue the previous statement). dellingr rejects this rather
    /// than guessing.
    LParenLineStart,
}

/// Type errors raised by arithmetic, comparison, indexing, and concat.
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
    pub fn new(kind: impl Into<ErrorKind>, line_num: usize, column: usize) -> Self {
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

    /// Attach a stack trace to this error.
    pub fn with_stack_trace(mut self, trace: Vec<StackFrame>) -> Self {
        self.stack_trace = trace;
        self
    }

    pub(crate) fn with_location(mut self, line_num: usize, column: usize) -> Self {
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

    // Keep reporting state in the existing optional box: widening Error adds
    // stack storage at every level of the bounded recursive Lua call path.
    fn context_mut(&mut self) -> &mut ErrorContext {
        self.context.get_or_insert_with(|| {
            Box::new(ErrorContext {
                name: None,
                reported: false,
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

    /// 1-based column number where the error surfaced (0 if unknown).
    pub fn column(&self) -> usize {
        self.column
    }

    /// 1-based line number where the error surfaced (0 if unknown).
    pub fn line_num(&self) -> usize {
        self.line_num
    }

    /// Whether the host can plausibly retry the operation after handling the
    /// error. Currently only some [`SyntaxError`] variants are recoverable.
    pub fn is_recoverable(&self) -> bool {
        self.kind.is_recoverable()
    }
}

impl ErrorKind {
    /// Build a named argument error from a structured validation reason.
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
        // Deliberately EOF only. An unterminated string is not fixable by
        // appending text: Lua short strings cannot span lines (and long
        // strings are unsupported), so a REPL that keeps reading would never
        // complete it.
        matches!(self, Self::UnexpectedEof)
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
            match prefix_frame.filter(|frame| frame.line != 0) {
                Some(frame) => match frame.source.as_deref() {
                    Some(source) => write!(f, "{source}:{}: {}", frame.line, self.kind)?,
                    None => write!(f, "{}:{}: {}", frame.line, self.column, self.kind)?,
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
            writeln!(f)?;
            writeln!(f, "stack traceback:")?;
            for frame in &self.stack_trace {
                writeln!(f, "\t{frame}")?;
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
            InvalidJump { ip, offset } => {
                write!(
                    f,
                    "internal error: invalid jump (instruction {ip}, offset {offset})"
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
            RuntimeError(msg) | ScriptError { message: msg, .. } => write!(f, "{msg}"),
            InternalError(msg) => {
                write!(f, "internal error: {msg}")
            }
        }
    }
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let func_name = self.func_name.as_deref().unwrap_or("<anonymous>");
        let extra = match &self.detail {
            ArgDetail::NoInteger => "number has no integer representation".into(),
            ArgDetail::IntervalEmpty => "interval is empty".into(),
            ArgDetail::OutOfBounds(detail) | ArgDetail::Custom(detail) => detail.clone(),
            ArgDetail::TypeMismatch => match (&self.expected, &self.received) {
                (Some(expected), Some(got)) => format!("{expected} expected, got {got}"),
                (Some(expected), None) => format!("{expected} expected, got no value"),
                (None, _) => "value expected".into(),
            },
        };

        write!(
            f,
            "bad argument #{} to '{}' ({})",
            self.arg_number, func_name, extra
        )
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
            UnexpectedTok(msg) => write!(f, "{msg}"),
            ExpectedToken { expected, found } => write!(f, "'{expected}' expected near {found}"),
            VarargOutsideVarargFunction => {
                write!(f, "cannot use '...' outside a vararg function")
            }
            LParenLineStart => write!(f, "ambiguous function call"),
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
