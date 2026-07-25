//! Lua's String Library

use std::sync::{Arc, OnceLock};

use crate::compiler;
use crate::error::{ArgError, ErrorKind};
use crate::instr::{ArgCount, RetCount};
use crate::patterns::{LuaCapture, LuaPattern};
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

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    match needle.len() {
        0 => Some(0),
        1 => haystack.iter().position(|b| *b == needle[0]),
        _ => haystack
            .windows(needle.len())
            .position(|candidate| candidate == needle),
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
        for i in 1..n {
            push_capture_value(state, bytes, pattern.capture(i))?;
        }
        Ok((n - 1) as u8)
    } else {
        push_capture_value(state, bytes, pattern.capture(0))?;
        Ok(1)
    }
}

fn append_capture_bytes(out: &mut Vec<u8>, bytes: &[u8], capture: LuaCapture) -> Result<()> {
    match capture {
        LuaCapture::Bytes { start, end } => append_bytes(out, &bytes[start..end])?,
        LuaCapture::Position(offset) => {
            let position = (offset + 1).to_string();
            append_bytes(out, position.as_bytes())?;
        }
    }
    Ok(())
}

fn append_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let next = crate::vm::checked_string_growth(out.len(), bytes.len())?;
    out.reserve(next - out.len());
    out.extend_from_slice(bytes);
    Ok(())
}

