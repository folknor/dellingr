//! Lua's String Library

use std::io::Write;
use std::sync::{Arc, OnceLock};

use crate::compiler;
use crate::error::ErrorKind;
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

fn push_capture_value(state: &mut State, bytes: &[u8], capture: LuaCapture, base: usize) {
    match capture {
        LuaCapture::Bytes { start, end } => state.push_bytes(&bytes[start..end]),
        LuaCapture::Position(offset) => state.push_number((base + offset + 1) as f64),
    }
}

fn push_captures(state: &mut State, bytes: &[u8], pattern: &LuaPattern<'_>, base: usize) -> u8 {
    let n = pattern.num_matches();
    if n > 1 {
        for i in 1..n {
            push_capture_value(state, bytes, pattern.capture(i), base);
        }
        (n - 1) as u8
    } else {
        push_capture_value(state, bytes, pattern.capture(0), base);
        1
    }
}

fn write_format(state: &State, result: &mut Vec<u8>, args: std::fmt::Arguments<'_>) -> Result<()> {
    result
        .write_fmt(args)
        .map_err(|err| state.error(ErrorKind::InternalError(err.to_string())))
}

fn append_capture_bytes(out: &mut Vec<u8>, bytes: &[u8], capture: LuaCapture, base: usize) {
    match capture {
        LuaCapture::Bytes { start, end } => out.extend_from_slice(&bytes[start..end]),
        LuaCapture::Position(offset) => {
            out.extend_from_slice((base + offset + 1).to_string().as_bytes());
        }
    }
}

