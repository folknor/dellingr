//! Internal implementation of Lua pattern matching on bytes.
//!
//! Lua patterns operate on bytes. Dellingr only needs the byte matching core.

use core::ops;
use std::sync::Arc;

use crate::cost_meter::CostMeter;

pub(crate) mod errors;
#[cfg(test)]
use self::errors::MalformedPattern;

mod luapat;
use self::luapat::{CompiledPattern, LUA_MAXMATCHES, compile, str_match};

pub(crate) use self::errors::{MatchError, PatternError};
pub(crate) use self::luapat::LuaCapture;

/// Whether every byte in `pattern` is literal to the Lua pattern compiler.
pub(crate) fn is_plain(pattern: &[u8]) -> bool {
    !pattern.iter().any(|byte| luapat::is_pattern_magic(*byte))
}

/// Whether `string.find` can use a plain search without changing Lua 5.4 behavior.
///
/// Lua's `string.find` fast path treats `)` as literal, although the pattern
/// matcher uses it to close captures. `string.gsub` does not have this quirk.
pub(crate) fn is_plain_for_find(pattern: &[u8]) -> bool {
    !pattern
        .iter()
        .any(|byte| luapat::is_pattern_magic(*byte) && *byte != b')')
}

/// An immutable compiled pattern program, shareable between matchers.
///
/// Matching never mutates the program: every search builds fresh scratch
/// state, so a cached program behaves exactly like a freshly compiled one,
/// including where lazily encoded malformed points raise.
#[derive(Clone)]
pub(crate) struct SharedPattern(Arc<CompiledPattern>);

impl SharedPattern {
    /// Compile `bytes` into a shareable program. Compilation cannot fail.
    pub(crate) fn compile(bytes: &[u8]) -> SharedPattern {
        SharedPattern(Arc::new(compile(bytes)))
    }

    /// A matcher over this program with empty capture output.
    pub(crate) fn matcher(&self) -> LuaPattern {
        LuaPattern::from_compiled(Arc::clone(&self.0))
    }
}

/// A compiled Lua string pattern and the captures from the latest match.
///
/// Compilation copies everything the matcher needs into `compiled`, so the
/// pattern bytes are not borrowed past construction.
pub(crate) struct LuaPattern {
    compiled: Arc<CompiledPattern>,
    matches: [LuaCapture; LUA_MAXMATCHES],
    n_match: usize,
}

impl LuaPattern {
    fn from_compiled(compiled: Arc<CompiledPattern>) -> LuaPattern {
        LuaPattern {
            compiled,
            matches: [LuaCapture::Bytes { start: 0, end: 0 }; LUA_MAXMATCHES],
            n_match: 0,
        }
    }

    /// Compile a Lua pattern from a slice of bytes. Compilation cannot fail.
    ///
    /// Reference Lua validates a pattern lazily, only as far as a match
    /// actually reaches. Malformed syntax, too many captures and bad
    /// backreferences compile into failure points raised by
    /// `matches_bytes_from` at the point reference would raise them.
    /// An unclosed capture is reported as
    /// `LuaCapture::Unfinished` for the consumer to reject.
    pub(crate) fn new(bytes: &[u8]) -> LuaPattern {
        LuaPattern::from_compiled(Arc::new(compile(bytes)))
    }

    /// Match a slice of bytes with this pattern, searching from the start.
    ///
    /// Test-only: every caller in the standard library matches against the
    /// whole subject with an explicit `init`, so that `%f` keeps its left
    /// context.
    #[cfg(test)]
    pub(crate) fn matches_bytes(
        &mut self,
        s: &[u8],
        meter: &mut CostMeter<'_>,
    ) -> Result<bool, MatchError> {
        self.matches_bytes_from(s, 0, meter)
    }

    /// Match the complete subject, beginning the search at `init`.
    pub(crate) fn matches_bytes_from(
        &mut self,
        s: &[u8],
        init: usize,
        meter: &mut CostMeter<'_>,
    ) -> Result<bool, MatchError> {
        let n_match = str_match(s, &self.compiled, init, &mut self.matches, meter)?;
        self.n_match = n_match;
        Ok(n_match > 0)
    }

