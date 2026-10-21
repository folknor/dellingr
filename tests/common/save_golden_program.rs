//! The program whose saved bytes are pinned by
//! `tests/fixtures/save_golden_current.bin`. Shared by the golden test and the
//! `regenerate_save_golden` example so the two cannot drift apart.

use dellingr::{ArgCount, RetCount, State};

/// Exercises every edge kind the walker can take: several globals, an
/// object-valued key, an object-valued value, a shared reference reached twice,
/// a cycle, a metatable, a closure with multiple upvalues, and strings first
/// seen at different depths.
const GOLDEN_PROGRAM: &str = r#"
    shared = { tag = "shared" }

    keyed = {}
    keyed[shared] = "by-object-key"

    nested = { inner = { deep = { leaf = "leaf" } } }
    nested.inner.sibling = shared

    cyclic = {}
    cyclic.self = cyclic
    cyclic.other = keyed

    meta = setmetatable({ marked = true }, { __index = shared, name = "meta" })

    function make(a, b)
        local c = a + b
        return function() return a + b + c end
    end
    captured = make(2, 3)

    ordered = { 1, 2, 3, extra = "tail" }
    math.golden_delta = { marker = "environment delta" }
    golden_pointer = tostring(ordered)
"#;

/// Fixture path relative to the crate root.
pub(crate) const GOLDEN_FIXTURE: &str = "tests/fixtures/save_golden_current.bin";

/// Run the golden program on a fresh standard State and save it.
pub(crate) fn golden_bytes() -> Vec<u8> {
    let mut state = State::new();
    state
        .load_string(GOLDEN_PROGRAM)
        .expect("golden program compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("golden program runs");
    state.save_state().expect("golden state saves").bytes
}
