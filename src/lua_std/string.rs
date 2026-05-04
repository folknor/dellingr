//! Lua's String Library

use crate::LuaType;
use crate::Result;
use crate::State;
use crate::instr::{ArgCount, RetCount};
use lua_patterns::LuaPattern;

fn is_plain_lua_pattern(pattern: &str) -> bool {
    !pattern.bytes().any(|b| {
        matches!(
            b,
            b'^' | b'$' | b'(' | b')' | b'%' | b'.' | b'[' | b']' | b'*' | b'+' | b'-' | b'?'
        )
    })
}

fn gsub_replacement(state: &mut State, repl_type: &LuaType, captures: &[&str]) -> Result<String> {
    match repl_type {
        LuaType::String => {
            let repl = state.to_string(3)?;
            let mut repl_result = String::new();
            let mut chars = repl.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '%' {
                    if let Some(&next) = chars.peek() {
                        if next == '%' {
                            repl_result.push('%');
                            chars.next();
                        } else if next.is_ascii_digit() {
                            let idx = next.to_digit(10).expect("ASCII digit has a decimal value")
                                as usize;
                            chars.next();
                            if idx == 0 {
                                repl_result.push_str(captures[0]);
                            } else if idx < captures.len() {
                                repl_result.push_str(captures[idx]);
                            }
                        } else {
                            repl_result.push(c);
                        }
                    } else {
                        repl_result.push(c);
                    }
                } else {
                    repl_result.push(c);
                }
            }
            Ok(repl_result)
        }
        LuaType::Table => {
            let key = if captures.len() > 1 {
                captures[1]
            } else {
                captures[0]
            };
            state.push_value(3)?;
            state.push_string(key.to_string());
            state.get_table(-2)?;
            let keep_original = state.typ(-1) == LuaType::Nil
                || (state.typ(-1) == LuaType::Boolean && !state.to_boolean(-1));
            if keep_original {
                state.pop(2);
                Ok(captures[0].to_string())
            } else {
                let val = state.to_string(-1)?;
                state.pop(2);
                Ok(val)
            }
        }
        LuaType::Function => {
            state.push_value(3)?;
            if captures.len() > 1 {
                for cap in captures.iter().skip(1) {
                    state.push_string((*cap).to_string());
                }
                state.call(
                    ArgCount::Fixed((captures.len() - 1) as u8),
                    RetCount::Fixed(1),
                )?;
            } else {
                state.push_string(captures[0].to_string());
                state.call(ArgCount::Fixed(1), RetCount::Fixed(1))?;
            }
            let keep_original = state.typ(-1) == LuaType::Nil
                || (state.typ(-1) == LuaType::Boolean && !state.to_boolean(-1));
            if keep_original {
                state.pop(1);
                Ok(captures[0].to_string())
            } else {
                let val = state.to_string(-1)?;
                state.pop(1);
                Ok(val)
            }
        }
        _ => Ok(captures[0].to_string()),
    }
}

