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

/// Structural companion for the trusted fixture, not a decoder for user input.
/// This independent reader deliberately fails when the wire schema changes.
pub(crate) fn golden_text(bytes: &[u8]) -> String {
    let mut reader = FixtureReader {
        bytes: payload_after_header(bytes),
        pos: 0,
    };
    let mut lines = Vec::new();
    lines.push(format!("standard_environment={}", reader.flag()));
    lines.push(format!("rng={:016x}", reader.number(8)));
    // Wire order is remaining, limit, configured, used; both counters are i64.
    lines.push(format!(
        "cost: remaining={} limit={} configured={} used={}",
        reader.number(8).cast_signed(),
        reader.number(8).cast_signed(),
        reader.flag(),
        reader.number(8)
    ));
    for id in 0..reader.count() {
        lines.push(format!("string[{id}]={}", reader.string()));
    }
    for id in 0..reader.count() {
        let start = reader.pos;
        reader.bytecode();
        let encoded = &reader.bytes[start..reader.pos];
        let hash = encoded
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
            });
        lines.push(format!(
            "bytecode[{id}]: bytes={} fnv1a={hash:016x}",
            encoded.len()
        ));
    }
    for id in 0..reader.count() {
        lines.push(format!("upvalue[{id}]={}", reader.value()));
    }
    for id in 0..reader.count() {
        reader.object(&format!("object[{id}]"), &mut lines);
    }
    reader.object("globals", &mut lines);
    lines.push(format!("global_pins={:?}", reader.values()));
    lines.push(format!("globals_sealed={}", reader.flag()));
    for id in 0..reader.count() {
        lines.push(format!("delta[{id}] token={}", reader.string()));
        lines.push(format!("  deleted={:?}", reader.values()));
        reader.entries("  upsert", &mut lines);
        if reader.flag() {
            lines.push(format!("  order={:?}", reader.values()));
        }
        if reader.flag() {
            lines.push(format!("  array_len={}", reader.number(8)));
            reader.entries("  array", &mut lines);
        }
        if reader.flag() {
            reader.entries("  hash_slot", &mut lines);
        }
        let metatable = match reader.number(1) {
            0 => "unchanged".to_owned(),
            1 => "cleared".to_owned(),
            2 => reader.value(),
            _ => panic!("unknown metatable delta in fixture"),
        };
        lines.push(format!("  metatable={metatable}"));
    }
    lines.push(format!("next_pointer_id={}", reader.number(8)));
    for id in 0..reader.count() {
        lines.push(format!(
            "pointer[{id}] {}={}",
            reader.value(),
            reader.number(8)
        ));
    }
    assert_eq!(
        reader.pos,
        reader.bytes.len(),
        "fixture schema consumed exactly"
    );
    lines.join("\n") + "\n"
}

/// Skip compatibility metadata, keeping the exact payload as the binary pin.
pub(crate) fn payload_after_header(bytes: &[u8]) -> &[u8] {
    let length = u32::from_le_bytes(bytes[8..12].try_into().expect("engine label length"));
    &bytes[12 + length as usize..]
}

struct FixtureReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl FixtureReader<'_> {
    fn take(&mut self, n: usize) -> &[u8] {
        let start = self.pos;
        self.pos += n;
        &self.bytes[start..self.pos]
    }

    fn number(&mut self, n: usize) -> u64 {
        self.take(n)
            .iter()
            .enumerate()
            .fold(0, |value, (i, byte)| value | (u64::from(*byte) << (i * 8)))
    }

    fn count(&mut self) -> usize {
        self.number(4) as usize
    }

    fn flag(&mut self) -> bool {
        match self.number(1) {
            0 => false,
            1 => true,
            _ => panic!("invalid fixture bool"),
        }
    }

    fn string(&mut self) -> String {
        let n = self.count();
        let escaped: String = self
            .take(n)
            .iter()
            .flat_map(|byte| std::ascii::escape_default(*byte))
            .map(char::from)
            .collect();
        format!("\"{escaped}\"")
    }

    fn value(&mut self) -> String {
        match self.number(1) {
            0 => "nil".to_owned(),
            1 => "false".to_owned(),
            2 => "true".to_owned(),
            3 => format!("number:{:016x}", self.number(8)),
            4 => format!("string[{}]", self.number(4)),
            5 => format!("object[{}]", self.number(4)),
            6 => format!("function:{}", self.string()),
            7 => format!("environment:{}", self.string()),
            _ => panic!("unknown fixture value tag"),
        }
    }

    fn values(&mut self) -> Vec<String> {
        (0..self.count()).map(|_| self.value()).collect()
    }

    fn entries(&mut self, label: &str, lines: &mut Vec<String>) {
        let count = self.count();
        lines.push(format!("{label}: count={count}"));
        for slot in 0..count {
            lines.push(format!(
                "{label}[{slot}] {} => {}",
                self.value(),
                self.value()
            ));
        }
    }

    fn object(&mut self, label: &str, lines: &mut Vec<String>) {
        match self.number(1) {
            0 => {
                lines.push(format!(
                    "{label}: table array_len={} array_slots={}",
                    self.number(8),
                    self.number(4)
                ));
                self.entries("  slot", lines);
                let mt = if self.flag() {
                    self.value()
                } else {
                    "none".to_owned()
                };
                lines.push(format!("  metatable={mt}"));
            }
            1 => {
                let chunk = self.number(4);
                let upvalues: Vec<_> = (0..self.count()).map(|_| self.number(4)).collect();
                lines.push(format!(
                    "{label}: closure bytecode[{chunk}] upvalues={upvalues:?}"
                ));
            }
            _ => panic!("unknown fixture object tag"),
        }
    }

    fn fixed_vector(&mut self, width: usize) {
        let n = self.count();
        self.take(n * width);
    }

    fn bytecode(&mut self) {
        self.fixed_vector(8); // Instructions.
        self.fixed_vector(8); // Number literals.
        for _ in 0..self.count() {
            let n = self.count();
            self.take(n);
        }
        for _ in 0..self.count() {
            self.fixed_vector(2);
        } // Templates.
        self.take(8); // Cache slots, parameters, locals and max_stack.
        self.fixed_vector(4); // Nested chunk ids.
        self.fixed_vector(2); // Upvalue descriptors.
        self.flag(); // Vararg.
        if self.flag() {
            let n = self.count();
            self.take(n);
        } // Name.
        self.take(4); // Defined line.
        if self.flag() {
            let n = self.count();
            self.take(n);
        } // Source.
        self.fixed_vector(4); // Line info.
    }
}
