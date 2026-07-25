//! Minimal internal wrapper around the vendored Lua pattern matcher.
//!
//! Lua patterns operate on bytes. Dellingr only needs the byte matching core.

use core::ops;

pub(crate) mod errors;
use self::errors::*;

mod luapat;
use self::luapat::{CompiledPattern, LUA_MAXMATCHES, compile, str_match};

pub(crate) use self::luapat::LuaCapture;

/// A compiled Lua string pattern and the captures from the latest match.
///
/// Compilation copies everything the matcher needs into `compiled`, so the
/// pattern bytes are not borrowed past construction.
pub(crate) struct LuaPattern {
    compiled: CompiledPattern,
    matches: [LuaCapture; LUA_MAXMATCHES],
    n_match: usize,
}

impl LuaPattern {
    /// Maybe create a new Lua pattern from a slice of bytes.
    pub(crate) fn from_bytes_try(bytes: &[u8]) -> Result<LuaPattern, PatternError> {
        let compiled = compile(bytes)?;
        Ok(LuaPattern {
            compiled,
            matches: [LuaCapture::Bytes { start: 0, end: 0 }; LUA_MAXMATCHES],
            n_match: 0,
        })
    }

    /// Match a slice of bytes with this pattern, searching from the start.
    ///
    /// Test-only: every caller in the standard library matches against the
    /// whole subject with an explicit `init`, so that `%f` keeps its left
    /// context (finding #33).
    #[cfg(test)]
    pub(crate) fn matches_bytes(&mut self, s: &[u8]) -> Result<bool, PatternError> {
        self.matches_bytes_from(s, 0)
    }

