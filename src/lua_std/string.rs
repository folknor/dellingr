//! Lua's String Library

use std::sync::{Arc, OnceLock};

use crate::compiler;
use crate::error::{ArgError, Error, ErrorKind};
use crate::instr::{ArgCount, RetCount};
use crate::patterns::{LuaCapture, LuaPattern, MatchError};
use crate::{LuaType, Result, State};

const GMATCH_WRAPPER_SRC: &str = r#"local iter, state = ...
return function()
    return iter(state)
end
"#;

static GMATCH_WRAPPER: OnceLock<Arc<crate::compiler::Bytecode>> = OnceLock::new();

fn gmatch_wrapper() -> Arc<crate::compiler::Bytecode> {
    Arc::clone(GMATCH_WRAPPER.get_or_init(|| {
        Arc::new(
            compiler::parse_str_named(GMATCH_WRAPPER_SRC, None)
                .expect("the static gmatch wrapper source must compile"),
        )
    }))
}

fn is_plain_lua_pattern(pattern: &[u8]) -> bool {
    !pattern.iter().any(|b| {
        matches!(
            b,
            b'^' | b'$' | b'(' | b')' | b'%' | b'.' | b'[' | b']' | b'*' | b'+' | b'-' | b'?'
        )
    })
}

fn charge_cost(state: &mut State, cost: u64) -> Result<()> {
    if state.cost_meter().consume(cost) {
        Ok(())
    } else {
        Err(state.budget_exceeded_error())
    }
}

