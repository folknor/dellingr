//! Lua's Table Library

use crate::LuaType;
use crate::State;

pub(crate) fn open_table(state: &mut State) {
    // Create the table table
    state.new_table();

    // Helper to add a function to the table at stack index -1
    macro_rules! add_fn {
        ($name:expr, $func:expr) => {
            state.push_rust_fn($func);
            state.push_string($name.to_string());
            state.set_table_raw(-3).unwrap();
        };
    }

    // table.insert(t, [pos,] value)
    // Inserts value at position pos in t, shifting elements up.
    // If pos is omitted, inserts at the end.
    add_fn!("insert", |state| {
        state.check_type(1, LuaType::Table)?;
        let num_args = state.get_top();

        if num_args == 2 {
            // table.insert(t, value) - append at end
            let len = state.table_len(1);
            // Stack: [t, value]
            state.push_number((len + 1) as f64);
            // Stack: [t, value, pos]
            // Swap to get [t, pos, value]
            state.insert(-2);
            state.table_insert_at(1)?;
        } else if num_args >= 3 {
            // table.insert(t, pos, value)
            state.check_type(2, LuaType::Number)?;
            // Stack: [t, pos, value]
            state.table_insert_at(1)?;
        }

        state.set_top(0);
        Ok(0)
    });

    // table.remove(t [, pos])
    // Removes and returns the element at position pos.
    // If pos is omitted, removes the last element.
    add_fn!("remove", |state| {
        state.check_type(1, LuaType::Table)?;
        let num_args = state.get_top();

        let pos = if num_args >= 2 {
            state.check_type(2, LuaType::Number)?;
            state.to_number(2)? as usize
        } else {
            state.table_len(1)
        };

        state.set_top(1);
        // Stack: [t]
        state.table_remove_at(1, pos)?;
        // Stack: [t, removed_value]
        state.remove(1); // Remove table, leave value
        Ok(1)
    });

    // table.sort(t [, comp])
    // Sorts the array portion of t in place.
    // comp is an optional comparison function.
    add_fn!("sort", |state| {
        state.check_type(1, LuaType::Table)?;
        let num_args = state.get_top();
        let has_comp = num_args >= 2;

        if has_comp {
            state.check_type(2, LuaType::Function)?;
        }

        state.table_sort(1, has_comp)?;
        state.set_top(0);
        Ok(0)
    });

    // table.unpack(list [, i [, j]])
    // Returns list[i], list[i+1], ..., list[j].
    // Default: i=1, j=#list
    add_fn!("unpack", |state| {
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
        // Stack: [t]

        if i > j {
            state.set_top(0);
            return Ok(0);
        }

        let count = j - i + 1;
        for idx in i..=j {
            state.push_number(idx as f64);
            state.get_table(1)?;
        }

        // Stack: [t, list[i], list[i+1], ..., list[j]]
        state.remove(1); // Remove table
        Ok(count as u8)
    });

    // table.pack(...)
    // Returns a new table with all arguments stored into keys 1, 2, etc.
    // and with a field "n" with the total number of arguments.
    add_fn!("pack", |state| {
        let num_args = state.get_top();

        // Create new table
        state.new_table();
        let table_idx = state.get_top() as isize;

        // Insert all arguments into the table
        for i in 1..=num_args {
            state.push_value(i as isize); // push the argument
            state.push_number(i as f64);  // push the index
            state.set_table_raw(table_idx)?;
        }

        // Add the "n" field
        state.push_number(num_args as f64);
        state.push_string("n".to_string());
        state.set_table_raw(table_idx)?;

        // Remove all original arguments, leave just the table
        // Table is at position (num_args + 1)
        for _ in 0..num_args {
            state.remove(1);
        }

        Ok(1)
    });

    // table.concat(list [, sep [, i [, j]]])
    // Returns list[i]..sep..list[i+1]..sep..list[j].
    // Default: sep="", i=1, j=#list
    add_fn!("concat", |state| {
        state.check_type(1, LuaType::Table)?;
        let num_args = state.get_top();

        let len = state.table_len(1);

        let sep = if num_args >= 2 {
            state.check_type(2, LuaType::String)?;
            state.to_string(2)
        } else {
            String::new()
        };

        let i = if num_args >= 3 {
            state.check_type(3, LuaType::Number)?;
            state.to_number(3)? as usize
        } else {
            1
        };

        let j = if num_args >= 4 {
            state.check_type(4, LuaType::Number)?;
            state.to_number(4)? as usize
        } else {
            len
        };

        state.set_top(1);

        if i > j || len == 0 {
            state.set_top(0);
            state.push_string(String::new());
            return Ok(1);
        }

        let mut result = String::new();
        for idx in i..=j {
            if idx > i {
                result.push_str(&sep);
            }
            state.push_number(idx as f64);
            state.get_table(1)?;
            result.push_str(&state.to_string(-1));
            state.pop(1);
        }

        state.set_top(0);
        state.push_string(result);
        Ok(1)
    });

    // Set the table table as a global
    state.set_global("table");
}