    /// The full match range from the latest successful match.
    pub(crate) fn range(&self) -> ops::Range<usize> {
        match self.capture(0) {
            LuaCapture::Bytes { start, end } => start..end,
            LuaCapture::Position(_) | LuaCapture::Unfinished => {
                unreachable!("the full match is always a byte range")
            }
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

    fn count_only(used: &mut u64) -> CostMeter<'_> {
        CostMeter::count_only(used)
    }

    #[test]
    fn plain_classifier_tracks_pattern_grammar_magic_bytes() {
        for byte in b"^$*+?.([%-)" {
            assert!(!is_plain(&[*byte]), "{byte:?} is a pattern construct");
        }
        assert!(is_plain_for_find(b")"));
        assert!(!is_plain_for_find(b")*"));
        assert!(is_plain(b"]"));
        assert!(is_plain(b"literal bytes"));
    }

    /// Compile `pattern` (which never fails) and return the error matching it
    /// against `subject` raises.
    fn match_err(pattern: &[u8], subject: &[u8]) -> PatternError {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        let mut matcher = LuaPattern::new(pattern);
        match matcher.matches_bytes(subject, &mut meter) {
            Err(MatchError::Pattern(error)) => error,
            other => panic!("{pattern:?} on {subject:?}: expected a pattern error, got {other:?}"),
        }
    }

    /// Whether `pattern` compiles and finds no match in `subject` without error.
    fn no_match_no_error(pattern: &[u8], subject: &[u8]) -> bool {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        let mut matcher = LuaPattern::new(pattern);
        matches!(matcher.matches_bytes(subject, &mut meter), Ok(false))
    }

    fn assert_exact_finite_cost(pattern: &[u8], subject: &[u8], cost: u64) {
        let mut matcher = LuaPattern::new(pattern);
        let cost_i64 = i64::try_from(cost).expect("test cost must fit in i64");
        let mut remaining = cost_i64;
        let mut used = 0;
        let mut meter = CostMeter::finite_budget(&mut remaining, &mut used);
        assert!(matcher.matches_bytes(subject, &mut meter).unwrap());
        assert_eq!(used, cost);
        assert_eq!(remaining, 0);

        let mut matcher = LuaPattern::new(pattern);
        let mut remaining = cost_i64 - 1;
        let mut used = 0;
        let mut meter = CostMeter::finite_budget(&mut remaining, &mut used);
        assert!(matches!(
            matcher.matches_bytes(subject, &mut meter),
            Err(MatchError::BudgetExceeded)
        ));
        assert_eq!(used, cost - 1);
        assert_eq!(remaining, 0);
    }

    #[test]
    fn byte_captures_and_matching() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        let mut pattern = LuaPattern::new(b"^(%a+)");
        assert!(pattern.matches_bytes(b"one dog", &mut meter).unwrap());
        assert_eq!(pattern.capture(0), LuaCapture::Bytes { start: 0, end: 3 });
        assert_eq!(pattern.capture(1), LuaCapture::Bytes { start: 0, end: 3 });
        assert_eq!(pattern.num_matches(), 2);
        assert!(!pattern.matches_bytes(b" one dog", &mut meter).unwrap());
    }