fn append_string_replacement(
    state: &State,
    out: &mut Vec<u8>,
    repl: &[u8],
    bytes: &[u8],
    captures: &[LuaCapture],
    base: usize,
) -> Result<()> {
    let mut i = 0usize;
    while i < repl.len() {
        if repl[i] == b'%' && i + 1 < repl.len() {
            let next = repl[i + 1];
            if next == b'%' {
                out.push(b'%');
                i += 2;
            } else if next.is_ascii_digit() {
                let idx = (next - b'0') as usize;
                if idx == 0 {
                    append_capture_bytes(out, bytes, captures[0], base);
                } else {
                    let capture = if captures.len() == 1 && idx == 1 {
                        captures[0]
                    } else {
                        *captures.get(idx).ok_or_else(|| {
                            state.error(ErrorKind::RuntimeError("invalid capture index".into()))
                        })?
                    };
                    append_capture_bytes(out, bytes, capture, base);
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
            out.push(repl[i]);
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
    base: usize,
) -> Result<()> {
    match repl_type {
        LuaType::String | LuaType::Number => {
            let repl = state.to_bytes_coerce(3)?.into_owned();
            append_string_replacement(state, out, &repl, bytes, captures, base)?;
        }
        LuaType::Table => {
            let key = if captures.len() > 1 {
                captures[1]
            } else {
                captures[0]
            };
            state.push_value(3)?;
            push_capture_value(state, bytes, key, base);
            state.get_table(-2)?;
            let keep_original = state.typ(-1) == LuaType::Nil
                || (state.typ(-1) == LuaType::Boolean && !state.to_boolean(-1));
            if keep_original {
                state.pop(2);
                append_capture_bytes(out, bytes, captures[0], base);
            } else {
                let t = state.typ(-1);
                if !matches!(t, LuaType::String | LuaType::Number) {
                    return Err(state.error(ErrorKind::RuntimeError(format!(
                        "invalid replacement value (a {})",
                        t.as_str()
                    ))));
                }
                let val = state.to_bytes_coerce(-1)?.into_owned();
                state.pop(2);
                out.extend_from_slice(&val);
            }
        }
        LuaType::Function => {
            state.push_value(3)?;
            if captures.len() > 1 {
                for cap in &captures[1..] {
                    push_capture_value(state, bytes, *cap, base);
                }
                state.call(
                    ArgCount::Fixed((captures.len() - 1) as u8),
                    RetCount::Fixed(1),
                )?;
            } else {
                push_capture_value(state, bytes, captures[0], base);
                state.call(ArgCount::Fixed(1), RetCount::Fixed(1))?;
            }
            let keep_original = state.typ(-1) == LuaType::Nil
                || (state.typ(-1) == LuaType::Boolean && !state.to_boolean(-1));
            if keep_original {
                state.pop(1);
                append_capture_bytes(out, bytes, captures[0], base);
            } else {
                let t = state.typ(-1);
                if !matches!(t, LuaType::String | LuaType::Number) {
                    return Err(state.error(ErrorKind::RuntimeError(format!(
                        "invalid replacement value (a {})",
                        t.as_str()
                    ))));
                }
                let val = state.to_bytes_coerce(-1)?.into_owned();
                state.pop(1);
                out.extend_from_slice(&val);
            }
        }
        _ => append_capture_bytes(out, bytes, captures[0], base),
    }
    Ok(())
}

pub(crate) fn open_string(state: &mut State) {
    // Create the string table
    state.new_table_with_capacity(10);

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
        state.check_type(1, LuaType::String)?;
        state.check_type(2, LuaType::Number)?;
        let num_args = state.get_top();

        let len = state.to_bytes(1)?.len();
        let i = state.to_number(2)? as isize;
        let j = if num_args >= 3 {
            state.check_type(3, LuaType::Number)?;
            state.to_number(3)? as isize
        } else {
            -1
        };

        let start = lua_start_index(len, i);
        let end = lua_end_index(len, j);
        let bytes = if start >= end || start >= len {
            Vec::new()
        } else {
            state.to_bytes(1)?[start..end].to_vec()
        };

        state.set_top(0);
        state.push_bytes(bytes);
        Ok(1)
    });

    // string.find(s, pattern [, init [, plain]])
    add_fn!("find", |state| {
        state.check_type(1, LuaType::String)?;
        state.check_type(2, LuaType::String)?;
        let num_args = state.get_top();

        let init = if num_args >= 3 {
            state.check_type(3, LuaType::Number)?;
            let s_len = state.to_bytes(1)?.len();
            lua_start_index(s_len, state.to_number(3)? as isize)
        } else {
            0
        };
        let plain = num_args >= 4 && state.to_boolean(4);

        let pattern_is_plain = {
            let pattern = state.to_bytes(2)?;
            plain || is_plain_lua_pattern(pattern)
        };

        if pattern_is_plain {
            let result = {
                let s = state.to_bytes(1)?;
                let pattern = state.to_bytes(2)?;

                if pattern.is_empty() {
                    (init <= s.len()).then_some((init + 1, init))
                } else if init >= s.len() {
                    None
                } else {
                    let search = &s[init..];
                    find_subslice(search, pattern).map(|pos| {
                        let start = init + pos + 1;
                        let end = start + pattern.len() - 1;
                        (start, end)
                    })
                }
            };

            state.set_top(0);
            if let Some((start, end)) = result {
                state.push_number(start as f64);
                state.push_number(end as f64);
                Ok(2)
            } else {
                state.push_nil();
                Ok(1)
            }
        } else {
            let s = state.to_bytes(1)?.to_vec();
            let pattern = state.to_bytes(2)?.to_vec();

            state.set_top(0);

            if init > s.len() {
                state.push_nil();
                return Ok(1);
            }
            let search = &s[init..];

            let mut matcher = LuaPattern::from_bytes_try(&pattern)
                .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?;
            match matcher
                .matches_bytes(search)
                .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?
            {
                true => {
                    let range = matcher.range();
                    state.push_number((init + range.start + 1) as f64);
                    state.push_number((init + range.end) as f64);

                    let n = matcher.num_matches();
                    if n > 1 {
                        for i in 1..n {
                            push_capture_value(state, search, matcher.capture(i), init);
                        }
                    }
                    Ok(2 + n.saturating_sub(1) as u8)
                }
                false => {
                    state.push_nil();
                    Ok(1)
                }
            }
        }
    });

    // string.format(formatstring, ...)
    add_fn!("format", |state| {
        state.check_type(1, LuaType::String)?;
        let fmt = state.to_bytes(1)?.to_vec();
        let num_args = state.get_top();

        let mut result = Vec::new();
        let mut arg_idx = 2usize;
        let mut i = 0usize;

        while i < fmt.len() {
            if fmt[i] != b'%' {
                result.push(fmt[i]);
                i += 1;
                continue;
            }

            if i + 1 >= fmt.len() {
                result.push(b'%');
                break;
            }

            let next = fmt[i + 1];
            match next {
                b'%' => {
                    result.push(b'%');
                    i += 2;
                }
                b's' => {
                    if arg_idx <= num_args {
                        result.extend_from_slice(state.to_bytes_coerce(arg_idx as isize)?.as_ref());
                        arg_idx += 1;
                    }
                    i += 2;
                }
                b'd' | b'i' => {
                    if arg_idx <= num_args {
                        if let Ok(n) = state.to_number(arg_idx as isize) {
                            write_format(state, &mut result, format_args!("{}", n as i64))?;
                        }
                        arg_idx += 1;
                    }
                    i += 2;
                }
                b'f' => {
                    if arg_idx <= num_args {
                        if let Ok(n) = state.to_number(arg_idx as isize) {
                            write_format(state, &mut result, format_args!("{n:.6}"))?;
                        }
                        arg_idx += 1;
                    }
                    i += 2;
                }
                b'g' => {
                    if arg_idx <= num_args {
                        if let Ok(n) = state.to_number(arg_idx as isize) {
                            write_format(state, &mut result, format_args!("{n}"))?;
                        }
                        arg_idx += 1;
                    }
                    i += 2;
                }
                b'x' => {
                    if arg_idx <= num_args {
                        if let Ok(n) = state.to_number(arg_idx as isize) {
                            write_format(state, &mut result, format_args!("{:x}", n as i64))?;
                        }
                        arg_idx += 1;
                    }
                    i += 2;
                }
                b'X' => {
                    if arg_idx <= num_args {
                        if let Ok(n) = state.to_number(arg_idx as isize) {
                            write_format(state, &mut result, format_args!("{:X}", n as i64))?;
                        }
                        arg_idx += 1;
                    }
                    i += 2;
                }
                b'o' => {
                    if arg_idx <= num_args {
                        if let Ok(n) = state.to_number(arg_idx as isize) {
                            write_format(state, &mut result, format_args!("{:o}", n as i64))?;
                        }
                        arg_idx += 1;
                    }
                    i += 2;
                }
                b'c' => {
                    if arg_idx <= num_args {
                        if let Ok(n) = state.to_number(arg_idx as isize) {
                            result.push(n as u8);
                        }
                        arg_idx += 1;
                    }
                    i += 2;
                }
                b'0'..=b'9' | b'.' | b'-' | b'+' | b' ' => {
                    let spec_start = i;
                    i += 1;
                    while i < fmt.len() && matches!(fmt[i], b'0'..=b'9' | b'.' | b'-' | b'+' | b' ')
                    {
                        i += 1;
                    }
                    if i >= fmt.len() {
                        result.extend_from_slice(&fmt[spec_start..]);
                        break;
                    }

                    let conv = fmt[i];
                    let spec = &fmt[spec_start..=i];
                    if arg_idx <= num_args {
                        match conv {
                            b's' => {
                                result.extend_from_slice(
                                    state.to_bytes_coerce(arg_idx as isize)?.as_ref(),
                                );
                            }
                            b'd' | b'i' => {
                                if let Ok(n) = state.to_number(arg_idx as isize) {
                                    let width = spec[1..spec.len() - 1]
                                        .iter()
                                        .filter(|b| b.is_ascii_digit())
                                        .fold(0usize, |acc, b| acc * 10 + (b - b'0') as usize);
                                    let zero_pad = spec.contains(&b'0');
                                    if zero_pad && width > 0 {
                                        write_format(
                                            state,
                                            &mut result,
                                            format_args!("{:0>width$}", n as i64, width = width),
                                        )?;
                                    } else if width > 0 {
                                        write_format(
                                            state,
                                            &mut result,
                                            format_args!("{:>width$}", n as i64, width = width),
                                        )?;
                                    } else {
                                        write_format(
                                            state,
                                            &mut result,
                                            format_args!("{}", n as i64),
                                        )?;
                                    }
                                }
                            }
                            b'f' => {
                                if let Ok(n) = state.to_number(arg_idx as isize) {
                                    if let Some(dot_pos) = spec.iter().position(|b| *b == b'.') {
                                        let precision = spec[dot_pos + 1..spec.len() - 1]
                                            .iter()
                                            .take_while(|b| b.is_ascii_digit())
                                            .fold(0usize, |acc, b| acc * 10 + (*b - b'0') as usize);
                                        write_format(
                                            state,
                                            &mut result,
                                            format_args!("{n:.precision$}"),
                                        )?;
                                    } else {
                                        write_format(state, &mut result, format_args!("{n:.6}"))?;
                                    }
                                }
                            }
                            _ => result.extend_from_slice(spec),
                        }
                        arg_idx += 1;
                    }
                    i += 1;
                }
                _ => {
                    result.push(b'%');
                    i += 1;
                }
            }
        }

        state.set_top(0);
        state.push_bytes(result);
        Ok(1)
    });

    // string.len(s)
    add_fn!("len", |state| {
        state.check_type(1, LuaType::String)?;
        let len = state.to_bytes(1)?.len();
        state.set_top(0);
        state.push_number(len as f64);
        Ok(1)
    });

    // string.upper(s)
    add_fn!("upper", |state| {
        state.check_type(1, LuaType::String)?;
        let mut s = state.to_bytes(1)?.to_vec();
        s.make_ascii_uppercase();
        state.set_top(0);
        state.push_bytes(s);
        Ok(1)
    });

    // string.lower(s)
    add_fn!("lower", |state| {
        state.check_type(1, LuaType::String)?;
        let mut s = state.to_bytes(1)?.to_vec();
        s.make_ascii_lowercase();
        state.set_top(0);
        state.push_bytes(s);
        Ok(1)
    });

    // string.reverse(s)
    add_fn!("reverse", |state| {
        state.check_type(1, LuaType::String)?;
        let mut s = state.to_bytes(1)?.to_vec();
        s.reverse();
        state.set_top(0);
        state.push_bytes(s);
        Ok(1)
    });

    // string.match(s, pattern [, init])
    add_fn!("match", |state| {
        state.check_type(1, LuaType::String)?;
        state.check_type(2, LuaType::String)?;
        let num_args = state.get_top();

        let s = state.to_bytes(1)?.to_vec();
        let pattern = state.to_bytes(2)?.to_vec();
        let init = if num_args >= 3 {
            state.check_type(3, LuaType::Number)?;
            lua_start_index(s.len(), state.to_number(3)? as isize)
        } else {
            0
        };

        state.set_top(0);

        if init > s.len() {
            state.push_nil();
            return Ok(1);
        }
        if pattern.is_empty() {
            state.push_bytes(b"");
            return Ok(1);
        }
        let search = &s[init..];
        let mut matcher = LuaPattern::from_bytes_try(&pattern)
            .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?;
        if matcher
            .matches_bytes(search)
            .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?
        {
            Ok(push_captures(state, search, &matcher, init))
        } else {
            state.push_nil();
            Ok(1)
        }
    });

    // string.gmatch(s, pattern)
    add_fn!("gmatch", |state| {
        state.check_type(1, LuaType::String)?;
        state.check_type(2, LuaType::String)?;

        let s = state.to_bytes(1)?.to_vec();
        let pattern = state.to_bytes(2)?.to_vec();
        let pattern = if pattern.first() == Some(&b'^') {
            let mut escaped = Vec::with_capacity(pattern.len() + 1);
            escaped.extend_from_slice(b"%^");
            escaped.extend_from_slice(&pattern[1..]);
            escaped
        } else {
            pattern
        };
        state.set_top(0);

        if pattern.is_empty() {
            state.new_table();
            state.push_string("pos");
            state.push_number(0.0);
            state.set_table_raw(-3)?;
            state.push_string("len");
            state.push_number(s.len() as f64);
            state.set_table_raw(-3)?;

            return push_gmatch_wrapper(state, "string.gmatch.empty_iter", gmatch_empty_iter);
        }

        state.new_table();
        state.push_string("s");
        state.push_bytes(&s);
        state.set_table_raw(-3)?;
        state.push_string("p");
        state.push_bytes(&pattern);
        state.set_table_raw(-3)?;
        state.push_string("pos");
        state.push_number(0.0);
        state.set_table_raw(-3)?;

        push_gmatch_wrapper(state, "string.gmatch.iter", gmatch_iter)
    });

    // string.gsub(s, pattern, repl [, n])
    add_fn!("gsub", |state| {
        state.check_type(1, LuaType::String)?;
        state.check_type(2, LuaType::String)?;
        state.check_any(3)?;
        let num_args = state.get_top();

        let s = state.to_bytes(1)?.to_vec();
        let pattern = state.to_bytes(2)?.to_vec();
        let max_replacements = if num_args >= 4 {
            state.check_type(4, LuaType::Number)?;
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
            state.set_top(0);
            state.push_bytes(s);
            state.push_number(0.0);
            return Ok(2);
        }

        if pattern.is_empty() {
            let mut result = Vec::with_capacity(s.len());
            let mut count = 0usize;
            for i in 0..=s.len() {
                if max_replacements.is_none_or(|max| count < max) {
                    let captures = [LuaCapture::Bytes { start: 0, end: 0 }];
                    append_gsub_replacement(state, &mut result, &repl_type, &s[i..], &captures, i)?;
                    count += 1;
                }
                if i < s.len() {
                    result.push(s[i]);
                }
            }

            state.set_top(0);
            state.push_bytes(result);
            state.push_number(count as f64);
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
                result.extend_from_slice(&s[pos..start]);

                let captures = [LuaCapture::Bytes {
                    start: match_start,
                    end: match_start + pattern.len(),
                }];
                append_gsub_replacement(state, &mut result, &repl_type, search, &captures, pos)?;

                pos = end;
                count += 1;
            }

            result.extend_from_slice(&s[pos..]);
            state.set_top(0);
            state.push_bytes(result);
            state.push_number(count as f64);
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

            let search = &s[pos..];
            if !matcher
                .matches_bytes(search)
                .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?
            {
                break;
            }

            let range = matcher.range();
            result.extend_from_slice(&s[pos..pos + range.start]);
            captures.clear();
            captures.extend((0..matcher.num_matches()).map(|i| matcher.capture(i)));
            append_gsub_replacement(state, &mut result, &repl_type, search, &captures, pos)?;
            count += 1;

            if range.start == range.end {
                let match_end = pos + range.end;
                let next_pos = match_end + 1;
                if match_end < s.len() {
                    result.push(s[match_end]);
                    pos = next_pos;
                } else {
                    pos = s.len() + 1;
                }
            } else {
                pos += range.end;
            }
            if anchored {
                break;
            }
        }

        if pos <= s.len() {
            result.extend_from_slice(&s[pos..]);
        }

        state.set_top(0);
        state.push_bytes(result);
        state.push_number(count as f64);
        Ok(2)
    });

    // Register the gmatch iterator functions up front so a persisted gmatch
    // closure (which captures one of them as an upvalue) can be resolved on
    // snapshot load, not only after gmatch has run once.
    #[cfg(feature = "snapshot")]
    {
        state
            .register_rust_fn("string.gmatch.iter", gmatch_iter)
            .expect("gmatch iter id registration cannot conflict");
        state
            .register_rust_fn("string.gmatch.empty_iter", gmatch_empty_iter)
            .expect("gmatch empty_iter id registration cannot conflict");
    }

    // Set the string table as a global
    state.set_global("string");
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
        state.push_rust_fn(func);
    }
    Ok(())
}

