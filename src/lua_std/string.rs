//! Lua's String Library

use std::sync::atomic::AtomicBool;

use super::{BytesCopy, STRING_LIBRARY_CAPACITY, arg_integer, opt_integer};
use crate::cost_meter::CostMeter;
use crate::cost_model::Charge;
use crate::error::{Error, ErrorKind, LibraryError, PatternErrorDetail, StringPatternFunction};
use crate::instr::{ArgCount, RetCount};
use crate::patterns::{LuaCapture, LuaPattern, MatchError, PatternError};
use crate::saturate::saturating_cast;
use crate::vm::{GmatchIter, StringArg};
use crate::{LuaType, Result, State};

// Library charges are registered in `crate::cost_model`: the code here
// measures units and the registry supplies the weight of the `Charge` of the
// function the work is done for. The rule (docs/budget.md, "String costs"):
// a string library call costs 1, `string.format` 1 per directive instead,
// and pattern matching adds 1 per matcher step or substring-scan comparison.
// Bytes read and produced are free, as `..`'s are, so no site here charges
// for subject, pattern, template, capture or output bytes; do not add one
// back. Changing what a function charges under the default model changes
// `cost_used` for existing scripts and replays, so it needs a
// COST_MODEL_VERSION bump and invalidates saves. Native calls are opaque to
// the `analyze_cost` static walk, so it cannot see these charges.
fn charge_call(state: &mut State, charge: Charge) -> Result<()> {
    state.charge(charge, 1)
}

/// Plain substring search: a first-byte skip loop over candidate windows,
/// over `haystack` from byte `from`. Returns the match offset relative to
/// `from`.
///
/// The cost charged is byte-for-byte identical to the naive nested scan this
/// replaced (1 per start position whose first byte mismatches, common prefix
/// plus the mismatching comparison per candidate, `needle.len()` for the
/// match), including where in the scan a finite budget runs out - the
/// algorithm change is invisible through `cost_used`, only wall time moves.
/// Both arguments are read in place (`StringArg`); the scan charges through
/// the borrowed meter with per-unit admission (`CostMeter::consume_units`).
fn find_subslice(
    state: &mut State,
    charge: Charge,
    haystack: &StringArg,
    from: usize,
    needle: &StringArg,
) -> Result<Option<usize>> {
    state
        .with_string_args(
            haystack,
            needle,
            charge,
            |haystack, needle, mut meter, interrupt| {
                scan_subslice(&mut meter, interrupt, &haystack[from..], needle)
            },
        )?
        .map_err(|err| match_error(state, StringPatternFunction::Find, err))
}

/// The bill is the naive scan's comparison count: per candidate, its shared
/// prefix with the needle plus one, or the needle's length on a match. A
/// needle sharing a long prefix with many candidates recompares that prefix
/// per candidate, and each prefix is compared in full before its bill is
/// admitted, so a nearly spent budget can stop after a long comparison. A
/// linear-time kernel (a Z-function or KMP table yields every candidate's
/// prefix length exactly) could keep this bill and only cut host time on
/// such inputs; it was considered and not taken, since the common case
/// already skips to first-byte candidates and the pathological case is
/// billed per compared byte when costing is on. It stays open as pure host
/// work, never as a cost change.
fn scan_subslice(
    meter: &mut CostMeter<'_>,
    interrupt: &AtomicBool,
    haystack: &[u8],
    needle: &[u8],
) -> std::result::Result<Option<usize>, MatchError> {
    let charge = |meter: &mut CostMeter<'_>, units: usize| {
        if meter.consume_units(units as u64) {
            Ok(())
        } else {
            Err(MatchError::BudgetExceeded)
        }
    };
    if needle.is_empty() {
        return Ok(Some(0));
    }
    if needle.len() > haystack.len() {
        return Ok(None);
    }
    let first = needle[0];
    let last_start = haystack.len() - needle.len();
    let mut start = 0usize;
    // safepoint: polls the interrupt flag once per candidate, below.
    while start <= last_start {
        let Some(offset) = memchr::memchr(first, &haystack[start..=last_start]) else {
            charge(meter, last_start - start + 1)?;
            return Ok(None);
        };
        charge(meter, offset)?;
        // A safepoint per candidate: each prefix comparison is one bounded
        // pass, but a needle sharing a long prefix with every candidate
        // repeats it once per subject byte.
        if crate::vm::interrupt_requested(interrupt) {
            return Err(MatchError::Interrupted);
        }
        let cand = start + offset;
        let window = &haystack[cand..cand + needle.len()];
        let prefix = window
            .iter()
            .zip(needle)
            .take_while(|(hay, need)| hay == need)
            .count();
        if prefix == needle.len() {
            charge(meter, needle.len())?;
            return Ok(Some(cand));
        }
        charge(meter, prefix + 1)?;
        start = cand + 1;
    }
    Ok(None)
}

fn match_error(state: &State, function: StringPatternFunction, error: MatchError) -> Error {
    match error {
        MatchError::Pattern(error) => pattern_runtime_error(state, function, error),
        MatchError::BudgetExceeded => state.budget_exceeded_error(),
        MatchError::Interrupted => state.interrupted_error(),
    }
}

fn pattern_runtime_error(
    state: &State,
    function: StringPatternFunction,
    error: PatternError,
) -> Error {
    // Keep PatternError's detail-only kind for hosts that match its existing
    // variants; the boxed Error context carries the string function separately.
    state
        .error(ErrorKind::PatternError(PatternErrorDetail::from(error)))
        .with_pattern_function(function)
}

fn lua_start_index(len: usize, idx: i64) -> usize {
    let len = i64::try_from(len).expect("string length fits in i64");
    if idx >= 0 {
        // Index 0 maps to -1, which clamps to 0; past `usize::MAX` behaves
        // exactly like `usize::MAX`.
        saturating_cast(idx - 1)
    } else {
        usize::try_from((len + idx).max(0)).expect("nonnegative string index fits in usize")
    }
}

fn lua_end_index(len: usize, idx: i64) -> usize {
    let len_i64 = i64::try_from(len).expect("string length fits in i64");
    if idx >= 0 {
        usize::try_from(idx.min(len_i64)).expect("string length fits in usize")
    } else {
        usize::try_from((len_i64 + idx + 1).max(0)).expect("nonnegative string index fits in usize")
    }
}

/// The byte range `string.sub(s, i, j)` returns from a subject of `len`
/// bytes, always a valid range of the subject: every empty result is `0..0`.
fn lua_sub_range(len: usize, start_idx: i64, end_idx: i64) -> std::ops::Range<usize> {
    let start = lua_start_index(len, start_idx);
    let end = lua_end_index(len, end_idx);
    if start >= end || start >= len {
        0..0
    } else {
        start..end
    }
}

