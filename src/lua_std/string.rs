//! Lua's String Library

use std::sync::{Arc, OnceLock};

use super::{STRING_LIBRARY_CAPACITY, arg_integer, check_any_named, check_type_named, opt_integer};
use crate::compiler;
use crate::error::{
    Error, ErrorKind, GmatchStateFault, InternalFault, LibraryError, PatternErrorDetail,
    StringPatternFunction,
};
use crate::instr::{ArgCount, RetCount};
use crate::patterns::{LuaCapture, LuaPattern, MatchError, PatternError};
use crate::{LuaType, Result, State};

use crate::gmatch_schema;

const GMATCH_WRAPPER_SRC: &str = r#"local iter, state = ...
return function()
    return iter(state)
end
"#;

// This chunk is immutable. Runtime caches are State-local, and snapshot bytecode
// ids are assigned per save, so sharing the Arc does not share mutable state.
// Its source name keeps errors raised in the wrapper identifiable in traces.
// It is process-global and compiled on the first gmatch in the process,
// whichever State that is (a RustFunc cannot carry upvalues, hence the shim).
// Compilation is deterministic and charges no cost, so which State pays for it
// is not observable; the only effect is one-time host work.
static GMATCH_WRAPPER: OnceLock<Arc<crate::compiler::Bytecode>> = OnceLock::new();

fn gmatch_wrapper() -> Arc<crate::compiler::Bytecode> {
    Arc::clone(GMATCH_WRAPPER.get_or_init(|| {
        Arc::new(
            compiler::parse_str_named(GMATCH_WRAPPER_SRC, Some("string.gmatch".to_owned()))
                .expect("the static gmatch wrapper source must compile"),
        )
    }))
}

// Native costs in the stdlib are deliberately per-function literals, not a
// declarative rule table. The numbers are part of the cost model: changing any
// of them (including the pattern bytes that find/match/gsub charge twice, or
// the free `string.len`, `print`, `tostring` and `select`) changes `cost_used` for
// existing scripts and replays, so it needs a COST_MODEL_VERSION bump and
// invalidates saves. A table would only relocate the literals and `analyze_cost`
// still could not see them, because native calls are opaque to the static
// walk. The rules are small and local; revisit them only together with an
// intentional cost-model change.
fn charge_bytes(state: &mut State, bytes: usize) -> Result<()> {
    state.consume_cost(bytes.max(1) as u64)
}

/// Charge `units` scan comparisons with the exact semantics of charging
/// them one at a time (see `CostMeter::consume_units`).
fn charge_scan_units(state: &mut State, units: usize) -> Result<()> {
    if state.cost_meter().consume_units(units as u64) {
        Ok(())
    } else {
        Err(state.budget_exceeded_error())
    }
}

/// Plain substring search: a first-byte skip loop over candidate windows.
///
/// The cost charged is byte-for-byte identical to the naive nested scan this
/// replaced (1 per start position whose first byte mismatches, common prefix
/// plus the mismatching comparison per candidate, `needle.len()` for the
/// match), including where in the scan a finite budget runs out - the
/// algorithm change is invisible through `cost_used`, only wall time moves.
fn find_subslice(state: &mut State, haystack: &[u8], needle: &[u8]) -> Result<Option<usize>> {
    if needle.is_empty() {
        return Ok(Some(0));
    }
    if needle.len() > haystack.len() {
        return Ok(None);
    }
    let first = needle[0];
    let last_start = haystack.len() - needle.len();
    let mut start = 0usize;
    while start <= last_start {
        let Some(offset) = haystack[start..=last_start]
            .iter()
            .position(|&b| b == first)
        else {
            charge_scan_units(state, last_start - start + 1)?;
            return Ok(None);
        };
        charge_scan_units(state, offset)?;
        let cand = start + offset;
        let window = &haystack[cand..cand + needle.len()];
        let prefix = window
            .iter()
            .zip(needle)
            .take_while(|(hay, need)| hay == need)
            .count();
        if prefix == needle.len() {
            charge_scan_units(state, needle.len())?;
            return Ok(Some(cand));
        }
        charge_scan_units(state, prefix + 1)?;
        start = cand + 1;
    }
    Ok(None)
}

