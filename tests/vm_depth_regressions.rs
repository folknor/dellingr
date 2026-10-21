use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

#[test]
fn cyclic_call_metamethod_returns_depth_error_and_state_recovers() {
    let mut state = State::new();
    state
        .load_string(
            "local callable = {}; setmetatable(callable, { __call = callable }); callable()",
        )
        .expect("the cyclic __call program should compile");

    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("a cyclic __call chain should hit the metamethod depth limit");
    assert!(matches!(
        error.kind(),
        ErrorKind::MetamethodDepthExceeded { .. }
    ));
    assert_eq!(state.get_top(), 0);

    state
        .load_string("return 7")
        .expect("a second program should compile after the error");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("the state should remain usable after the depth error");
    assert_eq!(
        state.to_number(-1).expect("the result should be numeric"),
        7.0
    );
}

#[test]
fn recursion_through_a_function_valued_call_metamethod_is_not_capped_at_metamethod_depth() {
    // Unoptimized builds use far more native stack per Lua call than the 2MB
    // default test-thread stack allows, so run on a dedicated deep stack.
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(function_valued_call_recursion)
        .expect("spawning the test thread should succeed")
        .join()
        .expect("deep-stack test thread should not panic");
}

fn function_valued_call_recursion() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            local counter = setmetatable({}, {
                __call = function(self, n)
                    if n == 0 then return 0 end
                    return 1 + self(n - 1)
                end,
            })
            return counter(500)
            "#,
        )
        .expect("the recursive callable program should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("recursion below the call-depth limit through __call should succeed");
    assert_eq!(state.to_number(-1).expect("numeric result"), 500.0);
}