/// `string.upper` and `string.lower`: a string with no byte the mapping
/// changes is returned as the argument value itself, with no copy and no
/// intern (so no collection point); any other string, and every number's
/// rendering, is copied, mapped and interned.
fn map_ascii_case(
    state: &mut State,
    function_name: &str,
    charge: Charge,
    changes: fn(&u8) -> bool,
    map: fn(&mut [u8]),
) -> Result<u8> {
    check_string_or_number(state, 1, function_name)?;
    if state.typ(1) == LuaType::String && !state.to_bytes(1)?.iter().any(changes) {
        charge_call(state, charge)?;
        state.set_top(1)?;
        return Ok(1);
    }
    let mut s = BytesCopy::of_string_or_number(state, 1)?;
    charge_call(state, charge)?;
    map(s.as_mut_slice());
    state.set_top(0)?;
    s.push(state)?;
    Ok(1)
}

/// A pattern function's subject or pattern: type-gated by
/// `check_string_or_number` (strings and numbers only, narrower than
/// `bytes_with_default_string_coercion` on purpose), then a string read in place in its slot and a
/// number rendered once. Reading in place needs no shared ownership of heap
/// string bytes: each read borrows the State only for its own span (the slot
/// roots the string for the whole call, `gsub` replacement calls included),
/// and the matcher and plain search charge through a meter split off the
/// State while the bytes are borrowed.
fn string_or_number_arg(state: &mut State, idx: isize, function_name: &str) -> Result<StringArg> {
    check_string_or_number(state, idx, function_name)?;
    state.string_arg(idx)
}

/// The byte length of a pattern-function argument.
fn string_arg_len(state: &State, arg: &StringArg) -> Result<usize> {
    Ok(state.string_arg_bytes(arg)?.len())
}

/// The type gate of `string_or_number_bytes`, without the copy. Used where an
/// argument must be validated before later arguments are read, so errors name
/// the arguments in the reference's order.
fn check_string_or_number(state: &mut State, idx: isize, function_name: &str) -> Result<()> {
    state.check_type_in(idx, &[LuaType::String, LuaType::Number], function_name)
}

/// Reference Lua raises "unfinished capture" only when a library function
/// reads a capture whose `(` the match never closed (`get_onecapture`), so
/// every consumer of a capture value routes `LuaCapture::Unfinished` here.
/// The stack top to keep below a pattern function's results while they are
/// copied out of its subject: up to the subject's slot when it is read there
/// (slot 1, so nothing else stays), nothing for a number's owned rendering.
fn subject_floor(subject: &StringArg) -> isize {
    subject.slot().unwrap_or(0)
}

fn unfinished_capture_error(state: &State, function: StringPatternFunction) -> Error {
    pattern_runtime_error(state, function, PatternError::UnfinishedCapture)
}

fn push_capture_value(
    state: &mut State,
    subject: &StringArg,
    capture: LuaCapture,
    function: StringPatternFunction,
) -> Result<()> {
    match capture {
        LuaCapture::Bytes { start, end } => state.push_string_arg_range(subject, start..end)?,
        LuaCapture::Position(offset) => state.push_number((offset + 1) as f64)?,
        LuaCapture::Unfinished => return Err(unfinished_capture_error(state, function)),
    }
    Ok(())
}

fn push_captures(
    state: &mut State,
    subject: &StringArg,
    pattern: &LuaPattern,
    function: StringPatternFunction,
) -> Result<u8> {
    let n = pattern.num_matches();
    if n > 1 {
        check_captures_finished(state, (1..n).map(|i| pattern.capture(i)), function)?;
    } else {
        check_captures_finished(state, [pattern.capture(0)], function)?;
    }
    if n > 1 {
        // bounded: one step per capture, at most LUA_MAXCAPTURES.
        for i in 1..n {
            push_capture_value(state, subject, pattern.capture(i), function)?;
        }
        crate::error::results_count(n - 1).map_err(|kind| state.error(kind))
    } else {
        push_capture_value(state, subject, pattern.capture(0), function)?;
        Ok(1)
    }
}

/// Capture bytes are produced strings and charge nothing; this only rejects
/// an unclosed capture before any capture value reaches the stack.
fn check_captures_finished(
    state: &State,
    captures: impl IntoIterator<Item = LuaCapture>,
    function: StringPatternFunction,
) -> Result<()> {
    // Every caller pushes exactly the captures it checks, so reject an
    // unclosed one before copying any capture bytes to the stack.
    // bounded: one step per capture, at most LUA_MAXCAPTURES.
    for capture in captures {
        if capture == LuaCapture::Unfinished {
            return Err(unfinished_capture_error(state, function));
        }
    }
    Ok(())
}

fn append_capture_bytes(
    state: &mut State,
    out: &mut Vec<u8>,
    subject: &StringArg,
    capture: LuaCapture,
    function: StringPatternFunction,
) -> Result<()> {
    match capture {
        LuaCapture::Bytes { start, end } => {
            append_subject_range(state, out, subject, start..end)?;
        }
        LuaCapture::Position(offset) => {
            let position = (offset + 1).to_string();
            append_bytes(out, position.as_bytes())?;
        }
        LuaCapture::Unfinished => return Err(unfinished_capture_error(state, function)),
    }
    Ok(())
}

/// `append_bytes` for a range of the subject, read in place.
fn append_subject_range(
    state: &State,
    out: &mut Vec<u8>,
    subject: &StringArg,
    range: std::ops::Range<usize>,
) -> Result<()> {
    let next = crate::limits::checked_string_growth(out.len(), range.len())?;
    out.reserve(next - out.len());
    out.extend_from_slice(&state.string_arg_bytes(subject)?[range]);
    Ok(())
}

/// Append `bytes` to a library result after checking the string size cap for
/// the whole append. Produced bytes are free (docs/budget.md, "String
/// costs"), so nothing is charged.
///
/// The growing producers (`gsub`, `string.format`, `table.concat`) append
/// into a plain `Vec` on purpose: a small-buffer builder that tests inline
/// or heap storage on every append was measured 3 to 15 percent slower on
/// long-result workloads to save one allocation on a short result. Short
/// storage is decided once, where a producer knows its length up front.
pub(super) fn append_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let next = crate::limits::checked_string_growth(out.len(), bytes.len())?;
    out.reserve(next - out.len());
    out.extend_from_slice(bytes);
    Ok(())
}

fn invalid_gsub_capture_error(state: &State, index: usize) -> Error {
    // Replacement-template errors stay runtime errors: PatternError describes
    // errors in the pattern, while this index refers to a replacement template.
    // The index is the replacement's `%N` digit, and the message renders it as
    // `%N`, the same spelling the pattern-side capture-index error uses.
    state.library_error(LibraryError::InvalidGsubCaptureIndex { index })
}

fn gsub_replacement_keeps_original(state: &State, index: isize) -> bool {
    state.typ(index) == LuaType::Nil
        || (state.typ(index) == LuaType::Boolean && !state.to_boolean(index))
}

fn invalid_gsub_escape_error(state: &State) -> Error {
    state.library_error(LibraryError::InvalidGsubEscape)
}

/// Why a string-replacement `gsub` scan stopped. The scan runs inside a
/// borrow of the State's arguments, so it carries the cause out and
/// `gsub_stop_error` builds the error once the borrow has ended. The scan
/// stops at its first failure, so the failure that wins is the one the
/// ordered scan reaches first.
enum GsubStop {
    Match(MatchError),
    Error(Error),
    InvalidCapture(usize),
    InvalidEscape,
    Unfinished,
}