fn match_error(state: &State, function: StringPatternFunction, error: MatchError) -> Error {
    match error {
        MatchError::Pattern(error) => pattern_runtime_error(state, function, error),
        MatchError::BudgetExceeded => state.budget_exceeded_error(),
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

/// Convert a nonnegative Lua integer to `usize`, saturating where `usize` is
/// narrower than `i64`, and mapping negatives to 0. For every caller (a
/// string index, a replacement count) a value past `usize::MAX` behaves
/// exactly like `usize::MAX`.
#[allow(clippy::disallowed_methods)]
fn saturating_usize(n: i64) -> usize {
    usize::try_from(n.max(0)).unwrap_or(usize::MAX)
}

fn lua_start_index(len: usize, idx: i64) -> usize {
    let len = i64::try_from(len).expect("string length fits in i64");
    if idx >= 0 {
        saturating_usize(idx - 1)
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

fn lua_sub_bytes(subject: &[u8], start_idx: i64, end_idx: i64) -> Vec<u8> {
    let start = lua_start_index(subject.len(), start_idx);
    let end = lua_end_index(subject.len(), end_idx);
    if start >= end || start >= subject.len() {
        Vec::new()
    } else {
        subject[start..end].to_vec()
    }
}

/// Lua's string-library arguments accept strings and numbers, but no other
/// values. Keep the type gate separate from
/// `bytes_with_default_string_coercion`, whose general conversion rules are
/// intentionally broader.
///
/// The returned `Vec` is a copy of the subject even when the argument is
/// already a string. That is deliberate: the pattern functions keep reading the
/// subject while they charge cost, push captures and call `gsub` replacement
/// functions, all of which need `&mut State`, so a borrow of the heap string
/// cannot live across them. Removing the copy needs shared ownership of string
/// bytes in the heap (an `Rc<[u8]>`-style handle) or unsafe aliasing of a
/// stack-rooted string; neither is worth it for a copy that is already charged
/// per byte and bounded by the string-size cap. `string.sub` shows the
/// borrow-only fast path where nothing mutates the state mid-read.
fn string_or_number_bytes(state: &mut State, idx: isize, function_name: &str) -> Result<Vec<u8>> {
    check_string_or_number(state, idx, function_name)?;
    state.bytes_with_default_string_coercion(idx)
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
fn unfinished_capture_error(state: &State, function: StringPatternFunction) -> Error {
    pattern_runtime_error(state, function, PatternError::UnfinishedCapture)
}

fn push_capture_value(
    state: &mut State,
    bytes: &[u8],
    capture: LuaCapture,
    function: StringPatternFunction,
) -> Result<()> {
    match capture {
        LuaCapture::Bytes { start, end } => state.push_bytes(&bytes[start..end])?,
        LuaCapture::Position(offset) => state.push_number((offset + 1) as f64)?,
        LuaCapture::Unfinished => return Err(unfinished_capture_error(state, function)),
    }
    Ok(())
}

fn push_captures(
    state: &mut State,
    bytes: &[u8],
    pattern: &LuaPattern,
    function: StringPatternFunction,
) -> Result<u8> {
    let n = pattern.num_matches();
    if n > 1 {
        preflight_capture_bytes(state, (1..n).map(|i| pattern.capture(i)), function)?;
    } else {
        preflight_capture_bytes(state, [pattern.capture(0)], function)?;
    }
    if n > 1 {
        for i in 1..n {
            push_capture_value(state, bytes, pattern.capture(i), function)?;
        }
        crate::error::results_count(n - 1).map_err(|kind| state.error(kind))
    } else {
        push_capture_value(state, bytes, pattern.capture(0), function)?;
        Ok(1)
    }
}

fn capture_byte_len(capture: LuaCapture) -> usize {
    match capture {
        LuaCapture::Bytes { start, end } => end - start,
        LuaCapture::Position(_) | LuaCapture::Unfinished => 0,
    }
}

fn preflight_capture_bytes(
    state: &mut State,
    captures: impl IntoIterator<Item = LuaCapture>,
    function: StringPatternFunction,
) -> Result<()> {
    // Every caller pushes exactly the captures it preflights, so reject an
    // unclosed one before copying any capture bytes to the stack.
    let mut total = 0usize;
    for capture in captures {
        if capture == LuaCapture::Unfinished {
            return Err(unfinished_capture_error(state, function));
        }
        total = total.saturating_add(capture_byte_len(capture));
    }
    state.consume_cost(total as u64)
}

fn append_capture_bytes(
    state: &mut State,
    out: &mut Vec<u8>,
    bytes: &[u8],
    capture: LuaCapture,
    function: StringPatternFunction,
) -> Result<()> {
    match capture {
        LuaCapture::Bytes { start, end } => append_bytes(state, out, &bytes[start..end])?,
        LuaCapture::Position(offset) => {
            let position = (offset + 1).to_string();
            append_bytes(state, out, position.as_bytes())?;
        }
        LuaCapture::Unfinished => return Err(unfinished_capture_error(state, function)),
    }
    Ok(())
}

fn append_bytes(state: &mut State, out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let next = crate::limits::checked_string_growth(out.len(), bytes.len())?;
    state.consume_cost(bytes.len() as u64)?;
    out.reserve(next - out.len());
    out.extend_from_slice(bytes);
    Ok(())
}

fn invalid_gsub_capture_error(state: &State, index: usize) -> Error {
    // Replacement-template errors stay runtime errors: PatternError describes
    // errors in the pattern, while this index refers to a replacement template.
    // Keep the index in typed error context without changing its rendered text.
    state.library_error(LibraryError::InvalidGsubCaptureIndex { index })
}

fn gsub_replacement_keeps_original(state: &State, index: isize) -> bool {
    state.typ(index) == LuaType::Nil
        || (state.typ(index) == LuaType::Boolean && !state.to_boolean(index))
}

fn invalid_gsub_escape_error(state: &State) -> Error {
    state.library_error(LibraryError::InvalidGsubEscape)
}

/// A string replacement parsed once: literal runs (with `%%` unescaped) and
/// `%N` capture references. Compiled on the first match. Invalid escapes
/// remain failure segments, so a malformed template errors only when
/// replacement reaches that position.
enum ReplSegment {
    Literal(Vec<u8>),
    Capture(usize),
    InvalidEscape,
}

struct ReplTemplate {
    segments: Vec<ReplSegment>,
    /// Original template length; each application charges it up front.
    repl_len: usize,
}

fn compile_replacement_template(repl: &[u8]) -> ReplTemplate {
    let mut segments = Vec::new();
    let mut literal = Vec::new();
    let mut i = 0usize;
    while i < repl.len() {
        if repl[i] == b'%' {
            match repl.get(i + 1) {
                Some(&b'%') => {
                    literal.push(b'%');
                    i += 2;
                }
                Some(next) if next.is_ascii_digit() => {
                    if !literal.is_empty() {
                        segments.push(ReplSegment::Literal(std::mem::take(&mut literal)));
                    }
                    segments.push(ReplSegment::Capture(usize::from(*next - b'0')));
                    i += 2;
                }
                _ => {
                    if !literal.is_empty() {
                        segments.push(ReplSegment::Literal(std::mem::take(&mut literal)));
                    }
                    segments.push(ReplSegment::InvalidEscape);
                    break;
                }
            }
        } else {
            literal.push(repl[i]);
            i += 1;
        }
    }
    if !literal.is_empty() {
        segments.push(ReplSegment::Literal(literal));
    }
    ReplTemplate {
        segments,
        repl_len: repl.len(),
    }
}

/// Append literal bytes, charging 1 per byte with unit-exact batching. Falls
/// back to the byte-at-a-time appender when the result would cross the string
/// size cap, so cap-boundary charging matches the unbatched path too.
fn append_bytes_units(state: &mut State, out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let Ok(next) = crate::limits::checked_string_growth(out.len(), bytes.len()) else {
        for byte in bytes {
            append_bytes(state, out, std::slice::from_ref(byte))?;
        }
        return Ok(());
    };
    charge_scan_units(state, bytes.len())?;
    out.reserve(next - out.len());
    out.extend_from_slice(bytes);
    Ok(())
}

fn append_templated_replacement(
    state: &mut State,
    out: &mut Vec<u8>,
    template: &ReplTemplate,
    bytes: &[u8],
    captures: &[LuaCapture],
    first_application: bool,
) -> Result<()> {
    if !first_application {
        state.consume_cost(template.repl_len as u64)?;
    }
    for segment in &template.segments {
        match segment {
            ReplSegment::Literal(literal) if !first_application => {
                append_bytes_units(state, out, literal)?;
            }
            ReplSegment::Literal(literal) => {
                for byte in literal {
                    append_bytes(state, out, std::slice::from_ref(byte))?;
                }
            }
            ReplSegment::Capture(idx) => {
                let capture = if *idx == 0 || (captures.len() == 1 && *idx == 1) {
                    captures[0]
                } else {
                    *captures
                        .get(*idx)
                        .ok_or_else(|| invalid_gsub_capture_error(state, *idx))?
                };
                append_capture_bytes(state, out, bytes, capture, StringPatternFunction::Gsub)?;
            }
            ReplSegment::InvalidEscape => return Err(invalid_gsub_escape_error(state)),
        }
    }
    Ok(())
}

/// The function-replacement arm of `append_gsub_replacement`, out of line
/// so its temporaries are not live while the replacement function runs.
#[inline(never)]
fn append_function_replacement(
    state: &mut State,
    out: &mut Vec<u8>,
    bytes: &[u8],
    captures: &[LuaCapture],
) -> Result<()> {
    let args = if captures.len() > 1 {
        &captures[1..]
    } else {
        &captures[..1]
    };
    preflight_capture_bytes(state, args.iter().copied(), StringPatternFunction::Gsub)?;
    state.push_value(3)?;
    if captures.len() > 1 {
        for cap in &captures[1..] {
            push_capture_value(state, bytes, *cap, StringPatternFunction::Gsub)?;
        }
        let argument_count =
            crate::error::results_count(captures.len() - 1).map_err(|kind| state.error(kind))?;
        state.call_unguarded(ArgCount::Fixed(argument_count), RetCount::Fixed(1))?;
    } else {
        push_capture_value(state, bytes, captures[0], StringPatternFunction::Gsub)?;
        state.call_unguarded(ArgCount::Fixed(1), RetCount::Fixed(1))?;
    }
    let keep_original = gsub_replacement_keeps_original(state, -1);
    if keep_original {
        state.pop(1)?;
        append_capture_bytes(state, out, bytes, captures[0], StringPatternFunction::Gsub)?;
    } else {
        let t = state.typ(-1);
        if !matches!(t, LuaType::String | LuaType::Number) {
            return Err(
                state.library_error(LibraryError::InvalidGsubReplacementValue { value_type: t })
            );
        }
        let val = state.bytes_with_default_string_coercion(-1)?;
        state.pop(1)?;
        append_bytes(state, out, &val)?;
    }
    Ok(())
}

fn append_gsub_replacement(
    state: &mut State,
    out: &mut Vec<u8>,
    repl_type: &LuaType,
    bytes: &[u8],
    captures: &[LuaCapture],
    template: &mut Option<ReplTemplate>,
) -> Result<()> {
    match repl_type {
        LuaType::String | LuaType::Number => {
            let first_application = if template.is_none() {
                // Parse once when a match exists, but keep the first result's
                // byte-at-a-time charges and lazy error position.
                let repl = state.bytes_with_default_string_coercion(3)?;
                state.consume_cost(repl.len() as u64)?;
                *template = Some(compile_replacement_template(&repl));
                true
            } else {
                false
            };
            let replacement_template = template.as_ref().ok_or_else(|| {
                Error::internal_fault(InternalFault::GsubReplacementTemplateMissing)
            })?;
            append_templated_replacement(
                state,
                out,
                replacement_template,
                bytes,
                captures,
                first_application,
            )?;
        }
        LuaType::Table => {
            let key = if captures.len() > 1 {
                captures[1]
            } else {
                captures[0]
            };
            preflight_capture_bytes(state, [key], StringPatternFunction::Gsub)?;
            state.push_value(3)?;
            push_capture_value(state, bytes, key, StringPatternFunction::Gsub)?;
            state.get_table_unguarded(-2)?;
            let keep_original = gsub_replacement_keeps_original(state, -1);
            if keep_original {
                state.pop(2)?;
                append_capture_bytes(state, out, bytes, captures[0], StringPatternFunction::Gsub)?;
            } else {
                let t = state.typ(-1);
                if !matches!(t, LuaType::String | LuaType::Number) {
                    return Err(
                        state.library_error(LibraryError::InvalidGsubReplacementValue {
                            value_type: t,
                        }),
                    );
                }
                let val = state.bytes_with_default_string_coercion(-1)?;
                state.pop(2)?;
                append_bytes(state, out, &val)?;
            }
        }
        LuaType::Function => {
            append_function_replacement(state, out, bytes, captures)?;
        }
        _ => append_capture_bytes(state, out, bytes, captures[0], StringPatternFunction::Gsub)?,
    }
    Ok(())
}

// Keep registration order stable: it determines table iteration order and
// snapshot baselines.
pub(super) const STRING_FUNCTIONS: &[(&str, crate::RustFunc)] = &[
    // string.sub(s, i [, j])
    ("sub", |state| {
        // Argument #1 is validated first, as in the reference.
        check_string_or_number(state, 1, "sub")?;
        let i = arg_integer(state, 2, "sub")?;
        let j = opt_integer(state, 3, "sub")?.unwrap_or(-1);
        let bytes = if state.typ(1) == LuaType::String {
            lua_sub_bytes(state.to_bytes(1)?, i, j)
        } else {
            let subject = string_or_number_bytes(state, 1, "sub")?;
            lua_sub_bytes(&subject, i, j)
        };

        charge_bytes(state, bytes.len())?;

        state.set_top(0)?;
        state.push_bytes(bytes)?;
        Ok(1)
    }),
    // string.find(s, pattern [, init [, plain]])
    ("find", |state| {
        let subject = string_or_number_bytes(state, 1, "find")?;
        let pattern = string_or_number_bytes(state, 2, "find")?;
        state.consume_cost((subject.len() + pattern.len()).max(1) as u64)?;
        charge_bytes(state, pattern.len())?;
        let num_args = state.get_top();

        let init =
            opt_integer(state, 3, "find")?.map_or(0, |init| lua_start_index(subject.len(), init));
        let plain = num_args >= 4 && state.to_boolean(4);

        let pattern_is_plain = { plain || crate::patterns::is_plain_for_find(&pattern) };

        if pattern_is_plain {
            // An empty needle is found, uncharged, at `init` itself, up to
            // and including the end of the subject, so `start - 1` is its
            // inclusive end.
            let result = if init > subject.len() {
                None
            } else {
                let search = &subject[init..];
                find_subslice(state, search, &pattern)?.map(|pos| {
                    let start = init + pos + 1;
                    (start, start + pattern.len() - 1)
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
            let s = subject;

            if init > s.len() {
                state.set_top(0)?;
                state.push_nil()?;
                return Ok(1);
            }
            let mut matcher = pattern_matcher(state, &pattern)?;
            state.set_top(0)?;
            let matched = {
                let mut meter = state.cost_meter();
                matcher.matches_bytes_from(&s, init, &mut meter)
            }
            .map_err(|err| match_error(state, StringPatternFunction::Find, err))?;
            match matched {
                true => {
                    let range = matcher.range();
                    state.push_number((range.start + 1) as f64)?;
                    state.push_number(range.end as f64)?;

                    let n = matcher.num_matches();
                    preflight_capture_bytes(
                        state,
                        (1..n).map(|i| matcher.capture(i)),
                        StringPatternFunction::Find,
                    )?;
                    if n > 1 {
                        for i in 1..n {
                            push_capture_value(
                                state,
                                &s,
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
    ("format", super::string_format::format),
    // string.len(s)
    ("len", |state| {
        let len = match state.typ(1) {
            LuaType::String => state.to_bytes(1)?.len(),
            _ => string_or_number_bytes(state, 1, "len")?.len(),
        };
        state.set_top(0)?;
        state.push_number(len as f64)?;
        Ok(1)
    }),
    // string.upper(s)
    ("upper", |state| {
        let mut s = string_or_number_bytes(state, 1, "upper")?;
        charge_bytes(state, s.len())?;
        s.make_ascii_uppercase();
        state.set_top(0)?;
        state.push_bytes(s)?;
        Ok(1)
    }),
    // string.lower(s)
    ("lower", |state| {
        let mut s = string_or_number_bytes(state, 1, "lower")?;
        charge_bytes(state, s.len())?;
        s.make_ascii_lowercase();
        state.set_top(0)?;
        state.push_bytes(s)?;
        Ok(1)
    }),
    // string.reverse(s)
    ("reverse", |state| {
        let mut s = string_or_number_bytes(state, 1, "reverse")?;
        charge_bytes(state, s.len())?;
        s.reverse();
        state.set_top(0)?;
        state.push_bytes(s)?;
        Ok(1)
    }),
    // string.match(s, pattern [, init])
    ("match", |state| {
        let s = string_or_number_bytes(state, 1, "match")?;
        let pattern = string_or_number_bytes(state, 2, "match")?;
        state.consume_cost((s.len() + pattern.len()).max(1) as u64)?;
        charge_bytes(state, pattern.len())?;
        let init = opt_integer(state, 3, "match")?.map_or(0, |init| lua_start_index(s.len(), init));

        if init > s.len() {
            state.set_top(0)?;
            state.push_nil()?;
            return Ok(1);
        }
        let mut matcher = pattern_matcher(state, &pattern)?;
        state.set_top(0)?;
        let matched = {
            let mut meter = state.cost_meter();
            matcher.matches_bytes_from(&s, init, &mut meter)
        }
        .map_err(|err| match_error(state, StringPatternFunction::Match, err))?;
        if matched {
            push_captures(state, &s, &matcher, StringPatternFunction::Match)
        } else {
            state.push_nil()?;
            Ok(1)
        }
    }),
    // string.gmatch(s, pattern [, init])
    ("gmatch", |state| {
        let s = string_or_number_bytes(state, 1, "gmatch")?;
        let pattern = string_or_number_bytes(state, 2, "gmatch")?;
        state.consume_cost((s.len() + pattern.len()).max(1) as u64)?;
        // Lua 5.4's optional `init`; past the end it parks the iterator at
        // `len + 1`, where the first call returns nil.
        let init = opt_integer(state, 3, "gmatch")?
            .map_or(0, |init| lua_start_index(s.len(), init))
            .min(s.len() + 1);
        // The pattern is stored as given: a leading `^` is literal because
        // the iterator compiles it under `AnchorMode::Literal`.
        state.set_top(0)?;

        // The general iterator would charge `max(1, 0)` for compiling an
        // empty pattern on its first call; this one charges nothing, so it
        // stays a separate iterator (with its own persisted schema).
        if pattern.is_empty() {
            state.new_table()?;
            state.push_string(gmatch_schema::POSITION_FIELD)?;
            state.push_number(init as f64)?;
            state.set_table_raw(-3)?;
            state.push_string(gmatch_schema::LENGTH_FIELD)?;
            state.push_number(s.len() as f64)?;
            state.set_table_raw(-3)?;

            return push_gmatch_wrapper(state, gmatch_schema::EMPTY_ITERATOR_ID, gmatch_empty_iter);
        }

        state.new_table()?;
        // Keep the interned flag key as a value under an integer key. The
        // iterator can fetch and reuse that string without interning it on
        // each match.
        state.push_number(gmatch_schema::COMPILE_CHARGED_SLOT)?;
        state.push_string(gmatch_schema::COMPILE_CHARGED_FIELD)?;
        state.set_table_raw(-3)?;
        state.push_string(gmatch_schema::SUBJECT_FIELD)?;
        state.push_bytes(&s)?;
        state.set_table_raw(-3)?;
        state.push_string(gmatch_schema::PATTERN_FIELD)?;
        state.push_bytes(&pattern)?;
        state.set_table_raw(-3)?;
        state.push_string(gmatch_schema::POSITION_FIELD)?;
        state.push_number(init as f64)?;
        state.set_table_raw(-3)?;

        push_gmatch_wrapper(state, gmatch_schema::ITERATOR_ID, gmatch_iter)
    }),
    // string.gsub(s, pattern, repl [, n])
    ("gsub", |state| {
        check_any_named(state, 3, "gsub")?;
        let s = string_or_number_bytes(state, 1, "gsub")?;
        let pattern = string_or_number_bytes(state, 2, "gsub")?;
        state.consume_cost((s.len() + pattern.len()).max(1) as u64)?;
        charge_bytes(state, pattern.len())?;
        let max_replacements = opt_integer(state, 4, "gsub")?.map(saturating_usize);
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
        if max_replacements == Some(0) {
            state.consume_cost(s.len() as u64)?;
            state.set_top(0)?;
            state.push_bytes(s)?;
            state.push_number(0.0)?;
            return Ok(2);
        }

        // Each path is its own out-of-line function so that only the path
        // in use is live on the native stack while a replacement function
        // runs (docs/limits.md, "Native stack").
        //
        // The empty pattern keeps its own path: the plain path would never
        // advance, and the Lua-pattern path gives the same results and
        // unbudgeted total but, once a replacement limit stops it, appends
        // the rest of the subject in one charge where this path charges per
        // byte. A finite budget can run out inside that tail, so the two
        // differ in where exhaustion is reported.
        if pattern.is_empty() {
            gsub_empty_pattern(state, &s, &repl_type, max_replacements)
        } else if crate::patterns::is_plain(&pattern) {
            gsub_plain_pattern(state, &s, &pattern, &repl_type, max_replacements)
        } else {
            gsub_lua_pattern(state, &s, &pattern, &repl_type, max_replacements)
        }
    }),
];

/// The matcher for argument 2 of `find`, `match` or `gsub`.
///
/// A string pattern comes from the State's compiled-pattern cache; a number
/// coerced to `pattern` has no interned key and compiles fresh. Compilation
/// is free in the cost model either way, and a cached program raises its lazy
/// malformed-pattern errors at exactly the points a fresh one would.
#[inline(never)]
fn gsub_empty_pattern(
    state: &mut State,
    s: &[u8],
    repl_type: &LuaType,
    max_replacements: Option<usize>,
) -> Result<u8> {
    let mut template = None;
    let mut result = Vec::with_capacity(s.len());
    let mut count = 0usize;
    for i in 0..=s.len() {
        if max_replacements.is_none_or(|max| count < max) {
            let captures = [LuaCapture::Bytes { start: i, end: i }];
            append_gsub_replacement(state, &mut result, repl_type, s, &captures, &mut template)?;
            count += 1;
        }
        if i < s.len() {
            append_bytes(state, &mut result, &s[i..=i])?;
        }
    }

    state.set_top(0)?;
    state.push_bytes(result)?;
    state.push_number(count as f64)?;
    Ok(2)
}

#[inline(never)]
fn gsub_plain_pattern(
    state: &mut State,
    s: &[u8],
    pattern: &[u8],
    repl_type: &LuaType,
    max_replacements: Option<usize>,
) -> Result<u8> {
    let mut template = None;
    let mut result = Vec::with_capacity(s.len());
    let mut pos = 0usize;
    let mut count = 0usize;

    while pos < s.len() {
        if max_replacements.is_some_and(|max| count >= max) {
            break;
        }

        let search = &s[pos..];
        let Some(match_start) = find_subslice(state, search, pattern)? else {
            break;
        };

        let start = pos + match_start;
        let end = start + pattern.len();
        append_bytes(state, &mut result, &s[pos..start])?;

        let captures = [LuaCapture::Bytes { start, end }];
        append_gsub_replacement(state, &mut result, repl_type, s, &captures, &mut template)?;

        pos = end;
        count += 1;
    }

    append_bytes(state, &mut result, &s[pos..])?;
    state.set_top(0)?;
    state.push_bytes(result)?;
    state.push_number(count as f64)?;
    Ok(2)
}

#[inline(never)]
fn gsub_lua_pattern(
    state: &mut State,
    s: &[u8],
    pattern: &[u8],
    repl_type: &LuaType,
    max_replacements: Option<usize>,
) -> Result<u8> {
    let mut template = None;
    let mut matcher = pattern_matcher(state, pattern)?;
    let mut result = Vec::with_capacity(s.len());
    let mut captures = Vec::new();
    let mut pos = 0usize;
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

    loop {
        if max_replacements.is_some_and(|max| count >= max) {
            break;
        }

        let matched = {
            let mut meter = state.cost_meter();
            matcher.matches_bytes_from(s, pos, &mut meter)
        }
        .map_err(|err| match_error(state, StringPatternFunction::Gsub, err))?;
        if !matched {
            break;
        }

        let range = matcher.range();
        if range.start == range.end && lastmatch == Some(range.end) {
            if pos == s.len() {
                break;
            }
            append_bytes(state, &mut result, &s[pos..=pos])?;
            pos += 1;
            continue;
        }
        append_bytes(state, &mut result, &s[pos..range.start])?;
        captures.clear();
        captures.extend((0..matcher.num_matches()).map(|i| matcher.capture(i)));
        append_gsub_replacement(state, &mut result, repl_type, s, &captures, &mut template)?;
        count += 1;
        pos = range.end;
        lastmatch = Some(range.end);
        if anchored {
            break;
        }
    }

    append_bytes(state, &mut result, &s[pos..])?;

    state.set_top(0)?;
    state.push_bytes(result)?;
    state.push_number(count as f64)?;
    Ok(2)
}

fn pattern_matcher(state: &mut State, pattern: &[u8]) -> Result<LuaPattern> {
    if state.typ(2) == LuaType::String {
        state.cached_pattern_matcher(2)
    } else {
        Ok(LuaPattern::new(pattern))
    }
}

pub(crate) fn open_string(state: &mut State) -> Result<()> {
    // Create the string table
    state.new_table_with_capacity(STRING_LIBRARY_CAPACITY)?;

    for &(name, func) in STRING_FUNCTIONS {
        state.register_lib_fn(-1, "string", name, func)?;
    }

    // Register the gmatch iterator functions up front so a persisted gmatch
    // closure (which captures one of them as an upvalue) can be resolved on
    // snapshot load, not only after gmatch has run once.
    state.register_lib_fn_id(gmatch_schema::ITERATOR_ID, gmatch_iter)?;
    state.register_lib_fn_id(gmatch_schema::EMPTY_ITERATOR_ID, gmatch_empty_iter)?;

    // Strings index through a shared metatable whose `__index` is this table,
    // captured here: rebinding or hiding the global `string` later does not
    // change what `s:upper()` resolves to, as in reference Lua.
    state.install_string_metatable()?;

    // Set the string table as a global
    state.set_global("string")?;
    Ok(())
}

fn push_gmatch_wrapper(
    state: &mut State,
    iter_id: &str,
    iter: fn(&mut State) -> Result<u8>,
) -> Result<u8> {
    state.push_chunk(gmatch_wrapper())?;
    state.push_lib_fn(iter_id, iter)?;
    state.push_value(1)?;
    state.remove(1)?;
    state.call_unguarded(ArgCount::Fixed(2), RetCount::Fixed(1))?;
    Ok(1)
}

fn invalid_gmatch_state(fault: GmatchStateFault) -> Error {
    Error::internal_fault(InternalFault::GmatchState(fault))
}

fn push_gmatch_string_field(state: &mut State, field: &'static str) -> Result<()> {
    state.push_string(field)?;
    state.get_table_raw(1)?;
    if state.typ(-1) != LuaType::String {
        return Err(invalid_gmatch_state(GmatchStateFault::StringFieldInvalid(
            field,
        )));
    }
    Ok(())
}

// Gmatch tables are private closure state. Save verification checks their
// serialized form, and runtime reads repeat the invariant so an absent,
// mistyped, or out-of-range field cannot silently restart an iterator.
fn gmatch_state_index(
    state: &State,
    stack_index: isize,
    field: &'static str,
    maximum: usize,
) -> Result<usize> {
    if state.typ(stack_index) != LuaType::Number {
        return Err(invalid_gmatch_state(GmatchStateFault::NumberFieldInvalid(
            field,
        )));
    }
    let number = state.to_number(stack_index)?;
    crate::vm::integer_index(number, maximum)
        .ok_or_else(|| invalid_gmatch_state(GmatchStateFault::NumberFieldOutOfRange(field)))
}

fn gmatch_empty_iter(state: &mut State) -> Result<u8> {
    check_type_named(state, 1, LuaType::Table, "gmatch")?;

    state.push_string(gmatch_schema::POSITION_FIELD)?;
    state.get_table_raw(1)?;
    let pos = gmatch_state_index(
        state,
        -1,
        gmatch_schema::POSITION_FIELD,
        crate::limits::MAX_STRING_BYTES + 1,
    )?;
    state.pop(1)?;

    state.push_string(gmatch_schema::LENGTH_FIELD)?;
    state.get_table_raw(1)?;
    let len = gmatch_state_index(
        state,
        -1,
        gmatch_schema::LENGTH_FIELD,
        crate::limits::MAX_STRING_BYTES,
    )?;
    state.pop(1)?;

    if pos > len.saturating_add(1) {
        return Err(invalid_gmatch_state(
            GmatchStateFault::PositionExceedsSubjectLength,
        ));
    }
    if pos > len {
        state.set_top(0)?;
        state.push_nil()?;
        return Ok(1);
    }

    state.push_string(gmatch_schema::POSITION_FIELD)?;
    state.push_number((pos + 1) as f64)?;
    state.set_table_raw(1)?;
    state.set_top(0)?;
    state.push_bytes(b"")?;
    Ok(1)
}

fn gmatch_iter(state: &mut State) -> Result<u8> {
    check_type_named(state, 1, LuaType::Table, "gmatch")?;

    push_gmatch_string_field(state, gmatch_schema::SUBJECT_FIELD)?;
    push_gmatch_string_field(state, gmatch_schema::PATTERN_FIELD)?;

    state.push_string(gmatch_schema::POSITION_FIELD)?;
    state.get_table_raw(1)?;
    let pos = gmatch_state_index(
        state,
        -1,
        gmatch_schema::POSITION_FIELD,
        crate::limits::MAX_STRING_BYTES + 1,
    )?;
    state.pop(1)?;

    // End of the previous accepted match, for Lua 5.4's empty-match rule.
    // A fresh iterator has none.
    state.push_string(gmatch_schema::LAST_MATCH_FIELD)?;
    state.get_table_raw(1)?;
    let lastmatch = match state.typ(-1) {
        LuaType::Nil => None,
        LuaType::Number => Some(gmatch_state_index(
            state,
            -1,
            gmatch_schema::LAST_MATCH_FIELD,
            crate::limits::MAX_STRING_BYTES,
        )?),
        _ => {
            return Err(invalid_gmatch_state(
                GmatchStateFault::LastMatchFieldInvalid(gmatch_schema::LAST_MATCH_FIELD),
            ));
        }
    };
    state.pop(1)?;

    let subject_len = state.to_bytes(2)?.len();
    if pos > subject_len.saturating_add(1) {
        return Err(invalid_gmatch_state(
            GmatchStateFault::PositionExceedsSubjectLength,
        ));
    }
    if lastmatch.is_some_and(|last| last > subject_len) {
        return Err(invalid_gmatch_state(
            GmatchStateFault::LastMatchExceedsSubjectLength,
        ));
    }
    if pos > subject_len {
        state.set_top(0)?;
        state.push_nil()?;
        return Ok(1);
    }

    state.push_number(gmatch_schema::COMPILE_CHARGED_SLOT)?;
    state.get_table_raw(1)?;
    let cache_key_matches = if state.typ(-1) == LuaType::String {
        state
            .to_bytes(-1)?
            .iter()
            .copied()
            .eq(gmatch_schema::COMPILE_CHARGED_FIELD
                .as_bytes()
                .iter()
                .copied())
    } else {
        false
    };
    if !cache_key_matches {
        return Err(invalid_gmatch_state(
            GmatchStateFault::CompileFlagKeyMissing,
        ));
    }
    state.get_table_raw(1)?;
    let compile_charged = match state.typ(-1) {
        LuaType::Nil => false,
        LuaType::Boolean if state.to_boolean(-1) => true,
        _ => {
            return Err(invalid_gmatch_state(GmatchStateFault::CompileFlagInvalid));
        }
    };
    state.pop(1)?;
    if !compile_charged {
        // Charge once per iterator, not per cache miss. This flag is part of
        // the saved Lua table, so GC, eviction and snapshot load cannot change
        // the deterministic cost of continuing an existing iterator.
        let pattern_len = state.to_bytes(3)?.len();
        charge_bytes(state, pattern_len)?;
        state.push_number(gmatch_schema::COMPILE_CHARGED_SLOT)?;
        state.get_table_raw(1)?;
        state.push_boolean(true)?;
        state.set_table_raw(1)?;
    }
    let (matched, range, captures) =
        state.with_gmatch_matcher(2, 3, |subject, matcher, mut meter| {
            // Lua 5.4 semantics: an empty match ending where the previous accepted
            // match ended is rejected and the scan resumes one byte later, so
            // `gmatch("abc", "%a*")` yields only "abc", not 5.2's "abc" then "".
            // Only a match starting at `pos` can be rejected, so this retries at
            // most once.
            let mut search = pos;
            let matched = loop {
                match matcher.matches_bytes_from(subject, search, &mut meter) {
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
            };
            let (range, captures) = if matches!(&matched, Ok(true)) {
                (
                    matcher.range(),
                    (0..matcher.num_matches())
                        .map(|i| matcher.capture(i))
                        .collect(),
                )
            } else {
                (0..0, Vec::new())
            };
            (matched, range, captures)
        })?;
    let matched = matched.map_err(|err| match_error(state, StringPatternFunction::Gmatch, err))?;
    if matched {
        let returned = if captures.len() > 1 {
            &captures[1..]
        } else {
            &captures[..1]
        };
        preflight_capture_bytes(
            state,
            returned.iter().copied(),
            StringPatternFunction::Gmatch,
        )?;
        state.push_string(gmatch_schema::POSITION_FIELD)?;
        state.push_number(range.end as f64)?;
        state.set_table_raw(1)?;
        state.push_string(gmatch_schema::LAST_MATCH_FIELD)?;
        state.push_number(range.end as f64)?;
        state.set_table_raw(1)?;

        let num_returns = if captures.len() > 1 {
            for capture in captures.iter().skip(1) {
                push_gmatch_capture(state, *capture)?;
            }
            crate::error::results_count(captures.len() - 1).map_err(|kind| state.error(kind))?
        } else {
            push_gmatch_capture(state, captures[0])?;
            1
        };
        state.remove(3)?;
        state.remove(2)?;
        state.remove(1)?;
        Ok(num_returns)
    } else {
        state.set_top(0)?;
        state.push_nil()?;
        Ok(1)
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
                state.consume_cost(1)?;
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
            let fast = find_subslice(&mut fast_state, haystack, needle).unwrap();
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
                    .set_cost_budget(i64::try_from(budget).expect("test scan budgets fit in i64"));
                let mut slow_state = State::new();
                slow_state
                    .set_cost_budget(i64::try_from(budget).expect("test scan budgets fit in i64"));
                let fast = find_subslice(&mut fast_state, haystack, needle);
                let slow = naive_find(&mut slow_state, haystack, needle);
                match (&fast, &slow) {
                    (Ok(a), Ok(b)) => assert_eq!(a, b, "{haystack:?}/{needle:?} @{budget}"),
                    (Err(_), Err(_)) => {}
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
