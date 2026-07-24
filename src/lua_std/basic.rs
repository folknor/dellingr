//! Lua's Standard Library

use crate::LuaType;
use crate::Result;
use crate::State;
use crate::error::{ArgError, ErrorKind};
use crate::numeral::parse_lua_numeral;

fn digit_value(byte: u8) -> Option<u32> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as u32),
        b'a'..=b'z' => Some((byte - b'a' + 10) as u32),
        b'A'..=b'Z' => Some((byte - b'A' + 10) as u32),
        _ => None,
    }
}

fn parse_integer_with_base(bytes: &[u8], base: u32) -> Option<f64> {
    let trimmed = bytes.trim_ascii();
    let (negative, digits) = match trimmed {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        _ => (false, trimmed),
    };

    if digits.is_empty() {
        return None;
    }

    let mut value = 0.0;
    for byte in digits {
        let digit = digit_value(*byte)?;
        if digit >= base {
            return None;
        }
        value = value * f64::from(base) + f64::from(digit);
    }

    if negative { Some(-value) } else { Some(value) }
}

pub(crate) fn base_ipairs(state: &mut State) -> Result<u8> {
    state.check_type(1, LuaType::Table)?;
    state.set_top(1);
    state.push_rust_fn(base_ipairs_iter);
    // Swap the table and function
    state.push_value(1)?;
    state.remove(1)?;
    // Push the initial index
    state.push_number(0.0);
    Ok(3)
}

pub(crate) fn base_ipairs_iter(state: &mut State) -> Result<u8> {
    state.check_type(1, LuaType::Table)?;
    state.check_type(2, LuaType::Number)?;
    state.set_top(2);
    let old_index = state.to_number(2)?;
    let new_index = old_index + 1.0;
    state.pop(1); // pop the old number
    state.push_number(new_index);
    state.get_table(1)?;
    // ipairs stops only on nil, not on false
    if state.typ(-1) != LuaType::Nil {
        state.push_number(new_index);
        state.replace(1)?; // Replaces the table with the index
        Ok(2)
    } else {
        state.set_top(0);
        state.push_nil();
        Ok(1)
    }
}

// next(table, key) - Returns the next key-value pair after key, or nil if done.
// If key is nil, returns the first key-value pair.
pub(crate) fn base_next(state: &mut State) -> Result<u8> {
    state.check_type(1, LuaType::Table)?;
    // If no key given, use nil
    if state.get_top() < 2 {
        state.push_nil();
    }
    state.set_top(2);
    // Stack: [table, key]
    // table_next pops key and pushes (next_key, next_value) or just nil
    let has_more = state.table_next(1)?;
    // Stack: [table, next_key, next_value?]
    if has_more {
        // Remove the table, return key and value
        state.remove(1)?;
        Ok(2)
    } else {
        // Remove the table, return nil
        state.remove(1)?;
        Ok(1)
    }
}

// pairs(table) - Returns next, table, nil for use with generic for.
pub(crate) fn base_pairs(state: &mut State) -> Result<u8> {
    state.check_type(1, LuaType::Table)?;
    state.set_top(1);
    // Return: next function, table, nil
    state.push_rust_fn(base_next);
    state.push_value(1)?; // table
    state.push_nil(); // initial key
    // Stack: [table, next, table, nil]
    // We need to return: [next, table, nil]
    state.remove(1)?; // Remove original table
    Ok(3)
}

