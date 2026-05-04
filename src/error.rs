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

#[derive(Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub line_num: usize,
    pub column: usize,
    /// Stack trace at the point of error (innermost frame first).
    pub stack_trace: Vec<StackFrame>,
}

#[derive(Debug)]
pub enum ErrorKind {
    TypeError(TypeError),
    ArgError(ArgError),
    SyntaxError(SyntaxError),
    /// Script exceeded its cost budget
    BudgetExceeded {
        used: u64,
        budget: i64,
    },
    /// Metamethod chain (__index/__newindex) exceeded maximum depth
    MetamethodDepthExceeded {
        depth: u32,
    },
    /// Invalid jump target (compiler bug or corrupt bytecode)
    InvalidJump {
        ip: usize,
        offset: isize,
    },
    /// Call stack depth exceeded (too much recursion)
    CallDepthExceeded {
        depth: u32,
    },
    /// Stack size exceeded (too many values on stack)
    StackOverflow {
        size: usize,
    },
    /// Invalid stack index
    InvalidStackIndex {
        index: isize,
    },
    /// Internal error (corrupt bytecode or VM bug)
    InternalError(String),
}

#[derive(Debug)]
pub struct ArgError {
    pub arg_number: isize,
    pub func_name: Option<String>,
    pub expected: Option<LuaType>,
    pub received: Option<LuaType>,
}

#[derive(Debug)]
pub enum SyntaxError {
    BadNumber,
    BreakOutsideLoop,
    InvalidCharacter(char),
    TooManyExpressions,
    TooManyLocals,
    TooManyNestedFunctions,
    TooManyNumbers,
    TooManyStrings,
    TooManyTableFields,
    UnclosedString,
    UnexpectedEof,
    /// Unexpected token. The String contains a description like "'...' outside vararg function"
    /// or "'<token>' near '<context>'".
    UnexpectedTok(String),
    LParenLineStart,
}

#[derive(Debug)]
pub enum TypeError {
    Arithmetic(LuaType),
    Comparison(LuaType, LuaType),
    Concat(LuaType),
    FunctionCall(LuaType),
    Length(LuaType),
    TableIndex(LuaType),
    TableKeyNan,
    TableKeyNil,
}

// main impls

impl Error {
    pub fn new(kind: impl Into<ErrorKind>, line_num: usize, column: usize) -> Self {
        Error {
            kind: kind.into(),
            line_num,
            column,
            stack_trace: Vec::new(),
        }
    }

    pub fn without_location(kind: ErrorKind) -> Self {
        Error::new(kind, 0, 0)
    }

    /// Attach a stack trace to this error.
    pub fn with_stack_trace(mut self, trace: Vec<StackFrame>) -> Self {
        self.stack_trace = trace;
        self
    }

    pub fn column(&self) -> usize {
        self.column
    }

    pub fn line_num(&self) -> usize {
        self.line_num
    }

    pub fn is_recoverable(&self) -> bool {
        self.kind.is_recoverable()
    }
}

impl ErrorKind {
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
