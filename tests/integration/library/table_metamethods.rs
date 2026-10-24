//! The table library's metamethod handling: `__len`, `__index` and
//! `__newindex` in insert/remove/concat/unpack/move, the error paths that
//! cannot be expressed in an example (no `pcall`), and charge parity between
//! the raw fast path and the metamethod path.
//!
//! The success-path behaviour against reference Lua 5.4 lives in
//! `examples/tables/metamethod_sequence.lua`.

use dellingr::error::{ErrorKind, LibraryError, LibraryFunction};
use dellingr::{ArgCount, RetCount, State};

use crate::support;

fn run(code: &str) -> dellingr::Result<()> {
    let mut state = State::new();
    dellingr::Program::compile(code, None).and_then(|program| state.load(&program))?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
}

#[test]
fn non_integer_len_is_rejected() {
    for call in [
        "table.insert(t, 1)",
        "table.remove(t)",
        "table.concat(t)",
        "table.unpack(t)",
        "unpack(t)",
    ] {
        let code =
            format!("local t = setmetatable({{}}, {{__len = function() return 2.5 end}})\n{call}");
        let err = run(&code).unwrap_err();
        assert!(
            err.to_string().contains("object length is not an integer"),
            "{call}: unexpected error {err}"
        );
    }
}

#[test]
fn non_number_len_is_rejected() {
    let err =
        run("local t = setmetatable({}, {__len = function() return {} end})\ntable.concat(t)")
            .unwrap_err();
    assert!(
        err.to_string().contains("object length is not an integer"),
        "{err}"
    );
}

#[test]
fn len_metamethod_drives_unpack_count() {
    let mut state = State::new();
    support::eval(
        &mut state,
        "local t = setmetatable({}, {__len = function() return 3 end, \
             __index = function(_, k) return k * 10 end})\n\
             return table.unpack(t)",
        3,
    );
    assert!((state.to_number(-1).unwrap() - 30.0).abs() < f64::EPSILON);
    assert!((state.to_number(-3).unwrap() - 10.0).abs() < f64::EPSILON);
}

#[test]
fn newindex_self_loop_hits_the_metamethod_depth_limit() {
    let err =
        run("local a = {}\nsetmetatable(a, {__newindex = a})\ntable.insert(a, 1)").unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::MetamethodDepthExceeded { .. }),
        "expected MetamethodDepthExceeded, got {err}"
    );
}

#[test]
fn newindex_non_callable_is_a_type_error() {
    let err = run("local t = setmetatable({}, {__newindex = 5})\ntable.insert(t, 1)").unwrap_err();
    assert!(err.to_string().contains("number"), "{err}");
}

#[test]
fn move_rejects_keys_past_exact_integer_range() {
    // Before the guard, an unbudgeted State looped about 2^62 times here, and
    // keys past 2^53 rounded onto each other.
    let err = run("local t = {1, 2}\ntable.move(t, 1, 2^62, 2)").unwrap_err();
    assert!(err.to_string().contains("exactly representable"), "{err}");
    // A destination range that crosses 2^53 would fold keys together.
    let err = run("local t = {1, 2}\ntable.move(t, 1, 2, 2^53)").unwrap_err();
    assert!(err.to_string().contains("exactly representable"), "{err}");
    // A singleton above 2^53 is still an exact f64 key, so it is accepted.
    run("local t = {1}\ntable.move(t, 1, 1, 2^53 + 2)\nif t[2^53 + 2] ~= 1 then error(\"lost\") end")
        .unwrap();
    // The boundary itself is still exact.
    run("local t = {1}\ntable.move(t, 1, 1, 2^53)\nif t[2^53] ~= 1 then error(\"lost\") end")
        .unwrap();
}

