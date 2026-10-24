//! Lua's Table Library

use super::{arg_integer, check_optional_type_named, check_type_named, opt_integer};
use crate::LuaType;
use crate::Result;
use crate::State;
use crate::error::{ArgDetail, LibraryError, LibraryFunction};
use crate::instr::{ArgCount, RetCount};
use crate::numeral::{MAX_EXACT_INTEGER, exact_i64, parse_lua_numeral};
use crate::vm::MetaName;

// `table.unpack` and global `unpack` are separate catalog entries, so they are
// distinct values with their own save ids whatever this wrapper compiles to
// (release LTO merges it with the global's identical wrapper); the wrapper
// only gives the native-call inventory its own edge.
fn table_unpack_values(state: &mut State) -> crate::Result<u8> {
    crate::native_call!(
        "table_unpack_values->unpack_values",
        super::unpack_values(state)
    )
}

// Keep registration order: it determines table iteration and snapshot baselines.
pub(super) const TABLE_FUNCTIONS: &[(&str, crate::RustFunc)] = &[
    // table.insert(t, [pos,] value) - costs 1
    // Inserts value at position pos in t, shifting elements up.
    // If pos is omitted, inserts at the end.
    //
    // The flat cost of insert and remove is a deliberate game-design choice
    // of the cost model (the budget is not a CPU meter), even though a
    // mid-array call shifts every later element. Charging
    // by shift distance would change costs for existing scripts and require a
    // COST_MODEL_VERSION bump. The metamethod path charges the same flat 1.
    //
    // Both follow Lua 5.4: the length comes from `__len` and every read and
    // write goes through `__index`/`__newindex` (5.4's lua_geti/lua_seti). A
    // table whose metatable defines none of the three takes the in-place
    // storage shift instead; the two paths are observably identical there and
    // charge the same.
    ("insert", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Table, "insert")?;
        let num_args = state.get_top();

        if has_sequence_metamethods(state, 1)? {
            return crate::native_call!(
                "lib:table.insert->insert_through_metamethods",
                insert_through_metamethods(state, num_args)
            );
        }

        if num_args == 2 {
            // table.insert(t, value) - append at end
            let len = state.table_len(1)?;
            state.table_insert_at(1, len + 1)?;
        } else if num_args == 3 {
            // table.insert(t, pos, value)
            let pos = arg_integer(state, 2, "insert")?;
            let len = i64::try_from(state.table_len(1)?).expect("table length fits in i64");
            if !(1..=len + 1).contains(&pos) {
                return Err(position_out_of_bounds(state, "insert"));
            }
            let pos = usize::try_from(pos).expect("validated insertion position is positive");
            state.table_insert_at(1, pos)?;
        } else {
            return Err(state.library_error(LibraryError::WrongNumberOfArguments {
                function: LibraryFunction::Insert,
            }));
        }

        state.set_top(0)?;
        Ok(0)
    }),
    // table.remove(t [, pos]) - costs 1
    // Removes and returns the element at position pos.
    // If pos is omitted, removes the last element.
    //
    // Flat charge: see table.insert.
    ("remove", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Table, "remove")?;
        if has_sequence_metamethods(state, 1)? {
            return crate::native_call!(
                "lib:table.remove->remove_through_metamethods",
                remove_through_metamethods(state)
            );
        }
        let len = state.table_len(1)?;
        let len_i = i64::try_from(len).expect("table length fits in i64");
        let pos = if let Some(pos) = opt_integer(state, 2, "remove")? {
            let valid = if len == 0 {
                pos == 0 || pos == 1
            } else {
                (1..=len_i + 1).contains(&pos)
            };
            if !valid {
                return Err(position_out_of_bounds(state, "remove"));
            }
            usize::try_from(pos).expect("validated removal position is nonnegative")
        } else {
            len
        };

        state.set_top(1)?;
        // Stack: [t]
        state.table_remove_at(1, pos)?;
        // Stack: [t, removed_value]
        state.remove(1)?; // Remove table, leave value
        Ok(1)
    }),
    // table.sort(t [, comp]) - costs n (array length)
    // Sorts the array portion of t in place.
    // comp is an optional comparison function.
    // Follows Lua 5.4: a table whose metatable defines `__len`, `__index` or
    // `__newindex` is sorted through them (length, reads, then writes); any
    // other table takes the raw in-place sort. Both paths charge the length
    // of the original range (at least 1) before any comparator runs.
    ("sort", |state| {
        check_type_named(state, 1, LuaType::Table, "sort")?;
        let has_comp = check_optional_type_named(state, 2, LuaType::Function, "sort")?;

        if has_sequence_metamethods(state, 1)? {
            return crate::native_call!(
                "lib:table.sort->sort_through_metamethods",
                sort_through_metamethods(state, has_comp)
            );
        }

        // Cost is charged inside table_sort BEFORE the comparator runs / the
        // table is mutated, so an exhausted budget blocks the sort.
        // The raw path writes the sorted snapshot back as a whole. Comparator
        // writes to array entries may be overwritten, and a metatable attached
        // during comparison is not used for that writeback. Lua leaves these
        // re-entrant mutations unspecified, so they are not a compatibility
        // guarantee.
        crate::native_call!("lib:table.sort->table_sort", state.table_sort(1, has_comp))?;
        state.set_top(0)?;
        Ok(0)
    }),
    // table.unpack(list [, i [, j]])
    // Returns list[i], list[i+1], ..., list[j].
    // Default: i=1, j=#list
    ("unpack", table_unpack_values),
    // table.pack(...) - costs 1
    // Returns a new table with all arguments stored into keys 1, 2, etc.
    // and with a field "n" with the total number of arguments.
    //
    // The flat charge is a deliberate game-design choice of the cost model
    // (the budget is not a CPU meter), not an oversight: do not charge per
    // argument.
    ("pack", |state| {
        state.consume_cost(1)?;
        let num_args = state.top_isize();

        // Create new table
        state.new_table()?;
        let table_idx = num_args + 1;

        // Insert all arguments into the table
        for i in 1..=num_args {
            state.push_number(i as f64)?; // push the index (key)
            state.push_value(i)?; // push the argument (value)
            state.set_table_raw(table_idx)?;
        }

        // Add the "n" field
        state.push_string("n")?;
        state.push_number(num_args as f64)?;
        state.set_table_raw(table_idx)?;

        // Move the table into slot 1 and drop the arguments in one truncate,
        // instead of O(n^2) remove-from-bottom shuffling.
        if num_args > 0 {
            state.replace(1)?;
            state.set_top(1)?;
        }

        Ok(1)
    }),
    // table.concat(list [, sep [, i [, j]]])
    // Returns list[i]..sep..list[i+1]..sep..list[j].
    // Default: sep="", i=1, j=#list
    // Follows Lua 5.4: `#list` honours `__len` (evaluated even when j is
    // given, as 5.4 does) and elements are read through `__index`. Lua 5.2
    // reads raw.
    ("concat", |state| {
        check_type_named(state, 1, LuaType::Table, "concat")?;
        let len = crate::native_call!(
            "lib:table.concat->sequence_len",
            sequence_len(state, 1, LibraryFunction::Concat)
        )?;

        let sep = if state.is_none_or_nil(2)? {
            Vec::new()
        } else {
            let bytes = if state.typ(2) == LuaType::Number {
                state.bytes_with_default_string_coercion(2)?
            } else {
                check_type_named(state, 2, LuaType::String, "concat")?;
                state.to_bytes(2)?.to_vec()
            };
            state.consume_cost(bytes.len() as u64)?;
            bytes
        };

        let i = opt_integer(state, 3, "concat")?.unwrap_or(1);
        let j = opt_integer(state, 4, "concat")?.unwrap_or(len);

        state.set_top(1)?;

        if i > j {
            if sep.is_empty() {
                state.consume_cost(1)?;
            }
            state.set_top(0)?;
            state.push_bytes(b"")?;
            return Ok(1);
        }

        // Keys past +-2^53 would round onto each other as f64 keys; refuse
        // such ranges as table.move, insert and remove do.
        check_sequence_keys(state, i, j, LibraryFunction::Concat)?;

        let mut result = Vec::new();
        for idx in i..=j {
            if idx > i {
                let next = crate::limits::checked_string_growth(result.len(), sep.len())?;
                state.consume_cost(sep.len() as u64)?;
                result.reserve(next - result.len());
                result.extend_from_slice(&sep);
            }
            state.consume_cost(1)?;
            state.push_number(idx as f64)?;
            crate::native_call!(
                "lib:table.concat->get_table_unguarded",
                state.get_table_unguarded(1)
            )?;
            let typ = state.typ(-1);
            match typ {
                LuaType::String | LuaType::Number => {
                    let bytes = state.bytes_with_default_string_coercion(-1)?;
                    let next = crate::limits::checked_string_growth(result.len(), bytes.len())?;
                    state.consume_cost(bytes.len() as u64)?;
                    result.reserve(next - result.len());
                    result.extend_from_slice(&bytes);
                }
                // Reference wording (5.4.7, and the installed 5.2 agrees):
                // the element's type and its index, not the generic
                // concatenation TypeError, which loses the index.
                _ => {
                    return Err(state.library_error(LibraryError::InvalidConcatValue {
                        value_type: typ,
                        index: idx,
                    }));
                }
            }
            state.pop(1)?;
        }

        state.set_top(0)?;
        state.push_bytes(result)?;
        Ok(1)
    }),
    // table.move(a1, f, e, t [, a2]) - costs one per copied element
    // Moves elements from table a1 to table a2 (or a1 if not given).
    // Copies a1[f..e] to a2[t..t+(e-f)]. Returns a2.
    // Follows Lua 5.4 (the function is absent in 5.2): reads go through
    // `__index` and writes through `__newindex`. When neither table's
    // metatable defines `__len`, `__index` or `__newindex`, both sides are
    // accessed raw, which is observably the same.
    ("move", |state| {
        check_type_named(state, 1, LuaType::Table, "move")?;
        let f = arg_integer(state, 2, "move")?;
        let e = arg_integer(state, 3, "move")?;
        let t = arg_integer(state, 4, "move")?;

        // Determine destination table (a2 or a1)
        let has_a2 = check_optional_type_named(state, 5, LuaType::Table, "move")?;
        let dest_idx: isize = if has_a2 { 5 } else { 1 };

        let count = if f <= e {
            // These are Lua's reference guards. Besides matching its error
            // behavior, they prove the count and destination key arithmetic
            // below cannot overflow.
            if !(f > 0 || e < i64::MAX + f) {
                return Err(super::argument_error(
                    state,
                    3,
                    "move",
                    ArgDetail::TooManyElementsToMove,
                ));
            }
            let count = e - f + 1;
            if t > i64::MAX - count + 1 {
                return Err(super::argument_error(
                    state,
                    4,
                    "move",
                    ArgDetail::DestinationWrapAround,
                ));
            }
            count
        } else {
            0
        };

        // dellingr numbers are f64, so a range is representable only if every
        // key in it stays distinct. A singleton can still be exact above 2^53.
        if count > 0 {
            let dest_last = t + (count - 1);
            if !(check_integer_key_range(f, e) && check_integer_key_range(t, dest_last)) {
                return Err(state.library_error(LibraryError::IntegerKeyRangeExceeded {
                    function: LibraryFunction::Move,
                }));
            }
        }

        let honour_meta = count > 0
            && (has_sequence_metamethods(state, 1)?
                || (has_a2 && has_sequence_metamethods(state, dest_idx)?));

        // Copy elements (if any). Same-table moves that shift the range right
        // must copy backwards so earlier writes do not clobber later reads.
        let same_table = count > 0 && state.raw_equal(1, dest_idx);
        let backwards = same_table && t > f && t <= e;
        if count == 0 {
            state.consume_cost(1)?;
        } else if honour_meta {
            // A handler can fail during a read or write, so charge each
            // attempted metamethod copy immediately before dispatch.
            for step in 0..count {
                let i = if backwards { count - 1 - step } else { step };
                state.consume_cost(1)?;
                crate::native_call!(
                    "lib:table.move->move_element:metamethods",
                    move_element(state, f + i, t + i, dest_idx, honour_meta)
                )?;
            }
        } else {
            // Charge raw copies in one batch for both finite and unlimited
            // budgets, with the outcome of charging 1 per copy: a budget that
            // runs out mid-range still copies the elements it paid for (in
            // copy order) before failing, exactly as the per-element loop
            // would. cost_used and the partial mutation therefore do not
            // depend on whether a budget is configured, and the hot copy loop
            // carries no budget checks.
            let used_before = state.cost_used();
            let paid_all = state
                .cost_meter()
                .consume_units(u64::try_from(count).expect("a nonempty move count is positive"));
            let paid = if paid_all {
                count
            } else {
                i64::try_from(state.cost_used() - used_before)
                    .expect("a partial charge is below the i64 move count")
            };
            if backwards {
                for step in 0..paid {
                    let i = count - 1 - step;
                    crate::native_call!(
                        "lib:table.move->move_element:raw-backward",
                        move_element(state, f + i, t + i, dest_idx, false)
                    )?;
                }
            } else {
                for i in 0..paid {
                    crate::native_call!(
                        "lib:table.move->move_element:raw-forward",
                        move_element(state, f + i, t + i, dest_idx, false)
                    )?;
                }
            }
            if !paid_all {
                return Err(state.budget_exceeded_error());
            }
        }

        // Return destination table - push it first, then clear below it
        state.push_value(dest_idx)?;
        // Now move it to position 1 and clear rest
        state.replace(1)?;
        state.set_top(1)?;
        Ok(1)
    }),
];

