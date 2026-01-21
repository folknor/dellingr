//! Integration tests that run all Lua example files.

use std::fs;
use std::path::Path;
use std::process::Command;

/// Run all .lua files in the examples directory and check they complete without errors.
#[test]
fn run_all_examples() {
    let examples_dir = Path::new("examples");
    
    assert!(examples_dir.exists(), "examples directory not found");
    
    let mut files: Vec<_> = fs::read_dir(examples_dir)
        .expect("Failed to read examples directory")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.path().extension().map_or(false, |ext| ext == "lua")
        })
        .collect();
    
    // Sort for consistent ordering
    files.sort_by_key(|entry| entry.path());
    
    assert!(!files.is_empty(), "No .lua files found in examples directory");
    
    let mut failures = Vec::new();
    
    for entry in &files {
        let path = entry.path();
        let filename = path.file_name().unwrap().to_string_lossy();
        
        println!("Running: {}", filename);
        
        let output = Command::new("cargo")
            .args(["run", "--quiet", "--", path.to_str().unwrap()])
            .output()
            .expect("Failed to execute command");
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            failures.push(format!(
                "{}: exit code {:?}\nstdout: {}\nstderr: {}",
                filename,
                output.status.code(),
                stdout,
                stderr
            ));
        } else {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Check for "false" in output which indicates a test failure
            if stdout.contains(": false") {
                failures.push(format!(
                    "{}: test assertion failed\noutput: {}",
                    filename, stdout
                ));
            }
        }
    }
    
    if !failures.is_empty() {
        panic!(
            "\n{} example(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
    }
    
    println!("\nAll {} examples passed!", files.len());
}
