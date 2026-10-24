//! Golden `cost_used` values for a fixed corpus.
//!
//! `COST_MODEL_VERSION` covers the cost of a given source: per-opcode charges,
//! native library charges and compiler emission (which opcodes a source turns
//! into, e.g. constant folding). If one of these assertions fails, the cost of
//! identical source changed: bump `COST_MODEL_VERSION`, document the new
//! version in `src/cost_meter.rs`, and update the versioned fixture.
//!
//! Snapshot equality and budget-derived expectations remain in their owning
//! suites: they are relations, not independent absolute cost goldens to copy.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, COST_MODEL_VERSION, RetCount, State};

use crate::support;
use crate::support::PushNewRustFn;

/// Versioned rows shared by source and native-call goldens.
const FIXTURE: &str = include_str!("../../fixtures/cost_golden.txt");

pub(super) struct Golden {
    pub(super) kind: &'static str,
    pub(super) name: &'static str,
    pub(super) expected: Option<u64>,
    pub(super) input: &'static str,
}

pub(super) fn goldens() -> Vec<Golden> {
    let mut names = std::collections::BTreeSet::new();
    FIXTURE
        .lines()
        .skip(1)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let fields: Vec<_> = line.splitn(4, '|').collect();
            assert_eq!(fields.len(), 4, "malformed cost golden: {line}");
            assert!(
                names.insert((fields[0], fields[1])),
                "duplicate golden: {line}"
            );
            let expected = if fields[2].is_empty() {
                assert!(matches!(fields[0], "free" | "free_error"));
                None
            } else {
                Some(
                    fields[2]
                        .parse()
                        .expect("golden cost is an unsigned integer"),
                )
            };
            assert!(matches!(
                fields[0],
                "run" | "error" | "failure" | "native" | "free" | "free_error" | "pin"
            ));
            Golden {
                kind: fields[0],
                name: fields[1],
                expected,
                input: fields[3],
            }
        })
        .collect()
}

pub(super) fn pinned_cost(name: &str) -> u64 {
    goldens()
        .into_iter()
        .find(|row| row.kind == "pin" && row.name == name)
        .expect("named cost pin exists")
        .expected
        .expect("named pin has an expected cost")
}

#[test]
fn cost_used_after_runtime_error_matches_golden_corpus() {
    for row in goldens()
        .into_iter()
        .filter(|row| matches!(row.kind, "error" | "failure"))
    {
        let (name, source) = (row.name, row.input);
        let expected = row.expected.expect("error golden has a cost");
        let exact_budget = i64::try_from(expected).expect("golden cost fits in i64");
        for budget in [None, Some(exact_budget), Some(1_000)] {
            let mut state = State::new();
            if let Some(budget) = budget {
                state.set_cost_budget(budget);
            }
            support::load(&mut state, source);
            let error = state
                .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                .expect_err("golden error source must fail at runtime");
            if row.kind == "error" {
                assert!(
                    matches!(error.kind(), ErrorKind::TypeError(_)),
                    "{name} (budget {budget:?}): expected a type error, got {error:?}"
                );
            }
            assert_eq!(
                state.cost_used(),
                expected,
                "{name} (budget {budget:?}): cost_used after a runtime error must count \
                 the executed costed opcodes; a change here needs a COST_MODEL_VERSION bump"
            );
        }
    }
}

#[test]
fn unpack_charges_nothing_for_lookups_reached_before_an_index_error() {
    let mut costs = Vec::new();
    for end in [1, 10] {
        let mut state = State::new();
        support::load(
            &mut state,
            format!(
                "local t = setmetatable({{}}, {{__index = function() return nil + 1 end}}) unpack(t, 1, {end})"
            ),
        );
        let err = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("first lookup errors");
        assert!(matches!(err.kind(), ErrorKind::TypeError(_)));
        costs.push(state.cost_used());
    }
    // Two tables, one field initializer, and the failing handler add.
    // The lookup itself adds no native charge.
    let expected = pinned_cost("unpack_failed_lookup");
    assert_eq!(costs, [expected, expected]);
}