impl From<Error> for GsubStop {
    fn from(error: Error) -> Self {
        Self::Error(error)
    }
}

fn gsub_stop_error(state: &State, stop: GsubStop) -> Error {
    match stop {
        GsubStop::Match(error) => match_error(state, StringPatternFunction::Gsub, error),
        GsubStop::Error(error) => error,
        GsubStop::InvalidCapture(index) => invalid_gsub_capture_error(state, index),
        GsubStop::InvalidEscape => invalid_gsub_escape_error(state),
        GsubStop::Unfinished => unfinished_capture_error(state, StringPatternFunction::Gsub),
    }
}

/// Apply a string replacement for one match, read straight from the
/// replacement's bytes as reference Lua's `add_s` does: literal runs (each
/// `%%` one `%`), `%0` the whole match, `%1`-`%9` a capture (`%1` the whole
/// match when the pattern has no captures), anything else after `%` an
/// invalid escape. Nothing is validated ahead of use: a capture or escape
/// fails only when an application reaches it, after the output before it
/// is appended, so a call with no match never looks at the replacement.
fn apply_string_replacement(
    out: &mut Vec<u8>,
    repl: &[u8],
    subject: &[u8],
    n_captures: usize,
    capture: impl Fn(usize) -> LuaCapture,
) -> std::result::Result<(), GsubStop> {
    let mut i = 0usize;
    // bounded: one pass over the replacement's bytes. The scans that apply
    // it repeatedly poll once per application (the empty-pattern position
    // loop, the matcher, `scan_subslice`).
    while i < repl.len() {
        if repl[i] != b'%' || repl.get(i + 1) == Some(&b'%') {
            i = append_literal_run(out, repl, i)?;
            continue;
        }
        let Some(digit) = repl.get(i + 1).filter(|b| b.is_ascii_digit()) else {
            return Err(GsubStop::InvalidEscape);
        };
        let idx = usize::from(digit - b'0');
        let selected = if idx == 0 || (n_captures == 1 && idx == 1) {
            capture(0)
        } else if idx < n_captures {
            capture(idx)
        } else {
            return Err(GsubStop::InvalidCapture(idx));
        };
        append_replacement_capture(out, subject, selected)?;
        i += 2;
    }
    Ok(())
}

/// Append the replacement's literal run from `start`: its bytes up to the
/// next escape other than `%%`, each `%%` as one `%`. The string size cap
/// is checked once for the whole run, so a run that crosses it is refused
/// naming the size the whole run would reach. Returns the index past it.
fn append_literal_run(
    out: &mut Vec<u8>,
    repl: &[u8],
    start: usize,
) -> std::result::Result<usize, GsubStop> {
    let mut len = 0usize;
    let mut end = start;
    // bounded: one pass over the rest of the replacement.
    while end < repl.len() {
        if repl[end] != b'%' {
            end += 1;
        } else if repl.get(end + 1) == Some(&b'%') {
            end += 2;
        } else {
            break;
        }
        len += 1;
    }
    let next = crate::limits::checked_string_growth(out.len(), len)?;
    out.reserve(next - out.len());
    let mut i = start;
    // bounded: one pass over the run just measured.
    while i < end {
        let stop = memchr::memchr(b'%', &repl[i..end]).map_or(end, |offset| i + offset);
        out.extend_from_slice(&repl[i..stop]);
        if stop == end {
            break;
        }
        // A `%` inside the run is always the first of a `%%` pair.
        out.push(b'%');
        i = stop + 2;
    }
    Ok(end)
}

/// Append one capture a string replacement names: a subject range, or a
/// position as its decimal integer.
fn append_replacement_capture(
    out: &mut Vec<u8>,
    subject: &[u8],
    capture: LuaCapture,
) -> std::result::Result<(), GsubStop> {
    match capture {
        LuaCapture::Bytes { start, end } => append_bytes(out, &subject[start..end])?,
        LuaCapture::Position(offset) => {
            let mut digits = [0u8; 20];
            let mut at = digits.len();
            let mut value = offset + 1;
            // bounded: one step per decimal digit of a usize, at most 20.
            loop {
                at -= 1;
                digits[at] = b'0' + saturating_cast::<u8, usize>(value % 10);
                value /= 10;
                if value == 0 {
                    break;
                }
            }
            append_bytes(out, &digits[at..])?;
        }
        LuaCapture::Unfinished => return Err(GsubStop::Unfinished),
    }
    Ok(())
}

/// What a string-replacement scan built: the output up to `copied` (the
/// rest of the subject is appended by `finish_gsub`) and the count.
struct GsubScan {
    result: Vec<u8>,
    copied: usize,
    count: usize,
}

/// `string.gsub` with a string or number replacement. No Lua runs during
/// the scan, so the whole scan runs in one borrow of the subject, the
/// replacement (a number's rendering, made up front: it allocates no heap
/// object) and the pattern's needle on the plain path, with one meter and
/// the interrupt flag. The charges, safepoints, size-cap checks and failure
/// order are the per-match ones of the callback paths. The replacement is
/// read in place on every application rather than compiled, so there is no
/// per-call plan to build and none worth caching across calls.
#[inline(never)]
fn gsub_with_string(
    state: &mut State,
    s: &StringArg,
    pattern: &StringArg,
    max_replacements: Option<usize>,
) -> Result<u8> {
    let repl = state.string_arg(3)?;
    let scanned = if string_arg_len(state, pattern)? == 0 {
        state.with_string_arg_slices(
            [s, &repl],
            Charge::StringGsub,
            |[subject, repl], _, interrupt| {
                gsub_empty_with_string(subject, repl, interrupt, max_replacements)
            },
        )?
    } else if crate::patterns::is_plain(state.string_arg_bytes(pattern)?) {
        state.with_string_arg_slices(
            [s, pattern, &repl],
            Charge::StringGsub,
            |[subject, needle, repl], mut meter, interrupt| {
                gsub_plain_with_string(
                    subject,
                    needle,
                    repl,
                    &mut meter,
                    interrupt,
                    max_replacements,
                )
            },
        )?
    } else {
        let mut matcher = pattern_matcher(state, pattern)?;
        state.with_string_arg_slices(
            [s, &repl],
            Charge::StringGsub,
            |[subject, repl], mut meter, interrupt| {
                gsub_lua_with_string(
                    &mut matcher,
                    subject,
                    repl,
                    &mut meter,
                    interrupt,
                    max_replacements,
                )
            },
        )?
    };
    let scan = scanned.map_err(|stop| gsub_stop_error(state, stop))?;
    finish_gsub(state, s, scan.result, scan.copied, scan.count)
}

