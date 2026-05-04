//! Minimal internal wrapper around the vendored Lua pattern matcher.
//!
//! Lua patterns operate on bytes. Dellingr only needs the byte matching core.

use core::ops;

pub(crate) mod errors;
use self::errors::*;

mod luapat;
use self::luapat::{LUA_MAXCAPTURES, LuaMatch, str_check, str_match};

/// A compiled Lua string pattern and the captures from the latest match.
pub(crate) struct LuaPattern<'a> {
    patt: &'a [u8],
    matches: [LuaMatch; LUA_MAXCAPTURES],
    n_match: usize,
}

impl<'a> LuaPattern<'a> {
    /// Maybe create a new Lua pattern from a slice of bytes.
    pub(crate) fn from_bytes_try(bytes: &'a [u8]) -> Result<LuaPattern<'a>, PatternError> {
        str_check(bytes)?;
        Ok(LuaPattern {
            patt: bytes,
            matches: [LuaMatch { start: 0, end: 0 }; LUA_MAXCAPTURES],
            n_match: 0,
        })
    }

    /// Match a slice of bytes with this pattern.
    pub(crate) fn matches_bytes(&mut self, s: &[u8]) -> bool {
        match str_match(s, self.patt, &mut self.matches) {
            Ok(n_match) => {
                self.n_match = n_match;
                n_match > 0
            }
            Err(_) => {
                self.n_match = 0;
                false
            }
        }
    }

    /// The full match range from the latest successful match.
    pub(crate) fn range(&self) -> ops::Range<usize> {
        self.capture(0)
    }

    /// The nth capture range from the latest successful match.
    pub(crate) fn capture(&self, i: usize) -> ops::Range<usize> {
        ops::Range {
            start: self.matches[i].start,
            end: self.matches[i].end,
        }
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
        assert!(pattern.matches_bytes(b"one dog"));
        assert_eq!(pattern.capture(0), 0..3);
        assert_eq!(pattern.capture(1), 0..3);
        assert_eq!(pattern.num_matches(), 2);
        assert!(!pattern.matches_bytes(b" one dog"));
    }

    #[test]
    fn multiple_byte_captures() {
        let mut pattern = LuaPattern::from_bytes_try(b"%s*(%d+)%s+(%S+)").unwrap();
        assert!(pattern.matches_bytes(b" 233   hello dolly"));
        assert_eq!(pattern.capture(1), 1..4);
        assert_eq!(pattern.capture(2), 7..12);
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
        ];

        for (pattern, expected) in bad {
            let result = LuaPattern::from_bytes_try(pattern);
            assert!(matches!(result, Err(error) if error == expected));
        }
    }
}