pub(crate) fn open_table(state: &mut State) -> Result<()> {
    state.new_table_with_capacity(super::TABLE_LIBRARY_CAPACITY)?;
    super::install_library_functions(state, super::TABLE_SEGMENT)?;

    // Set the table table as a global
    state.set_global("table")?;
    Ok(())
}

/// Check a sequence of integer keys that will be converted to f64. A single
/// key can be exact above 2^53, while a longer range must stay within the
/// largest interval where every integer is exactly representable.
fn check_integer_key_range(first: i64, last: i64) -> bool {
    if first == last {
        exact_i64(first as f64) == Some(first)
    } else {
        (-MAX_EXACT_INTEGER..=MAX_EXACT_INTEGER).contains(&first)
            && (-MAX_EXACT_INTEGER..=MAX_EXACT_INTEGER).contains(&last)
    }
}

/// Reject positions that would round when converted to numeric keys. A
/// singleton may lie outside +-2^53 if that particular integer is exact;
/// a range needs every intervening integer to be exact as well.
pub(super) fn check_sequence_keys(
    state: &State,
    first: i64,
    last: i64,
    function: LibraryFunction,
) -> Result<()> {
    if !check_integer_key_range(first, last) {
        return Err(state.library_error(LibraryError::IntegerKeyRangeExceeded { function }));
    }
    Ok(())
}