#[test]
fn metamethod_insert_and_remove_reject_oversized_shifts_before_access() {
    let limit = dellingr::limits::TABLE_SORT_MAX_LEN;
    let scripts = [
        (
            LibraryFunction::Insert,
            "sequence shift too large in 'insert'",
            format!(
                "local t = setmetatable({{}}, {{__len = function() return {limit} end, \
                 __index = function() error('insert shift started') end, \
                 __newindex = function() error('insert shift write started') end}})\n\
                 table.insert(t, 1, 0)"
            ),
        ),
        (
            LibraryFunction::Remove,
            "sequence shift too large in 'remove'",
            format!(
                "local t = setmetatable({{}}, {{__len = function() return {} end, \
                 __index = function() error('remove shift started') end, \
                 __newindex = function() error('remove shift write started') end}})\n\
                 table.remove(t, 1)",
                limit + 1
            ),
        ),
    ];

    for (function, message, script) in scripts {
        let err = run(&script).expect_err("an oversized shift is refused before access");
        assert!(
            matches!(
                err.kind(),
                ErrorKind::LibraryError(error @ LibraryError::SequenceShiftTooLarge { .. })
                    if error.function() == Some(function)
            ),
            "expected a shift error naming {function:?}, got {err}"
        );
        assert!(
            err.to_string().contains(message),
            "expected {message:?}, got {err}"
        );
    }
}

#[test]
fn metamethod_insert_allows_large_length_when_shift_distance_is_zero() {
    let limit = dellingr::limits::TABLE_SORT_MAX_LEN;
    run(&format!(
        r#"
        local writes = 0
        local t = setmetatable({{}}, {{
            __len = function() return {limit} + 1 end,
            __newindex = function(_, key, value)
                if key ~= {limit} + 2 or value ~= 9 then error("wrong append") end
                writes = writes + 1
            end,
        }})
        table.insert(t, {limit} + 2, 9)
        if writes ~= 1 then error("append did not reach __newindex") end
        "#
    ))
    .unwrap();
}

#[test]
fn metamethod_path_charges_like_the_raw_path() {
    // `__index` forces the metamethod path; `__tostring` keeps the fast path.
    // Every key read here is present, so no handler runs and the two scripts
    // differ only in which path the library takes.
    for op in [
        "table.insert(t, 1, 0)",
        "table.insert(t, 0)",
        "table.remove(t, 1)",
        "table.remove(t)",
        "table.move(t, 1, 5, 2)",
        "table.concat(t, ',')",
        "table.unpack(t)",
    ] {
        let script = |event: &str| {
            format!(
                "local t = {{1, 2, 3, 4, 5}}\nsetmetatable(t, {{{event} = function() end}})\n{op}"
            )
        };
        assert_eq!(
            support::cost_of(script("__tostring")),
            support::cost_of(script("__index")),
            "{op}: metamethod path charge differs from the fast path"
        );
    }
}

#[test]
fn sequence_len_passes_two_operands_and_accepts_integer_strings() {
    for op in [
        "table.insert(t, 4)",
        "table.insert(t, 1, 4)",
        "table.remove(t)",
        "table.concat(t)",
        "table.unpack(t)",
        "unpack(t)",
    ] {
        run(&format!(
            r##"
            local calls = 0
            local t
            t = setmetatable({{1, 2, 3}}, {{__len = function(...)
                calls = calls + 1
                local a, b = ...
                if select("#", ...) ~= 2 or not rawequal(a, t) or not rawequal(b, t) then
                    error("wrong length operands")
                end
                return "3"
            end}})
            {op}
            if calls ~= 1 then error("wrong length call count") end
            "##
        ))
        .unwrap();
    }
}

#[test]
fn library_newindex_uses_the_shared_depth_limit_across_function_calls() {
    // Spend all but one depth step on a table chain before entering one Lua
    // handler. Its nested library write must inherit the exhausted counter.
    let links = dellingr::limits::MAX_METAMETHOD_DEPTH - 1;
    let err = run(&format!(
        r#"
        local t = {{}}
        local tail = t
        for i = 1, {links} do
            local child = {{}}
            setmetatable(tail, {{__newindex = child}})
            tail = child
        end
        setmetatable(tail, {{__newindex = function(_, _, value)
            table.insert(t, value)
        end}})
        table.insert(t, 1)
        "#
    ))
    .unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::MetamethodDepthExceeded { .. }),
        "expected the shared metamethod limit, got {err}"
    );
}

