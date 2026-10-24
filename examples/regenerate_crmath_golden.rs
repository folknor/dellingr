//! Certify golden inputs and write them to an explicitly supplied output path.

// The oracle needs crate-private math helpers. Compile the kernels here rather
// than widening the library API for a maintenance tool. Some helpers are only
// used by the VM or the test harness.
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../src/crmath/mod.rs"]
mod crmath;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../src/crmath/tests/oracle.rs"]
mod oracle;
#[cfg(not(test))]
#[path = "../src/crmath/tests/rng.rs"]
mod rng;
#[cfg(not(test))]
use rng::TestRng;
#[cfg(not(test))]
#[path = "../src/crmath/tests/golden.rs"]
mod golden;
#[cfg(not(test))]
#[path = "tools/output.rs"]
mod output;

#[cfg(not(test))]
fn main() {
    let output = output::destination("regenerate_crmath_golden");
    let bytes = golden::regenerate_crmath_golden();
    output::write_changed(&output, bytes.as_bytes());
}

#[cfg(test)]
fn main() {}
