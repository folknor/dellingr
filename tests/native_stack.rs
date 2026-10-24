//! Recursion limits that bound native-stack growth, driven to their
//! boundaries together on a thread with the stack docs/limits.md states
//! they fit in (`LIMIT_TEST_STACK_BYTES`, which depends on the repository
//! profile). The gate runs this file in both profiles, default and snapshot.
//!
//! A native level is one call made from Rust, and what it costs is the sum
//! of the frames on the compiled caller path that made it. Each `Scenario`
//! below is one such path, named in reference/native-calls.toml with its
//! ordered edges; `scripts/native_calls.py` keeps that inventory, the
//! `// native-call:` markers in src/ and these lists in agreement.
//!
//! Every scenario recurses through its own path until `MAX_NATIVE_CALL_DEPTH`
//! is used up (callthrough levels fill any remainder its cycle size leaves),
//! holds a metamethod chain (`CHAINS`) across the last level so the
//! metamethod depth is at its limit too, and at the top runs one leaf
//! (`LEAVES`): the deepest work a callback can start there. Every scenario
//! runs with every chain kind and every leaf, so each path is measured
//! homogeneously against each deepest-point peak.
//!
//! Witnesses, without instrumenting the VM: each cycle records its scenario
//! name and level through the `mark` callback before it descends, the fill
//! levels, the chain top and the leaf record theirs, and the run must produce
//! exactly that sequence and its expected outcome. Steps whose callbacks
//! return values check them in Lua.

use dellingr::error::{
    ErrorKind, ErrorValue, LibraryFunction, PatternErrorDetail, SyntaxError, TypeError,
};
use dellingr::limits::{
    MAX_CHUNK_NESTING, MAX_METAMETHOD_DEPTH, MAX_NATIVE_CALL_DEPTH, MAX_SYNTAX_DEPTH, MAXCCALLS,
    SYNTAX_SUFFIX_COST,
};
use dellingr::{ArgCount, HostCallbacks, LuaType, Program, RetCount, State};

#[path = "common/stack.rs"]
mod stack;

#[path = "common/parse_stack.rs"]
mod parse_stack;
use parse_stack::PARSE_TEST_STACK_BYTES;

/// One kind of native level: Lua that recurses into `deep(n + 1)` through
/// one compiled caller path.
struct Scenario {
    name: &'static str,
    /// Lua run once, at the top of the chunk.
    setup: &'static str,
    /// One cycle, the body of `deep(n)`. `DESCEND` marks the one place it
    /// recurses, inside the handler the path calls.
    step: &'static str,
    /// Calls made from Rust per cycle.
    native: u32,
    /// Metamethod-depth units each cycle holds while it recurses.
    meta: u32,
    /// Each cycle ends in `error(obj)` once the recursion returns, so a run
    /// that completes ends in that script error.
    raises: bool,
}