pub(crate) fn open_string(state: &mut State) {
    // Create the string table
    state.new_table();

    // Helper to add a function to the table at stack index -1
    macro_rules! add_fn {
        ($name:expr, $func:expr) => {
            state.push_rust_fn($func);
            state.push_string($name.to_string());
            state.set_table_raw(-3).unwrap();
        };
    }

    // string.sub(s, i [, j])
    // Returns the substring from position i to j (inclusive).
    // Negative indices count from the end (-1 is the last character).
    // j defaults to -1 (end of string).
    add_fn!("sub", |state| {
        state.check_type(1, LuaType::String)?;
        state.check_type(2, LuaType::Number)?;
        let num_args = state.get_top();

        let s = state.to_string(1)?;
        let len = s.len() as isize;

        // Convert Lua 1-based index to 0-based, handling negatives
        let i = state.to_number(2)? as isize;
        let j = if num_args >= 3 {
            state.check_type(3, LuaType::Number)?;
            state.to_number(3)? as isize
        } else {
            -1 // default to end
        };

        // Lua index conversion: 1-based, negative counts from end
        let start = if i >= 0 {
            (i - 1).max(0) as usize
        } else {
            (len + i).max(0) as usize
        };

        let end = if j >= 0 {
            (j as usize).min(len as usize)
        } else {
            (len + j + 1).max(0) as usize
        };

        state.set_top(0);

        if start >= end || start >= len as usize {
            state.push_string(String::new());
        } else {
            // Handle UTF-8 safely by working with bytes
            let bytes = s.as_bytes();
            let end = end.min(bytes.len());
            let result = String::from_utf8_lossy(&bytes[start..end]).to_string();
            state.push_string(result);
        }

        Ok(1)
    });

    // string.find(s, pattern [, init [, plain]])
    // Looks for the first match of pattern in string s.
    // Returns start, end indices (1-based) or nil if not found.
    // If plain is true, does plain text matching (no patterns).
    add_fn!("find", |state| {
        state.check_type(1, LuaType::String)?;
        state.check_type(2, LuaType::String)?;
        let num_args = state.get_top();

        let s = state.to_string(1)?;
        let pattern = state.to_string(2)?;

        let init = if num_args >= 3 {
            state.check_type(3, LuaType::Number)?;
            let i = state.to_number(3)? as isize;
            if i >= 0 {
                (i - 1).max(0) as usize
            } else {
                (s.len() as isize + i).max(0) as usize
            }
        } else {
            0
        };

        let plain = if num_args >= 4 {
            state.to_boolean(4)
        } else {
            false
        };

        state.set_top(0);

        // Handle empty pattern - matches at position init+1 with zero length
        if pattern.is_empty() {
            let start = init + 1;
            state.push_number(start as f64);
            state.push_number(init as f64); // end is start-1 for empty match
            return Ok(2);
        }

        let search_str = if init > 0 && init < s.len() {
            &s[init..]
        } else if init >= s.len() {
            state.push_nil();
            return Ok(1);
        } else {
            &s[..]
        };

        if plain {
            // Plain text search
            if let Some(pos) = search_str.find(&pattern) {
                let start = init + pos + 1; // Convert to 1-based
                let end = start + pattern.len() - 1;
                state.push_number(start as f64);
                state.push_number(end as f64);
                Ok(2)
            } else {
                state.push_nil();
                Ok(1)
            }
        } else {
            // Pattern search using lua-patterns
            match LuaPattern::new_try(&pattern) {
                Ok(mut m) => {
                    if m.matches(search_str) {
                        let range = m.range();
                        let start = init + range.start + 1; // Convert to 1-based
                        let end = init + range.end;
                        state.push_number(start as f64);
                        state.push_number(end as f64);

                        // Also return captures if any
                        let captures = m.captures(search_str);
                        // captures[0] is full match, captures[1..] are capture groups
                        for cap in captures.iter().skip(1) {
                            state.push_string(cap.to_string());
                        }

                        let extra_captures = if captures.len() > 1 {
                            captures.len() - 1
                        } else {
                            0
                        };
                        Ok(2 + extra_captures as u8)
                    } else {
                        state.push_nil();
                        Ok(1)
                    }
                }
                Err(_) => {
                    // Invalid pattern, return nil
                    state.push_nil();
                    Ok(1)
                }
            }
        }
    });

    // string.format(formatstring, ...)
    // Returns a formatted string following printf conventions.
    // Supports: %s, %d, %i, %f, %g, %x, %X, %o, %c, %%
    add_fn!("format", |state| {
        state.check_type(1, LuaType::String)?;
        let fmt = state.to_string(1)?;
        let num_args = state.get_top();

        let mut result = String::new();
        let mut arg_idx = 2usize;
        let mut chars = fmt.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '%' {
                if let Some(&next) = chars.peek() {
                    match next {
                        '%' => {
                            result.push('%');
                            chars.next();
                        }
                        's' => {
                            chars.next();
                            if arg_idx <= num_args {
                                result.push_str(&state.to_string(arg_idx as isize)?);
                                arg_idx += 1;
                            }
                        }
                        'd' | 'i' => {
                            chars.next();
                            if arg_idx <= num_args {
                                if let Ok(n) = state.to_number(arg_idx as isize) {
                                    result.push_str(&format!("{}", n as i64));
                                }
                                arg_idx += 1;
                            }
                        }
                        'f' => {
                            chars.next();
                            if arg_idx <= num_args {
                                if let Ok(n) = state.to_number(arg_idx as isize) {
                                    result.push_str(&format!("{n:.6}"));
                                }
                                arg_idx += 1;
                            }
                        }
                        'g' => {
                            chars.next();
                            if arg_idx <= num_args {
                                if let Ok(n) = state.to_number(arg_idx as isize) {
                                    result.push_str(&format!("{n}"));
                                }
                                arg_idx += 1;
                            }
                        }
                        'x' => {
                            chars.next();
                            if arg_idx <= num_args {
                                if let Ok(n) = state.to_number(arg_idx as isize) {
                                    result.push_str(&format!("{:x}", n as i64));
                                }
                                arg_idx += 1;
                            }
                        }
                        'X' => {
                            chars.next();
                            if arg_idx <= num_args {
                                if let Ok(n) = state.to_number(arg_idx as isize) {
                                    result.push_str(&format!("{:X}", n as i64));
                                }
                                arg_idx += 1;
                            }
                        }
                        'o' => {
                            chars.next();
                            if arg_idx <= num_args {
                                if let Ok(n) = state.to_number(arg_idx as isize) {
                                    result.push_str(&format!("{:o}", n as i64));
                                }
                                arg_idx += 1;
                            }
                        }
                        'c' => {
                            chars.next();
                            if arg_idx <= num_args {
                                if let Ok(n) = state.to_number(arg_idx as isize)
                                    && let Some(ch) = char::from_u32(n as u32)
                                {
                                    result.push(ch);
                                }
                                arg_idx += 1;
                            }
                        }
                        // Handle width/precision specifiers like %5d, %.2f, %05d
                        '0'..='9' | '.' | '-' | '+' | ' ' => {
                            let mut spec = String::from("%");
                            // Collect the format specifier
                            while let Some(&ch) = chars.peek() {
                                if ch.is_ascii_digit()
                                    || ch == '.'
                                    || ch == '-'
                                    || ch == '+'
                                    || ch == ' '
                                {
                                    spec.push(ch);
                                    chars.next();
                                } else {
                                    break;
                                }
                            }
                            // Get the conversion character
                            if let Some(&conv) = chars.peek() {
                                spec.push(conv);
                                chars.next();

                                if arg_idx <= num_args {
                                    match conv {
                                        's' => {
                                            let s = state.to_string(arg_idx as isize)?;
                                            // For string, just append (ignore width for simplicity)
                                            result.push_str(&s);
                                        }
                                        'd' | 'i' => {
                                            if let Ok(n) = state.to_number(arg_idx as isize) {
                                                // Parse width from spec
                                                let width_str: String = spec[1..spec.len() - 1]
                                                    .chars()
                                                    .filter(char::is_ascii_digit)
                                                    .collect();
                                                let width: usize = width_str.parse().unwrap_or(0);
                                                let zero_pad = spec.contains('0');
                                                if zero_pad && width > 0 {
                                                    result.push_str(&format!(
                                                        "{:0>width$}",
                                                        n as i64,
                                                        width = width
                                                    ));
                                                } else if width > 0 {
                                                    result.push_str(&format!(
                                                        "{:>width$}",
                                                        n as i64,
                                                        width = width
                                                    ));
                                                } else {
                                                    result.push_str(&format!("{}", n as i64));
                                                }
                                            }
                                        }
                                        'f' => {
                                            if let Ok(n) = state.to_number(arg_idx as isize) {
                                                // Parse precision
                                                if let Some(dot_pos) = spec.find('.') {
                                                    let prec_str: String = spec
                                                        [dot_pos + 1..spec.len() - 1]
                                                        .chars()
                                                        .take_while(char::is_ascii_digit)
                                                        .collect();
                                                    let prec: usize = prec_str.parse().unwrap_or(6);
                                                    result.push_str(&format!("{n:.prec$}"));
                                                } else {
                                                    result.push_str(&format!("{n:.6}"));
                                                }
                                            }
                                        }
                                        _ => {
                                            // Unknown, just output as-is
                                            result.push_str(&spec);
                                        }
                                    }
                                    arg_idx += 1;
                                }
                            }
                        }
                        _ => {
                            // Unknown format, output as-is
                            result.push('%');
                        }
                    }
                } else {
                    result.push('%');
                }
            } else {
                result.push(c);
            }
        }

        state.set_top(0);
        state.push_string(result);
        Ok(1)
    });

    // string.len(s) - bonus, easy to add
    add_fn!("len", |state| {
        state.check_type(1, LuaType::String)?;
        let s = state.to_string(1)?;
        state.set_top(0);
        state.push_number(s.len() as f64);
        Ok(1)
    });

    // string.upper(s) - bonus
    add_fn!("upper", |state| {
        state.check_type(1, LuaType::String)?;
        let s = state.to_string(1)?;
        state.set_top(0);
        state.push_string(s.to_uppercase());
        Ok(1)
    });

    // string.lower(s) - bonus
    add_fn!("lower", |state| {
        state.check_type(1, LuaType::String)?;
        let s = state.to_string(1)?;
        state.set_top(0);
        state.push_string(s.to_lowercase());
        Ok(1)
    });

    // string.reverse(s)
    add_fn!("reverse", |state| {
        state.check_type(1, LuaType::String)?;
        let s = state.to_string(1)?;
        state.set_top(0);
        state.push_string(s.chars().rev().collect());
        Ok(1)
    });

    // string.match(s, pattern [, init])
    // Returns the captures from the first match of pattern in s.
    // If no captures, returns the whole match. Returns nil if no match.
    add_fn!("match", |state| {
        state.check_type(1, LuaType::String)?;
        state.check_type(2, LuaType::String)?;
        let num_args = state.get_top();

        let s = state.to_string(1)?;
        let pattern = state.to_string(2)?;

        let init = if num_args >= 3 {
            state.check_type(3, LuaType::Number)?;
            let i = state.to_number(3)? as isize;
            if i >= 0 {
                (i - 1).max(0) as usize
            } else {
                (s.len() as isize + i).max(0) as usize
            }
        } else {
            0
        };

        state.set_top(0);

        // Handle empty pattern - returns empty string
        if pattern.is_empty() {
            state.push_string(String::new());
            return Ok(1);
        }

        let search_str = if init > 0 && init < s.len() {
            &s[init..]
        } else if init >= s.len() {
            state.push_nil();
            return Ok(1);
        } else {
            &s[..]
        };

        match LuaPattern::new_try(&pattern) {
            Ok(mut m) => {
                if m.matches(search_str) {
                    let captures = m.captures(search_str);
                    if captures.len() > 1 {
                        // Has capture groups - return them (skip full match at index 0)
                        for cap in captures.iter().skip(1) {
                            state.push_string(cap.to_string());
                        }
                        Ok((captures.len() - 1) as u8)
                    } else {
                        // No capture groups - return the full match
                        state.push_string(captures[0].to_string());
                        Ok(1)
                    }
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
    // Returns an iterator function that returns successive matches.
    // For generic for loop: returns iterator, state_table, nil
    add_fn!("gmatch", |state| {
        state.check_type(1, LuaType::String)?;
        state.check_type(2, LuaType::String)?;

        let s = state.to_string(1)?;
        let pattern = state.to_string(2)?;

        state.set_top(0);

        // Handle empty pattern - return iterator that yields empty string once
        if pattern.is_empty() {
            // Return a simple iterator that returns empty string once then nil
            state.new_table();
            state.push_boolean(false); // done flag
            state.push_string("done".to_string());
            state.set_table_raw(-3)?;

            state.push_rust_fn(|state| {
                state.push_string("done".to_string());
                state.get_table(1)?;
                if state.to_boolean(-1) {
                    state.set_top(0);
                    state.push_nil();
                    return Ok(1);
                }
                state.pop(1);
                // Mark as done
                state.push_boolean(true);
                state.push_string("done".to_string());
                state.set_table_raw(1)?;
                state.set_top(0);
                state.push_string(String::new());
                Ok(1)
            });

            state.push_value(-2)?;
            state.remove(-3)?;
            state.push_nil();
            return Ok(3);
        }

        // Create a state table: { s = string, p = pattern, pos = 0 }
        state.new_table();
        state.push_string(s);
        state.push_string("s".to_string());
        state.set_table_raw(-3)?;
        state.push_string(pattern);
        state.push_string("p".to_string());
        state.set_table_raw(-3)?;
        state.push_number(0.0);
        state.push_string("pos".to_string());
        state.set_table_raw(-3)?;

        // Push the iterator function
        state.push_rust_fn(|state| {
            // Stack: [state_table, control_var (ignored)]
            state.check_type(1, LuaType::Table)?;

            // Get s, p, pos from state table
            state.push_string("s".to_string());
            state.get_table(1)?;
            let s = state.to_string(-1)?;
            state.pop(1);

            state.push_string("p".to_string());
            state.get_table(1)?;
            let pattern = state.to_string(-1)?;
            state.pop(1);

            state.push_string("pos".to_string());
            state.get_table(1)?;
            let pos = state.to_number(-1).unwrap_or(0.0) as usize;
            state.pop(1);

            if pos >= s.len() {
                state.set_top(0);
                state.push_nil();
                return Ok(1);
            }

            let search_str = &s[pos..];

            match LuaPattern::new_try(&pattern) {
                Ok(mut m) => {
                    if m.matches(search_str) {
                        let range = m.range();
                        let captures = m.captures(search_str);

                        // Update position in state table for next iteration
                        let new_pos = pos + range.end.max(1);
                        state.push_number(new_pos as f64);
                        state.push_string("pos".to_string());
                        state.set_table_raw(1)?;

                        state.set_top(0);

                        if captures.len() > 1 {
                            // Has capture groups
                            for cap in captures.iter().skip(1) {
                                state.push_string(cap.to_string());
                            }
                            Ok((captures.len() - 1) as u8)
                        } else {
                            // No captures - return full match
                            state.push_string(captures[0].to_string());
                            Ok(1)
                        }
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

        // Stack: [state_table, iterator]
        // Need to return: [iterator, state_table, nil]
        state.push_value(-2)?; // push state_table copy
        state.remove(-3)?; // remove original state_table
        // Stack: [iterator, state_table]
        state.push_nil(); // initial control var

        Ok(3) // iterator, state_table, nil
    });

    // string.gsub(s, pattern, repl [, n])
    // Returns a copy of s with up to n occurrences of pattern replaced by repl.
    // repl can be a string, table, or function.
    // Also returns the total number of matches as a second result.
    add_fn!("gsub", |state| {
        state.check_type(1, LuaType::String)?;
        state.check_type(2, LuaType::String)?;
        state.check_any(3)?;
        let num_args = state.get_top();

        let s = state.to_string(1)?;
        let pattern = state.to_string(2)?;
        let max_replacements = if num_args >= 4 {
            state.check_type(4, LuaType::Number)?;
            Some(state.to_number(4)? as usize)
        } else {
            None
        };

        let repl_type = state.typ(3);

        // Handle empty pattern - no replacement, return original string
        if pattern.is_empty() {
            state.set_top(0);
            state.push_string(s);
            state.push_number(0.0);
            return Ok(2);
        }

        if is_plain_lua_pattern(&pattern) {
            let mut result = String::new();
            let mut pos = 0usize;
            let mut count = 0usize;

            while pos < s.len() {
                if let Some(max) = max_replacements
                    && count >= max
                {
                    break;
                }

                let search_str = &s[pos..];
                let Some(match_start) = search_str.find(&pattern) else {
                    break;
                };

                let start = pos + match_start;
                let end = start + pattern.len();
                result.push_str(&s[pos..start]);

                let captures = [&s[start..end]];
                let replacement = gsub_replacement(state, &repl_type, &captures)?;
                result.push_str(&replacement);

                pos = end;
                count += 1;
            }

            result.push_str(&s[pos..]);

            state.set_top(0);
            state.push_string(result);
            state.push_number(count as f64);
            return Ok(2);
        }

        match LuaPattern::new_try(&pattern) {
            Ok(mut m) => {
                let mut result = String::new();
                let mut pos = 0usize;
                let mut count = 0usize;

                while pos < s.len() {
                    if let Some(max) = max_replacements
                        && count >= max
                    {
                        break;
                    }

                    let search_str = &s[pos..];
                    if m.matches(search_str) {
                        let range = m.range();
                        let captures = m.captures(search_str);

                        // Append text before match
                        result.push_str(&s[pos..pos + range.start]);

                        // Get replacement based on repl type
                        let replacement = gsub_replacement(state, &repl_type, &captures)?;

                        result.push_str(&replacement);
                        pos += range.end.max(1); // Advance at least 1
                        count += 1;
                    } else {
                        break;
                    }
                }

                // Append remaining text
                result.push_str(&s[pos..]);

                state.set_top(0);
                state.push_string(result);
                state.push_number(count as f64);
                Ok(2)
            }
            Err(_) => {
                state.set_top(0);
                state.push_string(s);
                state.push_number(0.0);
                Ok(2)
            }
        }
    });

    // Set the string table as a global
    state.set_global("string");
}