/// Copies `src[src_key]` into `dest[dest_key]`, where the source table is at
/// stack index 1 and the destination at `dest_idx`. With `honour_meta` the read
/// goes through `__index` and the write through `__newindex`; without it both
/// are raw. Leaves the stack as it found it.
fn move_element(
    state: &mut State,
    src_key: i64,
    dest_key: i64,
    dest_idx: isize,
    honour_meta: bool,
) -> Result<()> {
    // Both keys were checked for exact f64 representation before this call.
    state.push_number(dest_key as f64)?;
    state.push_number(src_key as f64)?;
    if honour_meta {
        crate::native_call!(
            "move_element->get_table_unguarded",
            state.get_table_unguarded(1)
        )?;
        crate::native_call!("move_element->set_index", set_index(state, dest_idx))
    } else {
        state.get_table_raw(1)?;
        state.set_table_raw(dest_idx)
    }
}

/// Whether the table at stack index `idx` has a metatable defining `__len`,
/// `__index` or `__newindex` - the events that make Lua 5.4's sequence
/// functions (lua_geti/lua_seti/luaL_len) differ from raw access. Tables
/// without one take the raw fast paths. `idx` must be a positive index.
fn has_sequence_metamethods(state: &mut State, idx: isize) -> Result<bool> {
    state.get_metatable_of(idx)?;
    if state.typ(-1) == LuaType::Nil {
        state.pop(1)?;
        return Ok(false);
    }
    let mut found = false;
    for event in [MetaName::Len, MetaName::Index, MetaName::NewIndex] {
        state.push_meta_name(event)?;
        state.get_table_raw(-2)?;
        found = state.typ(-1) != LuaType::Nil;
        state.pop(1)?;
        if found {
            break;
        }
    }
    state.pop(1)?;
    Ok(found)
}