/// The empty pattern: a replacement at every position, which examines no
/// subject byte and so charges nothing beyond the call.
fn gsub_empty_with_string(
    subject: &[u8],
    repl: &[u8],
    interrupt: &AtomicBool,
    max_replacements: Option<usize>,
) -> std::result::Result<GsubScan, GsubStop> {
    let mut result = Vec::with_capacity(subject.len());
    let mut count = 0usize;
    for i in 0..=subject.len() {
        // A safepoint per position: no matcher runs here, and a long
        // replacement of empty captures applies once per subject byte.
        if crate::vm::interrupt_requested(interrupt) {
            return Err(GsubStop::Match(MatchError::Interrupted));
        }
        if max_replacements.is_none_or(|max| count < max) {
            apply_string_replacement(&mut result, repl, subject, 1, |_| LuaCapture::Bytes {
                start: i,
                end: i,
            })?;
            count += 1;
        }
        if i < subject.len() {
            append_bytes(&mut result, &subject[i..=i])?;
        }
    }
    Ok(GsubScan {
        result,
        copied: subject.len(),
        count,
    })
}

/// A plain pattern: `scan_subslice` per match, charged and polled per
/// candidate exactly as `find_subslice` does.
fn gsub_plain_with_string(
    subject: &[u8],
    needle: &[u8],
    repl: &[u8],
    meter: &mut CostMeter<'_>,
    interrupt: &AtomicBool,
    max_replacements: Option<usize>,
) -> std::result::Result<GsubScan, GsubStop> {
    let mut result = Vec::new();
    let mut pos = 0usize;
    let mut count = 0usize;
    // safepoint: every iteration searches with `scan_subslice`, which polls
    // once per candidate, and a match advances past a non-empty needle.
    while pos < subject.len() {
        if max_replacements.is_some_and(|max| count >= max) {
            break;
        }
        let Some(offset) =
            scan_subslice(meter, interrupt, &subject[pos..], needle).map_err(GsubStop::Match)?
        else {
            break;
        };
        let start = pos + offset;
        let end = start + needle.len();
        if count == 0 {
            // Sized at the first match: a call with no match returns the
            // subject.
            result.reserve(subject.len());
        }
        append_bytes(&mut result, &subject[pos..start])?;
        apply_string_replacement(&mut result, repl, subject, 1, |_| LuaCapture::Bytes {
            start,
            end,
        })?;
        pos = end;
        count += 1;
    }
    Ok(GsubScan {
        result,
        copied: pos,
        count,
    })
}

/// A Lua pattern, with the empty-match rule and pending-copy cursor of
/// `gsub_lua_pattern`; captures are read off the matcher as the
/// replacement names them.
fn gsub_lua_with_string(
    matcher: &mut LuaPattern,
    subject: &[u8],
    repl: &[u8],
    meter: &mut CostMeter<'_>,
    interrupt: &AtomicBool,
    max_replacements: Option<usize>,
) -> std::result::Result<GsubScan, GsubStop> {
    let s_len = subject.len();
    let mut result = Vec::new();
    let mut pos = 0usize;
    let mut copied = 0usize;
    let mut count = 0usize;
    let anchored = matcher.is_anchored();
    let mut lastmatch = None;
    // safepoint: every iteration runs the matcher, whose first step polls
    // (`MatchState::charge`).
    loop {
        if max_replacements.is_some_and(|max| count >= max) {
            break;
        }
        if !matcher
            .matches_bytes_from(subject, pos, meter, interrupt)
            .map_err(GsubStop::Match)?
        {
            break;
        }
        let range = matcher.range();
        if range.start == range.end && lastmatch == Some(range.end) {
            if pos == s_len {
                break;
            }
            crate::limits::checked_string_growth(result.len(), pos + 1 - copied)?;
            pos += 1;
            continue;
        }
        if count == 0 {
            result.reserve(s_len);
        }
        append_bytes(&mut result, &subject[copied..range.start])?;
        apply_string_replacement(&mut result, repl, subject, matcher.num_matches(), |i| {
            matcher.capture(i)
        })?;
        count += 1;
        pos = range.end;
        copied = range.end;
        lastmatch = Some(range.end);
        if anchored {
            break;
        }
    }
    Ok(GsubScan {
        result,
        copied,
        count,
    })
}

/// The function-replacement arm of `append_gsub_replacement`, out of line
/// so its temporaries are not live while the replacement function runs.
#[inline(never)]
fn append_function_replacement(
    state: &mut State,
    out: &mut Vec<u8>,
    subject: &StringArg,
    captures: &[LuaCapture],
) -> Result<()> {
    let args = if captures.len() > 1 {
        &captures[1..]
    } else {
        &captures[..1]
    };
    check_captures_finished(state, args.iter().copied(), StringPatternFunction::Gsub)?;
    state.push_value(3)?;
    if captures.len() > 1 {
        // bounded: one step per capture, at most LUA_MAXCAPTURES.
        for cap in &captures[1..] {
            push_capture_value(state, subject, *cap, StringPatternFunction::Gsub)?;
        }
        let argument_count =
            crate::error::results_count(captures.len() - 1).map_err(|kind| state.error(kind))?;
        crate::native_call!(
            "append_function_replacement->call_unguarded:captures",
            state.call_unguarded(ArgCount::Fixed(argument_count), RetCount::Fixed(1))
        )?;
    } else {
        push_capture_value(state, subject, captures[0], StringPatternFunction::Gsub)?;
        crate::native_call!(
            "append_function_replacement->call_unguarded:whole",
            state.call_unguarded(ArgCount::Fixed(1), RetCount::Fixed(1))
        )?;
    }
    let keep_original = gsub_replacement_keeps_original(state, -1);
    if keep_original {
        state.pop(1)?;
        append_capture_bytes(
            state,
            out,
            subject,
            captures[0],
            StringPatternFunction::Gsub,
        )?;
    } else {
        append_gsub_result(state, out, 1)?;
    }
    Ok(())
}

/// Append the replacement value on top of the stack, read in place while it
/// is still rooted there, then pop `pop` slots. A value that is neither a
/// string nor a number raises before the size cap is checked.
fn append_gsub_result(state: &mut State, out: &mut Vec<u8>, pop: isize) -> Result<()> {
    if let Err(value_type) = state.append_gsub_result_top(out)? {
        return Err(state.library_error(LibraryError::InvalidGsubReplacementValue { value_type }));
    }
    state.pop(pop)
}

fn append_gsub_replacement(
    state: &mut State,
    out: &mut Vec<u8>,
    repl_type: &LuaType,
    subject: &StringArg,
    captures: &[LuaCapture],
) -> Result<()> {
    // String and number replacements take `gsub_with_string`; only a table
    // or a function reaches here.
    match repl_type {
        LuaType::Table => {
            let key = if captures.len() > 1 {
                captures[1]
            } else {
                captures[0]
            };
            check_captures_finished(state, [key], StringPatternFunction::Gsub)?;
            state.push_value(3)?;
            push_capture_value(state, subject, key, StringPatternFunction::Gsub)?;
            crate::native_call!(
                "append_gsub_replacement->get_table_unguarded",
                state.get_table_unguarded(-2)
            )?;
            let keep_original = gsub_replacement_keeps_original(state, -1);
            if keep_original {
                state.pop(2)?;
                append_capture_bytes(
                    state,
                    out,
                    subject,
                    captures[0],
                    StringPatternFunction::Gsub,
                )?;
            } else {
                append_gsub_result(state, out, 2)?;
            }
        }
        _ => {
            crate::native_call!(
                "append_gsub_replacement->append_function_replacement",
                append_function_replacement(state, out, subject, captures)
            )?;
        }
    }
    Ok(())
}