/// Lets the string metatable's `__index` run the current level's handler.
const STRING_HANDLER: &str = r#"getmetatable("").__index = function() return HANDLER() end"#;

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "index_field",
        setup: "",
        step: "local t = setmetatable({}, {__index = function() DESCEND end})
            local _ = t.x",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "index_method",
        setup: "",
        step: "local t = setmetatable({}, {__index = function() DESCEND return function() end end})
            t:x()",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "index_key",
        setup: "",
        step: "local t = setmetatable({}, {__index = function() DESCEND end})
            local _ = t[n]",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "index_rust",
        setup: "",
        step: "HANDLER = function() DESCEND end
            local t = setmetatable({}, {__index = call_handler})
            local _ = t.x",
        native: 2,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "newindex_field",
        setup: "",
        step: "setmetatable({}, {__newindex = function() DESCEND end}).x = 1",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "newindex_key",
        setup: "",
        step: "local t = setmetatable({}, {__newindex = function() DESCEND end})
            t[n] = 1",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "newindex_rust",
        setup: "",
        step: "HANDLER = function() DESCEND end
            setmetatable({}, {__newindex = call_handler}).x = 1",
        native: 2,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "length",
        setup: "",
        step: "local len = #setmetatable({}, {__len = function() DESCEND return 3 end})
            if len ~= 3 then error('length') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "string_index_field",
        setup: STRING_HANDLER,
        step: "HANDLER = function() DESCEND return 4 end
            if ('x').y ~= 4 then error('string index') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "string_index_key",
        setup: STRING_HANDLER,
        step: "HANDLER = function() DESCEND return 4 end
            if ('x')[n] ~= 4 then error('string index') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "index_string_handler",
        setup: STRING_HANDLER,
        step: "HANDLER = function() DESCEND return 4 end
            local t = setmetatable({}, {__index = 's'})
            if t.x ~= 4 then error('string handler') end",
        native: 1,
        meta: 2,
        raises: false,
    },
    Scenario {
        // The library table's own __index nests, so that step holds a unit
        // as well as the function handler's.
        name: "string_library_index",
        setup: "setmetatable(string, {__index = function() return HANDLER() end})",
        step: "HANDLER = function() DESCEND return 4 end
            if ('x').missing ~= 4 then error('string library') end",
        native: 1,
        meta: 2,
        raises: false,
    },
    Scenario {
        name: "host_get_table",
        setup: "",
        step: "local t = setmetatable({}, {__index = function() DESCEND return 4 end})
            if host_index(t, 'x') ~= 4 then error('host index') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "host_get_table_string",
        setup: STRING_HANDLER,
        step: "HANDLER = function() DESCEND return 4 end
            if host_index('x', 'y') ~= 4 then error('host index') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "host_call",
        setup: "",
        step: "callthrough(function() DESCEND end)",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "host_anchor",
        setup: "",
        step: "anchor_callthrough(function() DESCEND end)",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "host_tostring_bytes",
        setup: "",
        step: "local v = setmetatable({}, {__tostring = function() DESCEND return 'r' end})
            if host_tostring(v) ~= 'r' then error('host tostring') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "host_to_string_with_meta",
        setup: "",
        step: "local v = setmetatable({}, {__tostring = function() DESCEND return 'r' end})
            if host_to_string(v) ~= 'r' then error('host to_string') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "tostring",
        setup: "",
        step: "local v = setmetatable({}, {__tostring = function() DESCEND return 'r' end})
            if tostring(v) ~= 'r' then error('tostring') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "print",
        setup: "",
        step: "print(setmetatable({}, {__tostring = function() DESCEND return 'r' end}))",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "format",
        setup: "",
        step: "local v = setmetatable({}, {__tostring = function() DESCEND return 'r' end})
            if string.format('%s', v) ~= 'r' then error('format') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "error_tostring",
        setup: "",
        step: "error(setmetatable({}, {__tostring = function() DESCEND return 'r' end}))",
        native: 1,
        meta: 0,
        raises: true,
    },
    Scenario {
        name: "sort_comparator",
        setup: "",
        step: "local calls, t = 0, {2, 1}
            table.sort(t, function(a, b)
                calls = calls + 1
                if calls == 1 then DESCEND end
                return a < b
            end)
            if t[1] ~= 1 or t[2] ~= 2 then error('sort') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        // Three elements: the heapify pass compares the left child, then
        // the right.
        name: "sort_comparator_right",
        setup: "",
        step: "local calls, t = 0, {3, 1, 2}
            table.sort(t, function(a, b)
                calls = calls + 1
                if calls == 2 then DESCEND end
                return a < b
            end)
            if t[1] ~= 1 or t[2] ~= 2 or t[3] ~= 3 then error('sort') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        // The third comparison of three elements is the first of the
        // sort-down pass.
        name: "sort_comparator_sortdown",
        setup: "",
        step: "local calls, t = 0, {3, 1, 2}
            table.sort(t, function(a, b)
                calls = calls + 1
                if calls == 3 then DESCEND end
                return a < b
            end)
            if t[1] ~= 1 or t[2] ~= 2 or t[3] ~= 3 then error('sort') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        // A function-valued __call holds no metamethod-depth unit.
        name: "callable_rust",
        setup: "",
        step: "local calls, t = 0, setmetatable({2, 1}, {__call = table.sort})
            t(function(a, b)
                calls = calls + 1
                if calls == 1 then DESCEND end
                return a < b
            end)
            if t[1] ~= 1 or t[2] ~= 2 then error('sort') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "sort_metamethods_comparator",
        setup: "",
        step: "local calls, t = 0, setmetatable({2, 1}, {__index = function() end})
            table.sort(t, function(a, b)
                calls = calls + 1
                if calls == 1 then DESCEND end
                return a < b
            end)
            if rawget(t, 1) ~= 1 or rawget(t, 2) ~= 2 then error('sort') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "sort_metamethods_length",
        setup: "",
        step: "table.sort(setmetatable({}, {__len = function() DESCEND return 0 end}))",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "sort_metamethods_read",
        setup: "",
        step: "local first = true
            local t = setmetatable({}, {
                __len = function() return 2 end,
                __index = function(_, i)
                    if first then first = false DESCEND end
                    return 3 - i
                end,
            })
            table.sort(t)
            if rawget(t, 1) ~= 1 or rawget(t, 2) ~= 2 then error('sort') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "sort_metamethods_write",
        setup: "",
        step: "local first = true
            local t = setmetatable({}, {
                __len = function() return 2 end,
                __index = function(_, i) return 3 - i end,
                __newindex = function(t, k, v)
                    if first then first = false DESCEND end
                    rawset(t, k, v)
                end,
            })
            table.sort(t)
            if rawget(t, 1) ~= 1 or rawget(t, 2) ~= 2 then error('sort') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "insert_length",
        setup: "",
        step: "local t = setmetatable({}, {__len = function() DESCEND return 0 end})
            table.insert(t, 'v')
            if rawget(t, 1) ~= 'v' then error('insert') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "insert_shift_read",
        setup: "",
        step: "local t = setmetatable({}, {
                __len = function() return 1 end,
                __index = function() DESCEND return 'old' end,
            })
            table.insert(t, 1, 'new')
            if rawget(t, 1) ~= 'new' or rawget(t, 2) ~= 'old' then error('insert') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "insert_shift_write",
        setup: "",
        step: "local first = true
            local t = setmetatable({}, {
                __len = function() return 1 end,
                __index = function() return 'old' end,
                __newindex = function(t, k, v)
                    if first then first = false DESCEND end
                    rawset(t, k, v)
                end,
            })
            table.insert(t, 1, 'new')
            if rawget(t, 1) ~= 'new' or rawget(t, 2) ~= 'old' then error('insert') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "insert_value",
        setup: "",
        step: "local t = setmetatable({}, {
                __len = function() return 0 end,
                __newindex = function(t, k, v) DESCEND rawset(t, k, v) end,
            })
            table.insert(t, 'v')
            if rawget(t, 1) ~= 'v' then error('insert') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "remove_length",
        setup: "",
        step: "local t = setmetatable({}, {__len = function() DESCEND return 0 end})
            if table.remove(t) ~= nil then error('remove') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "remove_read",
        setup: "",
        step: "local t = setmetatable({}, {
                __len = function() return 1 end,
                __index = function() DESCEND return 'x' end,
            })
            if table.remove(t) ~= 'x' then error('remove') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        // The first read is the removed value, the second the shift.
        name: "remove_shift_read",
        setup: "",
        step: "local reads = 0
            local t = setmetatable({}, {
                __len = function() return 2 end,
                __index = function(_, i)
                    reads = reads + 1
                    if reads == 2 then DESCEND end
                    return i
                end,
            })
            if table.remove(t, 1) ~= 1 or rawget(t, 1) ~= 2 then error('remove') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "remove_shift_write",
        setup: "",
        step: "local first = true
            local t = setmetatable({}, {
                __len = function() return 2 end,
                __index = function(_, i) return i end,
                __newindex = function(t, k, v)
                    if first then first = false DESCEND end
                    rawset(t, k, v)
                end,
            })
            if table.remove(t, 1) ~= 1 or rawget(t, 1) ~= 2 then error('remove') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "remove_clear",
        setup: "",
        step: "local t = setmetatable({}, {
                __len = function() return 1 end,
                __index = function(_, i) return i end,
                __newindex = function(t, k, v) DESCEND rawset(t, k, v) end,
            })
            if table.remove(t) ~= 1 then error('remove') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "concat_length",
        setup: "",
        step: "local t = setmetatable({'a'}, {__len = function() DESCEND return 1 end})
            if table.concat(t) ~= 'a' then error('concat') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "concat_read",
        setup: "",
        step: "local t = setmetatable({}, {
                __len = function() return 1 end,
                __index = function() DESCEND return 'a' end,
            })
            if table.concat(t) ~= 'a' then error('concat') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "move_read",
        setup: "",
        step: "local src = setmetatable({}, {__index = function(_, i) DESCEND return i end})
            if table.move(src, 1, 1, 1, {})[1] ~= 1 then error('move') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "move_write",
        setup: "",
        step: "local dst = setmetatable({}, {__newindex = function(t, k, v) DESCEND rawset(t, k, v) end})
            table.move({7}, 1, 1, 1, dst)
            if rawget(dst, 1) ~= 7 then error('move') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "unpack_length",
        setup: "",
        step: "local t = setmetatable({5}, {__len = function() DESCEND return 1 end})
            if table.unpack(t) ~= 5 then error('unpack') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "unpack_read",
        setup: "",
        step: "local t = setmetatable({}, {__index = function() DESCEND return 5 end})
            if unpack(t, 1, 1) ~= 5 then error('unpack') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "gsub_empty_function",
        setup: "",
        step: "local first = true
            local r = string.gsub('a', '', function()
                if first then first = false DESCEND end
                return '-'
            end)
            if r ~= '-a-' then error('gsub') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "gsub_plain_function",
        setup: "",
        step: "local r = string.gsub('a', 'a', function() DESCEND return 'b' end)
            if r ~= 'b' then error('gsub') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "gsub_pattern_function",
        setup: "",
        step: "local r = string.gsub('a', '%a', function() DESCEND return 'b' end)
            if r ~= 'b' then error('gsub') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "gsub_pattern_captures",
        setup: "",
        step: "local r = string.gsub('a', '(%a)', function(c) DESCEND return c .. 'b' end)
            if r ~= 'ab' then error('gsub') end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        name: "gsub_empty_table",
        setup: "",
        step: "local first = true
            local repl = setmetatable({}, {__index = function()
                if first then first = false DESCEND end
                return '-'
            end})
            if string.gsub('a', '', repl) ~= '-a-' then error('gsub') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "gsub_plain_table",
        setup: "",
        step: "local repl = setmetatable({}, {__index = function() DESCEND return 'b' end})
            if string.gsub('a', 'a', repl) ~= 'b' then error('gsub') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "gsub_pattern_table",
        setup: "",
        step: "local repl = setmetatable({}, {__index = function() DESCEND return 'b' end})
            if string.gsub('a', '%a', repl) ~= 'b' then error('gsub') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        name: "tfor_lua",
        setup: "",
        step: "local first = true
            for _ in function()
                if first then first = false DESCEND end
                return nil
            end do end",
        native: 1,
        meta: 0,
        raises: false,
    },
    Scenario {
        // ipairs over a table with a metatable leaves the dispatch loop's
        // fast path for the Rust iterator, which reads through __index.
        name: "tfor_ipairs",
        setup: "",
        step: "local seen = 0
            local t = setmetatable({}, {__index = function(_, i)
                if i == 1 then DESCEND return true end
            end})
            for _ in ipairs(t) do seen = seen + 1 end
            if seen ~= 1 then error('ipairs') end",
        native: 1,
        meta: 1,
        raises: false,
    },
    Scenario {
        // A Rust iterator runs without a native level of its own; its call
        // into Lua through `State::call` is the level.
        name: "tfor_host_iterator",
        setup: "",
        step: "HANDLER = function() DESCEND end
            for _ in call_handler do error('the iterator returns nothing') end",
        native: 1,
        meta: 0,
        raises: false,
    },
];