#[test]
fn library_writes_bypass_newindex_for_existing_keys_in_table_chains() {
    run(r#"
        local calls = 0
        local t = setmetatable({1, 2, 3}, {__newindex = function(t, k, v)
            calls = calls + 1
            rawset(t, k, v)
        end})
        table.insert(t, 1, 9)
        if calls ~= 1 then error("existing insert slots invoked __newindex") end
        table.remove(t, 1)
        if calls ~= 1 then error("existing remove slots invoked __newindex") end
        local sink = setmetatable({"old"}, {__newindex = function()
            error("existing chain slot invoked __newindex")
        end})
        local proxy = setmetatable({}, {__newindex = sink})
        table.insert(proxy, "new")
        if sink[1] ~= "new" or rawget(proxy, 1) ~= nil then error("wrong target") end
        "#)
    .unwrap();
}

#[test]
fn same_table_move_order_depends_on_overlap() {
    for (dest, expected) in [(2, "4,3,2"), (4, "4,5,6"), (10, "10,11,12"), (0, "0,1,2")] {
        run(&format!(
            r#"
            local log = {{}}
            local t = setmetatable({{}}, {{
                __index = function(_, k) return k end,
                __newindex = function(_, k, _) log[#log + 1] = k end,
            }})
            table.move(t, 1, 3, {dest})
            if table.concat(log, ",") ~= "{expected}" then error("wrong move order") end
            "#
        ))
        .unwrap();
    }
}

#[test]
fn negative_len_removals_charge_the_flat_cost() {
    let script = |pos| {
        format!(
            "local t = setmetatable({{}}, {{__len = function() return -2 end, \
             __index = function() return 1 end, __newindex = function() end}}) \
             table.remove(t, {pos})"
        )
    };
    // table.remove charges a flat 1 on the metamethod path too, however many
    // elements shift; a deliberate game-design choice of the cost model.
    assert_eq!(support::cost_of(script(-5)), support::cost_of(script(-4)));
}

#[test]
fn sequence_values_stay_rooted_across_allocating_handlers() {
    let mut state = State::new();
    state
        .push_rust_fn(|state| {
            state.gc_collect();
            Ok(0)
        })
        .unwrap();
    state.set_global("force_gc").unwrap();
    state.gc_set_threshold(1);
    support::run(
        &mut state,
        r#"
            local p = setmetatable({}, {
                __len = function() force_gc(); return 3 end,
                __index = function(_, k)
                    force_gc()
                    return {marker = k, label = "value " .. k}
                end,
                __newindex = function(_, _, value)
                    force_gc()
                    if value and value.label ~= "value " .. value.marker then
                        error("write operand lost")
                    end
                end,
            })
            local removed = table.remove(p, 1)
            if removed.marker ~= 1 then error("removed value lost") end
            local a, b, c = table.unpack(p)
            force_gc()
            if a.marker ~= 1 or b.marker ~= 2 or c.marker ~= 3 then
                error("unpacked value lost")
            end
            table.insert(p, 1, {marker = 99, label = "value 99"})
            table.move(p, 1, 3, 10)
            "#,
    );
}

#[test]
fn insert_checks_length_before_rejecting_argument_count() {
    let err = run(
        "local t = setmetatable({}, {__len = function() error('length first') end}); \
         table.insert(t)",
    )
    .unwrap_err();
    assert!(err.to_string().contains("length first"), "{err}");
}

#[test]
fn sequence_mutations_reject_rounded_keys_before_writing() {
    for (len, op) in [
        ("2^53", "table.insert(t, 'x')"),
        ("2^53 + 2", "table.insert(t, 'x')"),
        ("2^53 + 2", "table.remove(t, 2^53)"),
    ] {
        let mut state = State::new();
        support::load(
            &mut state,
            format!(
                "writes = 0; local t = setmetatable({{}}, {{ \
                 __len = function() return {len} end, \
                 __newindex = function() writes = writes + 1 end }}); {op}"
            ),
        );
        let err = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .unwrap_err();
        assert!(err.to_string().contains("exactly representable"), "{err}");
        state.get_global("writes").unwrap();
        assert_eq!(state.to_number(-1).unwrap(), 0.0);
    }
    // A singleton beyond 2^53 can still be exact. No intermediate keys are used.
    run("local t = setmetatable({[2^53 + 2] = 'x'}, \
         {__len = function() return 2^53 + 2 end}); \
         if table.remove(t) ~= 'x' then error('lost exact singleton') end")
    .unwrap();
}