pub(crate) fn open_base(state: &mut State) {
    let mut add = |name, func| {
        #[cfg(feature = "snapshot")]
        state.set_global_stdlib_rust_fn(name, name, func);
        #[cfg(not(feature = "snapshot"))]
        state.set_global_rust_fn(name, func);
    };

    add("ipairs", base_ipairs);
    add("next", base_next);
    add("pairs", base_pairs);

    // Receives any number of arguments, and prints their values to `stdout`.
    // Output is routed through host callbacks, allowing the game engine to
    // redirect print output to per-script consoles.
    add("print", |state| {
        let top = state.get_top();
        let mut parts = Vec::with_capacity(top);
        for i in 1..=top {
            parts.push(state.to_string_with_meta(i as isize)?);
        }
        let message = parts.join("\t");
        state.host_print(&message);
        Ok(0)
    });

    // Raises an error with the given message.
    add("error", |state| {
        let message = if state.get_top() >= 1 {
            state.to_string_with_meta(1)?
        } else {
            "(error raised with no message)".to_string()
        };
        Err(state.error(ErrorKind::InternalError(message)))
    });

    // Returns the type of its only argument, coded as a string.
    add("type", |state| {
        state.check_any(1)?;
        let typ = state.typ(1);
        state.set_top(0);
        state.push_string(typ.as_str());
        Ok(1)
    });

    // Converts a value to a number, or a string in the given base to a number.
    add("tonumber", |state| {
        state.check_any(1)?;
        let num_args = state.get_top();

        if num_args >= 2 && state.typ(2) != LuaType::Nil {
            state.check_type(1, LuaType::String)?;
            state.check_type(2, LuaType::Number)?;
            let base_num = state.to_number(2)?;
            let base = base_num as i64;
            if !base_num.is_finite()
                || (base_num - base as f64).abs() > f64::EPSILON
                || !(2..=36).contains(&base)
            {
                let e = ArgError {
                    arg_number: 2,
                    func_name: Some("tonumber".to_string()),
                    expected: Some(LuaType::Number),
                    received: Some(LuaType::Number),
                };
                return Err(state.error(ErrorKind::ArgError(e)));
            }

            let num = parse_integer_with_base(state.to_bytes(1)?, base as u32);
            state.pop(state.get_top() as isize);
            if let Some(num) = num {
                state.push_number(num);
            } else {
                state.push_nil();
            }
            return Ok(1);
        }

        let typ = state.typ(1);
        match typ {
            LuaType::Number => {
                let num = state.to_number(1)?;
                state.pop(state.get_top() as isize);
                state.push_number(num);
                Ok(1)
            }
            LuaType::String => {
                let parsed = parse_lua_numeral(state.to_bytes(1)?);
                state.set_top(0);
                if let Some(num) = parsed {
                    state.push_number(num);
                } else {
                    state.push_nil();
                }
                Ok(1)
            }
            _ => {
                state.pop(state.get_top() as isize);
                state.push_nil();
                Ok(1)
            }
        }
    });

    // Converts any value to its string representation.
    add("tostring", |state| {
        state.check_any(1)?;
        if state.typ(1) == LuaType::String {
            state.set_top(1);
        } else {
            let s = state.to_string_with_meta(1)?;
            state.set_top(0);
            state.push_string(s);
        }
        Ok(1)
    });

    // unpack(list [, i [, j]])
    //
    // Returns list[i], list[i+1], ..., list[j]. Defaults to 1..#list,
    // matching Lua 5.2's global unpack.
    add("unpack", |state| {
        state.check_type(1, LuaType::Table)?;
        let num_args = state.get_top();
        let len = state.table_len(1);

        let i = if num_args >= 2 {
            state.check_type(2, LuaType::Number)?;
            state.to_number(2)? as usize
        } else {
            1
        };

        let j = if num_args >= 3 {
            state.check_type(3, LuaType::Number)?;
            state.to_number(3)? as usize
        } else {
            len
        };

        state.set_top(1);

        if i > j {
            state.set_top(0);
            return Ok(0);
        }

        let count = j - i + 1;
        for idx in i..=j {
            state.push_number(idx as f64);
            state.get_table(1)?;
        }

        state.remove(1)?;
        Ok(count as u8)
    });

    // getmetatable(object)
    // Returns the metatable of the given table, or nil if it doesn't have one.
    add("getmetatable", |state| {
        state.check_any(1)?;
        state.set_top(1);
        raw_metafield(state, 1)?;
        // Stack: [object, metatable_or_nil]
        state.remove(1)?;
        Ok(1)
    });

    // setmetatable(table, metatable)
    // Sets the metatable for the given table. metatable can be nil to remove it.
    // Returns the table.
    add("setmetatable", |state| {
        state.check_type(1, LuaType::Table)?;
        state.check_any(2)?;
        if !matches!(state.typ(2), LuaType::Table | LuaType::Nil) {
            return Err(arg_type_error(state, 2, "setmetatable", LuaType::Table));
        }
        // Ignore any extra arguments (Lua does) so set_metatable_of below reads
        // the replacement metatable, not a trailing argument.
        state.set_top(2);
        if raw_metafield(state, 1)? {
            return Err(state.error(ErrorKind::RuntimeError(
                "cannot change a protected metatable".to_string(),
            )));
        }
        state.pop(1);
        // Stack: [table, metatable]
        state.set_metatable_of(1)?;
        // Stack: [table]
        Ok(1)
    });

    // rawget(table, index)
    // Gets the value of table[index] without invoking __index metamethod.
    add("rawget", |state| {
        state.check_type(1, LuaType::Table)?;
        state.check_any(2)?;
        state.set_top(2);
        // Stack: [table, key]
        // Use the raw table access (set_table_raw's counterpart)
        state.push_value(2)?; // push key
        state.get_table_raw(1)?;
        // Stack: [table, key, value]
        state.remove(1)?;
        state.remove(1)?;
        Ok(1)
    });

    // rawset(table, index, value)
    // Sets table[index] = value without invoking __newindex metamethod.
    // Returns the table.
    add("rawset", |state| {
        state.check_type(1, LuaType::Table)?;
        state.check_any(2)?;
        state.check_any(3)?;
        state.set_top(3);
        // Stack: [table, key, value] - exactly what set_table_raw expects.
        state.set_table_raw(1)?;
        // Stack: [table]
        Ok(1)
    });

    // rawequal(v1, v2)
    // Returns true if v1 and v2 are primitively equal (without __eq metamethod).
    add("rawequal", |state| {
        state.check_any(1)?;
        state.check_any(2)?;
        let equal = state.raw_equal(1, 2);
        state.set_top(0);
        state.push_boolean(equal);
        Ok(1)
    });

    // rawlen(v)
    // Returns the length of a table or string without invoking __len metamethod.
    add("rawlen", |state| {
        state.check_any(1)?;
        let typ = state.typ(1);
        let len = match typ {
            LuaType::String => state.to_bytes(1)?.len(),
            LuaType::Table => state.table_len(1),
            _ => {
                let e = ArgError {
                    arg_number: 1,
                    func_name: Some("rawlen".to_string()),
                    expected: Some(LuaType::Table),
                    received: Some(typ),
                };
                return Err(state.error(ErrorKind::ArgError(e)));
            }
        };
        state.set_top(0);
        state.push_number(len as f64);
        Ok(1)
    });

    // select(index, ...)
    // If index is a number, returns all arguments after argument number index.
    // If index is "#", returns the total number of extra arguments.
    add("select", |state| {
        state.check_any(1)?;
        let num_args = state.get_top();

        // Check if first arg is "#"
        if state.typ(1) == LuaType::String && state.to_bytes(1)? == b"#" {
            // Return count of remaining args
            state.set_top(0);
            state.push_number((num_args - 1) as f64);
            return Ok(1);
        }

        // Otherwise, first arg should be a number
        state.check_type(1, LuaType::Number)?;
        let raw_index = state.to_number(1)? as isize;
        let vararg_count = num_args - 1;
        let index = if raw_index > 0 {
            raw_index as usize
        } else if raw_index < 0 {
            let index = vararg_count as isize + raw_index + 1;
            if index < 1 {
                let e = ArgError {
                    arg_number: 1,
                    func_name: Some("select".to_string()),
                    expected: Some(LuaType::Number),
                    received: Some(LuaType::Number),
                };
                return Err(state.error(ErrorKind::ArgError(e)));
            }
            index as usize
        } else {
            let e = ArgError {
                arg_number: 1,
                func_name: Some("select".to_string()),
                expected: Some(LuaType::Number),
                received: Some(LuaType::Number),
            };
            return Err(state.error(ErrorKind::ArgError(e)));
        };

        if index > vararg_count {
            // Index beyond args, return nothing
            state.set_top(0);
            return Ok(0);
        }

        // Return args from index onwards (1-based, so index 1 = first vararg)
        // Stack: [index, arg1, arg2, ..., argN]
        // We want to return args starting from position (1 + index)
        let start_pos = 1 + index; // 1-based position in stack
        let count = vararg_count - index + 1;

        // Remove args before start_pos
        for _ in 1..start_pos {
            state.remove(1)?;
        }

        Ok(count as u8)
    });

    // _G - Global environment table (proxy with metamethods)
    state.new_table(); // _G table
    state.new_table(); // metatable

    // __index: function(t, k) return globals[k] end
    #[cfg(feature = "snapshot")]
    state
        .set_table_str_key_named_rust_fn(-1, "__index", "_G.__index", global_env_index)
        .expect("_G metatable __index assignment cannot fail");
    #[cfg(not(feature = "snapshot"))]
    state
        .set_table_str_key_rust_fn(-1, "__index", global_env_index)
        .expect("_G metatable __index assignment cannot fail");

    // __newindex: function(t, k, v) globals[k] = v end
    #[cfg(feature = "snapshot")]
    state
        .set_table_str_key_named_rust_fn(-1, "__newindex", "_G.__newindex", global_env_newindex)
        .expect("_G metatable __newindex assignment cannot fail");
    #[cfg(not(feature = "snapshot"))]
    state
        .set_table_str_key_rust_fn(-1, "__newindex", global_env_newindex)
        .expect("_G metatable __newindex assignment cannot fail");

    state
        .set_metatable_of(1)
        .expect("_G metatable installation cannot fail");
    state.set_global("_G");
}