/// `table.insert`, `table.remove` and `table.pack` charge a flat 1 and
/// `unpack` is free, however many elements are shifted, visited or stored.
/// This is a deliberate game-design choice of the cost model (the budget is
/// not a CPU meter), pinned here so it is not re-filed as a bug. Each pair
/// performs the same costed bytecode operations on different data, so the
/// difference is exactly the data-dependent library charge: zero.
#[test]
fn table_library_charges_are_flat() {
    let setup = "local t = {} for i = 1, 50 do t[i] = i end";
    let pairs: &[(&str, &str, &str)] = &[
        // Front insert and append cost the same.
        ("insert", "local d = -49", "local d = 1"),
        // Front remove and pop cost the same.
        ("remove", "local d = -49", "local d = 0"),
        // unpack is free whatever the range.
        ("unpack", "local j = 10", "local j = 1"),
        ("unpack_empty", "local j = 1", "local j = 0"),
        // pack costs 1 whatever the argument count.
        ("pack", "local j = 10", "local j = 1"),
    ];
    for &(name, wide, narrow) in pairs {
        let expected = pinned_cost(&format!("flat:{name}"));
        let body = match name {
            "insert" => "local p = #t + d table.insert(t, p, 0)",
            "remove" => "local p = #t + d table.remove(t, p)",
            "unpack" | "unpack_empty" => "local a = unpack(t, 1, j)",
            "pack" => "local a = table.pack(unpack(t, 1, j))",
            _ => unreachable!("every pair names a body"),
        };
        let wide_cost = support::cost_of(format!("{setup} {wide} {body}"));
        let narrow_cost = support::cost_of(format!("{setup} {narrow} {body}"));
        assert_eq!(
            wide_cost - narrow_cost,
            expected,
            "{name}: wide {wide_cost}, narrow {narrow_cost}"
        );
    }
}

#[test]
fn cost_used_matches_golden_corpus() {
    let mut drift = Vec::new();
    for row in goldens().into_iter().filter(|row| row.kind == "run") {
        let (name, source) = (row.name, row.input);
        let expected = row.expected.expect("source golden has a cost");
        let mut state = State::new();
        support::run(&mut state, source.replace("\\n", "\n"));
        let actual = state.cost_used();
        if actual != expected {
            drift.push(format!("{name}: expected {expected}, got {actual}"));
        }
    }
    assert!(
        drift.is_empty(),
        "cost_used for identical source changed under COST_MODEL_VERSION {COST_MODEL_VERSION}; \
         bump COST_MODEL_VERSION, document the new version, and update the fixture:\n{}",
        drift.join("\n")
    );
}

fn fingerprint_bytes(fingerprint: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *fingerprint = (*fingerprint ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3);
    }
}

fn fixture_version(fixture: &str) -> u16 {
    fixture
        .lines()
        .next()
        .expect("cost fixture has a version")
        .strip_prefix("cost_model=")
        .expect("first line declares cost_model")
        .parse()
        .expect("cost model is a u16")
}

/// Fingerprint the complete non-comment rows in fixture order, including
/// names: specialized probes and diagnostics refer to cases by name, and the
/// corpus loops exercise rows in fixture order. This keeps a rename or reorder
/// visible instead of silently changing the meaning of a history entry.
fn fixture_fingerprint(fixture: &str) -> u64 {
    let mut fingerprint = 0xcbf2_9ce4_8422_2325;
    for line in fixture.lines().skip(1) {
        if !line.is_empty() && !line.starts_with('#') {
            fingerprint_bytes(&mut fingerprint, line.as_bytes());
            fingerprint_bytes(&mut fingerprint, b"\n");
        }
    }
    fingerprint
}

/// Append-only record of (cost model version, fixture row fingerprint). A
/// changed fixture row passes only by bumping `COST_MODEL_VERSION` and
/// appending a new pair: the last entry must be the current version with the
/// current fingerprint, and no fingerprint may appear under two versions.
/// Never edit or remove an existing entry; a diff that does is the signal a
/// reviewer looks for. The history starts at version 8, when this fixture
/// replaced the in-test corpus.
const COST_MODEL_HISTORY: &[(u16, u64)] = &[(8, 0x7731_8aae_9088_6a97)];