/// Lua 5.4's `luaL_len` for the table at the positive stack index `idx`: calls
/// `__len` when the metatable defines it, else returns the raw border. The
/// metamethod's result must be a number or numeral string with an exact
/// integer value; `function` names the caller in the error raised otherwise.
///
/// This lives here rather than in `State::table_len` (which is raw and used by
/// raw fast paths); a shared metamethod-aware length belongs beside it in
/// `vm/table_ops.rs` once more callers need one.
pub(super) fn sequence_len(
    state: &mut State,
    idx: isize,
    function: LibraryFunction,
) -> Result<i64> {
    // The array part is bounded by the stack and heap caps, far below i64::MAX.
    let raw = i64::try_from(state.table_len(idx)?).map_or(i64::MAX, i64::from);
    state.get_metatable_of(idx)?;
    if state.typ(-1) == LuaType::Nil {
        state.pop(1)?;
        return Ok(raw);
    }
    state.push_meta_name(MetaName::Len)?;
    state.get_table_raw(-2)?;
    state.remove(-2)?; // drop the metatable, keep the handler
    if state.typ(-1) == LuaType::Nil {
        state.pop(1)?;
        return Ok(raw);
    }
    state.push_value(idx)?;
    state.push_value(idx)?;
    crate::native_call!(
        "sequence_len->call_unguarded",
        state.call_unguarded(ArgCount::Fixed(2), RetCount::Fixed(1))
    )?;
    // Lua's length dispatch passes the operand twice, and luaL_len converts
    // the result with lua_tointegerx, including numeral strings.
    let len = match state.typ(-1) {
        LuaType::Number => exact_i64(state.to_number(-1)?),
        LuaType::String => parse_lua_numeral(state.to_bytes(-1)?).and_then(exact_i64),
        _ => None,
    };
    state.pop(1)?;
    len.ok_or_else(|| state.library_error(LibraryError::ObjectLengthNotInteger { function }))
}

