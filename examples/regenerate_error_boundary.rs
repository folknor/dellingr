//! Write the error-boundary transcript to an explicit destination.

#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../tests/common/error_boundary_cases.rs"]
mod error_boundary;
#[cfg(not(test))]
#[path = "tools/output.rs"]
mod output;

#[cfg(not(test))]
fn main() {
    let output = output::destination("regenerate_error_boundary");
    let transcript = error_boundary::boundary_transcript();
    output::write_changed(&output, transcript.as_bytes());
}

#[cfg(test)]
fn main() {}
