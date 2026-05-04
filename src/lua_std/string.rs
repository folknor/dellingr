//! Lua's String Library

use std::io::Write;

use crate::error::ErrorKind;
use crate::instr::{ArgCount, RetCount};
use crate::patterns::LuaPattern;
use crate::{LuaType, Result, State};

fn is_plain_lua_pattern(pattern: &[u8]) -> bool {
    !pattern.iter().any(|b| {
        matches!(
            b,
            b'^' | b'$' | b'(' | b')' | b'%' | b'.' | b'[' | b']' | b'*' | b'+' | b'-' | b'?'
        )
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        Some(0)
    } else {
        haystack
            .windows(needle.len())
            .position(|candidate| candidate == needle)
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

fn push_captures(state: &mut State, bytes: &[u8], pattern: &LuaPattern<'_>) -> u8 {
    let n = pattern.num_matches();
    if n > 1 {
        for i in 1..n {
            state.push_bytes(&bytes[pattern.capture(i)]);
        }
        (n - 1) as u8
    } else {
        state.push_bytes(&bytes[pattern.capture(0)]);
        1
    }
}

fn capture_slices<'a>(bytes: &'a [u8], pattern: &LuaPattern<'_>) -> Vec<&'a [u8]> {
    (0..pattern.num_matches())
        .map(|i| &bytes[pattern.capture(i)])
        .collect()
}

fn write_format(state: &State, result: &mut Vec<u8>, args: std::fmt::Arguments<'_>) -> Result<()> {
    result
        .write_fmt(args)
        .map_err(|err| state.error(ErrorKind::InternalError(err.to_string())))
}

fn gsub_replacement(state: &mut State, repl_type: &LuaType, captures: &[&[u8]]) -> Result<Vec<u8>> {
    match repl_type {
        LuaType::String => {
            let repl = state.to_bytes(3)?;
            let mut out = Vec::with_capacity(repl.len());
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
                            out.extend_from_slice(captures[0]);
                        } else if idx < captures.len() {
                            out.extend_from_slice(captures[idx]);
                        }
                        i += 2;
                    } else {
                        out.push(b'%');
                        i += 1;
                    }
                } else {
                    out.push(repl[i]);
                    i += 1;
                }
            }

            Ok(out)
        }
        LuaType::Table => {
            let key = if captures.len() > 1 {
                captures[1]
            } else {
                captures[0]
            };
            state.push_value(3)?;
            state.push_bytes(key);
            state.get_table(-2)?;
            let keep_original = state.typ(-1) == LuaType::Nil
                || (state.typ(-1) == LuaType::Boolean && !state.to_boolean(-1));
            if keep_original {
                state.pop(2);
                Ok(captures[0].to_vec())
            } else {
                let val = state.to_bytes_coerce(-1)?.into_owned();
                state.pop(2);
                Ok(val)
            }
        }
        LuaType::Function => {
            state.push_value(3)?;
            if captures.len() > 1 {
                for cap in &captures[1..] {
                    state.push_bytes(*cap);
                }
                state.call(
                    ArgCount::Fixed((captures.len() - 1) as u8),
                    RetCount::Fixed(1),
                )?;
            } else {
                state.push_bytes(captures[0]);
                state.call(ArgCount::Fixed(1), RetCount::Fixed(1))?;
            }
            let keep_original = state.typ(-1) == LuaType::Nil
                || (state.typ(-1) == LuaType::Boolean && !state.to_boolean(-1));
            if keep_original {
                state.pop(1);
                Ok(captures[0].to_vec())
            } else {
                let val = state.to_bytes_coerce(-1)?.into_owned();
                state.pop(1);
                Ok(val)
            }
        }
        _ => Ok(captures[0].to_vec()),
    }
}