fn push_gmatch_wrapper(
    state: &mut State,
    iter_id: &str,
    iter: fn(&mut State) -> Result<u8>,
) -> Result<u8> {
    state.push_chunk(gmatch_wrapper());
    push_named_or_plain_rust_fn(state, iter_id, iter)?;
    state.push_value(1)?;
    state.remove(1)?;
    state.call(ArgCount::Fixed(2), RetCount::Fixed(1))?;
    Ok(1)
}

fn gmatch_empty_iter(state: &mut State) -> Result<u8> {
    state.push_string("pos");
    state.get_table(1)?;
    let pos = state.to_number(-1).unwrap_or(0.0) as usize;
    state.pop(1);

    state.push_string("len");
    state.get_table(1)?;
    let len = state.to_number(-1).unwrap_or(0.0) as usize;
    state.pop(1);

    if pos > len {
        state.set_top(0);
        state.push_nil();
        return Ok(1);
    }

    state.push_string("pos");
    state.push_number((pos + 1) as f64);
    state.set_table_raw(1)?;
    state.set_top(0);
    state.push_bytes(b"");
    Ok(1)
}

fn gmatch_iter(state: &mut State) -> Result<u8> {
    state.check_type(1, LuaType::Table)?;

    state.push_string("s");
    state.get_table(1)?;
    let s = state.to_bytes(-1)?.to_vec();
    state.pop(1);

    state.push_string("p");
    state.get_table(1)?;
    let pattern = state.to_bytes(-1)?.to_vec();
    state.pop(1);

    state.push_string("pos");
    state.get_table(1)?;
    let pos = state.to_number(-1).unwrap_or(0.0) as usize;
    state.pop(1);

    if pos > s.len() {
        state.set_top(0);
        state.push_nil();
        return Ok(1);
    }

    let search = &s[pos..];
    let mut matcher = LuaPattern::from_bytes_try(&pattern)
        .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?;
    if matcher
        .matches_bytes(search)
        .map_err(|err| state.error(ErrorKind::RuntimeError(err.to_string())))?
    {
        let range = matcher.range();
        let new_pos = pos + range.end + usize::from(range.start == range.end);
        state.push_string("pos");
        state.push_number(new_pos as f64);
        state.set_table_raw(1)?;

        state.set_top(0);
        Ok(push_captures(state, search, &matcher, pos))
    } else {
        state.set_top(0);
        state.push_nil();
        Ok(1)
    }
}