/// Lua 5.4's `lua_seti`/`lua_settable` for library code: does `t[k] = v`
/// with `t` at the positive stack index `table_idx` and `k`, `v` on top of the
/// stack, honouring `__newindex` (function or table chain). Pops `k` and `v`.
///
/// Uses the VM's assignment dispatch so nested library and bytecode handlers
/// share the same depth limit. Both operands stay rooted until dispatch ends.
fn set_index(state: &mut State, table_idx: isize) -> Result<()> {
    crate::native_call!("set_index->set_table", state.set_table(table_idx))
}

/// `table.insert` for a table with sequence metamethods, following Lua 5.4's
/// `tinsert`: length via `__len`, shifts through `__index`/`__newindex`.
/// Charges like the raw path: the flat 1 already paid by the caller.
fn insert_through_metamethods(state: &mut State, num_args: usize) -> Result<u8> {
    let first_empty = crate::native_call!(
        "insert_through_metamethods->sequence_len",
        sequence_len(state, 1, LibraryFunction::Insert)
    )?
    .wrapping_add(1);
    // Lua evaluates the length even when the argument count is invalid.
    if num_args != 2 && num_args != 3 {
        return Err(state.library_error(LibraryError::WrongNumberOfArguments {
            function: LibraryFunction::Insert,
        }));
    }
    let pos = if num_args == 3 {
        let pos = arg_integer(state, 2, "insert")?;
        // 5.4's unsigned check: for nonnegative lengths,
        // 1 <= pos <= first_empty.
        if pos.wrapping_sub(1).cast_unsigned() >= first_empty.cast_unsigned() {
            return Err(position_out_of_bounds(state, "insert"));
        }
        pos
    } else {
        first_empty
    };
    // A negative length can admit pos > first_empty, where only pos is written.
    check_sequence_keys(state, pos, first_empty.max(pos), LibraryFunction::Insert)?;
    check_shift_distance(
        state,
        first_empty.saturating_sub(pos),
        LibraryFunction::Insert,
    )?;
    let mut i = first_empty;
    while i > pos {
        state.push_number(i as f64)?;
        state.push_number((i - 1) as f64)?;
        crate::native_call!(
            "insert_through_metamethods->get_table_unguarded",
            state.get_table_unguarded(1)
        )?;
        crate::native_call!(
            "insert_through_metamethods->set_index:shift",
            set_index(state, 1)
        )?;
        i -= 1;
    }
    state.push_number(pos as f64)?;
    // The value is the last argument: index 3 with a position, else 2.
    state.push_value(if num_args == 3 { 3 } else { 2 })?;
    crate::native_call!(
        "insert_through_metamethods->set_index:value",
        set_index(state, 1)
    )?;
    state.set_top(0)?;
    Ok(0)
}