/// The metamethod chain at the top of the recursion. A held chain ends in a
/// function handler that is the last native level and runs the leaf, so it
/// stays live across the leaf; a terminal chain is a deepest-point peak of
/// its own that can only end in `MetamethodDepthExceeded`.
struct Chain {
    name: &'static str,
    held: bool,
    /// Defines `top()`; `CHAIN_LEN` is the number of table steps.
    lua: &'static str,
}

const CHAINS: &[Chain] = &[
    Chain {
        name: "index",
        held: true,
        lua: r#"local chain = setmetatable({}, {__index = function() leaf() end})
            for _ = 1, CHAIN_LEN do chain = setmetatable({}, {__index = chain}) end
            function top() mark("top", 0) local _ = chain.missing end"#,
    },
    Chain {
        name: "newindex",
        held: true,
        lua: r#"local chain = setmetatable({}, {__newindex = function() leaf() end})
            for _ = 1, CHAIN_LEN do chain = setmetatable({}, {__newindex = chain}) end
            function top() mark("top", 0) chain.missing = 1 end"#,
    },
    Chain {
        // The string metatable's __index naming a string: every step is a
        // string step of the same lookup, resolved in a loop.
        name: "string",
        held: false,
        lua: r#"function top()
                mark("top", 0)
                getmetatable("").__index = ""
                local _ = ("x").y
            end"#,
    },
    Chain {
        // Alternating string and table steps: the string metatable's
        // __index is a table whose own __index is a string. Both steps nest,
        // so each takes a unit.
        name: "string_table",
        held: false,
        lua: r#"function top()
                mark("top", 0)
                getmetatable("").__index = setmetatable({}, {__index = ""})
                local _ = ("x").y
            end"#,
    },
];

enum Outcome {
    Completes,
    NativeRefused,
    MetaRefused,
    PatternTooComplex,
    SortComparison,
}

/// Work at the top of a held chain, with every native level in use
/// (`headroom` 0) or all but one.
struct Leaf {
    name: &'static str,
    headroom: u32,
    /// Shortens the chain by one cycle's metamethod units, so a step that
    /// holds some still reaches its call made from Rust.
    meta_room: bool,
    lua: &'static str,
    outcome: Outcome,
}