fn append_string_replacement(
    state: &State,
    out: &mut Vec<u8>,
    repl: &[u8],
    bytes: &[u8],
    captures: &[LuaCapture],
) -> Result<()> {
    let mut i = 0usize;
    while i < repl.len() {
        if repl[i] == b'%' && i + 1 < repl.len() {
            let next = repl[i + 1];
            if next == b'%' {
                append_bytes(out, b"%")?;
                i += 2;
            } else if next.is_ascii_digit() {
                let idx = (next - b'0') as usize;
                if idx == 0 {
                    append_capture_bytes(out, bytes, captures[0])?;
                } else {
                    let capture = if captures.len() == 1 && idx == 1 {
                        captures[0]
                    } else {
                        *captures.get(idx).ok_or_else(|| {
                            state.error(ErrorKind::RuntimeError("invalid capture index".into()))
                        })?
                    };
                    append_capture_bytes(out, bytes, capture)?;
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
            append_bytes(out, &repl[i..=i])?;
            i += 1;
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
) -> Result<()> {
    match repl_type {
        LuaType::String | LuaType::Number => {
            let repl = state.bytes_coerce(3)?;
            append_string_replacement(state, out, &repl, bytes, captures)?;
        }
        LuaType::Table => {
            let key = if captures.len() > 1 {
                captures[1]
            } else {
                captures[0]
            };
            state.push_value(3)?;
            push_capture_value(state, bytes, key)?;
            state.get_table(-2)?;
            let keep_original = state.typ(-1) == LuaType::Nil
                || (state.typ(-1) == LuaType::Boolean && !state.to_boolean(-1));
            if keep_original {
                state.pop(2)?;
                append_capture_bytes(out, bytes, captures[0])?;
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
                append_bytes(out, &val)?;
            }
        }
        LuaType::Function => {
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
                append_capture_bytes(out, bytes, captures[0])?;
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
                append_bytes(out, &val)?;
            }
        }
        _ => append_capture_bytes(out, bytes, captures[0])?,
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
        let subject = string_or_number_bytes(state, 1)?;
        let len = subject.len();
        let i = state.to_number(2)? as isize;
        let j = if state.check_optional_type(3, LuaType::Number)? {
            state.to_number(3)? as isize
        } else {
            -1
        };

        let start = lua_start_index(len, i);
        let end = lua_end_index(len, j);
        let bytes = if start >= end || start >= len {
            Vec::new()
        } else {
            subject[start..end].to_vec()
        };

        state.set_top(0)?;
        state.push_bytes(bytes)?;
        Ok(1)
    });

    // string.find(s, pattern [, init [, plain]])
    add_fn!("find", |state| {
        let subject = string_or_number_bytes(state, 1)?;
        let pattern = string_or_number_bytes(state, 2)?;
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
                    find_subslice(search, &pattern).map(|pos| {
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
            match matcher
                .matches_bytes_from(&s, init)
                .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?
            {
                true => {
                    let range = matcher.range();
                    state.push_number((range.start + 1) as f64)?;
                    state.push_number(range.end as f64)?;

                    let n = matcher.num_matches();
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
        let len = string_or_number_bytes(state, 1)?.len();
        state.set_top(0)?;
        state.push_number(len as f64)?;
        Ok(1)
    });

    // string.upper(s)
    add_fn!("upper", |state| {
        let mut s = string_or_number_bytes(state, 1)?;
        s.make_ascii_uppercase();
        state.set_top(0)?;
        state.push_bytes(s)?;
        Ok(1)
    });

    // string.lower(s)
    add_fn!("lower", |state| {
        let mut s = string_or_number_bytes(state, 1)?;
        s.make_ascii_lowercase();
        state.set_top(0)?;
        state.push_bytes(s)?;
        Ok(1)
    });

    // string.reverse(s)
    add_fn!("reverse", |state| {
        let mut s = string_or_number_bytes(state, 1)?;
        s.reverse();
        state.set_top(0)?;
        state.push_bytes(s)?;
        Ok(1)
    });

    // string.match(s, pattern [, init])
    add_fn!("match", |state| {
        let s = string_or_number_bytes(state, 1)?;
        let pattern = string_or_number_bytes(state, 2)?;
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
        if matcher
            .matches_bytes_from(&s, init)
            .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?
        {
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
            state.set_top(0)?;
            state.push_bytes(s)?;
            state.push_number(0.0)?;
            return Ok(2);
        }

        if pattern.is_empty() {
            let mut result = Vec::with_capacity(s.len());
            let mut count = 0usize;
            for i in 0..=s.len() {
                if max_replacements.is_none_or(|max| count < max) {
                    let captures = [LuaCapture::Bytes { start: i, end: i }];
                    append_gsub_replacement(state, &mut result, &repl_type, &s, &captures)?;
                    count += 1;
                }
                if i < s.len() {
                    append_bytes(&mut result, &s[i..=i])?;
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
                let Some(match_start) = find_subslice(search, &pattern) else {
                    break;
                };

                let start = pos + match_start;
                let end = start + pattern.len();
                append_bytes(&mut result, &s[pos..start])?;

                let captures = [LuaCapture::Bytes { start, end }];
                append_gsub_replacement(state, &mut result, &repl_type, &s, &captures)?;

                pos = end;
                count += 1;
            }

            append_bytes(&mut result, &s[pos..])?;
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

            if !matcher
                .matches_bytes_from(&s, pos)
                .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?
            {
                break;
            }

            let range = matcher.range();
            append_bytes(&mut result, &s[pos..range.start])?;
            captures.clear();
            captures.extend((0..matcher.num_matches()).map(|i| matcher.capture(i)));
            append_gsub_replacement(state, &mut result, &repl_type, &s, &captures)?;
            count += 1;

            if range.start == range.end {
                let match_end = range.end;
                let next_pos = match_end + 1;
                if match_end < s.len() {
                    append_bytes(&mut result, &s[match_end..=match_end])?;
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
            append_bytes(&mut result, &s[pos..])?;
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
    let s = state.to_bytes(-1)?.to_vec();
    state.pop(1)?;

    state.push_string("p")?;
    state.get_table(1)?;
    let pattern = state.to_bytes(-1)?.to_vec();
    state.pop(1)?;

    state.push_string("pos")?;
    state.get_table(1)?;
    let pos = state.to_number(-1).unwrap_or(0.0) as usize;
    state.pop(1)?;

    if pos > s.len() {
        state.set_top(0)?;
        state.push_nil()?;
        return Ok(1);
    }

    let mut matcher = LuaPattern::from_bytes_try(&pattern)
        .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?;
    if matcher
        .matches_bytes_from(&s, pos)
        .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?
    {
        let range = matcher.range();
        let new_pos = range.end + usize::from(range.start == range.end);
        state.push_string("pos")?;
        state.push_number(new_pos as f64)?;
        state.set_table_raw(1)?;

        state.set_top(0)?;
        push_captures(state, &s, &matcher)
    } else {
        state.set_top(0)?;
        state.push_nil()?;
        Ok(1)
    }
}
