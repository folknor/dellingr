//! Minimal internal wrapper around the vendored Lua pattern matcher.
//!
//! Lua patterns operate on bytes. Dellingr only needs the byte matching core.

use core::ops;

pub(crate) mod errors;
use self::errors::*;

mod luapat;
use self::luapat::{LUA_MAXCAPTURES, str_check, str_match};

pub(crate) use self::luapat::LuaCapture;

/// A compiled Lua string pattern and the captures from the latest match.
pub(crate) struct LuaPattern<'a> {
    patt: &'a [u8],
    matches: [LuaCapture; LUA_MAXCAPTURES],
    n_match: usize,
}

impl<'a> LuaPattern<'a> {
    /// Maybe create a new Lua pattern from a slice of bytes.
    pub(crate) fn from_bytes_try(bytes: &'a [u8]) -> Result<LuaPattern<'a>, PatternError> {
        str_check(bytes)?;
        Ok(LuaPattern {
            patt: bytes,
            matches: [LuaCapture::Bytes { start: 0, end: 0 }; LUA_MAXCAPTURES],
            n_match: 0,
        })
    }

    /// Match a slice of bytes with this pattern.
    pub(crate) fn matches_bytes(&mut self, s: &[u8]) -> Result<bool, PatternError> {
        let n_match = str_match(s, self.patt, &mut self.matches)?;
        self.n_match = n_match;
        Ok(n_match > 0)
    }

    /// The full match range from the latest successful match.
    pub(crate) fn range(&self) -> ops::Range<usize> {
        match self.capture(0) {
            LuaCapture::Bytes { start, end } => start..end,
            LuaCapture::Position(_) => unreachable!("the full match is always a byte range"),
        }
    }

    /// The nth capture range from the latest successful match.
    pub(crate) fn capture(&self, i: usize) -> LuaCapture {
        self.matches[i]
    }

    /// Number of captures from the latest successful match, including the full match.
    pub(crate) fn num_matches(&self) -> usize {
        self.n_match
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_captures_and_matching() {
        let mut pattern = LuaPattern::from_bytes_try(b"^(%a+)").unwrap();
        assert!(pattern.matches_bytes(b"one dog").unwrap());
        assert_eq!(pattern.capture(0), LuaCapture::Bytes { start: 0, end: 3 });
        assert_eq!(pattern.capture(1), LuaCapture::Bytes { start: 0, end: 3 });
        assert_eq!(pattern.num_matches(), 2);
        assert!(!pattern.matches_bytes(b" one dog").unwrap());
    }

    #[test]
    fn multiple_byte_captures() {
        let mut pattern = LuaPattern::from_bytes_try(b"%s*(%d+)%s+(%S+)").unwrap();
        assert!(pattern.matches_bytes(b" 233   hello dolly").unwrap());
        assert_eq!(pattern.capture(1), LuaCapture::Bytes { start: 1, end: 4 });
        assert_eq!(pattern.capture(2), LuaCapture::Bytes { start: 7, end: 12 });
    }

    #[test]
    fn bad_patterns() {
        let bad = [
            (
                b"bonzo %".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::EndsWithPercent),
            ),
            (b"bonzo (dog%(".as_slice(), PatternError::UnfinishedCapture),
            (
                b"alles [%a%[".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBracket),
            ),
            (
                b"bonzo (dog (cat)".as_slice(),
                PatternError::UnfinishedCapture,
            ),
            (
                b"frodo %f[%A".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBracket),
            ),
            (
                b"frodo (1) (2(3)%2)%1".as_slice(),
                PatternError::InvalidCaptureIndex(Some(1)),
            ),
            // L14: bounds/argument checks in the validator.
            (
                b"%b".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBalancedArguments),
            ),
            (
                b"%bx".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBalancedArguments),
            ),
            (
                b"%f".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingFrontierBracket),
            ),
            (
                b"%fx".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingFrontierBracket),
            ),
            (
                b"%f[".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBracket),
            ),
            (
                b"[".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBracket),
            ),
            (
                b"[a".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBracket),
            ),
            (b"(".as_slice(), PatternError::UnfinishedCapture),
        ];

        for (pattern, expected) in bad {
            let result = LuaPattern::from_bytes_try(pattern);
            assert!(matches!(result, Err(error) if error == expected));
        }
    }

    #[test]
    fn position_captures_are_typed() {
        let mut pattern = LuaPattern::from_bytes_try(b"()(a)()").unwrap();
        assert!(pattern.matches_bytes(b"abc").unwrap());
        assert_eq!(pattern.capture(1), LuaCapture::Position(0));
        assert_eq!(pattern.capture(2), LuaCapture::Bytes { start: 0, end: 1 });
        assert_eq!(pattern.capture(3), LuaCapture::Position(1));
    }

    #[test]
    fn end_anchors_try_the_end_position() {
        let mut pattern = LuaPattern::from_bytes_try(b"$").unwrap();
        assert!(pattern.matches_bytes(b"abc").unwrap());
        assert_eq!(pattern.range(), 3..3);
        assert!(pattern.matches_bytes(b"").unwrap());
        assert_eq!(pattern.range(), 0..0);

        let mut anchored = LuaPattern::from_bytes_try(b"^$").unwrap();
        assert!(anchored.matches_bytes(b"").unwrap());
        assert_eq!(anchored.range(), 0..0);
    }

    #[test]
    fn validator_skips_both_balance_delimiters() {
        // L14: `%b((` is valid - both `(` are balance delimiters, not captures.
        let mut pattern = LuaPattern::from_bytes_try(b"%b((").unwrap();
        assert!(pattern.matches_bytes(b"((").unwrap());
        assert_eq!(pattern.range(), 0..2);
    }

    #[test]
    fn frontier_at_end_is_safe() {
        let mut pattern = LuaPattern::from_bytes_try(b"%f[%z]").unwrap();
        assert!(pattern.matches_bytes(b"abc").unwrap());
        assert_eq!(pattern.range(), 3..3);
    }

    #[test]
    fn runtime_match_errors_are_not_swallowed() {
        let pattern = vec![b'a'; 201];
        let mut pattern = LuaPattern::from_bytes_try(&pattern).unwrap();
        assert!(matches!(
            pattern.matches_bytes(&vec![b'a'; 201]),
            Err(PatternError::MatchDepthExceeded)
        ));
    }
}