const LEAVES: &[Leaf] = &[
    Leaf {
        // The matcher at MAXCCALLS - 1 active calls through each recursive
        // item kind, then a collection that walks the deepest chunk tree.
        name: "accept",
        headroom: 0,
        meta_room: false,
        lua: r#"if string.find(OPTIONAL_SUBJECT, OPTIONAL_PATTERN) ~= 1 then error("optional") end
            if string.find(GREEDY_SUBJECT, GREEDY_PATTERN) ~= 1 then error("greedy") end
            if string.find("a", LAZY_PATTERN) ~= 1 then error("lazy") end
            arm_gc()
            local _ = {}
            check_gc()"#,
        outcome: Outcome::Completes,
    },
    Leaf {
        // Each recursive item kind refuses at MAXCCALLS and builds the
        // error with its frames live. Separate States exercise each kind.
        name: "pattern_reject",
        headroom: 0,
        meta_room: false,
        lua: "string.find(REJECT_SUBJECT, REJECT_PATTERN)",
        outcome: Outcome::PatternTooComplex,
    },
    Leaf {
        // The default sort comparison fails at the bottom of the heap sort
        // and builds its attributed error there, with the sort frames live.
        name: "sort_compare_error",
        headroom: 0,
        meta_room: false,
        lua: r#"table.sort({3, 1, 2, 5, 4, "x"})"#,
        outcome: Outcome::SortComparison,
    },
    Leaf {
        // The scenario's own step once more: refused on its own path.
        name: "refused",
        headroom: 0,
        meta_room: true,
        lua: "deep(CYCLES - 1)",
        outcome: Outcome::NativeRefused,
    },
    Leaf {
        // A step that nests: the outer handler table dispatches its own
        // __index. A one-level miss is terminal and takes no unit.
        name: "meta_refused",
        headroom: 0,
        meta_room: false,
        lua: "local _ = setmetatable({}, {__index = setmetatable({}, {__index = {}})}).x",
        outcome: Outcome::MetaRefused,
    },
    Leaf {
        // gmatch runs its wrapper chunk through a call made from Rust, so it
        // needs the last native level.
        name: "gmatch",
        headroom: 1,
        meta_room: false,
        lua: r#"local words = string.gmatch("ab", "%a")
            if words() ~= "a" then error("gmatch") end
            local empties = string.gmatch("ab", "")
            if empties() ~= "" then error("gmatch") end"#,
        outcome: Outcome::Completes,
    },
    Leaf {
        name: "gmatch_refused",
        headroom: 0,
        meta_room: false,
        lua: r#"string.gmatch("ab", "%a")"#,
        outcome: Outcome::NativeRefused,
    },
    Leaf {
        name: "gmatch_empty_refused",
        headroom: 0,
        meta_room: false,
        lua: r#"string.gmatch("ab", "")"#,
        outcome: Outcome::NativeRefused,
    },
];

/// The chunk for one scenario; the chain and leaf are chosen by globals.
fn chunk(scenario: &Scenario) -> String {
    let nested = MAX_CHUNK_NESTING - 1;
    let mut chains = String::new();
    for chain in CHAINS {
        let keyword = if chains.is_empty() { "if" } else { "elseif" };
        chains += &format!(
            "{keyword} CHAIN == \"{}\" then\n{}\n",
            chain.name, chain.lua
        );
    }
    let mut leaves = String::new();
    for leaf in LEAVES {
        let keyword = if leaves.is_empty() { "if" } else { "elseif" };
        leaves += &format!("{keyword} LEAF == \"{}\" then\n{}\n", leaf.name, leaf.lua);
    }
    let descend = format!("mark(\"{}\", n) deep(n + 1)", scenario.name);
    r#"-- The deepest chunk tree the parser accepts, alive with this chunk for
-- a collection to walk.
@OPEN@@CLOSE@
@SETUP@
function leaf()
    mark("leaf", 0)
    @LEAVES@end
end
@CHAINS@end
function fill(k)
    if k == 0 then top() return end
    callthrough(function() mark("fill", k) fill(k - 1) end)
end
function deep(n)
    if n == CYCLES then fill(FILL) return end
    @STEP@
end
deep(0)
"#
    .replace("@OPEN@", &"function nest() ".repeat(nested))
    .replace("@CLOSE@", &"end ".repeat(nested))
    .replace("@SETUP@", scenario.setup)
    .replace("@LEAVES@", &leaves)
    .replace("@CHAINS@", &chains)
    .replace("@STEP@", &scenario.step.replace("DESCEND", &descend))
}

/// How one run spends the limits.
struct Plan {
    cycles: u32,
    /// Callthrough levels after the cycles, so the leaf (or a terminal
    /// chain) runs with exactly `headroom` native levels left.
    fill: u32,
    /// Table steps in a held chain, so the leaf runs with the metamethod
    /// depth at its limit.
    chain_len: u32,
}

fn plan(scenario: &Scenario, chain: &Chain, leaf: Option<&Leaf>) -> Plan {
    let headroom = leaf.map_or(0, |leaf| leaf.headroom);
    // The host's call is the first native level; a held chain's function
    // handler is the last.
    let native = MAX_NATIVE_CALL_DEPTH - 1 - u32::from(chain.held) - headroom;
    // The chain's last unit, and one more cycle's when the leaf needs it.
    let room = leaf.is_some_and(|leaf| leaf.meta_room);
    let meta = MAX_METAMETHOD_DEPTH - 1 - if room { scenario.meta } else { 0 };
    let mut cycles = native / scenario.native;
    if let Some(by_meta) = meta.checked_div(scenario.meta) {
        cycles = cycles.min(by_meta);
    }
    Plan {
        cycles,
        fill: native - cycles * scenario.native,
        chain_len: meta - cycles * scenario.meta,
    }
}

/// What the callbacks observed during one run.
#[derive(Default)]
struct Harness {
    records: Vec<(String, u32)>,
    /// `gc_threshold()` before arming, right after, and at the check.
    gc: Vec<usize>,
}