// Keep this order stable: it is the installation order, which determines table
// iteration order and snapshot baselines, and the catalog positions. Each entry
// is named by its full catalog id; the `string` table key is the part after
// `string.` (`lua_std::CatalogSegment`).
pub(super) const STRING_FUNCTIONS: &[(&str, crate::RustFunc)] = &[
    // string.sub(s, i [, j])
    ("string.sub", |state| {
        // Argument #1 is validated first, as in the reference.
        check_string_or_number(state, 1, "sub")?;
        let i = arg_integer(state, 2, "sub")?;
        let j = opt_integer(state, 3, "sub")?.unwrap_or(-1);
        if state.typ(1) == LuaType::String {
            // Interned straight from the source, which stays rooted in slot
            // 1 across the intern's collection point. Dropping `i` and `j`
            // first frees the slot the push needs (`i` is required).
            let range = lua_sub_range(state.to_bytes(1)?.len(), i, j);
            charge_call(state, Charge::StringSub)?;
            state.set_top(1)?;
            state.push_bytes_from_stack_range(1, range)?;
            state.replace(1)?;
        } else {
            // A number's spelling is inline: intern the range straight from
            // it, with no copy of its own.
            let text = crate::number_text::number_to_text(state.to_number(1)?);
            let range = lua_sub_range(text.as_bytes().len(), i, j);
            charge_call(state, Charge::StringSub)?;
            state.set_top(0)?;
            state.push_bytes(&text.as_bytes()[range])?;
        }
        Ok(1)
    }),
    // string.find(s, pattern [, init [, plain]])
    ("string.find", |state| {
        let subject = string_or_number_arg(state, 1, "find")?;
        let pattern = string_or_number_arg(state, 2, "find")?;
        let subject_len = string_arg_len(state, &subject)?;
        let pattern_len = string_arg_len(state, &pattern)?;
        charge_call(state, Charge::StringFind)?;
        let num_args = state.get_top();

        let init =
            opt_integer(state, 3, "find")?.map_or(0, |init| lua_start_index(subject_len, init));
        let plain = num_args >= 4 && state.to_boolean(4);

        let pattern_is_plain =
            plain || crate::patterns::is_plain_for_find(state.string_arg_bytes(&pattern)?);

        if pattern_is_plain {
            // An empty needle is found, with no comparison charged, at `init` itself, up to
            // and including the end of the subject, so `start - 1` is its
            // inclusive end.
            let result = if init > subject_len {
                None
            } else {
                find_subslice(state, Charge::StringFind, &subject, init, &pattern)?.map(|pos| {
                    let start = init + pos + 1;
                    (start, start + pattern_len - 1)
                })
            };

            state.set_top(0)?;
            if let Some((start, end)) = result {
                state.push_number(start as f64)?;
                state.push_number(end as f64)?;
                Ok(2)
            } else {
                state.push_nil()?;
                Ok(1)
            }
        } else {
            if init > subject_len {
                state.set_top(0)?;
                state.push_nil()?;
                return Ok(1);
            }
            let mut matcher = pattern_matcher(state, &pattern)?;
            let matched = state
                .with_string_arg(
                    &subject,
                    Charge::StringFind,
                    |subject, mut meter, interrupt| {
                        matcher.matches_bytes_from(subject, init, &mut meter, interrupt)
                    },
                )?
                .map_err(|err| match_error(state, StringPatternFunction::Find, err))?;
            // Captures are copied out of the subject, so a string subject
            // stays in slot 1 below the results until they are pushed.
            state.set_top(subject_floor(&subject))?;
            match matched {
                true => {
                    let range = matcher.range();
                    state.push_number((range.start + 1) as f64)?;
                    state.push_number(range.end as f64)?;

                    let n = matcher.num_matches();
                    check_captures_finished(
                        state,
                        (1..n).map(|i| matcher.capture(i)),
                        StringPatternFunction::Find,
                    )?;
                    if n > 1 {
                        // bounded: one step per capture, at most LUA_MAXCAPTURES.
                        for i in 1..n {
                            push_capture_value(
                                state,
                                &subject,
                                matcher.capture(i),
                                StringPatternFunction::Find,
                            )?;
                        }
                    }
                    // The pattern module asserts at compile time that its
                    // match limit plus the two positions fits a u8.
                    let capture_returns = u8::try_from(n.saturating_sub(1))
                        .expect("Lua pattern captures fit the callback result count");
                    Ok(2 + capture_returns)
                }
                false => {
                    state.push_nil()?;
                    Ok(1)
                }
            }
        }
    }),
    // string.format(formatstring, ...)
    ("string.format", super::string_format::format),
    // string.len(s)
    ("string.len", |state| {
        let len = match state.typ(1) {
            LuaType::String => state.to_bytes(1)?.len(),
            _ => {
                check_string_or_number(state, 1, "len")?;
                crate::number_text::number_to_text(state.to_number(1)?)
                    .as_bytes()
                    .len()
            }
        };
        state.set_top(0)?;
        state.push_number(len as f64)?;
        Ok(1)
    }),
    // string.upper(s)
    ("string.upper", |state| {
        map_ascii_case(
            state,
            "upper",
            Charge::StringUpper,
            u8::is_ascii_lowercase,
            |s| {
                s.make_ascii_uppercase();
            },
        )
    }),
    // string.lower(s)
    ("string.lower", |state| {
        map_ascii_case(
            state,
            "lower",
            Charge::StringLower,
            u8::is_ascii_uppercase,
            |s| {
                s.make_ascii_lowercase();
            },
        )
    }),
    // string.reverse(s)
    ("string.reverse", |state| {
        check_string_or_number(state, 1, "reverse")?;
        let mut s = BytesCopy::of_string_or_number(state, 1)?;
        charge_call(state, Charge::StringReverse)?;
        s.as_mut_slice().reverse();
        state.set_top(0)?;
        s.push(state)?;
        Ok(1)
    }),
    // string.match(s, pattern [, init])
    ("string.match", |state| {
        let s = string_or_number_arg(state, 1, "match")?;
        let pattern = string_or_number_arg(state, 2, "match")?;
        let s_len = string_arg_len(state, &s)?;
        charge_call(state, Charge::StringMatch)?;
        let init = opt_integer(state, 3, "match")?.map_or(0, |init| lua_start_index(s_len, init));

        if init > s_len {
            state.set_top(0)?;
            state.push_nil()?;
            return Ok(1);
        }
        let mut matcher = pattern_matcher(state, &pattern)?;
        let matched = state
            .with_string_arg(&s, Charge::StringMatch, |subject, mut meter, interrupt| {
                matcher.matches_bytes_from(subject, init, &mut meter, interrupt)
            })?
            .map_err(|err| match_error(state, StringPatternFunction::Match, err))?;
        state.set_top(subject_floor(&s))?;
        if matched {
            push_captures(state, &s, &matcher, StringPatternFunction::Match)
        } else {
            state.push_nil()?;
            Ok(1)
        }
    }),
    // string.gmatch(s, pattern [, init])
    ("string.gmatch", |state| {
        let s = string_or_number_arg(state, 1, "gmatch")?;
        let pattern = string_or_number_arg(state, 2, "gmatch")?;
        let s_len = string_arg_len(state, &s)?;
        let pattern_len = string_arg_len(state, &pattern)?;
        charge_call(state, Charge::StringGmatch)?;
        // Lua 5.4's optional `init`; past the end it parks the iterator at
        // `len + 1`, where the first call returns nothing.
        let init = opt_integer(state, 3, "gmatch")?
            .map_or(0, |init| lua_start_index(s_len, init))
            .min(s_len + 1);

        // An empty pattern matches at every position without examining a
        // byte, so its iterator keeps only the subject's length and runs no
        // matcher.
        if pattern_len == 0 {
            state.set_top(0)?;
            state.push_gmatch(GmatchIter::Empty {
                pos: init,
                len: s_len,
            })?;
            return Ok(1);
        }

        // The iterator keeps the subject and pattern values. A string
        // argument is pushed as the value it already is (it is not
        // re-interned, which would hash every byte again); a number argument
        // is interned here, the one pattern function whose state keeps it
        // beyond the call. Both stay rooted on the stack until the iterator,
        // which roots them from then on, is published. The pattern is kept
        // as given: a leading `^` is literal because `push_gmatch` takes the
        // iterator's program from the cache under `AnchorMode::Literal`.
        state.set_top(2)?;
        state.push_string_arg(&s)?;
        state.push_string_arg(&pattern)?;
        let subject = state
            .string_ptr_at(3)?
            .expect("push_string_arg pushes a string");
        let pattern = state
            .string_ptr_at(4)?
            .expect("push_string_arg pushes a string");
        state.push_gmatch(GmatchIter::General {
            subject,
            pattern,
            pos: init,
            resumed: false,
        })?;
        Ok(1)
    }),
    // string.gsub(s, pattern, repl [, n])
    ("string.gsub", |state| {
        let s = string_or_number_arg(state, 1, "gsub")?;
        let pattern = string_or_number_arg(state, 2, "gsub")?;
        let pattern_len = string_arg_len(state, &pattern)?;
        let repl_type = state.typ(3);

        if !matches!(
            repl_type,
            LuaType::String | LuaType::Number | LuaType::Function | LuaType::Table
        ) {
            return Err(super::argument_error(
                state,
                3,
                "gsub",
                crate::error::ArgDetail::StringFunctionOrTableExpected,
            ));
        }
        // A negative count replaces nothing; one past `usize::MAX` behaves
        // exactly like `usize::MAX`.
        let max_replacements = opt_integer(state, 4, "gsub")?.map(saturating_cast::<usize, i64>);
        charge_call(state, Charge::StringGsub)?;
        if max_replacements == Some(0) {
            // The subject is the result: a string is returned as the value
            // it already is, a number's rendering interned.
            state.set_top(subject_floor(&s))?;
            state.push_string_arg(&s)?;
            state.push_number(0.0)?;
            return Ok(2);
        }

        // A string or number replacement never runs Lua, so its whole scan
        // runs in one borrow of the arguments (`gsub_with_string`), out of
        // line so none of it is in this frame while a callback path below
        // re-enters Lua.
        if matches!(repl_type, LuaType::String | LuaType::Number) {
            return gsub_with_string(state, &s, &pattern, max_replacements);
        }

        // Each callback path is its own out-of-line function so that only
        // the path in use is live on the native stack while a replacement
        // function runs (docs/limits.md, "Native stack").
        //
        // The empty pattern keeps its own path: the plain path would never
        // advance, and this one needs no matcher, examines no subject byte
        // and so charges nothing beyond the call, polling once per position.
        if pattern_len == 0 {
            crate::native_call!(
                "lib:string.gsub->gsub_empty_pattern",
                gsub_empty_pattern(state, &s, &repl_type, max_replacements)
            )
        } else if crate::patterns::is_plain(state.string_arg_bytes(&pattern)?) {
            crate::native_call!(
                "lib:string.gsub->gsub_plain_pattern",
                gsub_plain_pattern(state, &s, &pattern, &repl_type, max_replacements)
            )
        } else {
            crate::native_call!(
                "lib:string.gsub->gsub_lua_pattern",
                gsub_lua_pattern(state, &s, &pattern, &repl_type, max_replacements)
            )
        }
    }),
];

