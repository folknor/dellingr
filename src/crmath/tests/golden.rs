//! The golden-bits contract: `tests/fixtures/crmath_golden.txt` lists inputs
//! and the correctly rounded result for every kernel, certified by the
//! oracle. Every host must reproduce every line under every FMA path.

use super::oracle::{Func, certify, parse_wc_token};
use super::{TestRng, fma_modes, same, with_forced_fma};
use crate::crmath::hf;
use std::fmt::Write as _;

const GOLDEN: &str = include_str!("../../../tests/fixtures/crmath_golden.txt");
const GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/crmath_golden.txt"
);
const CORE_MATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/research/core-math/src/binary64"
);

fn parse_bits(token: &str) -> f64 {
    if token == "nan" {
        return f64::NAN;
    }
    f64::from_bits(u64::from_str_radix(token, 16).expect("golden fields are hex bit patterns"))
}

fn show_bits(x: f64) -> String {
    if x.is_nan() {
        "nan".to_owned()
    } else {
        format!("{:016x}", x.to_bits())
    }
}

#[test]
fn golden_results_on_every_fma_path() {
    let mut cases = 0;
    let mut failures = Vec::new();
    for line in GOLDEN.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let f = Func::from_name(fields[0]).expect("golden lines start with a function name");
        let (x, y, expected) = if f.binary() {
            (
                parse_bits(fields[1]),
                parse_bits(fields[2]),
                parse_bits(fields[3]),
            )
        } else {
            (parse_bits(fields[1]), 0.0, parse_bits(fields[2]))
        };
        for mode in fma_modes() {
            let got = with_forced_fma(mode, || f.port(x, y));
            if !same(got, expected) && failures.len() < 50 {
                failures.push(format!("{line}: got {} under {mode:?}", show_bits(got)));
            }
        }
        cases += 1;
    }
    assert!(cases > 0, "golden file is empty");
    assert!(
        failures.is_empty(),
        "{} golden mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Inputs worth pinning for every function: signed zeros, infinities, NaN,
/// subnormal and overflow boundaries, and exact-result points.
fn special_inputs() -> Vec<f64> {
    vec![
        0.0,
        -0.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        1.0,
        -1.0,
        0.5,
        -0.5,
        2.0,
        -2.0,
        3.0,
        -3.0,
        10.0,
        0.1,
        100.0,
        1000.0,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        f64::from_bits(1),
        -f64::from_bits(1),
        f64::from_bits(0x000f_ffff_ffff_ffff),
        f64::MAX,
        -f64::MAX,
        hf("0x1p-27"),
        hf("0x1p-54"),
        hf("0x1p-1022"),
        hf("0x1.fffffffffffffp-1"),
        hf("0x1.0000000000001p+0"),
        hf("0x1.921fb54442d18p+0"),
        hf("0x1.921fb54442d18p+1"),
        hf("0x1.62e42fefa39efp+9"),
        hf("0x1.62e42fefa39fp+9"),
        hf("-0x1.74385446d71c3p+9"),
        hf("-0x1.74910d52d3052p+9"),
        hf("0x1p+31"),
        hf("0x1p+52"),
        hf("0x1p+53"),
        hf("0x1p+1023"),
        1e22,
        1e300,
        -1e300,
        1e-300,
    ]
}

fn binary_special_inputs() -> Vec<f64> {
    vec![
        0.0,
        -0.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        1.0,
        -1.0,
        0.5,
        -0.5,
        2.0,
        -2.0,
        3.0,
        -3.0,
        0.25,
        10.0,
        1e300,
        -1e300,
        f64::from_bits(1),
        hf("0x1p-1022"),
        hf("0x1.fffffffffffffp-1"),
        f64::MAX,
    ]
}

/// A plausible-use range per function, for random inputs.
fn typical_range(f: Func) -> (f64, f64, f64, f64) {
    match f {
        Func::Sin | Func::Cos | Func::Tan => (-20.0, 20.0, 0.0, 0.0),
        Func::Asin | Func::Acos => (-1.0, 1.0, 0.0, 0.0),
        Func::Atan | Func::Exp => (-50.0, 50.0, 0.0, 0.0),
        Func::Atan2 => (-100.0, 100.0, -100.0, 100.0),
        Func::Ln | Func::Log10 => (0.0, 1000.0, 0.0, 0.0),
        Func::Pow => (0.0, 20.0, -10.0, 10.0),
    }
}

/// Worst-case inputs from a local CORE-MATH checkout, if present.
fn worst_cases(f: Func) -> Vec<(f64, f64)> {
    let path = format!("{CORE_MATH}/{0}/{0}.wc", f.name());
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let mut tokens = line.split(',');
        let Some(x) = tokens.next().and_then(parse_wc_token) else {
            continue;
        };
        let y = if f.binary() {
            match tokens.next().and_then(parse_wc_token) {
                Some(y) => y,
                None => continue,
            }
        } else {
            0.0
        };
        out.push((x, y));
    }
    out
}

/// Certify every input and fail with every discrepancy (port against
/// oracle, and between FMA paths).
fn check_inputs(f: Func, inputs: &[(f64, f64)]) -> Vec<String> {
    // Oracle panics are reported per input below; keep their default
    // messages from flooding the output.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let problems = check_inputs_quietly(f, inputs);
    std::panic::set_hook(previous_hook);
    problems
}