fn charge_bytes(state: &mut State, bytes: usize) -> Result<()> {
    charge_cost(state, bytes.max(1) as u64)
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

fn match_error(state: &State, error: MatchError) -> Error {
    match error {
        MatchError::Pattern(error) => state.error(ErrorKind::RuntimeError(error.to_string())),
        MatchError::BudgetExceeded => state.budget_exceeded_error(),
    }
}

fn lua_start_index(len: usize, idx: isize) -> usize {
    if idx >= 0 {
        (idx - 1).max(0) as usize
    } else {
        (len as isize + idx).max(0) as usize
    }
}

fn lua_end_index(len: usize, idx: isize) -> usize {
    if idx >= 0 {
        (idx as usize).min(len)
    } else {
        (len as isize + idx + 1).max(0) as usize
    }
}

fn lua_sub_bytes(subject: &[u8], start_idx: isize, end_idx: isize) -> Vec<u8> {
    let start = lua_start_index(subject.len(), start_idx);
    let end = lua_end_index(subject.len(), end_idx);
    if start >= end || start >= subject.len() {
        Vec::new()
    } else {
        subject[start..end].to_vec()
    }
}

/// Lua's string-library arguments accept strings and numbers, but no other
/// values. Keep the type gate separate from `bytes_coerce`, whose general
/// conversion rules are intentionally broader.
fn string_or_number_bytes(state: &mut State, idx: isize) -> Result<Vec<u8>> {
    match state.typ(idx) {
        LuaType::String | LuaType::Number => state.bytes_coerce(idx),
        received => Err(state.error(ErrorKind::ArgError(ArgError {
            arg_number: idx,
            func_name: None,
            expected: Some(LuaType::String),
            received: Some(received),
        }))),
    }
}

fn push_capture_value(state: &mut State, bytes: &[u8], capture: LuaCapture) -> Result<()> {
    match capture {
        LuaCapture::Bytes { start, end } => state.push_bytes(&bytes[start..end])?,
        LuaCapture::Position(offset) => state.push_number((offset + 1) as f64)?,
    }
    Ok(())
}

fn push_captures(state: &mut State, bytes: &[u8], pattern: &LuaPattern) -> Result<u8> {
    let n = pattern.num_matches();
    if n > 1 {
        preflight_capture_bytes(state, (1..n).map(|i| pattern.capture(i)))?;
    } else {
        preflight_capture_bytes(state, [pattern.capture(0)])?;
    }
    if n > 1 {
        for i in 1..n {
            push_capture_value(state, bytes, pattern.capture(i))?;
        }
        Ok((n - 1) as u8)
    } else {
        push_capture_value(state, bytes, pattern.capture(0))?;
        Ok(1)
    }
}

fn capture_byte_len(capture: LuaCapture) -> usize {
    match capture {
        LuaCapture::Bytes { start, end } => end - start,
        LuaCapture::Position(_) => 0,
    }
}

fn preflight_capture_bytes(
    state: &mut State,
    captures: impl IntoIterator<Item = LuaCapture>,
) -> Result<()> {
    let mut total = 0usize;
    for capture in captures {
        total = total.saturating_add(capture_byte_len(capture));
    }
    charge_cost(state, total as u64)
}

fn append_capture_bytes(
    state: &mut State,
    out: &mut Vec<u8>,
    bytes: &[u8],
    capture: LuaCapture,
) -> Result<()> {
    match capture {
        LuaCapture::Bytes { start, end } => append_bytes(state, out, &bytes[start..end])?,
        LuaCapture::Position(offset) => {
            let position = (offset + 1).to_string();
            append_bytes(state, out, position.as_bytes())?;
        }
    }
    Ok(())
}

fn append_bytes(state: &mut State, out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let next = crate::vm::checked_string_growth(out.len(), bytes.len())?;
    charge_cost(state, bytes.len() as u64)?;
    out.reserve(next - out.len());
    out.extend_from_slice(bytes);
    Ok(())
}

fn append_string_replacement(
    state: &mut State,
    out: &mut Vec<u8>,
    repl: &[u8],
    bytes: &[u8],
    captures: &[LuaCapture],
) -> Result<()> {
    charge_cost(state, repl.len() as u64)?;
    let mut i = 0usize;
    while i < repl.len() {
        if repl[i] == b'%' && i + 1 < repl.len() {
            let next = repl[i + 1];
            if next == b'%' {
                append_bytes(state, out, b"%")?;
                i += 2;
            } else if next.is_ascii_digit() {
                let idx = (next - b'0') as usize;
                if idx == 0 {
                    append_capture_bytes(state, out, bytes, captures[0])?;
                } else {
                    let capture = if captures.len() == 1 && idx == 1 {
                        captures[0]
                    } else {
                        *captures.get(idx).ok_or_else(|| {
                            state.error(ErrorKind::RuntimeError("invalid capture index".into()))
                        })?
                    };
                    append_capture_bytes(state, out, bytes, capture)?;
                }
                i += 2;
            } else {
                return Err(state.error(ErrorKind::RuntimeError(
                    "invalid use of '%' in replacement string".into(),
                )));
            }
        } else if repl[i] == b'%' {
            return Err(state.error(ErrorKind::RuntimeError(
                "invalid use of '%' in replacement string".into(),
            )));
        } else {
            append_bytes(state, out, &repl[i..=i])?;
            i += 1;
        }
    }
    Ok(())
}

/// A string replacement parsed once: literal runs (with `%%` unescaped) and
/// `%N` capture references. Compiled lazily after the first successful
/// replacement, so a malformed template still errors (or, with zero matches,
/// never errors) exactly where the per-match parser did.
enum ReplSegment {
    Literal(Vec<u8>),
    Capture(usize),
}

struct ReplTemplate {
    segments: Vec<ReplSegment>,
    /// Original template length; each application charges it up front,
    /// mirroring `append_string_replacement`.
    repl_len: usize,
}

fn compile_replacement_template(repl: &[u8]) -> Option<ReplTemplate> {
    let mut segments = Vec::new();
    let mut literal = Vec::new();
    let mut i = 0usize;
    while i < repl.len() {
        if repl[i] == b'%' {
            let next = *repl.get(i + 1)?;
            if next == b'%' {
                literal.push(b'%');
            } else if next.is_ascii_digit() {
                if !literal.is_empty() {
                    segments.push(ReplSegment::Literal(std::mem::take(&mut literal)));
                }
                segments.push(ReplSegment::Capture((next - b'0') as usize));
            } else {
                return None;
            }
            i += 2;
        } else {
            literal.push(repl[i]);
            i += 1;
        }
    }
    if !literal.is_empty() {
        segments.push(ReplSegment::Literal(literal));
    }
    Some(ReplTemplate {
        segments,
        repl_len: repl.len(),
    })
}

/// Append literal bytes, charging 1 per byte with unit-exact batching. Falls
/// back to the byte-at-a-time appender when the result would cross the string
/// size cap, so cap-boundary charging matches the unbatched path too.
fn append_bytes_units(state: &mut State, out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let Ok(next) = crate::vm::checked_string_growth(out.len(), bytes.len()) else {
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
) -> Result<()> {
    charge_cost(state, template.repl_len as u64)?;
    for segment in &template.segments {
        match segment {
            ReplSegment::Literal(literal) => append_bytes_units(state, out, literal)?,
            ReplSegment::Capture(idx) => {
                let capture = if *idx == 0 || (captures.len() == 1 && *idx == 1) {
                    captures[0]
                } else {
                    *captures.get(*idx).ok_or_else(|| {
                        state.error(ErrorKind::RuntimeError("invalid capture index".into()))
                    })?
                };
                append_capture_bytes(state, out, bytes, capture)?;
            }
        }
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
            if let Some(template) = template {
                append_templated_replacement(state, out, template, bytes, captures)?;
            } else {
                // First replacement: the per-match parser keeps template
                // errors surfacing at the same point (and not at all for
                // zero-match calls). A successful first pass proves the
                // template valid, so later matches use the compiled form.
                let repl = state.bytes_coerce(3)?;
                append_string_replacement(state, out, &repl, bytes, captures)?;
                *template = compile_replacement_template(&repl);
            }
        }
        LuaType::Table => {
            let key = if captures.len() > 1 {
                captures[1]
            } else {
                captures[0]
            };
            preflight_capture_bytes(state, [key])?;
            state.push_value(3)?;
            push_capture_value(state, bytes, key)?;
            state.get_table(-2)?;
            let keep_original = state.typ(-1) == LuaType::Nil
                || (state.typ(-1) == LuaType::Boolean && !state.to_boolean(-1));
            if keep_original {
                state.pop(2)?;
                append_capture_bytes(state, out, bytes, captures[0])?;
            } else {
                let t = state.typ(-1);
                if !matches!(t, LuaType::String | LuaType::Number) {
                    return Err(state.error(ErrorKind::RuntimeError(format!(
                        "invalid replacement value (a {})",
                        t.as_str()
                    ))));
                }
                let val = state.bytes_coerce(-1)?;
                state.pop(2)?;
                append_bytes(state, out, &val)?;
            }
        }
        LuaType::Function => {
            let args = if captures.len() > 1 {
                &captures[1..]
            } else {
                &captures[..1]
            };
            preflight_capture_bytes(state, args.iter().copied())?;
            state.push_value(3)?;
            if captures.len() > 1 {
                for cap in &captures[1..] {
                    push_capture_value(state, bytes, *cap)?;
                }
                state.call(
                    ArgCount::Fixed((captures.len() - 1) as u8),
                    RetCount::Fixed(1),
                )?;
            } else {
                push_capture_value(state, bytes, captures[0])?;
                state.call(ArgCount::Fixed(1), RetCount::Fixed(1))?;
            }
            let keep_original = state.typ(-1) == LuaType::Nil
                || (state.typ(-1) == LuaType::Boolean && !state.to_boolean(-1));
            if keep_original {
                state.pop(1)?;
                append_capture_bytes(state, out, bytes, captures[0])?;
            } else {
                let t = state.typ(-1);
                if !matches!(t, LuaType::String | LuaType::Number) {
                    return Err(state.error(ErrorKind::RuntimeError(format!(
                        "invalid replacement value (a {})",
                        t.as_str()
                    ))));
                }
                let val = state.bytes_coerce(-1)?;
                state.pop(1)?;
                append_bytes(state, out, &val)?;
            }
        }
        _ => append_capture_bytes(state, out, bytes, captures[0])?,
    }
    Ok(())
}

pub(crate) fn open_string(state: &mut State) -> Result<()> {
    // Create the string table
    state.new_table_with_capacity(10)?;

    // Helper to add a function to the table at stack index -1.
    macro_rules! add_fn {
        ($name:expr, $func:expr) => {
            #[cfg(feature = "snapshot")]
            state
                .set_table_str_key_named_rust_fn(-1, $name, concat!("string.", $name), $func)
                .expect("string library registration cannot fail");
            #[cfg(not(feature = "snapshot"))]
            state
                .set_table_str_key_rust_fn(-1, $name, $func)
                .expect("string library registration cannot fail");
        };
    }

    // string.sub(s, i [, j])
    add_fn!("sub", |state| {
        state.check_type(2, LuaType::Number)?;
        let i = state.to_number(2)? as isize;
        let j = if state.check_optional_type(3, LuaType::Number)? {
            state.to_number(3)? as isize
        } else {
            -1
        };
        let bytes = if state.typ(1) == LuaType::String {
            lua_sub_bytes(state.to_bytes(1)?, i, j)
        } else {
            let subject = string_or_number_bytes(state, 1)?;
            lua_sub_bytes(&subject, i, j)
        };

        charge_bytes(state, bytes.len())?;

        state.set_top(0)?;
        state.push_bytes(bytes)?;
        Ok(1)
    });

    // string.find(s, pattern [, init [, plain]])
    add_fn!("find", |state| {
        let subject = string_or_number_bytes(state, 1)?;
        let pattern = string_or_number_bytes(state, 2)?;
        charge_cost(state, (subject.len() + pattern.len()).max(1) as u64)?;
        charge_bytes(state, pattern.len())?;
        let num_args = state.get_top();

        let init = if state.check_optional_type(3, LuaType::Number)? {
            lua_start_index(subject.len(), state.to_number(3)? as isize)
        } else {
            0
        };
        let plain = num_args >= 4 && state.to_boolean(4);

        let pattern_is_plain = { plain || is_plain_lua_pattern(&pattern) };

        if pattern_is_plain {
            let result = {
                if pattern.is_empty() {
                    (init <= subject.len()).then_some((init + 1, init))
                } else if init >= subject.len() {
                    None
                } else {
                    let search = &subject[init..];
                    find_subslice(state, search, &pattern)?.map(|pos| {
                        let start = init + pos + 1;
                        let end = start + pattern.len() - 1;
                        (start, end)
                    })
                }
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

            state.set_top(0)?;

            if init > s.len() {
                state.push_nil()?;
                return Ok(1);
            }
            let mut matcher = LuaPattern::from_bytes_try(&pattern)
                .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?;
            let matched = {
                let mut meter = state.cost_meter();
                matcher.matches_bytes_from(&s, init, &mut meter)
            }
            .map_err(|err| match_error(state, err))?;
            match matched {
                true => {
                    let range = matcher.range();
                    state.push_number((range.start + 1) as f64)?;
                    state.push_number(range.end as f64)?;

                    let n = matcher.num_matches();
                    preflight_capture_bytes(state, (1..n).map(|i| matcher.capture(i)))?;
                    if n > 1 {
                        for i in 1..n {
                            push_capture_value(state, &s, matcher.capture(i))?;
                        }
                    }
                    Ok(2 + n.saturating_sub(1) as u8)
                }
                false => {
                    state.push_nil()?;
                    Ok(1)
                }
            }
        }
    });

    // string.format(formatstring, ...)
    add_fn!("format", super::string_format::format);

    // string.len(s)
    add_fn!("len", |state| {
        let len = match state.typ(1) {
            LuaType::String => state.to_bytes(1)?.len(),
            _ => string_or_number_bytes(state, 1)?.len(),
        };
        state.set_top(0)?;
        state.push_number(len as f64)?;
        Ok(1)
    });

    // string.upper(s)
    add_fn!("upper", |state| {
        let mut s = string_or_number_bytes(state, 1)?;
        charge_bytes(state, s.len())?;
        s.make_ascii_uppercase();
        state.set_top(0)?;
        state.push_bytes(s)?;
        Ok(1)
    });

    // string.lower(s)
    add_fn!("lower", |state| {
        let mut s = string_or_number_bytes(state, 1)?;
        charge_bytes(state, s.len())?;
        s.make_ascii_lowercase();
        state.set_top(0)?;
        state.push_bytes(s)?;
        Ok(1)
    });

    // string.reverse(s)
    add_fn!("reverse", |state| {
        let mut s = string_or_number_bytes(state, 1)?;
        charge_bytes(state, s.len())?;
        s.reverse();
        state.set_top(0)?;
        state.push_bytes(s)?;
        Ok(1)
    });

    // string.match(s, pattern [, init])
    add_fn!("match", |state| {
        let s = string_or_number_bytes(state, 1)?;
        let pattern = string_or_number_bytes(state, 2)?;
        charge_cost(state, (s.len() + pattern.len()).max(1) as u64)?;
        charge_bytes(state, pattern.len())?;
        let init = if state.check_optional_type(3, LuaType::Number)? {
            lua_start_index(s.len(), state.to_number(3)? as isize)
        } else {
            0
        };

        state.set_top(0)?;

        if init > s.len() {
            state.push_nil()?;
            return Ok(1);
        }
        if pattern.is_empty() {
            state.push_bytes(b"")?;
            return Ok(1);
        }
        let mut matcher = LuaPattern::from_bytes_try(&pattern)
            .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?;
        let matched = {
            let mut meter = state.cost_meter();
            matcher.matches_bytes_from(&s, init, &mut meter)
        }
        .map_err(|err| match_error(state, err))?;
        if matched {
            push_captures(state, &s, &matcher)
        } else {
            state.push_nil()?;
            Ok(1)
        }
    });

    // string.gmatch(s, pattern)
    add_fn!("gmatch", |state| {
        let s = string_or_number_bytes(state, 1)?;
        let pattern = string_or_number_bytes(state, 2)?;
        charge_cost(state, (s.len() + pattern.len()).max(1) as u64)?;
        let pattern = if pattern.first() == Some(&b'^') {
            let escaped_len = crate::vm::checked_string_growth(pattern.len(), 1)?;
            let mut escaped = Vec::with_capacity(escaped_len);
            escaped.extend_from_slice(b"%^");
            escaped.extend_from_slice(&pattern[1..]);
            escaped
        } else {
            pattern
        };
        state.set_top(0)?;

        if pattern.is_empty() {
            state.new_table()?;
            state.push_string("pos")?;
            state.push_number(0.0)?;
            state.set_table_raw(-3)?;
            state.push_string("len")?;
            state.push_number(s.len() as f64)?;
            state.set_table_raw(-3)?;

            return push_gmatch_wrapper(state, "string.gmatch.empty_iter", gmatch_empty_iter);
        }

        state.new_table()?;
        state.push_string("s")?;
        state.push_bytes(&s)?;
        state.set_table_raw(-3)?;
        state.push_string("p")?;
        state.push_bytes(&pattern)?;
        state.set_table_raw(-3)?;
        state.push_string("pos")?;
        state.push_number(0.0)?;
        state.set_table_raw(-3)?;

        push_gmatch_wrapper(state, "string.gmatch.iter", gmatch_iter)
    });

    // string.gsub(s, pattern, repl [, n])
    add_fn!("gsub", |state| {
        state.check_any(3)?;
        let s = string_or_number_bytes(state, 1)?;
        let pattern = string_or_number_bytes(state, 2)?;
        charge_cost(state, (s.len() + pattern.len()).max(1) as u64)?;
        charge_bytes(state, pattern.len())?;
        let max_replacements = if state.check_optional_type(4, LuaType::Number)? {
            let n = state.to_number(4)? as isize;
            Some(if n <= 0 { 0 } else { n as usize })
        } else {
            None
        };
        let repl_type = state.typ(3);

        if !matches!(
            repl_type,
            LuaType::String | LuaType::Number | LuaType::Function | LuaType::Table
        ) {
            return Err(state.error(ErrorKind::RuntimeError(
                "bad argument #3 to 'gsub' (string/function/table expected)".into(),
            )));
        }
        if max_replacements == Some(0) {
            charge_cost(state, s.len() as u64)?;
            state.set_top(0)?;
            state.push_bytes(s)?;
            state.push_number(0.0)?;
            return Ok(2);
        }

        let mut template = None;

        if pattern.is_empty() {
            let mut result = Vec::with_capacity(s.len());
            let mut count = 0usize;
            for i in 0..=s.len() {
                if max_replacements.is_none_or(|max| count < max) {
                    let captures = [LuaCapture::Bytes { start: i, end: i }];
                    append_gsub_replacement(
                        state,
                        &mut result,
                        &repl_type,
                        &s,
                        &captures,
                        &mut template,
                    )?;
                    count += 1;
                }
                if i < s.len() {
                    append_bytes(state, &mut result, &s[i..=i])?;
                }
            }

            state.set_top(0)?;
            state.push_bytes(result)?;
            state.push_number(count as f64)?;
            return Ok(2);
        }

        if is_plain_lua_pattern(&pattern) {
            let mut result = Vec::with_capacity(s.len());
            let mut pos = 0usize;
            let mut count = 0usize;

            while pos < s.len() {
                if max_replacements.is_some_and(|max| count >= max) {
                    break;
                }

                let search = &s[pos..];
                let Some(match_start) = find_subslice(state, search, &pattern)? else {
                    break;
                };

                let start = pos + match_start;
                let end = start + pattern.len();
                append_bytes(state, &mut result, &s[pos..start])?;

                let captures = [LuaCapture::Bytes { start, end }];
                append_gsub_replacement(
                    state,
                    &mut result,
                    &repl_type,
                    &s,
                    &captures,
                    &mut template,
                )?;

                pos = end;
                count += 1;
            }

            append_bytes(state, &mut result, &s[pos..])?;
            state.set_top(0)?;
            state.push_bytes(result)?;
            state.push_number(count as f64)?;
            return Ok(2);
        }

        let mut matcher = LuaPattern::from_bytes_try(&pattern)
            .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?;
        let mut result = Vec::with_capacity(s.len());
        let mut captures = Vec::new();
        let mut pos = 0usize;
        let mut count = 0usize;
        let anchored = pattern.first() == Some(&b'^');

        while pos <= s.len() {
            if max_replacements.is_some_and(|max| count >= max) {
                break;
            }

            let matched = {
                let mut meter = state.cost_meter();
                matcher.matches_bytes_from(&s, pos, &mut meter)
            }
            .map_err(|err| match_error(state, err))?;
            if !matched {
                break;
            }

            let range = matcher.range();
            append_bytes(state, &mut result, &s[pos..range.start])?;
            captures.clear();
            captures.extend((0..matcher.num_matches()).map(|i| matcher.capture(i)));
            append_gsub_replacement(state, &mut result, &repl_type, &s, &captures, &mut template)?;
            count += 1;

            if range.start == range.end {
                let match_end = range.end;
                let next_pos = match_end + 1;
                if match_end < s.len() {
                    append_bytes(state, &mut result, &s[match_end..=match_end])?;
                    pos = next_pos;
                } else {
                    pos = s.len() + 1;
                }
            } else {
                pos = range.end;
            }
            if anchored {
                break;
            }
        }

        if pos <= s.len() {
            append_bytes(state, &mut result, &s[pos..])?;
        }

        state.set_top(0)?;
        state.push_bytes(result)?;
        state.push_number(count as f64)?;
        Ok(2)
    });

    // Register the gmatch iterator functions up front so a persisted gmatch
    // closure (which captures one of them as an upvalue) can be resolved on
    // snapshot load, not only after gmatch has run once.
    #[cfg(feature = "snapshot")]
    {
        state
            .register_rust_fn("string.gmatch.iter", gmatch_iter)
            .map_err(|err| {
                crate::error::Error::without_location(ErrorKind::InternalError(err.to_string()))
            })?;
        state
            .register_rust_fn("string.gmatch.empty_iter", gmatch_empty_iter)
            .map_err(|err| {
                crate::error::Error::without_location(ErrorKind::InternalError(err.to_string()))
            })?;
    }

    // Set the string table as a global
    state.set_global("string");
    Ok(())
}

fn push_named_or_plain_rust_fn(
    state: &mut State,
    #[cfg_attr(not(feature = "snapshot"), allow(unused_variables))] id: &str,
    func: fn(&mut State) -> Result<u8>,
) -> Result<()> {
    #[cfg(feature = "snapshot")]
    {
        state.push_named_rust_fn(id, func)?;
    }
    #[cfg(not(feature = "snapshot"))]
    {
        state.push_rust_fn(func)?;
    }
    Ok(())
}

fn push_gmatch_wrapper(
    state: &mut State,
    iter_id: &str,
    iter: fn(&mut State) -> Result<u8>,
) -> Result<u8> {
    state.push_chunk(gmatch_wrapper())?;
    push_named_or_plain_rust_fn(state, iter_id, iter)?;
    state.push_value(1)?;
    state.remove(1)?;
    state.call(ArgCount::Fixed(2), RetCount::Fixed(1))?;
    Ok(1)
}

fn gmatch_empty_iter(state: &mut State) -> Result<u8> {
    state.push_string("pos")?;
    state.get_table(1)?;
    let pos = state.to_number(-1).unwrap_or(0.0) as usize;
    state.pop(1)?;

    state.push_string("len")?;
    state.get_table(1)?;
    let len = state.to_number(-1).unwrap_or(0.0) as usize;
    state.pop(1)?;

    if pos > len {
        state.set_top(0)?;
        state.push_nil()?;
        return Ok(1);
    }

    state.push_string("pos")?;
    state.push_number((pos + 1) as f64)?;
    state.set_table_raw(1)?;
    state.set_top(0)?;
    state.push_bytes(b"")?;
    Ok(1)
}

fn gmatch_iter(state: &mut State) -> Result<u8> {
    state.check_type(1, LuaType::Table)?;

    state.push_string("s")?;
    state.get_table(1)?;

    state.push_string("p")?;
    state.get_table(1)?;

    state.push_string("pos")?;
    state.get_table(1)?;
    let pos = state.to_number(-1).unwrap_or(0.0) as usize;
    state.pop(1)?;

    if pos > state.to_bytes(2)?.len() {
        state.set_top(0)?;
        state.push_nil()?;
        return Ok(1);
    }

    state.memoize_gmatch_pattern(3)?;
    let (matched, range, captures) = {
        let (subject, matcher, mut meter) = state.gmatch_subject_matcher_and_cost_meter(2, 3)?;
        let matched = matcher.matches_bytes_from(subject, pos, &mut meter);
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
    };
    let matched = matched.map_err(|err| match_error(state, err))?;
    if matched {
        let returned = if captures.len() > 1 {
            &captures[1..]
        } else {
            &captures[..1]
        };
        preflight_capture_bytes(state, returned.iter().copied())?;
        let new_pos = range.end + usize::from(range.start == range.end);
        state.push_string("pos")?;
        state.push_number(new_pos as f64)?;
        state.set_table_raw(1)?;

        let num_returns = if captures.len() > 1 {
            for capture in captures.iter().skip(1) {
                push_gmatch_capture(state, *capture)?;
            }
            (captures.len() - 1) as u8
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
                charge_cost(state, 1)?;
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
                fast_state.set_cost_budget(budget as i64);
                let mut slow_state = State::new();
                slow_state.set_cost_budget(budget as i64);
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
