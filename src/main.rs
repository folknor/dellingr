use std::env::args;
use std::fs;
use std::process::exit;

use lua::State;

fn main() {
    let args: Vec<String> = args().collect();

    // Parse arguments
    let mut filename = None;
    let mut limit = None;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--limit" | "-l" => {
                i += 1;
                if i < args.len() {
                    limit = args[i].parse().ok();
                }
            }
            arg if !arg.starts_with('-') => {
                filename = Some(arg.to_string());
            }
            _ => {}
        }
        i += 1;
    }

    let filename = match filename {
        Some(f) => f,
        None => {
            eprintln!("Usage: lua [--limit N] <file.lua>");
            exit(1);
        }
    };

    let source = match fs::read_to_string(&filename) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {}: {}", filename, e);
            exit(1);
        }
    };

    let mut state = State::new();

    if let Some(l) = limit {
        state.set_instruction_limit(l);
    }

    let result = state.load_string(&source).and_then(|()| state.call(0, 0));

    println!("Instructions executed: {}", state.instructions_executed());

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        exit(1);
    }
}
