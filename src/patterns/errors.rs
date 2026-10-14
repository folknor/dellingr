use core::fmt;

use crate::error::{MalformedPatternDetail, PatternErrorDetail};

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

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&PatternErrorDetail::from(self), f)
    }
}

impl From<&PatternError> for PatternErrorDetail {
    fn from(error: &PatternError) -> Self {
        match error {
            PatternError::InvalidPatternCapture => Self::InvalidPatternCapture,
            PatternError::InvalidCaptureIndex(index) => Self::InvalidCaptureIndex(*index),
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
                PatternError::InvalidCaptureIndex(None),
                "invalid capture index",
            ),
            (
                PatternError::InvalidCaptureIndex(Some(-1)),
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