    /// Match the complete subject, beginning the search at `init`.
    pub(crate) fn matches_bytes_from(
        &mut self,
        s: &[u8],
        init: usize,
    ) -> Result<bool, PatternError> {
        let n_match = str_match(s, &self.compiled, init, &mut self.matches)?;
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
    fn class_close_follows_reference_do_while() {
        // C29: at least one class byte is consumed before ']' can close the
        // class, so `[]]` is a class containing ']'.
        let mut literal_bracket = LuaPattern::from_bytes_try(b"[]]").unwrap();
        assert!(literal_bracket.matches_bytes(b"]").unwrap());
        assert_eq!(literal_bracket.range(), 0..1);
        assert!(!literal_bracket.matches_bytes(b"x").unwrap());

        // `[^]]` is "any byte except ']'".
        let mut complement = LuaPattern::from_bytes_try(b"[^]]").unwrap();
        assert!(complement.matches_bytes(b"x").unwrap());
        assert!(!complement.matches_bytes(b"]").unwrap());

        // An escaped `%]` is a class byte too, and does not close the class.
        let mut escaped = LuaPattern::from_bytes_try(b"[%]]").unwrap();
        assert!(escaped.matches_bytes(b"]").unwrap());

        // `[]` and `[^]` never close, matching reference "malformed pattern
        // (missing ']')" instead of parsing as an empty class.
        for pattern in [b"[]".as_slice(), b"[^]".as_slice(), b"[%]".as_slice()] {
            assert!(matches!(
                LuaPattern::from_bytes_try(pattern),
                Err(PatternError::MalformedPattern(
                    MalformedPattern::MissingBracket
                ))
            ));
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
    fn escaped_percent_patterns_match_and_dangling_percent_is_rejected() {
        for (pattern, subject, range) in [
            (b"%%".as_slice(), b"%".as_slice(), 0..1),
            (b"%d+%%".as_slice(), b"100%".as_slice(), 0..4),
            (b"%%%%".as_slice(), b"%%".as_slice(), 0..2),
        ] {
            let mut matcher = LuaPattern::from_bytes_try(pattern).unwrap();
            assert!(matcher.matches_bytes(subject).unwrap());
            assert_eq!(matcher.range(), range);
        }
        assert!(matches!(
            LuaPattern::from_bytes_try(b"%"),
            Err(PatternError::MalformedPattern(
                MalformedPattern::EndsWithPercent
            ))
        ));
    }

    #[test]
    fn validator_counts_position_captures_and_preserves_closed_lengths() {
        let mut matcher = LuaPattern::from_bytes_try(b"()(a)%2").unwrap();
        assert!(matcher.matches_bytes(b"aa").unwrap());
        assert_eq!(matcher.capture(1), LuaCapture::Position(0));
        assert_eq!(matcher.capture(2), LuaCapture::Bytes { start: 0, end: 1 });
    }

    #[test]
    fn capture_limit_allows_32_and_rejects_33() {
        let position = b"()".repeat(LUA_MAXMATCHES - 1);
        let mut position_matcher = LuaPattern::from_bytes_try(&position).unwrap();
        assert!(position_matcher.matches_bytes(b"x").unwrap());
        assert_eq!(position_matcher.num_matches(), LUA_MAXMATCHES);
        assert_eq!(
            position_matcher.capture(LUA_MAXMATCHES - 1),
            LuaCapture::Position(0)
        );

        let ordinary = b"(x)".repeat(LUA_MAXMATCHES - 1);
        let subject = b"x".repeat(LUA_MAXMATCHES - 1);
        let mut ordinary_matcher = LuaPattern::from_bytes_try(&ordinary).unwrap();
        assert!(ordinary_matcher.matches_bytes(&subject).unwrap());
        assert_eq!(ordinary_matcher.num_matches(), LUA_MAXMATCHES);
        assert_eq!(
            ordinary_matcher.capture(LUA_MAXMATCHES - 1),
            LuaCapture::Bytes { start: 31, end: 32 }
        );

        let mixed = [b"()".repeat(16), b"(x)".repeat(16)].concat();
        let mut mixed_matcher = LuaPattern::from_bytes_try(&mixed).unwrap();
        assert!(mixed_matcher.matches_bytes(b"xxxxxxxxxxxxxxxx").unwrap());
        assert_eq!(mixed_matcher.num_matches(), LUA_MAXMATCHES);

        for pattern in [b"()".repeat(LUA_MAXMATCHES), b"(x)".repeat(LUA_MAXMATCHES)] {
            assert!(matches!(
                LuaPattern::from_bytes_try(&pattern),
                Err(PatternError::TooManyCaptures)
            ));
        }
    }

    #[test]
    fn escaped_uppercase_literals_match_their_original_byte() {
        for class in b"BEFHIJKMNOQRTVY" {
            for pattern in [vec![b'%', *class], vec![b'[', b'%', *class, b']']] {
                let mut matcher = LuaPattern::from_bytes_try(&pattern).unwrap();
                assert!(matcher.matches_bytes(&[*class]).unwrap(), "{pattern:?}");
                assert!(
                    !matcher
                        .matches_bytes(&[class.to_ascii_lowercase()])
                        .unwrap()
                );
            }
        }
    }

    #[test]
    fn space_class_includes_vertical_tab() {
        for byte in [0x0b, 0x0c] {
            let mut space = LuaPattern::from_bytes_try(b"%s").unwrap();
            assert!(space.matches_bytes(&[byte]).unwrap());
            let mut non_space = LuaPattern::from_bytes_try(b"%S").unwrap();
            assert!(!non_space.matches_bytes(&[byte]).unwrap());
        }
    }

    #[test]
    fn empty_pattern_matches_the_initial_empty_range() {
        for subject in [b"".as_slice(), b"abc".as_slice()] {
            let mut matcher = LuaPattern::from_bytes_try(b"").unwrap();
            assert!(matcher.matches_bytes(subject).unwrap());
            assert_eq!(matcher.range(), 0..0);
            assert_eq!(matcher.num_matches(), 1);
        }
    }

    #[test]
    fn position_capture_backreference_does_not_match() {
        let mut matcher = LuaPattern::from_bytes_try(b"()%1").unwrap();
        assert!(!matcher.matches_bytes(b"abc").unwrap());
    }

    #[test]
    fn zero_capture_index_formats_without_overflow() {
        let error = match LuaPattern::from_bytes_try(b"(a)%0") {
            Ok(_) => panic!("%0 must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "invalid capture index %0");
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
        let pattern = b"a?".repeat(200);
        let mut pattern = LuaPattern::from_bytes_try(&pattern).unwrap();
        assert!(matches!(
            pattern.matches_bytes(&[b'a'; 200]),
            Err(PatternError::MatchDepthExceeded)
        ));
    }

    #[test]
    fn tail_transitions_do_not_consume_or_leak_match_depth() {
        let literal_pattern = vec![b'a'; 201];
        let mut literals = LuaPattern::from_bytes_try(&literal_pattern).unwrap();
        assert!(literals.matches_bytes(&vec![b'a'; 201]).unwrap());

        let boundary_pattern = b"a?".repeat(199);
        let mut boundary = LuaPattern::from_bytes_try(&boundary_pattern).unwrap();
        assert!(boundary.matches_bytes(&[b'a'; 199]).unwrap());

        let mut scan = LuaPattern::from_bytes_try(b"a%d").unwrap();
        assert!(!scan.matches_bytes(&vec![b'a'; 250]).unwrap());
    }

    #[test]
    fn accept_empty_repeats_do_not_consume_match_depth() {
        // A `*` or `-` item that cannot match once takes reference's depth-free
        // accept-empty transition, so an arbitrarily long run of them still
        // matches the empty string. Both boundaries verified against 5.2/5.4.
        for suffix in *b"*-" {
            let pattern = [b'b', suffix].repeat(200);
            let mut matcher = LuaPattern::from_bytes_try(&pattern).unwrap();
            assert!(matcher.matches_bytes(b"").unwrap(), "{suffix:?}");
            assert_eq!(matcher.range(), 0..0);
        }
    }

    #[test]
    fn position_captures_consume_a_depth_level() {
        // Reference routes `()` through `start_capture`, which recurses. One
        // initial frame + 32 position captures + N optionals must therefore hit
        // the limit at exactly the same N as reference does.
        let prefix = b"()".repeat(LUA_MAXMATCHES - 1);

        let accepted = [prefix.clone(), b"a?".repeat(167)].concat();
        let mut accepted = LuaPattern::from_bytes_try(&accepted).unwrap();
        assert!(accepted.matches_bytes(&[b'a'; 167]).unwrap());

        let rejected = [prefix, b"a?".repeat(168)].concat();
        let mut rejected = LuaPattern::from_bytes_try(&rejected).unwrap();
        assert!(matches!(
            rejected.matches_bytes(&[b'a'; 168]),
            Err(PatternError::MatchDepthExceeded)
        ));
    }

    #[test]
    fn bracket_ranges_follow_reference_branch_order() {
        // The escape / range / literal arms are mutually exclusive and ordered,
        // exactly as reference `matchbracketclass`. Flattening them changes
        // class membership. Every expectation here was measured against both
        // Lua 5.2 and 5.4.
        let cases: [(&[u8], &[u8], &[u8]); 4] = [
            // A descending range is empty and does not contribute its start.
            (b"[z-a]", b"", b"za-5q"),
            // `%a` cannot also open a range, so `-` stays a literal member.
            (b"[%a-z]", b"za-q", b"5"),
            (b"[%d-a]", b"a-5", b"zq"),
            // `%-` is an escaped literal, so only `-` and `a` are members.
            (b"[%--a]", b"a-", b"z5q"),
        ];

        for (pattern, members, non_members) in cases {
            let mut matcher = LuaPattern::from_bytes_try(pattern).unwrap();
            for byte in members {
                assert!(
                    matcher.matches_bytes(&[*byte]).unwrap(),
                    "{pattern:?} {byte}"
                );
            }
            for byte in non_members {
                assert!(
                    !matcher.matches_bytes(&[*byte]).unwrap(),
                    "{pattern:?} {byte}"
                );
            }
        }
    }

    #[test]
    fn resumed_matches_keep_absolute_offsets_and_left_context() {
        let mut frontier = LuaPattern::from_bytes_try(b"%f[%a]%a").unwrap();
        assert!(!frontier.matches_bytes_from(b"ab", 1).unwrap());

        let mut end = LuaPattern::from_bytes_try(b"$").unwrap();
        assert!(end.matches_bytes_from(b"ab", 1).unwrap());
        assert_eq!(end.range(), 2..2);

        let mut captures = LuaPattern::from_bytes_try(b"()(b)()").unwrap();
        assert!(captures.matches_bytes_from(b"ab", 1).unwrap());
        assert_eq!(captures.range(), 1..2);
        assert_eq!(captures.capture(1), LuaCapture::Position(1));
        assert_eq!(captures.capture(2), LuaCapture::Bytes { start: 1, end: 2 });
        assert_eq!(captures.capture(3), LuaCapture::Position(2));
    }
}