const PIN_CONSUMER_SOURCES: &[(&str, &str)] = &[
    ("cost_golden", include_str!("cost_golden.rs")),
    ("string_charges", include_str!("string_charges.rs")),
    (
        "reentrant_opcode_accounting",
        include_str!("reentrant_opcode_accounting.rs"),
    ),
];

fn source_reads_pin(test_body: &str, module_source: &str, name: &str) -> bool {
    if test_body.contains(&format!("pinned_cost(\"{name}\")")) {
        return true;
    }

    const FORMATTED_LOOKUP: &str = "pinned_cost(&format!(\"";
    test_body.match_indices(FORMATTED_LOOKUP).any(|(start, _)| {
        let template_start = start + FORMATTED_LOOKUP.len();
        let template_tail = &test_body[template_start..];
        let Some(template_end) = template_tail.find('"') else {
            return false;
        };
        let template = &template_tail[..template_end];
        let Some((prefix, placeholder_tail)) = template.split_once('{') else {
            return false;
        };
        let Some((_, suffix)) = placeholder_tail.split_once('}') else {
            return false;
        };
        let Some(dynamic_name) = name
            .strip_prefix(prefix)
            .and_then(|tail| tail.strip_suffix(suffix))
        else {
            return false;
        };
        !dynamic_name.is_empty() && module_source.contains(&format!("\"{dynamic_name}\""))
    })
}

fn named_test_body<'a>(source: &'a str, test_name: &str) -> Option<&'a str> {
    let marker = format!("#[test]\nfn {test_name}() {{");
    let body = source.split_once(&marker)?.1;
    Some(body.split("\n#[test]").next().unwrap_or(body))
}

fn tests_reading_pin<'a>(source: &'a str, name: &str) -> Vec<&'a str> {
    const TEST_MARKER: &str = "#[test]\nfn ";
    source
        .match_indices(TEST_MARKER)
        .filter_map(|(start, _)| {
            let test_start = start + TEST_MARKER.len();
            let test_tail = &source[test_start..];
            let test_name_end = test_tail.find('(')?;
            let test_name = &test_tail[..test_name_end];
            let body = test_tail.split("\n#[test]").next().unwrap_or(test_tail);
            source_reads_pin(body, source, name).then_some(test_name)
        })
        .collect()
}

#[test]
fn golden_corpus_fingerprint_matches_cost_model_version() {
    assert_eq!(
        fixture_version(FIXTURE),
        COST_MODEL_VERSION,
        "the fixture's cost_model header must name the current version"
    );
    assert!(
        COST_MODEL_HISTORY
            .windows(2)
            .all(|pair| pair[0].0 < pair[1].0),
        "COST_MODEL_HISTORY versions must strictly increase"
    );
    let fingerprint = fixture_fingerprint(FIXTURE);
    for &(version, recorded) in COST_MODEL_HISTORY {
        assert!(
            version == COST_MODEL_VERSION || recorded != fingerprint,
            "the fixture rows match those recorded for cost model {version}; \
             a fingerprint belongs to exactly one version"
        );
    }
    assert_eq!(
        COST_MODEL_HISTORY.last(),
        Some(&(COST_MODEL_VERSION, fingerprint)),
        "cost fixture rows changed: bump COST_MODEL_VERSION, document it in \
         src/cost_meter.rs, update the fixture header and append the new \
         (version, fingerprint) pair to COST_MODEL_HISTORY"
    );
}

#[test]
fn every_cost_pin_names_a_test_that_reads_it() {
    for row in goldens().into_iter().filter(|row| row.kind == "pin") {
        let (module, test_name) = row
            .input
            .split_once("::")
            .map_or((row.input, None), |(module, test)| (module, Some(test)));
        let source = PIN_CONSUMER_SOURCES
            .iter()
            .find_map(|(candidate, source)| (*candidate == module).then_some(*source))
            .unwrap_or_else(|| panic!("{} names an unknown pin consumer module", row.name));

        if let Some(test_name) = test_name {
            let body = named_test_body(source, test_name)
                .unwrap_or_else(|| panic!("{} names missing test {module}::{test_name}", row.name));
            assert!(
                source_reads_pin(body, source, row.name),
                "{} is not read by {module}::{test_name}",
                row.name
            );
        } else {
            // A module-only consumer is accepted only when one test in that
            // module uniquely reads this pin.
            let consumers = tests_reading_pin(source, row.name);
            assert_eq!(
                consumers.len(),
                1,
                "{} must resolve to one test in {module}, found {consumers:?}",
                row.name
            );
        }
    }
}

