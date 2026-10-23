//! Lua's Standard Library

use super::{check_any_named, check_type_named};
use crate::LuaType;
use crate::Result;
use crate::State;
use crate::error::{ArgDetail, ErrorKind, ErrorValue, LibraryError, PrefixLocation};
use crate::limits::{TONUMBER_BASE_MAX, TONUMBER_BASE_MIN};
use crate::numeral::parse_lua_numeral;
use crate::vm::MetaName;
use crate::vm::TostringRendering;

fn digit_value(byte: u8) -> Option<u32> {
    match byte {
        b'0'..=b'9' => Some(u32::from(byte - b'0')),
        b'a'..=b'z' => Some(u32::from(byte - b'a' + 10)),
        b'A'..=b'Z' => Some(u32::from(byte - b'A' + 10)),
        _ => None,
    }
}

/// Convert a positive `error` level to the stored trace-frame index.
///
/// Levels beyond the trace index range intentionally saturate.
#[allow(clippy::disallowed_methods)]
fn error_trace_frame(level: i64) -> u32 {
    u32::try_from(level).unwrap_or(u32::MAX).saturating_sub(1)
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

// A separate wrapper so the global name gets its own callback address where the
// build keeps one. Active callback-name lookup and snapshot reverse lookup are
// keyed by the Rust function address. Release LTO folds it with
// `table.unpack`'s identical wrapper, so there both resolve to one id.
fn base_unpack_values(state: &mut State) -> Result<u8> {
    // native-call: base_unpack_values->unpack_values
    super::unpack_values(state)
}

pub(crate) fn base_ipairs(state: &mut State) -> Result<u8> {
    check_type_named(state, 1, LuaType::Table, "ipairs")?;
    state.set_top(1)?;
    // `open_base` registered this id once; pushing directly avoids another
    // snapshot registry lookup on every ipairs call.
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
    // native-call: base_ipairs_iter->get_table_unguarded
    state.get_table_unguarded(1)?;
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

// The whole of `_G`'s metatable: protection, and nothing that changes access.
pub(super) const GLOBAL_ENV_PROTECTION_FIELDS: &[(&str, bool)] =
    &[(MetaName::Metatable.as_str(), false)];

pub(super) const NON_BUILTIN_GLOBALS: &[(&str, crate::RustFunc)] = &[
    // Raises an error with the given message.
    ("error", |state| {
        // The value is built before the level argument is validated, so a
        // table's `__tostring` runs (and any error it raises wins) exactly as
        // before the payload was typed. `error()` with no argument raises nil,
        // as `error(nil)` does. `error_value` owns the rule that a string
        // value bypasses `__tostring`; see `State::tostring_rendering` for
        // the opposite rule `tostring` follows.
        let value = if state.get_top() >= 1 {
            // native-call: lib:error->error_value
            state.error_value(1)?
        } else {
            ErrorValue::Nil
        };
        let prefix_location = if state.get_top() < 2 {
            PrefixLocation::Current
        } else {
            let level = super::opt_integer(state, 2, "error")?.unwrap_or(1);
            if level <= 0 {
                PrefixLocation::Suppressed
            } else {
                PrefixLocation::TraceFrame(error_trace_frame(level))
            }
        };
        // Position information is added only to a string, as Lua 5.4 does
        // (5.2 also prefixes numbers). The level is still validated above
        // for every value, as in both references.
        let prefix_location = if matches!(value, ErrorValue::String(_)) {
            prefix_location
        } else {
            PrefixLocation::Suppressed
        };
        Err(state.error(ErrorKind::ScriptError {
            value,
            prefix: prefix_location,
        }))
    }),
];

pub(crate) fn open_base(state: &mut State) -> Result<()> {
    macro_rules! add {
        ($name:expr, $func:expr) => {
            state.register_lib_global_fn($name, $func)?;
        };
    }

    state.register_lib_fn_id("ipairs.iter", base_ipairs_iter)?;
    add!("ipairs", base_ipairs);
    add!("next", base_next);
    add!("pairs", base_pairs);

    // Receives any number of arguments, and prints their values to `stdout`.
    // Output is routed through host callbacks, allowing the game engine to
    // redirect print output to per-script consoles.
    add!("print", |state| {
        let top = state.top_isize();
        let mut message = Vec::new();
        for stack_index in 1..=top {
            if stack_index != 1 {
                crate::limits::checked_string_growth(message.len(), 1)?;
                message.push(b'\t');
            }
            // The host receives the exact bytes, so the joined message is
            // bounded by the string cap on its raw length.
            // native-call: lib:print->tostring_bytes_unguarded
            let part = state.tostring_bytes_unguarded(stack_index)?;
            crate::limits::checked_string_growth(message.len(), part.len())?;
            message.extend_from_slice(&part);
        }
        state.host_print(&message);
        Ok(0)
    });

    for &(name, func) in NON_BUILTIN_GLOBALS {
        state.register_lib_global_fn(name, func)?;
    }

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
        if !state.is_none_or_nil(2)? {
            check_type_named(state, 1, LuaType::String, "tonumber")?;
            let base = super::arg_integer(state, 2, "tonumber")?;
            if !(TONUMBER_BASE_MIN..=TONUMBER_BASE_MAX).contains(&base) {
                return Err(super::argument_error(
                    state,
                    2,
                    "tonumber",
                    ArgDetail::BaseOutOfRange,
                ));
            }

            let base = u32::try_from(base).expect("validated tonumber base fits in u32");
            let num = parse_integer_with_base(state.to_bytes(1)?, base);
            let top = state.top_isize();
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
                let top = state.top_isize();
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
                let top = state.top_isize();
                state.pop(top)?;
                state.push_nil()?;
                Ok(1)
            }
        }
    });

    // Converts any value to its string representation.
    add!("tostring", |state| {
        check_any_named(state, 1, "tostring")?;
        // native-call: lib:tostring->tostring_rendering
        match state.tostring_rendering(1)? {
            // Argument 1 is its own rendering: return it without re-interning.
            TostringRendering::Itself => state.set_top(1)?,
            // Byte-exact: a `__tostring` result may hold non-UTF-8 bytes,
            // which reference Lua returns unchanged.
            TostringRendering::Bytes(bytes) => {
                state.set_top(0)?;
                state.push_bytes(bytes)?;
            }
        }
        Ok(1)
    });

    // unpack(list [, i [, j]])
    //
    // Returns list[i], list[i+1], ..., list[j]. Defaults to 1..#list. The
    // implementation is shared with `table.unpack`; see `unpack_values` for
    // its length and lookup behavior.
    add!("unpack", base_unpack_values);

    // getmetatable(object)
    // Returns the value's metatable (tables or strings), respecting protection.
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
        state.check_type_in(2, &[LuaType::Table, LuaType::Nil], "setmetatable")?;
        // Ignore any extra arguments (Lua does) so set_metatable_of below reads
        // the replacement metatable, not a trailing argument.
        state.set_top(2)?;
        if raw_metafield(state, 1)? {
            return Err(state.library_error(LibraryError::ProtectedMetatable));
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
        state.consume_cost(1)?;
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
            LuaType::Table => state.table_len(1)?,
            _ => {
                return Err(super::argument_error(
                    state,
                    1,
                    "rawlen",
                    ArgDetail::TableOrStringExpected,
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
                ArgDetail::IndexOutOfRange,
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

    // _G - the global table itself, so `pairs(_G)`, `rawget(_G, k)` and
    // `rawset(_G, k, v)` see and change the real globals.
    state.push_global_table()?;
    state.new_table()?; // metatable

    // Protect it with a metatable that changes nothing else. Bare global
    // access is raw, so a script-set `__index` or `__newindex` would apply to
    // `_G.x` but not to `x` and split the two. Reference `_G` has no
    // metatable, so `getmetatable(_G)` is nil there and false here (both
    // falsy), and `setmetatable(_G, mt)` - which reference allows - errors
    // loudly instead.
    for &(name, value) in GLOBAL_ENV_PROTECTION_FIELDS {
        state.push_string(name)?;
        state.push_boolean(value)?;
        state.set_table_raw(-3)?;
    }

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
    state.push_meta_name(MetaName::Metatable)?;
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