struct Quiet;

impl HostCallbacks for Quiet {
    fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, _message: &[u8]) {}
}

fn harness(state: &mut State) -> &mut Harness {
    state
        .user_data_mut::<Harness>()
        .expect("every test State carries a Harness")
}

/// `mark(token, n)`: a witness record.
fn mark(state: &mut State) -> dellingr::Result<u8> {
    let token = String::from_utf8_lossy(state.to_bytes(1)?).into_owned();
    let level = state.to_number(2)? as u32;
    harness(state).records.push((token, level));
    Ok(0)
}

/// `callthrough(f)` calls `f()` through `State::call`.
fn callthrough(state: &mut State) -> dellingr::Result<u8> {
    state.push_value(1)?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))?;
    Ok(0)
}

/// Calls the global `HANDLER` through `State::call` and returns nothing,
/// whatever it was called with: a Rust `__index`, `__newindex` or
/// generic-for iterator.
fn call_handler(state: &mut State) -> dellingr::Result<u8> {
    state.get_global("HANDLER")?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))?;
    Ok(0)
}

/// `anchor_callthrough(f)` anchors `f` and calls it through `call_anchor`.
fn anchor_callthrough(state: &mut State) -> dellingr::Result<u8> {
    let anchor = state.anchor_function_at(1)?;
    let result = state.call_anchor(&anchor, ArgCount::Fixed(0), RetCount::Fixed(0));
    state.release_anchor(anchor);
    result?;
    Ok(0)
}

/// `host_index(t, k)` reads `t[k]` through `State::get_table`.
fn host_index(state: &mut State) -> dellingr::Result<u8> {
    state.set_top(2)?;
    state.get_table(1)?;
    Ok(1)
}

fn host_tostring(state: &mut State) -> dellingr::Result<u8> {
    let bytes = state.tostring_bytes(1)?;
    state.push_bytes(bytes)?;
    Ok(1)
}

fn host_to_string(state: &mut State) -> dellingr::Result<u8> {
    let string = state.to_string_with_meta(1)?;
    state.push_string(string)?;
    Ok(1)
}

/// Arms a collection on the next allocation: an initial threshold of 0
/// collects then, and the minimum of `usize::MAX` it then recomputes is
/// the witness that it ran. Allocates nothing itself.
fn arm_gc(state: &mut State) -> dellingr::Result<u8> {
    let before = state.gc_threshold();
    state.gc_set_policy(0, usize::MAX, 1);
    let armed = state.gc_threshold();
    harness(state).gc.extend([before, armed]);
    Ok(0)
}

fn check_gc(state: &mut State) -> dellingr::Result<u8> {
    let after = state.gc_threshold();
    harness(state).gc.push(after);
    Ok(0)
}

const CALLBACKS: &[(&str, dellingr::RustFunc)] = &[
    ("mark", mark),
    ("callthrough", callthrough),
    ("call_handler", call_handler),
    ("anchor_callthrough", anchor_callthrough),
    ("host_index", host_index),
    ("host_tostring", host_tostring),
    ("host_to_string", host_to_string),
    ("arm_gc", arm_gc),
    ("check_gc", check_gc),
];

/// The documented native-stack envelope is measured only on x86_64 Linux.
/// Fail before entering a deep stack path on other targets so the envelope
/// cannot appear to pass where it has not been stated.
fn require_native_stack_target(test: &str) {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        panic!(
            "{test}: native stack envelope is stated only for x86_64 Linux; current target is {}-{}",
            std::env::consts::ARCH,
            std::env::consts::OS,
        );
    }
}

fn set_string(state: &mut State, name: &str, value: &str) -> dellingr::Result<()> {
    state.push_string(value)?;
    state.set_global(name)
}

fn set_number(state: &mut State, name: &str, value: u32) -> dellingr::Result<()> {
    state.push_number(f64::from(value))?;
    state.set_global(name)
}

/// A State with the callbacks and the leaf's pattern inputs, automatic
/// collection off so the only collection is the one the leaf arms.
fn test_state() -> dellingr::Result<State> {
    let mut state = State::with_callbacks(Box::new(Quiet));
    state.gc_disable_auto();
    state.set_user_data(Harness::default());
    for &(name, f) in CALLBACKS {
        state.push_rust_fn(f)?;
        state.set_global(name)?;
    }
    let deepest = MAXCCALLS - 1;
    set_string(&mut state, "OPTIONAL_PATTERN", &"a?".repeat(deepest))?;
    set_string(&mut state, "OPTIONAL_SUBJECT", &"a".repeat(deepest))?;
    set_string(&mut state, "REJECT_PATTERN", &"a?".repeat(MAXCCALLS))?;
    set_string(&mut state, "REJECT_SUBJECT", &"a".repeat(MAXCCALLS))?;
    // Each greedy item takes exactly one byte and recurses into the next.
    let greedy: String = (0..deepest)
        .map(|i| if i % 2 == 0 { "a*" } else { "b*" })
        .collect();
    let subject: String = (0..deepest)
        .map(|i| if i % 2 == 0 { 'a' } else { 'b' })
        .collect();
    set_string(&mut state, "GREEDY_PATTERN", &greedy)?;
    set_string(&mut state, "GREEDY_SUBJECT", &subject)?;
    // Each minimal item first tries zero bytes, which the next item
    // accepts, so every one recurses at the same position.
    set_string(&mut state, "LAZY_PATTERN", &"a-".repeat(deepest))?;
    Ok(state)
}

