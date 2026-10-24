//! A register-allocation regression: calls whose argument is the result of
//! an immediately called closure, made while twenty locals are live.

use crate::support;

#[test]
fn immediate_closure_call_argument_survives_twenty_live_locals() {
    let (mut state, recorder) = support::Recorder::state();
    support::run(
        &mut state,
        r#"
        local function test(name, condition)
            if condition then
                print("[PASS] " .. name)
            else
                print("[FAIL] " .. name)
                error("Test failed: " .. name)
            end
        end

        local a1,a2,a3,a4,a5,a6,a7,a8,a9,a10,a11,a12,a13,a14,a15,a16,a17,a18,a19,a20

        test("pattern captures", (function()
            local a, b = string.match("hello world", "(%a+) (%a+)")
            return a == "hello" and b == "world"
        end)())

        test("pattern gsub count", (function()
            local result, count = string.gsub("hello", "l", "L")
            return result == "heLLo" and count == 2
        end)())
    "#,
    );
    assert_eq!(
        recorder.prints(),
        vec![
            b"[PASS] pattern captures".to_vec(),
            b"[PASS] pattern gsub count".to_vec(),
        ],
        "the wide-frame call regression must report both checks as passing"
    );
}
