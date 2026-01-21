//! Lua's Standard Library

use crate::LuaType;
use crate::State;

pub(crate) fn open_base(state: &mut State) {
    let mut add = |name, func| {
        state.push_rust_fn(func);
        state.set_global(name);
    };

    add("ipairs", |state| {
        state.check_type(1, LuaType::Table)?;
        state.set_top(1);
        state.push_rust_fn(|state| {
            state.check_type(1, LuaType::Table)?;
            state.check_type(2, LuaType::Number)?;
            state.set_top(2);
            let old_index = state.to_number(2).unwrap();
            let new_index = old_index + 1.0;
            state.pop(1); // pop the old number
            state.push_number(new_index);
            state.get_table(1)?;
            if state.to_boolean(-1) {
                state.push_number(new_index);
                state.replace(1); // Replaces the table with the index
                Ok(2)
            } else {
                state.set_top(0);
                state.push_nil();
                Ok(1)
            }
        });
        // Swap the table and function
        state.push_value(1);
        state.remove(1);
        // Push the initial index
        state.push_number(0.0);
        Ok(3)
    });

    // next(table, key) - Returns the next key-value pair after key, or nil if done.
    // If key is nil, returns the first key-value pair.
    add("next", |state| {
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
            state.remove(1);
            Ok(2)
        } else {
            // Remove the table, return nil
            state.remove(1);
            Ok(1)
        }
    });

    // pairs(table) - Returns next, table, nil for use with generic for.
    add("pairs", |state| {
        state.check_type(1, LuaType::Table)?;
        state.set_top(1);
        // Return: next function, table, nil
        state.get_global("next");
        state.push_value(1); // table
        state.push_nil();    // initial key
        // Stack: [table, next, table, nil]
        // We need to return: [next, table, nil]
        state.remove(1); // Remove original table
        Ok(3)
    });

    // Receives any number of arguments, and prints their values to `stdout`.
    add("print", |state| {
        let top = state.get_top();
        let mut first = true;
        for i in 1..=top {
            let s = state.to_string_with_meta(i as isize)?;
            if first {
                print!("{}", s);
                first = false;
            } else {
                print!("\t{}", s);
            }
        }
        println!();
        Ok(0)
    });

    // Returns the type of its only argument, coded as a string.
    add("type", |state| {
        state.check_any(1)?;
        let typ = state.typ(1);
        let type_str = typ.to_string();
        state.pop(state.get_top() as isize);
        state.push_string(type_str);
        Ok(1)
    });

    // Converts a string to a number, returns nil if invalid.
    add("tonumber", |state| {
        state.check_any(1)?;
        let typ = state.typ(1);
        match typ {
            LuaType::Number => {
                let num = state.to_number(1).unwrap();
                state.pop(state.get_top() as isize);
                state.push_number(num);
                Ok(1)
            }
            LuaType::String => {
                let s = state.to_string(1);
                state.pop(state.get_top() as isize);
                if let Ok(num) = s.parse::<f64>() {
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
        let s = state.to_string_with_meta(1)?;
        state.pop(state.get_top() as isize);
        state.push_string(s);
        Ok(1)
    });

    // unpack(list)
    //
    // Returns list[1], list[2], ... list[#list]. The Lua version can take
    // additional arguments to return only part of the list, but that isn't
    // supported yet.
    add("unpack", |state| {
        state.check_type(1, LuaType::Table)?;
        let mut i = 1.0;
        loop {
            state.push_number(i);
            state.get_table(1)?;
            if let LuaType::Nil = state.typ(-1) {
                state.pop(1);
                break;
            } else {
                i += 1.0;
            }
        }
        Ok(i as u8 - 1)
    });

    // getmetatable(object)
    // Returns the metatable of the given table, or nil if it doesn't have one.
    add("getmetatable", |state| {
        state.check_any(1)?;
        state.set_top(1);
        state.get_metatable_of(1);
        // Stack: [object, metatable_or_nil]
        state.remove(1);
        Ok(1)
    });

    // setmetatable(table, metatable)
    // Sets the metatable for the given table. metatable can be nil to remove it.
    // Returns the table.
    add("setmetatable", |state| {
        state.check_type(1, LuaType::Table)?;
        state.set_top(2);
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
        state.push_value(2); // push key
        state.get_table_raw(1)?;
        // Stack: [table, key, value]
        state.remove(1);
        state.remove(1);
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
        // Stack: [table, key, value]
        // Swap key and value for set_table_raw which expects [table, value, key]
        state.push_value(3); // push value
        state.push_value(2); // push key
        state.set_table_raw(1)?;
        // Stack: [table, key, value]
        state.set_top(1);
        Ok(1)
    });

    // select(index, ...)
    // If index is a number, returns all arguments after argument number index.
    // If index is "#", returns the total number of extra arguments.
    add("select", |state| {
        state.check_any(1)?;
        let num_args = state.get_top();

        // Check if first arg is "#"
        if state.typ(1) == LuaType::String && state.to_string(1) == "#" {
            // Return count of remaining args
            state.set_top(0);
            state.push_number((num_args - 1) as f64);
            return Ok(1);
        }

        // Otherwise, first arg should be a number
        state.check_type(1, LuaType::Number)?;
        let index = state.to_number(1)? as isize;

        if index <= 0 {
            // Negative or zero index: return all args (Lua behavior for negative is from end)
            // For simplicity, treat <= 0 as returning all
            state.remove(1);
            return Ok((num_args - 1) as u8);
        }

        let index = index as usize;

        if index > num_args - 1 {
            // Index beyond args, return nothing
            state.set_top(0);
            return Ok(0);
        }

        // Return args from index onwards (1-based, so index 1 = first vararg)
        // Stack: [index, arg1, arg2, ..., argN]
        // We want to return args starting from position (1 + index)
        let start_pos = 1 + index; // 1-based position in stack
        let count = num_args - index;

        // Remove args before start_pos
        for _ in 1..start_pos {
            state.remove(1);
        }

        Ok(count as u8)
    });

    // _G - Global environment table (proxy with metamethods)
    state.new_table(); // _G table
    state.new_table(); // metatable

    // __index: function(t, k) return globals[k] end
    state.push_rust_fn(|state| {
        state.check_any(2)?;
        let key = state.to_string(2);
        state.set_top(0);
        state.get_global(&key);
        Ok(1)
    });
    state.push_string("__index".to_string());
    state.set_table_raw(-3).unwrap();

    // __newindex: function(t, k, v) globals[k] = v end
    state.push_rust_fn(|state| {
        state.check_any(2)?;
        state.check_any(3)?;
        let key = state.to_string(2);
        state.push_value(3);
        state.set_global(&key);
        state.set_top(0);
        Ok(0)
    });
    state.push_string("__newindex".to_string());
    state.set_table_raw(-3).unwrap();

    state.set_metatable_of(1).unwrap();
    state.set_global("_G");
}
