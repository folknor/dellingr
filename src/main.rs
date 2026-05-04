use std::env::args;
use std::fs;
use std::process::exit;

use dellingr::{ArgCount, RetCount, State, analyze_cost};

fn main() {
    let args: Vec<String> = args().collect();

    // Parse arguments
    let mut filename = None;
    let mut limit: Option<i64> = None;
    let mut analyze = false;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--limit" | "-l" => {
                i += 1;
                if i < args.len() {
                    limit = match args[i].parse() {
                        Ok(n) => Some(n),
                        Err(_) => {
                            eprintln!("Warning: invalid --limit value '{}', ignoring", args[i]);
                            None
                        }
                    };
                }
            }
            "--analyze" | "-a" => {
                analyze = true;
            }
            "--help" | "-h" => {
                println!("Usage: dellingr [OPTIONS] <file.lua>");
                println!();
                println!("Options:");
                println!("  -l, --limit N    Set cost budget limit");
                println!("  -a, --analyze    Analyze cost without executing");
                println!("  -h, --help       Show this help message");
                exit(0);
            }
            arg if !arg.starts_with('-') => {
                filename = Some(arg.to_string());
            }
            other => {
                eprintln!("Unknown option: {other}");
                exit(1);
            }
        }
        i += 1;
    }

    let filename = match filename {
        Some(f) => f,
        None => {
            eprintln!("Usage: dellingr [--limit N] [--analyze] <file.lua>");
            exit(1);
        }
    };

    let source = match fs::read_to_string(&filename) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {filename}: {e}");
            exit(1);
        }
    };

    if analyze {
        // Static cost analysis mode
        match analyze_cost(&source) {
            Ok(analysis) => {
                println!("{analysis}");
            }
            Err(e) => {
                eprintln!("Parse error: {e}");
                exit(1);
            }
        }
    } else {
        // Normal execution mode
        let mut state = State::new();

        if let Some(l) = limit {
            state.set_cost_budget(l);
        }

        let result = state
            .load_string_named(&source, Some(filename.clone()))
            .and_then(|()| state.call(ArgCount::Fixed(0), RetCount::Fixed(0)));

        println!("Cost used: {}", state.cost_used());

        if let Err(e) = result {
            eprintln!("Error: {e}");
            exit(1);
        }
    }
}