fn run(
    program: &Program,
    scenario: &Scenario,
    chain: &Chain,
    leaf: Option<&Leaf>,
    rejection: &str,
) -> (dellingr::Result<()>, Harness, Plan) {
    let plan = plan(scenario, chain, leaf);
    let mut state = test_state().expect("test State setup");
    match rejection {
        "optional" => {}
        "greedy" => {
            let pattern: String = (0..MAXCCALLS)
                .map(|i| if i % 2 == 0 { "a*" } else { "b*" })
                .collect();
            let subject: String = (0..MAXCCALLS)
                .map(|i| if i % 2 == 0 { 'a' } else { 'b' })
                .collect();
            set_string(&mut state, "REJECT_PATTERN", &pattern).expect("pattern input");
            set_string(&mut state, "REJECT_SUBJECT", &subject).expect("pattern input");
        }
        "minimal" => {
            set_string(&mut state, "REJECT_PATTERN", &"a-".repeat(MAXCCALLS))
                .expect("pattern input");
        }
        other => panic!("unknown rejection shape: {other}"),
    }
    set_string(&mut state, "CHAIN", chain.name).expect("globals");
    set_string(&mut state, "LEAF", leaf.map_or("none", |leaf| leaf.name)).expect("globals");
    set_number(&mut state, "CYCLES", plan.cycles).expect("globals");
    set_number(&mut state, "FILL", plan.fill).expect("globals");
    set_number(&mut state, "CHAIN_LEN", plan.chain_len).expect("globals");
    state.load(program).expect("loading the scenario chunk");
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    // Whatever happened, the State is usable and back at rest.
    Program::compile("return 7", None)
        .and_then(|program| state.load(&program))
        .and_then(|()| state.call(ArgCount::Fixed(0), RetCount::Fixed(1)))
        .expect("State reuse");
    assert_eq!(
        state.to_number(-1).expect("reuse result"),
        7.0,
        "State reuse"
    );
    let observed = std::mem::take(harness(&mut state));
    (result, observed, plan)
}

fn check(
    program: &Program,
    scenario: &Scenario,
    chain: &Chain,
    leaf: Option<&Leaf>,
    outcome: &Outcome,
    rejection: &str,
) {
    let label = format!(
        "{} / {} / {} / {rejection}",
        scenario.name,
        chain.name,
        leaf.map_or("-", |leaf| leaf.name)
    );
    let (result, observed, plan) = run(program, scenario, chain, leaf, rejection);

    let mut expected: Vec<(String, u32)> = (0..plan.cycles)
        .map(|level| (scenario.name.to_owned(), level))
        .collect();
    expected.extend((1..=plan.fill).rev().map(|k| ("fill".to_owned(), k)));
    expected.push(("top".to_owned(), 0));
    if leaf.is_some() {
        expected.push(("leaf".to_owned(), 0));
    }
    assert_eq!(observed.records, expected, "{label}: witness records");

    let kind = result.as_ref().map_err(dellingr::error::Error::kind);
    match outcome {
        Outcome::Completes if scenario.raises => match kind {
            Err(ErrorKind::ScriptError {
                value:
                    ErrorValue::Object {
                        value_type: LuaType::Table,
                        rendering,
                    },
                ..
            }) if rendering == b"r" => {}
            other => panic!("{label}: expected the step's error(obj), got {other:?}"),
        },
        Outcome::Completes => {
            if let Err(error) = kind {
                panic!("{label}: the run must complete: {error}");
            }
        }
        Outcome::NativeRefused => match kind {
            Err(ErrorKind::NativeCallDepthExceeded { depth, limit })
                if *depth == MAX_NATIVE_CALL_DEPTH && *limit == MAX_NATIVE_CALL_DEPTH => {}
            other => panic!("{label}: expected NativeCallDepthExceeded, got {other:?}"),
        },
        Outcome::MetaRefused => match kind {
            Err(ErrorKind::MetamethodDepthExceeded { depth, limit })
                if *depth == MAX_METAMETHOD_DEPTH && *limit == MAX_METAMETHOD_DEPTH => {}
            other => panic!("{label}: expected MetamethodDepthExceeded, got {other:?}"),
        },
        Outcome::PatternTooComplex => match kind {
            Err(ErrorKind::PatternError(PatternErrorDetail::MatchDepthExceeded)) => {}
            other => panic!("{label}: expected pattern too complex, got {other:?}"),
        },
        Outcome::SortComparison => {
            match kind {
                Err(ErrorKind::TypeError(TypeError::Comparison(_, _))) => {}
                other => panic!("{label}: expected a sort comparison error, got {other:?}"),
            }
            let attribution = result
                .as_ref()
                .err()
                .and_then(dellingr::error::Error::library_function);
            assert_eq!(
                attribution,
                Some(LibraryFunction::Sort),
                "{label}: sort attribution"
            );
        }
    }

    if leaf.is_some_and(|leaf| leaf.name == "accept") {
        // Disabled during setup, armed to collect, and recomputed by the
        // collection the leaf's allocation ran.
        assert_eq!(
            observed.gc,
            [usize::MAX, 0, usize::MAX],
            "{label}: collection witness"
        );
    } else {
        assert!(
            observed.gc.is_empty(),
            "{label}: unexpected collection witness"
        );
    }
}

fn scenario_envelope(name: &'static str) {
    require_native_stack_target(name);
    let scenario = SCENARIOS
        .iter()
        .find(|scenario| scenario.name == name)
        .expect("each envelope test names an inventoried scenario");
    stack::on_limit_test_stack(name, move || {
        let program = Program::compile(chunk(scenario), None)
            .unwrap_or_else(|error| panic!("{name}: the chunk must compile: {error}"));
        for chain in CHAINS {
            if chain.held {
                for leaf in LEAVES {
                    let rejections: &[&str] = if leaf.name == "pattern_reject" {
                        &["optional", "greedy", "minimal"]
                    } else {
                        &["optional"]
                    };
                    for rejection in rejections {
                        check(
                            &program,
                            scenario,
                            chain,
                            Some(leaf),
                            &leaf.outcome,
                            rejection,
                        );
                    }
                }
            } else {
                check(
                    &program,
                    scenario,
                    chain,
                    None,
                    &Outcome::MetaRefused,
                    "optional",
                );
            }
        }
    });
}