/// `table.remove` for a table with sequence metamethods, following Lua 5.4's
/// `tremove`. Charges like the raw path.
fn remove_through_metamethods(state: &mut State) -> Result<u8> {
    let size = crate::native_call!(
        "remove_through_metamethods->sequence_len",
        sequence_len(state, 1, LibraryFunction::Remove)
    )?;
    let mut pos = opt_integer(state, 2, "remove")?.unwrap_or(size);
    // 5.4's unsigned check: for nonnegative lengths, pos == size or
    // 1 <= pos <= size + 1. A negative length also admits negative positions.
    if pos != size && pos.wrapping_sub(1).cast_unsigned() > size.cast_unsigned() {
        return Err(position_out_of_bounds(state, "remove"));
    }
    check_sequence_keys(state, pos, size.max(pos), LibraryFunction::Remove)?;
    // Before reading the removed value or invoking any shift handler.
    check_shift_distance(state, size.saturating_sub(pos), LibraryFunction::Remove)?;
    state.set_top(1)?;
    state.push_number(pos as f64)?;
    crate::native_call!(
        "remove_through_metamethods->get_table_unguarded:removed",
        state.get_table_unguarded(1)
    )?; // Stack: [t, removed]
    while pos < size {
        state.push_number(pos as f64)?;
        state.push_number((pos + 1) as f64)?;
        crate::native_call!(
            "remove_through_metamethods->get_table_unguarded:shift",
            state.get_table_unguarded(1)
        )?;
        crate::native_call!(
            "remove_through_metamethods->set_index:shift",
            set_index(state, 1)
        )?;
        pos += 1;
    }
    state.push_number(pos as f64)?;
    state.push_nil()?;
    crate::native_call!(
        "remove_through_metamethods->set_index:clear",
        set_index(state, 1)
    )?;
    state.remove(1)?;
    Ok(1)
}

/// `table.sort` for a table with sequence metamethods, following Lua 5.4's
/// `sort`: length via `__len`, elements read through `__index` and written
/// back through `__newindex`. The elements are sorted in a scratch table
/// rooted on the stack, so a comparator can force a collection. Both paths
/// use the same heap sort and comparison routine, so equal inputs have the
/// same order and comparator call sequence. The charge is the original
/// length, at least 1, paid before any read beyond `__len` or comparator call.
fn sort_through_metamethods(state: &mut State, has_comp: bool) -> Result<u8> {
    let len = crate::native_call!(
        "sort_through_metamethods->sequence_len",
        sequence_len(state, 1, LibraryFunction::Sort)
    )?;
    let n = state.checked_table_sort_len(len)?;
    state.consume_cost(len.max(1).cast_unsigned())?;
    if len < 2 {
        state.set_top(0)?;
        return Ok(0);
    }
    state.set_top(if has_comp { 2 } else { 1 })?;
    state.new_table()?;
    let s = state.get_top() as isize;

    for i in 1..=len {
        state.push_number(i as f64)?;
        state.push_number(i as f64)?;
        crate::native_call!(
            "sort_through_metamethods->get_table_unguarded",
            state.get_table_unguarded(1)
        )?;
        state.set_table_raw(s)?;
    }
    crate::native_call!(
        "sort_through_metamethods->table_sort_scratch",
        state.table_sort_scratch(s, n, has_comp)
    )?;

    for i in 1..=len {
        state.push_number(i as f64)?;
        state.push_number(i as f64)?;
        state.get_table_raw(s)?;
        crate::native_call!("sort_through_metamethods->set_table", state.set_table(1))?;
    }
    state.set_top(0)?;
    Ok(0)
}

/// Bound the shifts of a metamethod-path `table.insert` or `table.remove`
/// independently of their flat charge, before any shift handler runs. The
/// exclusive limit is the one `table.sort` uses; the error names `function`.
/// A distance of zero or less (an append, or a negative `__len`) always passes.
fn check_shift_distance(state: &State, distance: i64, function: LibraryFunction) -> Result<()> {
    if distance >= crate::limits::TABLE_SORT_MAX_LEN {
        return Err(state.library_error(LibraryError::SequenceShiftTooLarge { function }));
    }
    Ok(())
}

fn position_out_of_bounds(state: &State, func_name: &str) -> crate::error::Error {
    super::argument_error(state, 2, func_name, ArgDetail::PositionOutOfBounds)
}