    #[test]
    fn multiple_byte_captures() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        let mut pattern = LuaPattern::new(b"%s*(%d+)%s+(%S+)");
        assert!(
            pattern
                .matches_bytes(b" 233   hello dolly", &mut meter)
                .unwrap()
        );
        assert_eq!(pattern.capture(1), LuaCapture::Bytes { start: 1, end: 4 });
        assert_eq!(pattern.capture(2), LuaCapture::Bytes { start: 7, end: 12 });
    }

    #[test]
    fn bad_patterns_raise_when_the_match_reaches_them() {
        // Every subject here reaches the malformed point. "unfinished capture"
        // is not a match error at all: see `unclosed_captures_are_reported_as_values`.
        let bad = [
            (
                b"bonzo %".as_slice(),
                b"bonzo ".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::EndsWithPercent),
            ),
            (
                b"alles [%a%[".as_slice(),
                b"alles x".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBracket),
            ),
            (
                b"frodo %f[%A".as_slice(),
                b"frodo ".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBracket),
            ),
            (
                b"frodo (1) (2(3)%2)%1".as_slice(),
                b"frodo 1 23".as_slice(),
                PatternError::InvalidCaptureIndex(1),
            ),
            (
                b"%b".as_slice(),
                b"x".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBalancedArguments),
            ),
            (
                b"%bx".as_slice(),
                b"x".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBalancedArguments),
            ),
            (
                b"%f".as_slice(),
                b"x".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingFrontierBracket),
            ),
            (
                b"%fx".as_slice(),
                b"x".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingFrontierBracket),
            ),
            (
                b"%f[".as_slice(),
                b"x".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBracket),
            ),
            // A bracket class is delimited before any byte is tested, so these
            // raise even against an empty subject, as in reference.
            (
                b"[".as_slice(),
                b"".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBracket),
            ),
            (
                b"[a".as_slice(),
                b"".as_slice(),
                PatternError::MalformedPattern(MalformedPattern::MissingBracket),
            ),
            (
                b"a)".as_slice(),
                b"a".as_slice(),
                PatternError::InvalidPatternCapture,
            ),
            (
                b"(a%1)".as_slice(),
                b"aa".as_slice(),
                PatternError::InvalidCaptureIndex(0),
            ),
        ];

        for (pattern, subject, expected) in bad {
            assert_eq!(match_err(pattern, subject), expected, "{pattern:?}");
        }
    }

    #[test]
    fn malformed_tails_behind_a_failing_prefix_are_not_errors() {
        // Reference validates lazily, so each of these returns nil in lua5.2
        // and lua5.4. `x%1` and `x[` never error for any subject without an x.
        for (pattern, subject) in [
            (b"a)".as_slice(), b"b".as_slice()),
            (b"x(".as_slice(), b"abc".as_slice()),
            (b"x%1".as_slice(), b"abc".as_slice()),
            (b"x[".as_slice(), b"abc".as_slice()),
            (b"x%".as_slice(), b"abc".as_slice()),
            (b"x%b".as_slice(), b"abc".as_slice()),
            (b"x%f".as_slice(), b"abc".as_slice()),
        ] {
            assert!(no_match_no_error(pattern, subject), "{pattern:?}");
        }
        let too_many = [b"a".as_slice(), &b"()".repeat(40)].concat();
        assert!(no_match_no_error(&too_many, b"b"));
    }

    #[test]
    fn unclosed_captures_are_reported_as_values() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        for (pattern, subject) in [
            (b"(".as_slice(), b"a".as_slice()),
            (b"(a".as_slice(), b"abc".as_slice()),
            (b"bonzo (dog (cat)".as_slice(), b"bonzo dog cat".as_slice()),
        ] {
            let mut matcher = LuaPattern::new(pattern);
            assert!(
                matcher.matches_bytes(subject, &mut meter).unwrap(),
                "{pattern:?}"
            );
            assert_eq!(matcher.capture(1), LuaCapture::Unfinished, "{pattern:?}");
        }
        // A closed capture before the unclosed one is still a real value.
        let mut matcher = LuaPattern::new(b"(a)(b");
        assert!(matcher.matches_bytes(b"ab", &mut meter).unwrap());
        assert_eq!(matcher.capture(1), LuaCapture::Bytes { start: 0, end: 1 });
        assert_eq!(matcher.capture(2), LuaCapture::Unfinished);
        // With no match there is no capture to report, and no error.
        assert!(no_match_no_error(b"x(", b"abc"));
    }

    #[test]
    fn class_close_follows_reference_do_while() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        // A class consumes at least one byte before ']' can close it, so `[]]`
        // is a class containing ']'.
        let mut literal_bracket = LuaPattern::new(b"[]]");
        assert!(literal_bracket.matches_bytes(b"]", &mut meter).unwrap());
        assert_eq!(literal_bracket.range(), 0..1);
        assert!(!literal_bracket.matches_bytes(b"x", &mut meter).unwrap());

        // `[^]]` is "any byte except ']'".
        let mut complement = LuaPattern::new(b"[^]]");
        assert!(complement.matches_bytes(b"x", &mut meter).unwrap());
        assert!(!complement.matches_bytes(b"]", &mut meter).unwrap());

        // An escaped `%]` is a class byte too, and does not close the class.
        let mut escaped = LuaPattern::new(b"[%]]");
        assert!(escaped.matches_bytes(b"]", &mut meter).unwrap());

        // `[]` and `[^]` never close, matching reference "malformed pattern
        // (missing ']')" instead of parsing as an empty class.
        for pattern in [b"[]".as_slice(), b"[^]".as_slice(), b"[%]".as_slice()] {
            assert_eq!(
                match_err(pattern, b"]"),
                PatternError::MalformedPattern(MalformedPattern::MissingBracket)
            );
        }
    }

    #[test]
    fn position_captures_are_typed() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        let mut pattern = LuaPattern::new(b"()(a)()");
        assert!(pattern.matches_bytes(b"abc", &mut meter).unwrap());
        assert_eq!(pattern.capture(1), LuaCapture::Position(0));
        assert_eq!(pattern.capture(2), LuaCapture::Bytes { start: 0, end: 1 });
        assert_eq!(pattern.capture(3), LuaCapture::Position(1));
    }

    #[test]
    fn escaped_percent_patterns_match_and_dangling_percent_is_rejected() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        for (pattern, subject, range) in [
            (b"%%".as_slice(), b"%".as_slice(), 0..1),
            (b"%d+%%".as_slice(), b"100%".as_slice(), 0..4),
            (b"%%%%".as_slice(), b"%%".as_slice(), 0..2),
        ] {
            let mut matcher = LuaPattern::new(pattern);
            assert!(matcher.matches_bytes(subject, &mut meter).unwrap());
            assert_eq!(matcher.range(), range);
        }
        assert_eq!(
            match_err(b"%", b"x"),
            PatternError::MalformedPattern(MalformedPattern::EndsWithPercent)
        );
    }

    #[test]
    fn validator_counts_position_captures_and_preserves_closed_lengths() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        let mut matcher = LuaPattern::new(b"()(a)%2");
        assert!(matcher.matches_bytes(b"aa", &mut meter).unwrap());
        assert_eq!(matcher.capture(1), LuaCapture::Position(0));
        assert_eq!(matcher.capture(2), LuaCapture::Bytes { start: 0, end: 1 });
    }

    #[test]
    fn capture_limit_allows_32_and_rejects_33() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        let position = b"()".repeat(LUA_MAXMATCHES - 1);
        let mut position_matcher = LuaPattern::new(&position);
        assert!(position_matcher.matches_bytes(b"x", &mut meter).unwrap());
        assert_eq!(position_matcher.num_matches(), LUA_MAXMATCHES);
        assert_eq!(
            position_matcher.capture(LUA_MAXMATCHES - 1),
            LuaCapture::Position(0)
        );

        let ordinary = b"(x)".repeat(LUA_MAXMATCHES - 1);
        let subject = b"x".repeat(LUA_MAXMATCHES - 1);
        let mut ordinary_matcher = LuaPattern::new(&ordinary);
        assert!(
            ordinary_matcher
                .matches_bytes(&subject, &mut meter)
                .unwrap()
        );
        assert_eq!(ordinary_matcher.num_matches(), LUA_MAXMATCHES);
        assert_eq!(
            ordinary_matcher.capture(LUA_MAXMATCHES - 1),
            LuaCapture::Bytes { start: 31, end: 32 }
        );

        let mixed = [b"()".repeat(16), b"(x)".repeat(16)].concat();
        let mut mixed_matcher = LuaPattern::new(&mixed);
        assert!(
            mixed_matcher
                .matches_bytes(b"xxxxxxxxxxxxxxxx", &mut meter)
                .unwrap()
        );
        assert_eq!(mixed_matcher.num_matches(), LUA_MAXMATCHES);

        let rejected = [
            (b"()".repeat(LUA_MAXMATCHES), b"x".to_vec()),
            (b"(x)".repeat(LUA_MAXMATCHES), b"x".repeat(LUA_MAXMATCHES)),
        ];
        for (pattern, subject) in rejected {
            assert_eq!(match_err(&pattern, &subject), PatternError::TooManyCaptures);
        }
    }

    #[test]
    fn escaped_uppercase_literals_match_their_original_byte() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        for class in b"BEFHIJKMNOQRTVY" {
            for pattern in [vec![b'%', *class], vec![b'[', b'%', *class, b']']] {
                let mut matcher = LuaPattern::new(&pattern);
                assert!(
                    matcher.matches_bytes(&[*class], &mut meter).unwrap(),
                    "{pattern:?}"
                );
                assert!(
                    !matcher
                        .matches_bytes(&[class.to_ascii_lowercase()], &mut meter)
                        .unwrap()
                );
            }
        }
    }

    #[test]
    fn space_class_includes_vertical_tab() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        for byte in [0x0b, 0x0c] {
            let mut space = LuaPattern::new(b"%s");
            assert!(space.matches_bytes(&[byte], &mut meter).unwrap());
            let mut non_space = LuaPattern::new(b"%S");
            assert!(!non_space.matches_bytes(&[byte], &mut meter).unwrap());
        }
    }

    #[test]
    fn empty_pattern_matches_the_initial_empty_range() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        for subject in [b"".as_slice(), b"abc".as_slice()] {
            let mut matcher = LuaPattern::new(b"");
            assert!(matcher.matches_bytes(subject, &mut meter).unwrap());
            assert_eq!(matcher.range(), 0..0);
            assert_eq!(matcher.num_matches(), 1);
        }
    }

    #[test]
    fn position_capture_backreference_does_not_match() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        let mut matcher = LuaPattern::new(b"()%1");
        assert!(!matcher.matches_bytes(b"abc", &mut meter).unwrap());
    }

    #[test]
    fn zero_capture_index_formats_without_overflow() {
        let error = match_err(b"(a)%0", b"a");
        assert_eq!(error.to_string(), "invalid capture index %0");
    }

    #[test]
    fn end_anchors_try_the_end_position() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        let mut pattern = LuaPattern::new(b"$");
        assert!(pattern.matches_bytes(b"abc", &mut meter).unwrap());
        assert_eq!(pattern.range(), 3..3);
        assert!(pattern.matches_bytes(b"", &mut meter).unwrap());
        assert_eq!(pattern.range(), 0..0);

        let mut anchored = LuaPattern::new(b"^$");
        assert!(anchored.matches_bytes(b"", &mut meter).unwrap());
        assert_eq!(anchored.range(), 0..0);
    }

    #[test]
    fn validator_skips_both_balance_delimiters() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        // `%b((` is valid because both `(` bytes are balance delimiters, not
        // captures.
        let mut pattern = LuaPattern::new(b"%b((");
        assert!(pattern.matches_bytes(b"((", &mut meter).unwrap());
        assert_eq!(pattern.range(), 0..2);
    }

    #[test]
    fn frontier_at_end_is_safe() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        let mut pattern = LuaPattern::new(b"%f[%z]");
        assert!(pattern.matches_bytes(b"abc", &mut meter).unwrap());
        assert_eq!(pattern.range(), 3..3);
    }

    #[test]
    fn runtime_match_errors_are_not_swallowed() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        let pattern = b"a?".repeat(200);
        let mut pattern = LuaPattern::new(&pattern);
        assert!(matches!(
            pattern.matches_bytes(&[b'a'; 200], &mut meter),
            Err(MatchError::Pattern(PatternError::MatchDepthExceeded))
        ));
    }

    #[test]
    fn tail_transitions_do_not_consume_or_leak_match_depth() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        let literal_pattern = vec![b'a'; 201];
        let mut literals = LuaPattern::new(&literal_pattern);
        assert!(
            literals
                .matches_bytes(&vec![b'a'; 201], &mut meter)
                .unwrap()
        );

        let boundary_pattern = b"a?".repeat(199);
        let mut boundary = LuaPattern::new(&boundary_pattern);
        assert!(boundary.matches_bytes(&[b'a'; 199], &mut meter).unwrap());

        let mut scan = LuaPattern::new(b"a%d");
        assert!(!scan.matches_bytes(&vec![b'a'; 250], &mut meter).unwrap());
    }

    #[test]
    fn accept_empty_repeats_do_not_consume_match_depth() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        // A `*` or `-` item that cannot match once takes reference's depth-free
        // accept-empty transition, so an arbitrarily long run of them still
        // matches the empty string. Both boundaries verified against 5.2/5.4.
        for suffix in *b"*-" {
            let pattern = [b'b', suffix].repeat(200);
            let mut matcher = LuaPattern::new(&pattern);
            assert!(
                matcher.matches_bytes(b"", &mut meter).unwrap(),
                "{suffix:?}"
            );
            assert_eq!(matcher.range(), 0..0);
        }
    }

    #[test]
    fn position_captures_consume_a_depth_level() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        // Reference routes `()` through `start_capture`, which recurses. One
        // initial frame + 32 position captures + N optionals must therefore hit
        // the limit at exactly the same N as reference does.
        let prefix = b"()".repeat(LUA_MAXMATCHES - 1);

        let accepted = [prefix.clone(), b"a?".repeat(167)].concat();
        let mut accepted = LuaPattern::new(&accepted);
        assert!(accepted.matches_bytes(&[b'a'; 167], &mut meter).unwrap());

        let rejected = [prefix, b"a?".repeat(168)].concat();
        let mut rejected = LuaPattern::new(&rejected);
        assert!(matches!(
            rejected.matches_bytes(&[b'a'; 168], &mut meter),
            Err(MatchError::Pattern(PatternError::MatchDepthExceeded))
        ));
    }

    #[test]
    fn bracket_ranges_follow_reference_branch_order() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
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
            let mut matcher = LuaPattern::new(pattern);
            for byte in members {
                assert!(
                    matcher.matches_bytes(&[*byte], &mut meter).unwrap(),
                    "{pattern:?} {byte}"
                );
            }
            for byte in non_members {
                assert!(
                    !matcher.matches_bytes(&[*byte], &mut meter).unwrap(),
                    "{pattern:?} {byte}"
                );
            }
        }
    }

    #[test]
    fn resumed_matches_keep_absolute_offsets_and_left_context() {
        let mut used = 0;
        let mut meter = count_only(&mut used);
        let mut frontier = LuaPattern::new(b"%f[%a]%a");
        assert!(!frontier.matches_bytes_from(b"ab", 1, &mut meter).unwrap());

        let mut end = LuaPattern::new(b"$");
        assert!(end.matches_bytes_from(b"ab", 1, &mut meter).unwrap());
        assert_eq!(end.range(), 2..2);

        let mut captures = LuaPattern::new(b"()(b)()");
        assert!(captures.matches_bytes_from(b"ab", 1, &mut meter).unwrap());
        assert_eq!(captures.range(), 1..2);
        assert_eq!(captures.capture(1), LuaCapture::Position(1));
        assert_eq!(captures.capture(2), LuaCapture::Bytes { start: 1, end: 2 });
        assert_eq!(captures.capture(3), LuaCapture::Position(2));
    }

    #[test]
    fn finite_budget_charges_balanced_matches_exactly() {
        assert_exact_finite_cost(b"%b()", b"(a)", 5);
    }

    #[test]
    fn finite_budget_charges_backreferences_exactly() {
        assert_exact_finite_cost(b"(a)%1", b"aa", 7);
    }

    #[test]
    fn finite_budget_charges_each_outer_search_attempt() {
        assert_exact_finite_cost(b"b", b"aaab", 9);
    }

    #[test]
    fn finite_budget_charges_greedy_and_minimal_expansion_exactly() {
        assert_exact_finite_cost(b"a*b", b"aaab", 9);
        assert_exact_finite_cost(b"a-b", b"aaab", 14);
    }

    #[test]
    fn finite_budget_stops_pathological_nested_greedies() {
        let mut matcher = LuaPattern::new(b"^a*a*a*b");
        let mut remaining = 20;
        let mut used = 0;
        let mut meter = CostMeter::finite_budget(&mut remaining, &mut used);
        assert!(matches!(
            matcher.matches_bytes(b"aaaa", &mut meter),
            Err(MatchError::BudgetExceeded)
        ));
        assert_eq!(used, 20);
        assert_eq!(remaining, 0);
    }
}
