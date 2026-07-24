use dellingr::{ArgCount, RetCount, State};

fn run_number(code: &str) -> f64 {
    let mut state = State::new();
    state.load_string(code).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    state.to_number(-1).unwrap()
}

#[test]
fn filter_in_place_survives_inline_and_map_tombstones() {
    for size in [4, 5] {
        let code = format!(
            "local t = {{}}; for i = 1, {size} do t[i] = i end; for k in pairs(t) do if k % 2 == 0 then t[k] = nil end end; local total = 0; for _, v in pairs(t) do total = total + v end; return total"
        );
        assert_eq!(
            run_number(&code),
            (1..=size).filter(|i| i % 2 != 0).sum::<usize>() as f64
        );
    }
}

#[test]
fn deleted_controls_and_reinsertion_keep_iteration_correct() {
    assert_eq!(
        run_number(
            "local t={a=1,b=2,c=3,d=4}; local k=next(t); t[k]=nil; t.c=nil; local n=next(t,k); return n == 'b' and 1 or 0"
        ),
        1.0
    );
    assert_eq!(
        run_number(
            "local t={a=1,b=2,c=3}; t.b=nil; t.b=4; local s=''; for k in pairs(t) do s=s..k end; return s == 'acb' and 1 or 0"
        ),
        1.0
    );
}

#[test]
fn collectable_tombstone_key_survives_next_control_lookup() {
    let mut state = State::new();
    state.gc_disable_auto();
    state.push_rust_fn(|state| {
        state.gc_collect();
        Ok(0)
    });
    state.set_global("force_gc");
    state
        .load_string("local t = {}; local a = {}; local b = {}; t[a] = 1; t[b] = 2; local control = next(t); t[control] = nil; force_gc(); return next(t, control) == b and 1 or 0")
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 1.0);
}