/// Pushes the raw metatable when unprotected, otherwise its `__metatable` value.
fn raw_metafield(state: &mut State, idx: isize) -> Result<bool> {
    state.get_metatable_of(idx)?;
    if state.typ(-1) == LuaType::Nil {
        return Ok(false);
    }
    state.push_value(-1)?;
    state.push_string("__metatable");
    state.get_table_raw(-2)?;
    if state.typ(-1) != LuaType::Nil {
        state.remove(-2)?;
        state.remove(-2)?;
        Ok(true)
    } else {
        state.pop(2);
        Ok(false)
    }
}

fn arg_type_error(
    state: &State,
    arg_number: isize,
    func_name: &str,
    expected: LuaType,
) -> crate::error::Error {
    state.error(ErrorKind::ArgError(ArgError {
        arg_number,
        func_name: Some(func_name.to_string()),
        expected: Some(expected),
        received: Some(state.typ(arg_number)),
    }))
}

fn global_env_index(state: &mut State) -> Result<u8> {
    state.check_any(2)?;
    let key = state.to_string(2)?;
    state.set_top(0);
    state.get_global(&key);
    Ok(1)
}

fn global_env_newindex(state: &mut State) -> Result<u8> {
    state.check_any(2)?;
    state.check_any(3)?;
    let key = state.to_string(2)?;
    state.push_value(3)?;
    state.set_global(&key);
    state.set_top(0);
    Ok(0)
}