#[inline(never)]
fn gsub_empty_pattern(
    state: &mut State,
    s: &StringArg,
    repl_type: &LuaType,
    max_replacements: Option<usize>,
) -> Result<u8> {
    let s_len = string_arg_len(state, s)?;
    let mut result = Vec::with_capacity(s_len);
    let mut count = 0usize;
    for i in 0..=s_len {
        // A safepoint per position: no matcher runs here, and a long
        // template of empty captures applies once per subject byte.
        state.poll_interrupt()?;
        if max_replacements.is_none_or(|max| count < max) {
            let captures = [LuaCapture::Bytes { start: i, end: i }];
            crate::native_call!(
                "gsub_empty_pattern->append_gsub_replacement",
                append_gsub_replacement(state, &mut result, repl_type, s, &captures)
            )?;
            count += 1;
        }
        if i < s_len {
            append_subject_range(state, &mut result, s, i..i + 1)?;
        }
    }

    state.set_top(0)?;
    state.push_owned_bytes(result)?;
    state.push_number(count as f64)?;
    Ok(2)
}

#[inline(never)]
fn gsub_plain_pattern(
    state: &mut State,
    s: &StringArg,
    pattern: &StringArg,
    repl_type: &LuaType,
    max_replacements: Option<usize>,
) -> Result<u8> {
    let s_len = string_arg_len(state, s)?;
    let pattern_len = string_arg_len(state, pattern)?;
    // Sized at the first match: a call with no match returns the subject.
    let mut result = Vec::new();
    let mut pos = 0usize;
    let mut count = 0usize;

    // safepoint: every iteration searches with `find_subslice`, which polls
    // once per candidate, and a match advances past a non-empty pattern.
    while pos < s_len {
        if max_replacements.is_some_and(|max| count >= max) {
            break;
        }

        let Some(match_start) = find_subslice(state, Charge::StringGsub, s, pos, pattern)? else {
            break;
        };

        let start = pos + match_start;
        let end = start + pattern_len;
        if count == 0 {
            result.reserve(s_len);
        }
        append_subject_range(state, &mut result, s, pos..start)?;

        let captures = [LuaCapture::Bytes { start, end }];
        crate::native_call!(
            "gsub_plain_pattern->append_gsub_replacement",
            append_gsub_replacement(state, &mut result, repl_type, s, &captures)
        )?;

        pos = end;
        count += 1;
    }

    finish_gsub(state, s, result, pos, count)
}

