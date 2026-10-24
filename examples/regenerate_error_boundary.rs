//! Write the error-boundary transcript to an explicit destination.

// Reuse the corpus runner and callback implementation from the comparing test.
// The example needs only the recorder's error and reset methods.
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../tests/integration/support/recorder.rs"]
mod recorder;

#[cfg(not(test))]
mod support {
    pub(crate) use crate::recorder::Recorder;
}

#[cfg(not(test))]
#[path = "../tests/integration/failures/error_boundary.rs"]
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
