//! The program whose saved bytes are pinned by
//! `tests/fixtures/save_golden_current.bin`. Shared by the golden test and the
//! `regenerate_save_golden` example so the two cannot drift apart.

use dellingr::{ArgCount, RetCount, State};

/// Covers graph sharing, cycles, table storage history, iterator state and
/// environment deltas. The codec's unit test checks the decoded coverage.
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
    golden_function_pointer = tostring(print)
    golden_environment_pointer = tostring(math)
    golden_deleted = true
    golden_deleted = nil
    function siblings()
        local cell = 1
        return function() return cell end, function() cell = cell + 1 end
    end
    sibling_read, sibling_write = siblings()
    iterator = string.gmatch("a b", "%a+")
    iterator()
    empty_iterator = string.gmatch("", "")
    dead = { gone = true, kept = true }
    dead.gone = nil
    _G[false] = "boolean global"
    math[1] = "first"
    math[3] = "third"
    local saved_cos = math.cos
    math.cos = nil
    for i = 1, 100 do math["discard" .. i] = true end
    for i = 1, 100 do math["discard" .. i] = nil end
    math.cos = saved_cos
    math.sin = nil
    math.temporary = true
    math.temporary = nil
    setmetatable(math, { golden = true })
    table.golden_delta = true

"#;

/// Fixture path relative to the crate root.
pub(crate) const GOLDEN_FIXTURE: &str = "tests/fixtures/save_golden_current.bin";

/// Run the golden program on a fresh standard State and save it.
pub(crate) fn golden_bytes() -> Vec<u8> {
    let mut state = State::new();
    dellingr::Program::compile(GOLDEN_PROGRAM, None)
        .and_then(|program| state.load(&program))
        .expect("golden program compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("golden program runs");
    state.seal_globals();
    state.save_state().expect("golden state saves").bytes
}

/// Text companion: offsets, exact bytes and printable ASCII expose local edits
/// without replacing the binary codec pin. Generated and checked together.
pub(crate) fn golden_text(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut text = String::new();
    for (row, chunk) in bytes.chunks(16).enumerate() {
        write!(text, "{:08x} ", row * 16).expect("String writes cannot fail");
        for byte in chunk {
            write!(text, "{byte:02x} ").expect("String writes cannot fail");
        }
        text.push('|');
        for byte in chunk {
            text.push(if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '.'
            });
        }
        text.push_str("|\n");
    }
    text
}
