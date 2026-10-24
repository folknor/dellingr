//! `tests/fixtures/crmath_golden.txt` through the library as a script reaches
//! it: the `math` functions and the `^` operator of a normally compiled build.
//!
//! The library's own golden (`crmath::tests::golden`) forces each FMA backend
//! through test-only dispatch hooks, so it cannot see the selection and
//! inlining a shipped build makes. This runs every row through the production
//! path instead, and the release golden check shape runs it optimized. The
//! unit golden pins each function's row count; this test refuses any row it
//! cannot read and requires every function, so it cannot skip silently.

use dellingr::{ArgCount, Program, RetCount, State};

const GOLDEN: &str = include_str!("../../fixtures/crmath_golden.txt");

/// Each fixture function name, whether it takes two inputs, and the script
/// that computes it from `x, y = ...`. `atan2`'s operands are passed in
/// fixture order, as the kernel takes them. `log10` is `math.log` with base
/// 10, which calls the log10 kernel; `pow` is the `^` operator on two
/// registers, which the compiler never folds.
const FUNCTIONS: [(&str, bool, &str); 11] = [
    ("sin", false, "return math.sin(x)"),
    ("cos", false, "return math.cos(x)"),
    ("tan", false, "return math.tan(x)"),
    ("asin", false, "return math.asin(x)"),
    ("acos", false, "return math.acos(x)"),
    ("atan", false, "return math.atan(x)"),
    ("atan2", true, "return math.atan2(x, y)"),
    ("exp", false, "return math.exp(x)"),
    ("log", false, "return math.log(x)"),
    ("log10", false, "return math.log(x, 10)"),
    ("pow", true, "return x ^ y"),
];

fn parse_bits(token: &str) -> f64 {
    if token == "nan" {
        return f64::NAN;
    }
    f64::from_bits(
        u64::from_str_radix(token, 16)
            .unwrap_or_else(|error| panic!("golden field {token:?} is not hex bits: {error}")),
    )
}

fn show(x: f64) -> String {
    if x.is_nan() {
        "nan".to_owned()
    } else {
        format!("{:016x}", x.to_bits())
    }
}

#[test]
fn golden_results_through_the_math_library() {
    let programs: Vec<Program> = FUNCTIONS
        .iter()
        .map(|(name, _, body)| {
            Program::compile(format!("local x, y = ...\n{body}"), Some(name.as_bytes()))
                .expect("golden script compiles")
        })
        .collect();
    let mut state = State::new();
    let mut seen = [0usize; FUNCTIONS.len()];
    let mut failures = Vec::new();
    for line in GOLDEN.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let index = FUNCTIONS
            .iter()
            .position(|(name, ..)| Some(name) == fields.first())
            .unwrap_or_else(|| panic!("golden row names an unknown function: {line}"));
        let binary = FUNCTIONS[index].1;
        assert_eq!(
            fields.len(),
            if binary { 4 } else { 3 },
            "malformed golden row: {line}"
        );
        seen[index] += 1;
        let x = parse_bits(fields[1]);
        let (y, expected) = if binary {
            (parse_bits(fields[2]), parse_bits(fields[3]))
        } else {
            (0.0, parse_bits(fields[2]))
        };
        state.load(&programs[index]).expect("golden script loads");
        state.push_number(x).expect("stack has room");
        state.push_number(y).expect("stack has room");
        state
            .call(ArgCount::Fixed(2), RetCount::Fixed(1))
            .unwrap_or_else(|error| panic!("{line}: {error}"));
        let got = state.to_number(-1).expect("the script returns a number");
        state.pop(1).expect("the result is on the stack");
        let same = if expected.is_nan() {
            got.is_nan()
        } else {
            got.to_bits() == expected.to_bits()
        };
        if !same && failures.len() < 50 {
            failures.push(format!("{line}: got {}", show(got)));
        }
    }
    for (index, (name, ..)) in FUNCTIONS.iter().enumerate() {
        assert!(seen[index] > 0, "the golden has no {name} rows");
    }
    assert!(
        failures.is_empty(),
        "math library results differ from the golden:\n{}",
        failures.join("\n")
    );
}
