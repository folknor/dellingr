//! Lua's Standard Library

use super::{check_any_named, check_type_named};
use crate::LuaType;
use crate::Result;
use crate::State;
use crate::error::{ArgDetail, ArgError, ErrorKind, PrefixLocation};
use crate::numeral::parse_lua_numeral;

fn digit_value(byte: u8) -> Option<u32> {
    match byte {
        b'0'..=b'9' => Some(u32::from(byte - b'0')),
        b'a'..=b'z' => Some(u32::from(byte - b'a' + 10)),
        b'A'..=b'Z' => Some(u32::from(byte - b'A' + 10)),
        _ => None,
    }
}

fn parse_integer_with_base(bytes: &[u8], base: u32) -> Option<f64> {
    // Trim with the same `isspace` set as `parse_lua_numeral`; `trim_ascii`
    // omits `\v`, which reference Lua accepts.
    let start = bytes
        .iter()
        .position(|&b| !crate::numeral::is_lua_whitespace(b))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|&b| !crate::numeral::is_lua_whitespace(b))
        .map_or(start, |i| i + 1);
    let trimmed = &bytes[start..end];
    let (negative, digits) = match trimmed {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        _ => (false, trimmed),
    };

    if digits.is_empty() {
        return None;
    }

    // Accumulate in f64 deliberately. This is Lua 5.2's `luaB_tonumber`
    // (`n = n * base + digit` in `lua_Number`), so long digit strings round
    // to a float. Lua 5.4 instead wraps a 64-bit integer, which in a VM with
    // no integer subtype would turn `tonumber(("f"):rep(18), 16)` into `-1`:
    // a silently wrong number rather than a rounded one.
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

// Keep a separate callback address for the global name. Active callback-name
// lookup and snapshot reverse lookup are keyed by the Rust function address.
fn base_unpack_values(state: &mut State) -> Result<u8> {
    super::unpack_values(state)
}

pub(crate) fn base_ipairs(state: &mut State) -> Result<u8> {
    check_type_named(state, 1, LuaType::Table, "ipairs")?;
    state.set_top(1)?;
    state.push_rust_fn(base_ipairs_iter)?;
    // Swap the table and function
    state.push_value(1)?;
    state.remove(1)?;
    // Push the initial index
    state.push_number(0.0)?;
    Ok(3)
}

pub(crate) fn base_ipairs_iter(state: &mut State) -> Result<u8> {
    check_type_named(state, 1, LuaType::Table, "ipairs")?;
    check_type_named(state, 2, LuaType::Number, "ipairs")?;
    state.set_top(2)?;
    let old_index = state.to_number(2)?;
    let new_index = old_index + 1.0;
    state.pop(1)?; // pop the old number
    state.push_number(new_index)?;
    state.get_table(1)?;
    // ipairs stops only on nil, not on false
    if state.typ(-1) != LuaType::Nil {
        state.push_number(new_index)?;
        state.replace(1)?; // Replaces the table with the index
        Ok(2)
    } else {
        state.set_top(0)?;
        state.push_nil()?;
        Ok(1)
    }
}

// next(table, key) - Returns the next key-value pair after key, or nil if done.
// If key is nil, returns the first key-value pair.
pub(crate) fn base_next(state: &mut State) -> Result<u8> {
    check_type_named(state, 1, LuaType::Table, "next")?;
    // If no key given, use nil
    if state.get_top() < 2 {
        state.push_nil()?;
    }
    state.set_top(2)?;
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
    check_type_named(state, 1, LuaType::Table, "pairs")?;
    state.set_top(1)?;
    // Return: next function, table, nil
    state.push_rust_fn(base_next)?;
    state.push_value(1)?; // table
    state.push_nil()?; // initial key
    // Stack: [table, next, table, nil]
    // We need to return: [next, table, nil]
    state.remove(1)?; // Remove original table
    Ok(3)
}