/// Push `gsub`'s two results: the output (`result` plus the subject from
/// `copied` on), and the replacement count. With no replacement the output
/// is the subject itself, returned as the value it already is (a number's
/// rendering interned), so the call copies, interns and collects nothing.
fn finish_gsub(
    state: &mut State,
    s: &StringArg,
    mut result: Vec<u8>,
    copied: usize,
    count: usize,
) -> Result<u8> {
    if count == 0 {
        state.set_top(subject_floor(s))?;
        state.push_string_arg(s)?;
    } else {
        let s_len = string_arg_len(state, s)?;
        append_subject_range(state, &mut result, s, copied..s_len)?;
        state.set_top(0)?;
        state.push_owned_bytes(result)?;
    }
    state.push_number(count as f64)?;
    Ok(2)
}

#[inline(never)]
fn gsub_lua_pattern(
    state: &mut State,
    s: &StringArg,
    pattern: &StringArg,
    repl_type: &LuaType,
    max_replacements: Option<usize>,
) -> Result<u8> {
    let mut matcher = pattern_matcher(state, pattern)?;
    let s_len = string_arg_len(state, s)?;
    // Sized at the first match: a call with no match returns the subject.
    let mut result = Vec::new();
    let mut captures = Vec::new();
    let mut pos = 0usize;
    // The subject before `copied` is in `result`; the bytes from `copied` to
    // `pos` are pending, appended at the next match or at the end.
    let mut copied = 0usize;
    let mut count = 0usize;
    let anchored = matcher.is_anchored();
    // Lua 5.4 empty-match semantics (`lastmatch`): a match is rejected
    // when it is empty and ends where the previous accepted match ended,
    // and the scan then copies one byte and moves on. So
    // `("abc"):gsub("%a*", "-")` is `"-", 1`, not 5.2's `"--", 2`. A
    // rejected match always starts at `pos` (any match found further on
    // ends past the previous one), so the search-based matcher only has to
    // check the match it found.
    let mut lastmatch = None;

    // safepoint: every iteration runs the matcher, whose first step polls
    // (`MatchState::charge`).
    loop {
        if max_replacements.is_some_and(|max| count >= max) {
            break;
        }

        let matched = state
            .with_string_arg(s, Charge::StringGsub, |subject, mut meter, interrupt| {
                matcher.matches_bytes_from(subject, pos, &mut meter, interrupt)
            })?
            .map_err(|err| match_error(state, StringPatternFunction::Gsub, err))?;
        if !matched {
            break;
        }

        let range = matcher.range();
        if range.start == range.end && lastmatch == Some(range.end) {
            if pos == s_len {
                break;
            }
            // The skipped byte stays pending, but the size cap is checked
            // here as if it were appended, so a refusal still comes before
            // the next matcher run.
            crate::limits::checked_string_growth(result.len(), pos + 1 - copied)?;
            pos += 1;
            continue;
        }
        if count == 0 {
            result.reserve(s_len);
        }
        append_subject_range(state, &mut result, s, copied..range.start)?;
        // The capture buffer is reused across matches. Both replacement arms
        // take the same slice, and a function replacement validates all its
        // arguments before pushing the call window. Reading captures off the
        // matcher instead was weighed and not taken: the copy is a few Copy
        // values beside a capture push (an intern for a byte capture) and a
        // table read or Lua call, and a second capture source would have to
        // reach the frames live across the replacement's native re-entry.
        captures.clear();
        captures.extend((0..matcher.num_matches()).map(|i| matcher.capture(i)));
        crate::native_call!(
            "gsub_lua_pattern->append_gsub_replacement",
            append_gsub_replacement(state, &mut result, repl_type, s, &captures)
        )?;
        count += 1;
        pos = range.end;
        copied = range.end;
        lastmatch = Some(range.end);
        if anchored {
            break;
        }
    }

    finish_gsub(state, s, result, copied, count)
}

/// The matcher for argument 2 of `find`, `match` or `gsub`.
///
/// A string pattern comes from the State's compiled-pattern cache; a number
/// coerced to `pattern` has no interned key and compiles fresh. Compilation
/// is free in the cost model either way, and a cached program raises its lazy
/// malformed-pattern errors at exactly the points a fresh one would.
fn pattern_matcher(state: &mut State, pattern: &StringArg) -> Result<LuaPattern> {
    match pattern {
        StringArg::Slot(idx) => state.cached_pattern_matcher(*idx),
        StringArg::Rendered(bytes) => Ok(LuaPattern::new(bytes)),
    }
}

pub(crate) fn open_string(state: &mut State) -> Result<()> {
    // Create the string table
    state.new_table_with_capacity(STRING_LIBRARY_CAPACITY)?;

    super::install_library_functions(state, super::STRING_SEGMENT)?;

    // Strings index through a shared metatable whose `__index` is this table,
    // captured here: rebinding or hiding the global `string` later does not
    // change what `s:upper()` resolves to, as in reference Lua.
    state.install_string_metatable()?;

    // Set the string table as a global
    state.pop_to_global("string")
}