// Repeating the scenario in module and function names makes the full test
// path a unique substring even when scenario names are suffixes or prefixes
// of each other. Keep the inventory scanner's list literal above.
macro_rules! scenario_tests {
    ($($name:ident),+ $(,)?) => {
        $(mod $name {
            #[test]
            fn $name() {
                super::scenario_envelope(stringify!($name));
            }
        })+

        #[test]
        fn scenario_test_registration_is_complete() {
            let registered = [$(stringify!($name)),+];
            let inventoried: Vec<_> = SCENARIOS.iter().map(|scenario| scenario.name).collect();
            assert_eq!(registered.as_slice(), inventoried);
            let filters = [$(concat!(stringify!($name), "::", stringify!($name))),+];
            for (i, filter) in filters.iter().enumerate() {
                for (j, other) in filters.iter().enumerate() {
                    assert_eq!(other.contains(filter), i == j, "{filter} / {other}");
                }
            }
        }
    };
}

scenario_tests!(
    index_field,
    index_method,
    index_key,
    index_rust,
    newindex_field,
    newindex_key,
    newindex_rust,
    length,
    string_index_field,
    string_index_key,
    index_string_handler,
    string_library_index,
    host_get_table,
    host_get_table_string,
    host_call,
    host_anchor,
    host_tostring_bytes,
    host_to_string_with_meta,
    tostring,
    print,
    format,
    error_tostring,
    sort_comparator,
    sort_comparator_right,
    sort_comparator_sortdown,
    callable_rust,
    sort_metamethods_comparator,
    sort_metamethods_length,
    sort_metamethods_read,
    sort_metamethods_write,
    insert_length,
    insert_shift_read,
    insert_shift_write,
    insert_value,
    remove_length,
    remove_read,
    remove_shift_read,
    remove_shift_write,
    remove_clear,
    concat_length,
    concat_read,
    move_read,
    move_write,
    unpack_length,
    unpack_read,
    gsub_empty_function,
    gsub_plain_function,
    gsub_pattern_function,
    gsub_pattern_captures,
    gsub_empty_table,
    gsub_plain_table,
    gsub_pattern_table,
    tfor_lua,
    tfor_ipairs,
    tfor_host_iterator,
);

#[test]
fn recursive_index_function_stops_at_the_native_call_limit() {
    require_native_stack_target("recursive_index_function_stops_at_the_native_call_limit");
    // Every handler is a call made from Rust, and the native limit is the
    // lower one, so it refuses the recursion before the metamethod depth
    // would.
    const { assert!(MAX_NATIVE_CALL_DEPTH < MAX_METAMETHOD_DEPTH) };
    stack::on_limit_test_stack("recursive_index_function", || {
        let mut state = State::new();
        let error = Program::compile(
            r#"
            local t = setmetatable({}, {__index = function(t, k) return t[k + 1] end})
            local _ = t[1]
            "#,
            None,
        )
        .and_then(|program| state.load(&program))
        .and_then(|()| state.call(ArgCount::Fixed(0), RetCount::Fixed(0)))
        .expect_err("unbounded __index recursion must fail");
        match error.kind() {
            ErrorKind::NativeCallDepthExceeded { depth, limit } => {
                assert_eq!(*depth, MAX_NATIVE_CALL_DEPTH);
                assert_eq!(*limit, MAX_NATIVE_CALL_DEPTH);
            }
            other => panic!("expected NativeCallDepthExceeded, got {other}"),
        }
    });
}

/// The parser's deepest accepted nesting, with the innermost function
/// resolving a name from the outermost scope (the upvalue walk recurses
/// through every enclosing function while the parse descent is live), and
/// the first rejected one, both on the parse-test stack. The syntax budget
/// is what binds: every statement list charges one unit (the root's and
/// each function body's), and the innermost `return outer` two more (its
/// expression and unary parsers), so the deepest chain is three units short
/// of `MAX_SYNTAX_DEPTH`.
#[test]
fn deepest_nested_functions_parse_on_the_parse_test_stack() {
    stack::on_stack("deepest_nested_functions", PARSE_TEST_STACK_BYTES, || {
        let source = |depth: usize| {
            format!(
                "local outer = 1 {}return outer{}",
                "local function f() ".repeat(depth),
                " end".repeat(depth)
            )
        };
        let deepest = MAX_SYNTAX_DEPTH as usize - 3;
        let mut state = State::new();
        Program::compile(source(deepest), None)
            .and_then(|program| state.load(&program))
            .unwrap_or_else(|error| panic!("depth {deepest} must compile: {error}"));
        let Err(error) = Program::compile(source(deepest + 1), None) else {
            panic!("depth {} must be rejected", deepest + 1);
        };
        assert!(
            matches!(
                error.kind(),
                ErrorKind::SyntaxError(SyntaxError::TooManySyntaxLevels)
            ),
            "depth {}: {error}",
            deepest + 1
        );
    });
}

fn syntax_boundary(source: impl Fn(usize) -> String, locals_limit: bool) -> usize {
    for depth in 0..=MAX_SYNTAX_DEPTH as usize + 1 {
        if let Err(error) = Program::compile(source(depth), None) {
            assert!(
                matches!(
                    (locals_limit, error.kind()),
                    (true, ErrorKind::SyntaxError(SyntaxError::TooManyLocals))
                        | (
                            false,
                            ErrorKind::SyntaxError(SyntaxError::TooManySyntaxLevels)
                        )
                ),
                "depth {depth}: {error}"
            );
            return depth.checked_sub(1).expect("the unnested shape compiles");
        }
    }
    panic!("shape did not reach its expected limit");
}

