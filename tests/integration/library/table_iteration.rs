use dellingr::{ArgCount, RetCount, State};

use crate::support;
use crate::support::PushNewRustFn;

#[test]
fn filter_in_place_survives_inline_and_map_tombstones() {
    for size in [4, 5] {
        let code = format!(
            "local t = {{}}; for i = 1, {size} do t[i] = i end; for k in pairs(t) do if k % 2 == 0 then t[k] = nil end end; local total = 0; for _, v in pairs(t) do total = total + v end; return total"
        );
        assert_eq!(
            support::eval_number(&code),
            (1..=size).filter(|i| i % 2 != 0).sum::<usize>() as f64
        );
    }
}

#[test]
fn deleted_controls_and_reinsertion_keep_iteration_correct() {
    assert_eq!(
        support::eval_number(
            "local t={a=1,b=2,c=3,d=4}; local k=next(t); t[k]=nil; t.c=nil; local n=next(t,k); if n ~= 'b' then error('next after a deleted control gave ' .. tostring(n)) end return 1"
        ),
        1.0
    );
    assert_eq!(
        support::eval_number(
            "local t={a=1,b=2,c=3}; t.b=nil; t.b=4; local s=''; for k in pairs(t) do s=s..k end; if s ~= 'acb' then error('reinserted key iterated as ' .. s) end return 1"
        ),
        1.0
    );
}

#[test]
fn collectable_tombstone_key_survives_next_control_lookup() {
    let mut state = State::new();
    state.gc_disable_auto();
    state
        .push_new_rust_fn(|state| {
            state.gc_collect();
            Ok(0)
        })
        .unwrap();
    state.set_global("force_gc").unwrap();
    support::eval(
        &mut state,
        "local t = {}; local a = {}; local b = {}; t[a] = 1; t[b] = 2; local control = next(t); t[control] = nil; force_gc(); if next(t, control) ~= b then error('next from a collected tombstone control lost its place') end return 1",
        1,
    );
    assert_eq!(state.to_number(-1).unwrap(), 1.0);
}

#[test]
fn pairs_cursor_preserves_mutation_and_same_site_recursion_semantics() {
    assert_eq!(
        support::eval_number(
            "local t={a=1,b=2,c=3,d=4}; local s=''; for k in pairs(t) do s=s..k; if k=='a' then t.b=nil; t.e=5 end end; if s ~= 'acde' then error('delete and add during pairs iterated ' .. s) end return 1",
        ),
        1.0
    );
    assert_eq!(
        support::eval_number(
            "local t={a=1,b=2,c=3,d=4}; local s=''; for k in pairs(t) do s=s..k; if k=='a' then t.e=5 end end; if s ~= 'abcde' then error('add during pairs iterated ' .. s) end return 1",
        ),
        1.0
    );
    assert_eq!(
        support::eval_number(
            "local t={a=1,b=2,c=3,d=4,e=5}; local s=''; for k in pairs(t) do s=s..k; if k=='c' then t.a=nil; t.b=nil; t.d=nil; t.f=6 end end; if s ~= 'abcef' then error('deleting several keys during pairs iterated ' .. s) end return 1",
        ),
        1.0
    );
    assert_eq!(
        support::eval_number(
            "local t={a=1,b=2,c=3,d=4}; local s=''; for k in pairs(t) do s=s..k; if k=='a' then t.b=nil; t.b=2 end end; if s ~= 'acdb' then error('delete and reinsert during pairs iterated ' .. s) end return 1",
        ),
        1.0
    );
    assert_eq!(
        support::eval_number(
            "local function walk(t, depth) local s=''; for k in pairs(t) do if depth > 0 then walk(t, depth-1) end s=s..k end return s end; local s = walk({a=1,b=2,c=3}, 1); if s ~= 'abc' then error('same-site recursion iterated ' .. s) end return 1",
        ),
        1.0
    );
}

#[test]
fn pairs_cursor_keeps_invalid_controls_and_costs_unchanged() {
    let mut state = State::new();
    support::run(
        &mut state,
        "local t={a=1,b=2,c=3}; for k in pairs(t) do end; for k in pairs(t) do end",
    );
    assert_eq!(state.cost_used(), 4);

    let mut invalid = State::new();
    support::load(&mut invalid, "local t={a=1}; return next(t, 'missing')");
    let error = invalid
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect_err("invalid next control must take the generic builtin path");
    assert!(error.to_string().contains("invalid key to 'next'"));
}
