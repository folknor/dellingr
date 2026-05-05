use std::fmt;

use crate::LuaType;

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
    /// Stack trace at the point of error (innermost frame first).
    pub stack_trace: Vec<StackFrame>,
}

/// Top-level error categories. See the type-specific enums (e.g.
/// [`TypeError`], [`SyntaxError`], [`ArgError`]) for finer-grained variants.
#[derive(Debug)]
pub enum ErrorKind {
    /// Operation applied to a value of the wrong Lua type.
    TypeError(TypeError),
    /// Wrong number or type of arguments to a builtin or host function.
    ArgError(ArgError),
    /// Parser rejected the source.
    SyntaxError(SyntaxError),
    /// Script exceeded its cost budget.
    BudgetExceeded {
        /// Total cost consumed when the budget was exceeded.
        used: u64,
        /// The budget that was set.
        budget: i64,
    },
    /// Metamethod chain (`__index` / `__newindex`) exceeded maximum depth.
    MetamethodDepthExceeded {
        /// Recursion depth reached before bailing out.
        depth: u32,
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
    },
    /// Stack size exceeded (too many values on stack).
    StackOverflow {
        /// Stack size that triggered the overflow.
        size: usize,
    },
    /// Invalid stack index passed to a host API.
    InvalidStackIndex {
        /// The bad index, as supplied by the caller.
        index: isize,
    },
    /// Internal error (corrupt bytecode or VM bug). The string is a
    /// human-readable description; report these as bugs.
    InternalError(String),
}

/// Argument error raised by builtins or host functions.
#[derive(Debug)]
pub struct ArgError {
    /// 1-based argument index that failed validation. Negative values count
    /// from the end of the argument list.
    pub arg_number: isize,
    /// Name of the function the argument was passed to (if known).
    pub func_name: Option<String>,
    /// Expected Lua type for this argument.
    pub expected: Option<LuaType>,
    /// Lua type the caller actually supplied.
    pub received: Option<LuaType>,
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
    /// More expressions in a single source construct than the VM accepts.
    TooManyExpressions,
    /// Function body has more locals than the VM accepts.
    TooManyLocals,
    /// Source nests function definitions deeper than the VM accepts.
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
    /// Unexpected token. The String contains a description like
    /// `"'...' outside vararg function"` or `"'<token>' near '<context>'"`.
    UnexpectedTok(String),
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
    /// Returns true if this is a SyntaxError that can be fixed by appending
    /// more text to the source code.
    pub fn is_recoverable(&self) -> bool {
        // matches!(self, Self::UnclosedString | Self::UnexpectedEof)
        matches!(self, Self::UnexpectedEof)
    }
}

// `Display` impls

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line_num, self.column, self.kind)?;
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

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ErrorKind::*;
        match self {
            ArgError(e) => e.fmt(f),
            SyntaxError(e) => e.fmt(f),
            TypeError(e) => e.fmt(f),
            BudgetExceeded { used, budget } => {
                write!(
                    f,
                    "budget exceeded: used {used} cost with budget of {budget}"
                )
            }
            MetamethodDepthExceeded { depth } => {
                write!(f, "metamethod chain too deep (depth {depth})")
            }
            InvalidJump { ip, offset } => {
                write!(
                    f,
                    "internal error: invalid jump (instruction {ip}, offset {offset})"
                )
            }
            CallDepthExceeded { depth } => {
                write!(f, "call stack overflow (depth {depth})")
            }
            StackOverflow { size } => {
                write!(f, "stack overflow ({size} values)")
            }
            InvalidStackIndex { index } => {
                write!(f, "internal error: invalid stack index ({index})")
            }
            InternalError(msg) => {
                write!(f, "internal error: {msg}")
            }
        }
    }
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let func_name = match &self.func_name {
            Some(s) => s.as_str(),
            None => "<anonymous>",
        };
        let extra = match (&self.expected, &self.received) {
            (Some(expected), Some(got)) => format!("{expected} expected, got {got}"),
            (Some(expected), None) => format!("{expected} expected, got no value"),
            (None, _) => "value expected".into(),
        };

        write!(
            f,
            "bad argument #{} to {} ({})",
            self.arg_number, func_name, extra
        )
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use SyntaxError::*;
        match self {
            BadNumber => write!(f, "malformed number"),
            BreakOutsideLoop => write!(f, "<break> at line 1 not inside a loop"),
            InvalidCharacter(c) => write!(f, "invalid character '{c}'"),
            TooManyExpressions => write!(f, "too many expressions in a single list (limit 255)"),
            TooManyLocals => write!(f, "too many local variables"),
            TooManyNestedFunctions => write!(f, "too many nested functions (limit 255)"),
            TooManyNumbers => write!(f, "too many literal numbers"),
            TooManyStrings => write!(f, "too many literal strings"),
            TooManyTableFields => write!(f, "too many fields in table constructor (limit 255)"),
            UnclosedString => write!(f, "unfinished string"),
            UnexpectedEof => write!(f, "unexpected <eof>"),
            UnexpectedTok(msg) => write!(f, "{msg}"),
            LParenLineStart => write!(f, "ambiguous function call"),
        }
    }
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use TypeError::*;
        match self {
            Arithmetic(typ) => write!(f, "attempt to perform arithmetic on a {typ} value"),
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
