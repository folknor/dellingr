// hotpath through 0.14 installed CountingAllocator internally under
// hotpath-alloc; from 0.16 onward it is a plain generic type the consumer must
// declare as #[global_allocator] themselves - the crate no longer wires it up
// on its own. Without this, hotpath-alloc builds fall back to the system
// allocator and track_alloc/track_dealloc never fire, so every function
// silently reports 0 bytes.
#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static ALLOC: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

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
    let mut quiet = false;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--limit" | "-l" => {
                i += 1;
                let value = match args.get(i) {
                    Some(value) => value,
                    None => {
                        eprintln!("Error: --limit requires a value");
                        exit(1);
                    }
                };
                limit = match value.parse::<i64>() {
                    Ok(n) => Some(n),
                    Err(_) => {
                        eprintln!(
                            "Error: invalid --limit value '{value}': expected a signed 64-bit integer"
                        );
                        exit(1);
                    }
                };
            }
            "--analyze" | "-a" => {
                analyze = true;
            }
            "--quiet" | "-q" => {
                quiet = true;
            }
            "--help" | "-h" => {
                println!("Usage: dellingr [OPTIONS] <file.lua>");
                println!();
                println!("Options:");
                println!("  -l, --limit N    Set cost budget limit");
                println!("  -a, --analyze    Analyze cost without executing");
                println!("  -q, --quiet      Suppress cost summary output");
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
            eprintln!("Usage: dellingr [--limit N] [--analyze] [--quiet] <file.lua>");
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

        if !quiet {
            println!("Cost used: {}", state.cost_used());
        }

        if let Err(e) = result {
            eprintln!("Error: {e}");
            exit(1);
        }
    }
}
