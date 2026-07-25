use core::fmt;

/// Error type returned by _try methods
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum PatternError {
    InvalidPatternCapture,
    InvalidCaptureIndex(Option<i8>),
    MalformedPattern(MalformedPattern),
    TooManyCaptures,
    MatchDepthExceeded,
    UnfinishedCapture,
}

/// Error returned while executing a compiled Lua pattern.
#[derive(PartialEq, Debug)]
pub(crate) enum MatchError {
    Pattern(PatternError),
    BudgetExceeded,
}

impl From<PatternError> for MatchError {
    fn from(error: PatternError) -> Self {
        Self::Pattern(error)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum MalformedPattern {
    EndsWithPercent,
    MissingBalancedArguments,
    MissingBracket,
    MissingFrontierBracket,
}

impl MalformedPattern {
    const fn as_str(self) -> &'static str {
        match self {
            Self::EndsWithPercent => "ends with '%'",
            Self::MissingBalancedArguments => "missing arguments to '%b'",
            Self::MissingBracket => "missing ']'",
            Self::MissingFrontierBracket => "missing '[' after '%f' in pattern",
        }
    }
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPatternCapture => write!(f, "invalid pattern capture"),
            Self::InvalidCaptureIndex(None) => write!(f, "invalid capture index"),
            Self::InvalidCaptureIndex(Some(idx)) => {
                write!(f, "invalid capture index %{}", i16::from(*idx) + 1)
            }
            Self::MalformedPattern(what) => {
                let what = what.as_str();
                write!(f, "malformed pattern ({what})")
            }
            Self::TooManyCaptures => write!(f, "too many captures"),
            Self::MatchDepthExceeded => write!(f, "pattern too complex"),
            Self::UnfinishedCapture => write!(f, "unfinished capture"),
        }
    }
}
