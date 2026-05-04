//! Differential testing: compare our VM against lua5.2 and lua5.4
//!
//! This test requires lua5.2 and lua5.4 to be installed on the system.
//! On Ubuntu/Debian: apt install lua5.2 lua5.4

use std::process::Command;

/// Run differential tests comparing our VM against lua5.2 and lua5.4.
///
/// This test is skipped if lua5.2 or lua5.4 is not available.
#[test]
fn differential_test() {
    // Check if lua5.2 and lua5.4 are available
    let lua52_available = Command::new("lua5.2")
        .arg("-v")
        .output()
        .is_ok_and(|o| o.status.success());

    let lua54_available = Command::new("lua5.4")
        .arg("-v")
        .output()
        .is_ok_and(|o| o.status.success());

    if !lua52_available || !lua54_available {
        eprintln!("Skipping differential test: lua5.2 and/or lua5.4 not available");
        eprintln!("Install with: apt install lua5.2 lua5.4");
        return;
    }

    // Run the differential test script
    let output = Command::new("./diff_test.sh")
        .output()
        .expect("Failed to run diff_test.sh");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Print the output for visibility in test results
    println!("{stdout}");
    if !stderr.is_empty() {
        eprintln!("{stderr}");
    }

    assert!(
        output.status.success(),
        "Differential test failed!\n\nstdout:\n{stdout}\n\nstderr:\n{stderr}"
    );
}