pub(crate) fn open_string(state: &mut State) {
    // Create the string table
    state.new_table();

    // Helper to add a function to the table at stack index -1
    macro_rules! add_fn {
        ($name:expr, $func:expr) => {
            state.push_rust_fn($func);
            state.push_string($name);
            state
                .set_table_raw(-3)
                .expect("string library registration cannot fail");
        };
    }

    // string.sub(s, i [, j])
    add_fn!("sub", |state| {
        state.check_type(1, LuaType::String)?;
        state.check_type(2, LuaType::Number)?;
        let num_args = state.get_top();

        let s = state.to_bytes(1)?.to_vec();
        let len = s.len();
        let i = state.to_number(2)? as isize;
        let j = if num_args >= 3 {
            state.check_type(3, LuaType::Number)?;
            state.to_number(3)? as isize
        } else {
            -1
        };

        let start = lua_start_index(len, i);
        let end = lua_end_index(len, j);

        state.set_top(0);
        if start >= end || start >= len {
            state.push_bytes(b"");
        } else {
            state.push_bytes(&s[start..end]);
        }
        Ok(1)
    });

    // string.find(s, pattern [, init [, plain]])
    add_fn!("find", |state| {
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
        let plain = num_args >= 4 && state.to_boolean(4);

        state.set_top(0);

        if pattern.is_empty() {
            let start = init + 1;
            state.push_number(start as f64);
            state.push_number(init as f64);
            return Ok(2);
        }

        if init >= s.len() {
            state.push_nil();
            return Ok(1);
        }
        let search = &s[init..];

        if plain {
            if let Some(pos) = find_subslice(search, &pattern) {
                let start = init + pos + 1;
                let end = start + pattern.len() - 1;
                state.push_number(start as f64);
                state.push_number(end as f64);
                Ok(2)
            } else {
                state.push_nil();
                Ok(1)
            }
        } else {
            match LuaPattern::from_bytes_try(&pattern) {
                Ok(mut matcher) => {
                    if matcher.matches_bytes(search) {
                        let range = matcher.range();
                        state.push_number((init + range.start + 1) as f64);
                        state.push_number((init + range.end) as f64);

                        let n = matcher.num_matches();
                        if n > 1 {
                            for i in 1..n {
                                state.push_bytes(&search[matcher.capture(i)]);
                            }
                        }
                        Ok(2 + n.saturating_sub(1) as u8)
                    } else {
                        state.push_nil();
                        Ok(1)
                    }
                }
                Err(_) => {
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

        if pattern.is_empty() {
            state.push_bytes(b"");
            return Ok(1);
        }
        if init >= s.len() {
            state.push_nil();
            return Ok(1);
        }

        let search = &s[init..];
        match LuaPattern::from_bytes_try(&pattern) {
            Ok(mut matcher) => {
                if matcher.matches_bytes(search) {
                    Ok(push_captures(state, search, &matcher))
                } else {
                    state.push_nil();
                    Ok(1)
                }
            }
            Err(_) => {
                state.push_nil();
                Ok(1)
            }
        }
    });

    // string.gmatch(s, pattern)
    add_fn!("gmatch", |state| {
        state.check_type(1, LuaType::String)?;
        state.check_type(2, LuaType::String)?;

        let s = state.to_bytes(1)?.to_vec();
        let pattern = state.to_bytes(2)?.to_vec();
        state.set_top(0);

        if pattern.is_empty() {
            state.new_table();
            state.push_boolean(false);
            state.push_string("done");
            state.set_table_raw(-3)?;

            state.push_rust_fn(|state| {
                state.push_string("done");
                state.get_table(1)?;
                if state.to_boolean(-1) {
                    state.set_top(0);
                    state.push_nil();
                    return Ok(1);
                }
                state.pop(1);
                state.push_boolean(true);
                state.push_string("done");
                state.set_table_raw(1)?;
                state.set_top(0);
                state.push_bytes(b"");
                Ok(1)
            });

            state.push_value(-2)?;
            state.remove(-3)?;
            state.push_nil();
            return Ok(3);
        }

        state.new_table();
        state.push_bytes(&s);
        state.push_string("s");
        state.set_table_raw(-3)?;
        state.push_bytes(&pattern);
        state.push_string("p");
        state.set_table_raw(-3)?;
        state.push_number(0.0);
        state.push_string("pos");
        state.set_table_raw(-3)?;

        state.push_rust_fn(|state| {
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

            if pos >= s.len() {
                state.set_top(0);
                state.push_nil();
                return Ok(1);
            }

            let search = &s[pos..];
            match LuaPattern::from_bytes_try(&pattern) {
                Ok(mut matcher) => {
                    if matcher.matches_bytes(search) {
                        let range = matcher.range();
                        let new_pos = pos + range.end.max(1);
                        state.push_number(new_pos as f64);
                        state.push_string("pos");
                        state.set_table_raw(1)?;

                        state.set_top(0);
                        Ok(push_captures(state, search, &matcher))
                    } else {
                        state.set_top(0);
                        state.push_nil();
                        Ok(1)
                    }
                }
                Err(_) => {
                    state.set_top(0);
                    state.push_nil();
                    Ok(1)
                }
            }
        });

        state.push_value(-2)?;
        state.remove(-3)?;
        state.push_nil();
        Ok(3)
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

        if pattern.is_empty() {
            state.set_top(0);
            state.push_bytes(s);
            state.push_number(0.0);
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

                let captures = [&s[start..end]];
                result.extend(gsub_replacement(state, &repl_type, &captures)?);

                pos = end;
                count += 1;
            }

            result.extend_from_slice(&s[pos..]);
            state.set_top(0);
            state.push_bytes(result);
            state.push_number(count as f64);
            return Ok(2);
        }

        match LuaPattern::from_bytes_try(&pattern) {
            Ok(mut matcher) => {
                let mut result = Vec::with_capacity(s.len());
                let mut pos = 0usize;
                let mut count = 0usize;

                while pos <= s.len() {
                    if max_replacements.is_some_and(|max| count >= max) {
                        break;
                    }

                    let search = &s[pos..];
                    if !matcher.matches_bytes(search) {
                        break;
                    }

                    let range = matcher.range();
                    result.extend_from_slice(&s[pos..pos + range.start]);
                    let captures = capture_slices(search, &matcher);
                    result.extend(gsub_replacement(state, &repl_type, &captures)?);
                    count += 1;

                    if range.start == range.end {
                        let next_pos = pos + range.end;
                        if next_pos < s.len() {
                            result.push(s[next_pos]);
                            pos = next_pos + 1;
                        } else {
                            pos = s.len() + 1;
                        }
                    } else {
                        pos += range.end;
                    }
                }

                if pos <= s.len() {
                    result.extend_from_slice(&s[pos..]);
                }

                state.set_top(0);
                state.push_bytes(result);
                state.push_number(count as f64);
                Ok(2)
            }
            Err(_) => {
                state.set_top(0);
                state.push_bytes(s);
                state.push_number(0.0);
                Ok(2)
            }
        }
    });

    // Set the string table as a global
    state.set_global("string");
}
