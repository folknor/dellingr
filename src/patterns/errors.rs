use core::fmt;

use crate::error::{MalformedPatternDetail, PatternErrorDetail};

/// A malformed-pattern or match-limit error. Raised at match time, at the
/// point reference Lua would raise it; `Copy` so the compiled item stream can
/// carry one in `Item::Fail`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PatternError {
    InvalidPatternCapture,
    /// Zero-based capture index (`%1` is 0, `%0` is -1). The matcher always
    /// knows it; the public `PatternErrorDetail` keeps an `Option` for its
    /// index-less display form, which this engine never produces.
    InvalidCaptureIndex(i8),
    MalformedPattern(MalformedPattern),
    TooManyCaptures,
    MatchDepthExceeded,
    UnfinishedCapture,
}

/// Error returned while executing a compiled Lua pattern.
#[derive(Clone, Copy, PartialEq, Debug)]
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

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&PatternErrorDetail::from(self), f)
    }
}

impl From<&PatternError> for PatternErrorDetail {
    fn from(error: &PatternError) -> Self {
        match error {
            PatternError::InvalidPatternCapture => Self::InvalidPatternCapture,
            PatternError::InvalidCaptureIndex(index) => Self::InvalidCaptureIndex(Some(*index)),
            PatternError::MalformedPattern(reason) => Self::MalformedPattern(match reason {
                MalformedPattern::EndsWithPercent => MalformedPatternDetail::EndsWithPercent,
                MalformedPattern::MissingBalancedArguments => {
                    MalformedPatternDetail::MissingBalancedArguments
                }
                MalformedPattern::MissingBracket => MalformedPatternDetail::MissingBracket,
                MalformedPattern::MissingFrontierBracket => {
                    MalformedPatternDetail::MissingFrontierBracket
                }
            }),
            PatternError::TooManyCaptures => Self::TooManyCaptures,
            PatternError::MatchDepthExceeded => Self::MatchDepthExceeded,
            PatternError::UnfinishedCapture => Self::UnfinishedCapture,
        }
    }
}

impl From<PatternError> for PatternErrorDetail {
    fn from(error: PatternError) -> Self {
        Self::from(&error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_details_keep_existing_display_text() {
        let cases = [
            (
                PatternError::InvalidPatternCapture,
                "invalid pattern capture",
            ),
            (
                PatternError::InvalidCaptureIndex(-1),
                "invalid capture index %0",
            ),
            (
                PatternError::MalformedPattern(MalformedPattern::EndsWithPercent),
                "malformed pattern (ends with '%')",
            ),
            (
                PatternError::MalformedPattern(MalformedPattern::MissingBalancedArguments),
                "malformed pattern (missing arguments to '%b')",
            ),
            (
                PatternError::MalformedPattern(MalformedPattern::MissingBracket),
                "malformed pattern (missing ']')",
            ),
            (
                PatternError::MalformedPattern(MalformedPattern::MissingFrontierBracket),
                "malformed pattern (missing '[' after '%f' in pattern)",
            ),
            (PatternError::TooManyCaptures, "too many captures"),
            (PatternError::MatchDepthExceeded, "pattern too complex"),
            (PatternError::UnfinishedCapture, "unfinished capture"),
        ];

        for (error, expected) in cases {
            let detail = PatternErrorDetail::from(&error);
            assert_eq!(error.to_string(), expected);
            assert_eq!(detail.to_string(), expected);
        }
    }
}