/// Keep all parser cycles in this target: release native-stack sweeps do
/// not run the library's parser tests. Unary padding measures the units a
/// wrapper consumes independently of the documented units table.
#[test]
fn parse_shapes_and_documented_units_fit_the_parse_test_stack() {
    stack::on_stack("parse_shapes", PARSE_TEST_STACK_BYTES, || {
        let docs = include_str!("../docs/limits.md");
        let suffix_units = 2 + SYNTAX_SUFFIX_COST as usize;
        for (row, units, open, close, statement) in [
            ("parenthesis,", 2, "(", ")", false),
            ("table constructor", 2, "{", "}", false),
            ("call argument or index,", suffix_units, "f(", ")", false),
            ("call argument or index,", suffix_units, "a[", "]", false),
            ("unary operator,", 1, "- ", "", false),
            ("unary operator,", 1, "not ", "", false),
            ("right operand", 1, "x ^ ", "", false),
            ("`if` body", 2, "if true then ", " end", true),
            ("`do`, loop", 1, "do ", " end", true),
            ("`do`, loop", 1, "while false do ", " end", true),
            ("`do`, loop", 1, "repeat ", " until true", true),
            ("`do`, loop", 1, "function f() ", " end", true),
        ] {
            let documented = docs
                .lines()
                .find(|line| line.trim_start().starts_with(&format!("| {row}")))
                .expect("each parse shape has a documented units row");
            let cell = documented.split('|').nth(2).expect("units column").trim();
            let documented_units = if cell == "2 + `SYNTAX_SUFFIX_COST`" {
                suffix_units
            } else {
                cell.parse::<usize>().expect("numeric units column")
            };
            assert_eq!(units, documented_units, "{row}");
            let source = |depth: usize, padding: usize| {
                let expression = format!("{}x", "not ".repeat(padding));
                if statement {
                    format!(
                        "{}local x = {expression}{}",
                        open.repeat(depth),
                        close.repeat(depth)
                    )
                } else {
                    format!(
                        "local x = {}{expression}{}",
                        open.repeat(depth),
                        close.repeat(depth)
                    )
                }
            };
            let unwrapped = syntax_boundary(|padding| source(0, padding), false);
            let wrapped = syntax_boundary(|padding| source(1, padding), false);
            assert_eq!(unwrapped - wrapped, units, "{open}: measured units");
            let deepest = syntax_boundary(|depth| source(depth, 0), false);
            assert_eq!(deepest, unwrapped / units, "{open}: accepted boundary");
        }

        // Empty innermost calls have different leaf headroom from the
        // expression-bearing calls used to measure the units above.
        let calls = syntax_boundary(
            |depth| format!("return {}{}", "f(".repeat(depth + 1), ")".repeat(depth + 1)),
            false,
        ) + 1;
        assert_eq!(calls, (MAX_SYNTAX_DEPTH as usize - 1) / suffix_units);
        // Constructors containing calls retain both parser cycles.
        syntax_boundary(
            |depth| {
                let mut expression = String::from("{g()}");
                for _ in 0..depth {
                    expression = format!("{{f({expression})}}");
                }
                format!("return {expression}")
            },
            false,
        );
        for open in ["for i = 1, 1 do ", "for key in iterator do "] {
            syntax_boundary(
                |depth| format!("{}return 1 {}", open.repeat(depth), "end ".repeat(depth)),
                true,
            );
        }
    });
}

/// Recurring native stack only: bytes per cycle of each scenario and per
/// table step of each held chain. The per-scenario full-depth tests cover the deepest
/// points (leaf work, chain tops, error construction and unwinding), which
/// this measurement deliberately excludes.
#[test]
#[ignore = "on-demand native stack measurement"]
fn native_bytes_per_level() {
    require_native_stack_target("native_bytes_per_level");
    fn record(state: &mut State) -> dellingr::Result<u8> {
        let local = 0_u8;
        let address = std::ptr::from_ref(&local).addr();
        state.push_number(address as f64)?;
        Ok(1)
    }
    fn report(state: &mut State) -> dellingr::Result<u8> {
        let name = String::from_utf8_lossy(state.to_bytes(1)?).into_owned();
        let bytes = state.to_number(2)?;
        let native = state.to_number(3)?;
        let meta = state.to_number(4)?;
        println!("{name}\t{bytes}\tnative {native}\tmeta {meta}");
        Ok(0)
    }
    fn probe_state() -> State {
        let mut state = test_state().expect("test State setup");
        state.push_rust_fn(record).unwrap();
        state.set_global("record").unwrap();
        state.push_rust_fn(report).unwrap();
        state.set_global("report").unwrap();
        state
    }
    stack::on_limit_test_stack("native_bytes_per_level", || {
        for scenario in SCENARIOS {
            let mut state = probe_state();
            // Eight cycles, reported from the deepest one (a raising
            // scenario fails on the way back); every gap must agree.
            let source = format!(
                "{setup}
                 local addresses = {{}}
                 local function finish()
                     local gap = math.abs(addresses[2] - addresses[1])
                     for i = 3, #addresses do
                         if math.abs(addresses[i] - addresses[i - 1]) ~= gap then
                             error('uneven gaps')
                         end
                     end
                     report('{name}', gap, {native}, {meta})
                 end
                 function deep(n)
                     addresses[n + 1] = record()
                     if n < 8 then {step} else finish() end
                 end
                 deep(0)",
                setup = scenario.setup,
                step = scenario.step.replace("DESCEND", "deep(n + 1)"),
                name = scenario.name,
                native = scenario.native,
                meta = scenario.meta,
            );
            let program = Program::compile(source, None).unwrap();
            state.load(&program).unwrap();
            let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
            if scenario.raises {
                match result.as_ref().map_err(dellingr::error::Error::kind) {
                    Err(ErrorKind::ScriptError {
                        value:
                            ErrorValue::Object {
                                value_type: LuaType::Table,
                                rendering,
                            },
                        ..
                    }) if rendering == b"r" => {}
                    other => panic!(
                        "{}: expected the step's error(obj) rendered as r, got {other:?}",
                        scenario.name
                    ),
                }
            } else if let Err(error) = result {
                panic!("{}: {error}", scenario.name);
            }
        }
        // A chain step: the handler's address after 8 and after 16 steps.
        for (name, setup, access) in [
            ("index", "__index", "local _ = chain.missing"),
            ("newindex", "__newindex", "chain.missing = 1"),
        ] {
            let mut state = probe_state();
            let source = format!(
                "local function at(steps)
                     local address
                     local chain = setmetatable({{}}, {{{setup} = function() address = record() end}})
                     for _ = 1, steps do chain = setmetatable({{}}, {{{setup} = chain}}) end
                     {access}
                     return address
                 end
                 report('chain_{name}', math.abs(at(16) - at(8)) / 8, 0, 1)"
            );
            let program = Program::compile(source, None).unwrap();
            state.load(&program).unwrap();
            state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
        }
    });
}