/// The step of every `string.gmatch` iterator (`GmatchIter`): the call of an
/// iterator enters it with the iterator itself in slot 1 and the call's own
/// arguments, which it ignores, above. Anything else in slot 1 (a host
/// calling the step's registered id directly) is refused before the stack
/// changes.
///
/// The record is trusted: only `string.gmatch` creates one, and snapshot load
/// checks a saved one against the bounds `GmatchIter` documents. A step
/// stages every result first and commits the advanced record last, so a
/// refused push or an interrupted or refused match leaves the iterator where
/// it was and a later call retries the same match. A refusal after the step
/// returns is not the step's: a host call with a fixed result count that
/// fails the stack cap while padding the results (`finish_call`) leaves the
/// committed advance, as `State::call` documents for every callee's writes,
/// so a retry sees the next match.
pub(super) fn gmatch_step(state: &mut State) -> Result<u8> {
    let Some((iterator, record)) = state.gmatch_at(1) else {
        return Err(state.library_error(LibraryError::NotAGmatchIterator));
    };
    let (subject, pattern, pos, resumed) = match record {
        GmatchIter::Empty { pos, len } => {
            if pos > len {
                // Exhausted: no results, as reference `gmatch_aux` returns.
                state.set_top(0)?;
                return Ok(0);
            }
            // The ignored call arguments go first, so they cannot crowd the
            // result off the stack.
            state.set_top(1)?;
            state.push_bytes(b"")?;
            state.set_gmatch(iterator, GmatchIter::Empty { pos: pos + 1, len });
            return Ok(1);
        }
        GmatchIter::General {
            subject,
            pattern,
            pos,
            resumed,
        } => (subject, pattern, pos, resumed),
    };
    if pos > state.string_ptr_len(subject) {
        // Exhausted: no results, as reference `gmatch_aux` returns.
        state.set_top(0)?;
        return Ok(0);
    }
    // End of the previous accepted match, for Lua 5.4's empty-match rule:
    // every accepted match leaves the iterator at its end, and a fresh one
    // has none.
    let lastmatch = resumed.then_some(pos);
    // The subject goes into slot 2 for the search and the capture pushes,
    // which read it from the stack. The iterator holds its compiled program,
    // so nothing reads the pattern in slot 3; it is pushed anyway so the
    // step's stack footprint, and with it where a call at the stack cap is
    // refused, stays as it was when the matcher looked the program up there.
    state.set_top(1)?;
    state.push_string_ptr(subject)?;
    state.push_string_ptr(pattern)?;

    let (matched, matcher) =
        state.with_gmatch_program(iterator, 2, |subject, matcher, mut meter, interrupt| {
            // Lua 5.4 semantics: an empty match ending where the previous accepted
            // match ended is rejected and the scan resumes one byte later, so
            // `gmatch("abc", "%a*")` yields only "abc", not 5.2's "abc" then "".
            // Only a match starting at `pos` can be rejected, so this retries at
            // most once.
            let mut search = pos;
            // bounded: at most two matcher runs (one retry), each of which
            // polls per step.
            loop {
                match matcher.matches_bytes_from(subject, search, &mut meter, interrupt) {
                    Ok(true) => {
                        let found = matcher.range();
                        if found.start == found.end && lastmatch == Some(found.end) {
                            if search == subject.len() {
                                break Ok(false);
                            }
                            search += 1;
                            continue;
                        }
                        break Ok(true);
                    }
                    other => break other,
                }
            }
        })?;
    let matched = matched.map_err(|err| match_error(state, StringPatternFunction::Gmatch, err))?;
    if matched {
        // Captures are read off the matcher in place: the returned ones are
        // 1.. when the pattern has captures, else the whole match.
        let range = matcher.range();
        let n = matcher.num_matches();
        let returned = if n > 1 { 1..n } else { 0..1 };
        check_captures_finished(
            state,
            returned.clone().map(|i| matcher.capture(i)),
            StringPatternFunction::Gmatch,
        )?;
        let num_returns =
            crate::error::results_count(returned.len()).map_err(|kind| state.error(kind))?;
        // bounded: one step per capture, at most LUA_MAXCAPTURES.
        for i in returned {
            push_gmatch_capture(state, matcher.capture(i))?;
        }

        // Every result is staged: the commit cannot fail, and the callback
        // ABI keeps only the top `num_returns` values.
        state.set_gmatch(
            iterator,
            GmatchIter::General {
                subject,
                pattern,
                pos: range.end,
                resumed: true,
            },
        );
        Ok(num_returns)
    } else {
        // No match left: no results, like the exhausted exit above.
        state.set_top(0)?;
        Ok(0)
    }
}

fn push_gmatch_capture(state: &mut State, capture: LuaCapture) -> Result<()> {
    match capture {
        LuaCapture::Bytes { start, end } => state.push_bytes_from_stack_range(2, start..end),
        LuaCapture::Position(offset) => state.push_number((offset + 1) as f64),
        LuaCapture::Unfinished => Err(unfinished_capture_error(
            state,
            StringPatternFunction::Gmatch,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The byte-at-a-time nested scan `find_subslice` replaced. The skip-scan
    /// claims charge-for-charge equivalence with this loop, budget-exhaustion
    /// boundary included; this twin keeps that claim checked.
    fn naive_find(state: &mut State, haystack: &[u8], needle: &[u8]) -> Result<Option<usize>> {
        if needle.is_empty() {
            return Ok(Some(0));
        }
        if needle.len() > haystack.len() {
            return Ok(None);
        }
        for start in 0..=haystack.len() - needle.len() {
            for (offset, needle_byte) in needle.iter().enumerate() {
                state.charge(Charge::StringFind, 1)?;
                if haystack[start + offset] != *needle_byte {
                    break;
                }
                if offset + 1 == needle.len() {
                    return Ok(Some(start));
                }
            }
        }
        Ok(None)
    }

    /// `find_subslice` over owned arguments, which read as a stack slot does.
    fn fast_find(state: &mut State, haystack: &[u8], needle: &[u8]) -> Result<Option<usize>> {
        let haystack = StringArg::Rendered(haystack.to_vec());
        let needle = StringArg::Rendered(needle.to_vec());
        find_subslice(state, Charge::StringFind, &haystack, 0, &needle)
    }

    #[test]
    fn find_subslice_matches_the_naive_scan_at_every_budget() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"xxxxab", b"ab"),
            (b"aaaaaa", b"aab"),
            (b"abababab", b"abb"),
            (b"hello world", b"world"),
            (b"hello world", b"worlds"),
            (b"aaa", b"aaaa"),
            (b"", b""),
            (b"abc", b""),
            (b"mississippi", b"issip"),
            (b"aaaaab", b"aaab"),
            (b"ab", b"ab"),
            (b"ba", b"ab"),
        ];
        for (haystack, needle) in cases {
            // Unbudgeted runs must agree on result and total charge.
            let mut fast_state = State::new();
            let mut slow_state = State::new();
            let fast = fast_find(&mut fast_state, haystack, needle).unwrap();
            let slow = naive_find(&mut slow_state, haystack, needle).unwrap();
            assert_eq!(fast, slow, "result for {haystack:?}/{needle:?}");
            assert_eq!(
                fast_state.cost_used(),
                slow_state.cost_used(),
                "cost for {haystack:?}/{needle:?}"
            );
            let total = fast_state.cost_used();

            // Every budget up to just past the total must succeed or fail at
            // the same point with identical counters.
            for budget in 0..=total + 1 {
                let mut fast_state = State::new();
                fast_state
                    .set_cost_budget(i64::try_from(budget).expect("test scan budgets fit in i64"))
                    .expect("the State is metered");
                let mut slow_state = State::new();
                slow_state
                    .set_cost_budget(i64::try_from(budget).expect("test scan budgets fit in i64"))
                    .expect("the State is metered");
                let fast = fast_find(&mut fast_state, haystack, needle);
                let slow = naive_find(&mut slow_state, haystack, needle);
                match (&fast, &slow) {
                    (Ok(a), Ok(b)) => assert_eq!(a, b, "{haystack:?}/{needle:?} @{budget}"),
                    (Err(fast_error), Err(slow_error)) => {
                        assert!(
                            matches!(fast_error.kind(), ErrorKind::BudgetExceeded { .. }),
                            "fast scan returned a non-budget error: {fast_error}"
                        );
                        assert!(
                            matches!(slow_error.kind(), ErrorKind::BudgetExceeded { .. }),
                            "naive scan returned a non-budget error: {slow_error}"
                        );
                    }
                    _ => panic!(
                        "budget outcome diverged for {haystack:?}/{needle:?} @{budget}: \
                         fast={fast:?} slow={slow:?}"
                    ),
                }
                assert_eq!(
                    fast_state.cost_used(),
                    slow_state.cost_used(),
                    "cost_used for {haystack:?}/{needle:?} @{budget}"
                );
                assert_eq!(
                    fast_state.cost_remaining(),
                    slow_state.cost_remaining(),
                    "cost_remaining for {haystack:?}/{needle:?} @{budget}"
                );
            }
        }
    }
}