pub(crate) fn open_base(state: &mut State) -> Result<()> {
    macro_rules! add {
        ($name:expr, $func:expr) => {
            state.register_lib_global_fn($name, $func)?;
        };
    }

    add!("ipairs", base_ipairs);
    add!("next", base_next);
    add!("pairs", base_pairs);

    // Receives any number of arguments, and prints their values to `stdout`.
    // Output is routed through host callbacks, allowing the game engine to
    // redirect print output to per-script consoles.
    add!("print", |state| {
        let top = state.get_top();
        let mut message = String::new();
        for i in 1..=top {
            let stack_index = isize::try_from(i).expect("the value stack limit fits in isize");
            if i != 1 {
                crate::vm::checked_string_growth(message.len(), 1)?;
                message.push('\t');
            }
            // The host receives `&str`, so each part is converted lossily:
            // every maximal invalid UTF-8 subsequence becomes one 3-byte
            // U+FFFD. Measure the converted length from the raw bytes first
            // and check it against the cap before materializing expanded text.
            // Conversion keeps an already capped raw value plus a `message`
            // whose length never exceeds the cap. The raw bytes come from
            // `bytes_with_tostring_meta`, which also covers `__tostring`
            // results that are byte strings.
            let raw = state.bytes_with_tostring_meta(stack_index)?;
            let lossy_len: usize = raw
                .utf8_chunks()
                .map(|chunk| {
                    chunk.valid().len()
                        + if chunk.invalid().is_empty() {
                            0
                        } else {
                            char::REPLACEMENT_CHARACTER.len_utf8()
                        }
                })
                .sum();
            crate::vm::checked_string_growth(message.len(), lossy_len)?;
            message.reserve(lossy_len);
            for chunk in raw.utf8_chunks() {
                message.push_str(chunk.valid());
                if !chunk.invalid().is_empty() {
                    message.push(char::REPLACEMENT_CHARACTER);
                }
            }
        }
        state.host_print(&message);
        Ok(0)
    });

    // Raises an error with the given message.
    add!("error", |state| {
        let message = if state.get_top() >= 1 {
            state.to_string_with_meta(1)?
        } else {
            "(error raised with no message)".to_string()
        };
        let prefix_location = if state.get_top() < 2 {
            PrefixLocation::Current
        } else {
            let level = super::arg_integer(state, 2, "error")?;
            if level <= 0 {
                PrefixLocation::Suppressed
            } else {
                PrefixLocation::TraceFrame(
                    u32::try_from(level).unwrap_or(u32::MAX).saturating_sub(1),
                )
            }
        };
        Err(state.error(ErrorKind::ScriptError {
            message,
            prefix: prefix_location,
        }))
    });

    // Returns the type of its only argument, coded as a string.
    add!("type", |state| {
        check_any_named(state, 1, "type")?;
        let typ = state.typ(1);
        state.set_top(0)?;
        state.push_string(typ.as_str())?;
        Ok(1)
    });

    // Converts a value to a number, or a string in the given base to a number.
    add!("tonumber", |state| {
        check_any_named(state, 1, "tonumber")?;
        if !state.is_none_or_nil(2) {
            check_type_named(state, 1, LuaType::String, "tonumber")?;
            let base = super::arg_integer(state, 2, "tonumber")?;
            if !(2..=36).contains(&base) {
                return Err(super::argument_error(
                    state,
                    2,
                    "tonumber",
                    ArgDetail::OutOfBounds("base out of range".to_owned()),
                ));
            }

            let base = u32::try_from(base).expect("validated tonumber base fits in u32");
            let num = parse_integer_with_base(state.to_bytes(1)?, base);
            let top =
                isize::try_from(state.get_top()).expect("the value stack limit fits in isize");
            state.pop(top)?;
            if let Some(num) = num {
                state.push_number(num)?;
            } else {
                state.push_nil()?;
            }
            return Ok(1);
        }

        let typ = state.typ(1);
        match typ {
            LuaType::Number => {
                let num = state.to_number(1)?;
                let top =
                    isize::try_from(state.get_top()).expect("the value stack limit fits in isize");
                state.pop(top)?;
                state.push_number(num)?;
                Ok(1)
            }
            LuaType::String => {
                let parsed = parse_lua_numeral(state.to_bytes(1)?);
                state.set_top(0)?;
                if let Some(num) = parsed {
                    state.push_number(num)?;
                } else {
                    state.push_nil()?;
                }
                Ok(1)
            }
            _ => {
                let top =
                    isize::try_from(state.get_top()).expect("the value stack limit fits in isize");
                state.pop(top)?;
                state.push_nil()?;
                Ok(1)
            }
        }
    });

    // Converts any value to its string representation.
    add!("tostring", |state| {
        check_any_named(state, 1, "tostring")?;
        if state.typ(1) == LuaType::String {
            state.set_top(1)?;
        } else {
            // Byte-exact: a `__tostring` result may hold non-UTF-8 bytes,
            // which reference Lua returns unchanged.
            let bytes = state.bytes_with_tostring_meta(1)?;
            state.set_top(0)?;
            state.push_bytes(bytes)?;
        }
        Ok(1)
    });

    // unpack(list [, i [, j]])
    //
    // Returns list[i], list[i+1], ..., list[j]. Defaults to 1..#list,
    // matching Lua 5.2's global unpack.
    add!("unpack", base_unpack_values);

    // getmetatable(object)
    // Returns the metatable of the given table, or nil if it doesn't have one.
    add!("getmetatable", |state| {
        check_any_named(state, 1, "getmetatable")?;
        state.set_top(1)?;
        raw_metafield(state, 1)?;
        // Stack: [object, metatable_or_nil]
        state.remove(1)?;
        Ok(1)
    });

    // setmetatable(table, metatable)
    // Sets the metatable for the given table. metatable can be nil to remove it.
    // Returns the table.
    add!("setmetatable", |state| {
        check_type_named(state, 1, LuaType::Table, "setmetatable")?;
        check_any_named(state, 2, "setmetatable")?;
        if !matches!(state.typ(2), LuaType::Table | LuaType::Nil) {
            return Err(arg_type_error(state, 2, "setmetatable", LuaType::Table));
        }
        // Ignore any extra arguments (Lua does) so set_metatable_of below reads
        // the replacement metatable, not a trailing argument.
        state.set_top(2)?;
        if raw_metafield(state, 1)? {
            return Err(state.error(ErrorKind::RuntimeError(
                "cannot change a protected metatable".to_string(),
            )));
        }
        state.pop(1)?;
        // Stack: [table, metatable]
        state.set_metatable_of(1)?;
        // Stack: [table]
        Ok(1)
    });

    // rawget(table, index)
    // Gets the value of table[index] without invoking __index metamethod.
    add!("rawget", |state| {
        check_type_named(state, 1, LuaType::Table, "rawget")?;
        check_any_named(state, 2, "rawget")?;
        state.set_top(2)?;
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
    add!("rawset", |state| {
        check_type_named(state, 1, LuaType::Table, "rawset")?;
        check_any_named(state, 2, "rawset")?;
        check_any_named(state, 3, "rawset")?;
        state.set_top(3)?;
        // Stack: [table, key, value] - exactly what set_table_raw expects.
        state.set_table_raw(1)?;
        // Stack: [table]
        Ok(1)
    });

    // rawequal(v1, v2)
    // Returns true if v1 and v2 are primitively equal (without __eq metamethod).
    add!("rawequal", |state| {
        check_any_named(state, 1, "rawequal")?;
        check_any_named(state, 2, "rawequal")?;
        let equal = state.raw_equal(1, 2);
        state.set_top(0)?;
        state.push_boolean(equal)?;
        Ok(1)
    });

    // rawlen(v)
    // Returns the length of a table or string without invoking __len metamethod.
    add!("rawlen", |state| {
        check_any_named(state, 1, "rawlen")?;
        let typ = state.typ(1);
        let len = match typ {
            LuaType::String => state.to_bytes(1)?.len(),
            LuaType::Table => state.table_len(1),
            _ => {
                return Err(super::argument_error(
                    state,
                    1,
                    "rawlen",
                    ArgDetail::Custom("table or string expected".to_owned()),
                ));
            }
        };
        state.set_top(0)?;
        state.push_number(len as f64)?;
        Ok(1)
    });

    // select(index, ...)
    // If index is a number, returns all arguments after argument number index.
    // If index is "#", returns the total number of extra arguments.
    add!("select", |state| {
        check_any_named(state, 1, "select")?;
        let num_args = state.get_top();

        // Check if first arg is "#"
        if state.typ(1) == LuaType::String && state.to_bytes(1)? == b"#" {
            // Return count of remaining args
            state.set_top(0)?;
            state.push_number((num_args - 1) as f64)?;
            return Ok(1);
        }

        let raw_index = super::arg_integer(state, 1, "select")?;
        let vararg_count = num_args - 1;
        let vararg_count_i64 =
            i64::try_from(vararg_count).expect("Lua argument stack limit fits in i64");
        let resolved_index = if raw_index < 0 {
            vararg_count_i64 + raw_index + 1
        } else {
            raw_index
        };
        if raw_index == 0 || (raw_index < 0 && resolved_index < 1) {
            return Err(super::argument_error(
                state,
                1,
                "select",
                ArgDetail::OutOfBounds("index out of range".to_owned()),
            ));
        }

        if raw_index > vararg_count_i64 {
            // A positive index beyond the supplied arguments returns nothing.
            state.set_top(0)?;
            return Ok(0);
        }
        let index = usize::try_from(resolved_index)
            .expect("select index is bounded by the argument stack length");

        // Return args from index onwards (1-based, so index 1 = first vararg)
        // Stack: [index, arg1, arg2, ..., argN]
        // We want to return args starting from position (1 + index)
        let start_pos = 1 + index; // 1-based position in stack
        let count = vararg_count - index + 1;

        // Drop the args before start_pos in one move. Removing them one at a
        // time shifted the whole frame per argument, O(n^2). With the single
        // move, select does the same O(n) value copying as passing `...` on,
        // which the VM does not charge per value either, so select stays free.
        state.remove_bottom(start_pos - 1)?;

        crate::error::results_count(count).map_err(|kind| state.error(kind))
    });

    // _G - Global environment table (proxy with metamethods)
    state.new_table()?; // _G table
    state.new_table()?; // metatable

    // __index: function(t, k) return globals[k] end
    state.register_lib_fn(-1, "_G", "__index", global_env_index)?;

    // __newindex: function(t, k, v) globals[k] = v end
    state.register_lib_fn(-1, "_G", "__newindex", global_env_newindex)?;

    // Address the _G table relative to the top: a host may call open_libs
    // with values already on the stack, so absolute index 1 is not ours.
    state.set_metatable_of(-2)?;
    state.set_global("_G")?;
    Ok(())
}

/// Pushes the raw metatable when unprotected, otherwise its `__metatable` value.
fn raw_metafield(state: &mut State, idx: isize) -> Result<bool> {
    state.get_metatable_of(idx)?;
    if state.typ(-1) == LuaType::Nil {
        return Ok(false);
    }
    state.push_value(-1)?;
    state.push_string("__metatable")?;
    state.get_table_raw(-2)?;
    if state.typ(-1) != LuaType::Nil {
        state.remove(-2)?;
        state.remove(-2)?;
        Ok(true)
    } else {
        state.pop(2)?;
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
        detail: crate::error::ArgDetail::TypeMismatch,
        expected: Some(expected),
        received: Some(state.typ(arg_number)),
    }))
}

fn global_env_index(state: &mut State) -> Result<u8> {
    check_any_named(state, 2, "_G.__index")?;
    if state.typ(2) == LuaType::String {
        let key = state.to_string(2)?;
        state.set_top(0)?;
        state.get_global(&key)?;
    } else {
        state.push_value(2)?;
        state.get_table_raw(1)?;
    }
    Ok(1)
}

fn global_env_newindex(state: &mut State) -> Result<u8> {
    check_any_named(state, 2, "_G.__newindex")?;
    check_any_named(state, 3, "_G.__newindex")?;
    if state.typ(2) == LuaType::String {
        let key = state.to_string(2)?;
        state.push_value(3)?;
        state.set_global(&key)?;
    } else {
        state.push_value(2)?;
        state.push_value(3)?;
        state.set_table_raw(1)?;
    }
    state.set_top(0)?;
    Ok(0)
}