#[test]
fn changelog_declares_the_current_cost_model_once() {
    let changelog = include_str!("../../../CHANGELOG.md");
    let unreleased = changelog
        .split("\n## ")
        .nth(1)
        .expect("Unreleased section exists");
    let marker = "`COST_MODEL_VERSION` is ";
    let versions: Vec<_> = unreleased
        .split(marker)
        .skip(1)
        .map(|tail| {
            tail.split(|ch: char| !ch.is_ascii_digit())
                .next()
                .expect("version follows marker")
                .parse::<u16>()
                .expect("documented cost model is numeric")
        })
        .collect();
    assert_eq!(versions, [COST_MODEL_VERSION]);
}

/// A Rust callback running under a Lua caller always sees that caller's opcode
/// cost fully flushed: `OP_CALL` (and every metamethod call out of the
/// dispatch loop) settles the pending batch first. A mid-run
/// `set_cost_budget` therefore resets a complete meter, and `cost_used`
/// afterwards counts only post-reset work, whatever the batching interval
/// made of the prefix.
#[test]
fn mid_run_budget_reset_sees_a_flushed_meter() {
    fn reset(state: &mut State) -> dellingr::Result<u8> {
        let expected = *state
            .user_data::<u64>()
            .expect("test installed expected cost");
        assert_eq!(state.cost_used(), expected, "meter before reset");
        assert_eq!(
            state.cost_remaining(),
            1_000_000 - i64::try_from(expected).map_or(i64::MAX, i64::from)
        );
        state.set_cost_budget(1_000_000);
        Ok(0)
    }

    let mut results = Vec::new();
    let interval = dellingr::limits::COST_CHECK_INTERVAL;
    for (setup, call_site, setup_cost) in [
        ("", "reset()", 0),
        (
            "local t = setmetatable({}, {__index = reset})",
            "local v = t.x",
            3,
        ),
        (
            "local t = setmetatable({}, {__index = reset})",
            "local k = 'x' local v = t[k]",
            3,
        ),
        (
            "local t = setmetatable({}, {__newindex = reset})",
            "t.x = 1",
            4,
        ),
        (
            "local t = setmetatable({}, {__newindex = reset})",
            "local k = 'x' t[k] = 1",
            4,
        ),
        (
            "local t = setmetatable({}, {__len = reset})",
            "local v = #t",
            3,
        ),
        ("local t = setmetatable({}, {__call = reset})", "t()", 3),
        ("getmetatable('').__call = reset", "local s = 'x' s()", 1),
        (
            "getmetatable('').__index = reset",
            "local v = ('x').missing",
            1,
        ),
        ("", "for v in reset, nil, nil do end", 0),
    ] {
        for prefix in [0, 1, 7, interval - 1, interval, interval + 1, 20_000] {
            let mut state = State::new();
            state.set_cost_budget(1_000_000);
            state.set_user_data(prefix + setup_cost);
            state.push_new_rust_fn(reset).unwrap();
            state.set_global("reset").unwrap();
            support::run(
                &mut state,
                format!(
                    "{setup} local s = 0 for i = 1, {prefix} do s = s + i end \
                     {call_site} local a = {{}} a.x = 1 a.y = 2 local z = a.x + a.y"
                ),
            );
            results.push((state.cost_used(), state.cost_remaining()));
        }
    }
    assert!(
        results.windows(2).all(|w| w[0] == w[1]),
        "post-reset cost must not depend on the pre-reset prefix: {results:?}"
    );
    let (used, remaining) = results[0];
    assert_eq!(used, pinned_cost("mid_run_budget_reset"), "post-reset work");
    assert_eq!(
        remaining,
        1_000_000 - i64::try_from(used).expect("cost fits in i64")
    );
}