fn check_inputs_quietly(f: Func, inputs: &[(f64, f64)]) -> Vec<String> {
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let chunk = inputs.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = inputs
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    let mut problems = Vec::new();
                    for &(x, y) in part {
                        let port = f.port(x, y);
                        for mode in fma_modes() {
                            let other = with_forced_fma(mode, || f.port(x, y));
                            if !same(port, other) {
                                problems.push(format!(
                                    "{} {x:e} {y:e}: FMA path {mode:?} gives {} not {}",
                                    f.name(),
                                    show_bits(other),
                                    show_bits(port)
                                ));
                            }
                        }
                        let certified = std::panic::catch_unwind(|| certify(f, x, y))
                            .unwrap_or_else(|_| {
                                Err(format!(
                                    "{} x={x:e} ({:016x}) y={y:e} ({:016x}): oracle panicked",
                                    f.name(),
                                    x.to_bits(),
                                    y.to_bits()
                                ))
                            });
                        match certified {
                            Ok(expected) if same(expected, port) => {}
                            Ok(expected) => problems.push(format!(
                                "{} x={x:e} ({:016x}) y={y:e} ({:016x}): port {} expected {}",
                                f.name(),
                                x.to_bits(),
                                y.to_bits(),
                                show_bits(port),
                                show_bits(expected)
                            )),
                            Err(message) => problems.push(message),
                        }
                    }
                    problems
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("checker threads do not panic"))
            .collect()
    })
}

fn sweep(f: Func) {
    let inputs = worst_cases(f);
    assert!(
        !inputs.is_empty(),
        "no worst-case file for {} under research/core-math",
        f.name()
    );
    let problems = check_inputs(f, &inputs);
    let shown: Vec<_> = problems.iter().take(100).cloned().collect();
    assert!(
        problems.is_empty(),
        "{} of {} worst cases failed for {}:\n{}",
        problems.len(),
        inputs.len(),
        f.name(),
        shown.join("\n")
    );
}

macro_rules! sweeps {
    ($($name:ident => $func:expr,)*) => {
        $(
            /// Every CORE-MATH worst case for this function against the
            /// oracle. Needs `research/core-math`; run in release.
            #[test]
            #[ignore = "long-running; needs a CORE-MATH checkout under research/"]
            fn $name() {
                sweep($func);
            }
        )*
    };
}

sweeps! {
    sweep_sin => Func::Sin,
    sweep_cos => Func::Cos,
    sweep_tan => Func::Tan,
    sweep_asin => Func::Asin,
    sweep_acos => Func::Acos,
    sweep_atan => Func::Atan,
    sweep_atan2 => Func::Atan2,
    sweep_exp => Func::Exp,
    sweep_log => Func::Ln,
    sweep_log10 => Func::Log10,
    sweep_pow => Func::Pow,
}

/// Random inputs against the oracle, beyond what the golden file pins.
#[test]
#[ignore = "long-running; run in release"]
fn random_against_oracle() {
    let mut rng = TestRng::new(0xdead_beef);
    for f in Func::ALL {
        let (lo, hi, ylo, yhi) = typical_range(f);
        let mut inputs = Vec::new();
        for _ in 0..20_000 {
            inputs.push((rng.in_range(lo, hi), rng.in_range(ylo, yhi)));
            inputs.push((rng.finite(), rng.finite()));
        }
        let problems = check_inputs(f, &inputs);
        let shown: Vec<_> = problems.iter().take(50).cloned().collect();
        assert!(problems.is_empty(), "{}:\n{}", f.name(), shown.join("\n"));
    }
}

/// Rewrites the golden file. Run after a deliberate change to the inputs:
/// `brokkr test regenerate_crmath_golden --release`.
#[test]
#[ignore = "rewrites tests/fixtures/crmath_golden.txt"]
fn regenerate_crmath_golden() {
    let mut out = String::new();
    out.push_str(
        "# Correctly rounded results of the deterministic math kernels (src/crmath).\n\
         # Format: function x-bits [y-bits] result-bits, hex IEEE 754 binary64; nan is any NaN.\n\
         # Certified by the interval oracle in src/crmath/tests/oracle.rs. Regenerate with\n\
         # `brokkr test regenerate_crmath_golden --release` (worst cases need research/core-math).\n",
    );
    let mut rng = TestRng::new(0x601d);
    for f in Func::ALL {
        let mut inputs = Vec::new();
        if f.binary() {
            for &x in &binary_special_inputs() {
                for &y in &binary_special_inputs() {
                    inputs.push((x, y));
                }
            }
        } else {
            inputs.extend(special_inputs().into_iter().map(|x| (x, 0.0)));
        }
        let (lo, hi, ylo, yhi) = typical_range(f);
        for _ in 0..300 {
            inputs.push((rng.in_range(lo, hi), rng.in_range(ylo, yhi)));
        }
        for _ in 0..300 {
            let y = if f.binary() { rng.finite() } else { 0.0 };
            inputs.push((rng.finite(), y));
        }
        let worst = worst_cases(f);
        let step = (worst.len() / 500).max(1);
        inputs.extend(worst.iter().step_by(step).copied());
        for (x, y) in inputs {
            let r = certify(f, x, y).unwrap_or_else(|message| panic!("{message}"));
            if f.binary() {
                writeln!(
                    out,
                    "{} {} {} {}",
                    f.name(),
                    show_bits(x),
                    show_bits(y),
                    show_bits(r)
                )
            } else {
                writeln!(out, "{} {} {}", f.name(), show_bits(x), show_bits(r))
            }
            .expect("writing to a String cannot fail");
        }
    }
    std::fs::write(GOLDEN_PATH, out).expect("tests/fixtures is writable");
}
