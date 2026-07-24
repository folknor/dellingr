//! Lua's Table Library

use crate::LuaType;
use crate::State;
use crate::error::{ErrorKind, TypeError};

pub(crate) fn open_table(state: &mut State) {
    // Create the table table
    state.new_table_with_capacity(8);

    // Helper to add a function to the table at stack index -1.
    macro_rules! add_fn {
        ($name:expr, $func:expr) => {
            #[cfg(feature = "snapshot")]
            state
                .set_table_str_key_named_rust_fn(-1, $name, concat!("table.", $name), $func)
                .expect("table library registration cannot fail");
            #[cfg(not(feature = "snapshot"))]
            state
                .set_table_str_key_rust_fn(-1, $name, $func)
                .expect("table library registration cannot fail");
        };
    }

    // table.insert(t, [pos,] value) - costs 1
    // Inserts value at position pos in t, shifting elements up.
    // If pos is omitted, inserts at the end.
    add_fn!("insert", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Table)?;
        let num_args = state.get_top();

        if num_args == 2 {
            // table.insert(t, value) - append at end
            let len = state.table_len(1);
            state.table_insert_at(1, len + 1)?;
        } else if num_args == 3 {
            // table.insert(t, pos, value)
            state.check_type(2, LuaType::Number)?;
            let pos = table_position(state, 2, "insert")?;
            let len = state.table_len(1) as i64;
            if !(1..=len + 1).contains(&pos) {
                return Err(position_out_of_bounds(state, "insert"));
            }
            state.table_insert_at(1, pos as usize)?;
        } else {
            return Err(state.error(ErrorKind::RuntimeError(
                "wrong number of arguments to 'insert'".to_string(),
            )));
        }

        state.set_top(0);
        Ok(0)
    });

    // table.remove(t [, pos]) - costs 1
    // Removes and returns the element at position pos.
    // If pos is omitted, removes the last element.
    add_fn!("remove", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Table)?;
        let num_args = state.get_top();

        let len = state.table_len(1);
        let len_i = len as i64;
        let pos = if num_args >= 2 && state.typ(2) != LuaType::Nil {
            state.check_type(2, LuaType::Number)?;
            let pos = table_position(state, 2, "remove")?;
            let valid = if len == 0 {
                pos == 0 || pos == 1
            } else {
                (1..=len_i + 1).contains(&pos)
            };
            if !valid {
                return Err(position_out_of_bounds(state, "remove"));
            }
            pos as usize
        } else {
            len
        };

        state.set_top(1);
        // Stack: [t]
        state.table_remove_at(1, pos)?;
        // Stack: [t, removed_value]
        state.remove(1)?; // Remove table, leave value
        Ok(1)
    });

    // table.sort(t [, comp]) - costs n (array length)
    // Sorts the array portion of t in place.
    // comp is an optional comparison function.
    add_fn!("sort", |state| {
        state.check_type(1, LuaType::Table)?;
        let num_args = state.get_top();
        let has_comp = num_args >= 2;

        if has_comp {
            state.check_type(2, LuaType::Function)?;
        }

        let n = state.table_sort(1, has_comp)?;
        state.consume_cost(n.max(1) as u64)?;
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

        // j >= i here (the i > j case returned above), so the span cannot
        // underflow. Bound it BEFORE the `+ 1` so a huge j (e.g. from 1e300)
        // cannot overflow the count and slip past the ceiling check.
        let span = j - i;
        if span >= 255 {
            return Err(state.error(ErrorKind::RuntimeError("too many results to unpack".into())));
        }
        for idx in i..=j {
            state.push_number(idx as f64);
            state.get_table(1)?;
        }

        // Stack: [t, list[i], list[i+1], ..., list[j]]
        state.remove(1)?; // Remove table
        let count = span + 1;
        Ok(count as u8)
    });

    // table.pack(...) - costs 1
    // Returns a new table with all arguments stored into keys 1, 2, etc.
    // and with a field "n" with the total number of arguments.
    add_fn!("pack", |state| {
        state.consume_cost(1)?;
        let num_args = state.get_top();

        // Create new table
        state.new_table();
        let table_idx = state.get_top() as isize;

        // Insert all arguments into the table
        for i in 1..=num_args {
            state.push_number(i as f64); // push the index (key)
            state.push_value(i as isize)?; // push the argument (value)
            state.set_table_raw(table_idx)?;
        }

        // Add the "n" field
        state.push_string("n");
        state.push_number(num_args as f64);
        state.set_table_raw(table_idx)?;

        // Remove all original arguments, leave just the table
        // Table is at position (num_args + 1)
        for _ in 0..num_args {
            state.remove(1)?;
        }

        Ok(1)
    });

    // table.concat(list [, sep [, i [, j]]]) - costs 1
    // Returns list[i]..sep..list[i+1]..sep..list[j].
    // Default: sep="", i=1, j=#list
    add_fn!("concat", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Table)?;
        let num_args = state.get_top();

        let len = state.table_len(1);

        let sep = if num_args >= 2 {
            state.check_type(2, LuaType::String)?;
            state.to_bytes(2)?.to_vec()
        } else {
            Vec::new()
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
            state.push_bytes(b"");
            return Ok(1);
        }

        let mut result = Vec::new();
        for idx in i..=j {
            if idx > i {
                result.extend_from_slice(&sep);
            }
            state.push_number(idx as f64);
            state.get_table(1)?;
            let typ = state.typ(-1);
            match typ {
                LuaType::String | LuaType::Number => {
                    result.extend_from_slice(state.to_bytes_coerce(-1)?.as_ref());
                }
                _ => return Err(state.error(ErrorKind::TypeError(TypeError::Concat(typ)))),
            }
            state.pop(1);
        }

        state.set_top(0);
        state.push_bytes(result);
        Ok(1)
    });

    // table.move(a1, f, e, t [, a2]) - costs 1
    // Moves elements from table a1 to table a2 (or a1 if not given).
    // Copies a1[f..e] to a2[t..t+(e-f)]. Returns a2.
    add_fn!("move", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Table)?;
        state.check_type(2, LuaType::Number)?;
        state.check_type(3, LuaType::Number)?;
        state.check_type(4, LuaType::Number)?;

        let f = state.to_number(2)? as isize;
        let e = state.to_number(3)? as isize;
        let t = state.to_number(4)? as isize;

        // Determine destination table (a2 or a1)
        let has_a2 = state.get_top() >= 5 && state.typ(5) != LuaType::Nil;
        if has_a2 {
            state.check_type(5, LuaType::Table)?;
        }
        let dest_idx: isize = if has_a2 { 5 } else { 1 };

        // Copy elements (if any). Same-table moves that shift the range right
        // must copy backwards so earlier writes do not clobber later reads.
        if f <= e {
            let count = e - f + 1;
            let same_table = state.raw_equal(1, dest_idx);
            if same_table && t > f {
                for i in (0..count).rev() {
                    let src_key = (f + i) as f64;
                    let dest_key = (t + i) as f64;

                    state.push_number(dest_key);
                    state.push_number(src_key);
                    state.get_table(1)?;
                    state.set_table_raw(dest_idx)?;
                }
            } else {
                for i in 0..count {
                    let src_key = (f + i) as f64;
                    let dest_key = (t + i) as f64;

                    state.push_number(dest_key);
                    state.push_number(src_key);
                    state.get_table(1)?;
                    state.set_table_raw(dest_idx)?;
                }
            }
        }

        // Return destination table - push it first, then clear below it
        state.push_value(dest_idx)?;
        // Now move it to position 1 and clear rest
        state.replace(1)?;
        state.set_top(1);
        Ok(1)
    });

    // Set the table table as a global
    state.set_global("table");
}

/// Validates that argument `arg_number` is an integer-valued number and returns
/// it as `i64`. A negative or out-of-range value is a valid integer and is left
/// for the caller's bounds check to report as "position out of bounds".
fn table_position(state: &mut State, arg_number: isize, func_name: &str) -> crate::Result<i64> {
    let number = state.to_number(arg_number)?;
    let minimum = i64::MIN as f64;
    let maximum = f64::from_bits((i64::MAX as f64).to_bits() - 1);
    let outside_i64 = number.total_cmp(&minimum).is_lt() || number.total_cmp(&maximum).is_gt();
    if !number.is_finite() || outside_i64 || number.trunc().to_bits() != number.to_bits() {
        return Err(state.error(ErrorKind::RuntimeError(format!(
            "bad argument #{arg_number} to '{func_name}' (number has no integer representation)"
        ))));
    }
    Ok(number as i64)
}

fn position_out_of_bounds(state: &State, func_name: &str) -> crate::error::Error {
    state.error(ErrorKind::RuntimeError(format!(
        "bad argument #2 to '{func_name}' (position out of bounds)"
    )))
}
