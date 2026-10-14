// hotpath through 0.14 installed CountingAllocator internally under
// hotpath-alloc; from 0.16 onward it is a plain generic type the consumer must
// declare as #[global_allocator] themselves - the crate no longer wires it up
// on its own. Without this, hotpath-alloc builds fall back to the system
// allocator and track_alloc/track_dealloc never fire, so every function
// silently reports 0 bytes.
#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static ALLOC: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::process::exit;

use dellingr::{ArgCount, RetCount, State, analyze_cost};

#[derive(Clone, Copy)]
enum OptionKind {
    Limit,
    Analyze,
    Quiet,
    Help,
}

struct OptionSpec {
    short: Option<&'static str>,
    long: &'static str,
    value: Option<&'static str>,
    description: &'static str,
    kind: OptionKind,
}

const OPTIONS: &[OptionSpec] = &[
    OptionSpec {
        short: Some("-l"),
        long: "--limit",
        value: Some("N"),
        description: "Set cost budget limit",
        kind: OptionKind::Limit,
    },
    OptionSpec {
        short: Some("-a"),
        long: "--analyze",
        value: None,
        description: "Analyze cost without executing",
        kind: OptionKind::Analyze,
    },
    OptionSpec {
        short: Some("-q"),
        long: "--quiet",
        value: None,
        description: "Suppress cost summary output",
        kind: OptionKind::Quiet,
    },
    OptionSpec {
        short: Some("-h"),
        long: "--help",
        value: None,
        description: "Show this help message",
        kind: OptionKind::Help,
    },
];

struct CliArgs {
    filename: OsString,
    limit: Option<i64>,
    analyze: bool,
    quiet: bool,
}

fn usage_line() -> String {
    let options = OPTIONS
        .iter()
        .map(|option| {
            let value = option
                .value
                .map_or_else(String::new, |value| format!(" {value}"));
            format!("[{}{value}]", option.long)
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("Usage: dellingr {options} [--] <file.lua>")
}

fn help_text() -> String {
    let options = OPTIONS
        .iter()
        .map(|option| {
            let short = option
                .short
                .map_or_else(String::new, |short| format!("{short}, "));
            let value = option
                .value
                .map_or_else(String::new, |value| format!(" {value}"));
            format!("  {short}{}{value}\t{}", option.long, option.description)
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}\n\nOptions:\n{options}\n", usage_line())
}

fn find_option(arg: &OsStr) -> Option<&'static OptionSpec> {
    OPTIONS.iter().find(|option| {
        arg == OsStr::new(option.long) || option.short.is_some_and(|short| arg == OsStr::new(short))
    })
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Option<CliArgs>, String> {
    let mut args = args.into_iter();
    let mut filename = None;
    let mut limit = None;
    let mut analyze = false;
    let mut quiet = false;
    let mut options_ended = false;

    while let Some(arg) = args.next() {
        if !options_ended && arg == OsStr::new("--") {
            options_ended = true;
            continue;
        }

        if !options_ended {
            if let Some(option) = find_option(&arg) {
                match option.kind {
                    OptionKind::Limit => {
                        let value = args
                            .next()
                            .ok_or_else(|| "--limit requires a value".to_owned())?;
                        let value_text = value.to_str().ok_or_else(|| {
                            format!(
                                "invalid --limit value '{}': expected a signed 64-bit integer",
                                value.to_string_lossy()
                            )
                        })?;
                        limit = Some(value_text.parse::<i64>().map_err(|_| {
                            format!(
                                "invalid --limit value '{value_text}': expected a signed 64-bit integer"
                            )
                        })?);
                    }
                    OptionKind::Analyze => analyze = true,
                    OptionKind::Quiet => quiet = true,
                    OptionKind::Help => return Ok(None),
                }
                continue;
            }

            if arg != OsStr::new("-") && arg.to_string_lossy().starts_with('-') {
                return Err(format!("unknown option '{}'", arg.to_string_lossy()));
            }
        }

        if filename.is_some() {
            return Err(format!(
                "only one input file may be specified (received '{}')",
                arg.to_string_lossy()
            ));
        }
        filename = Some(arg);
    }

    if analyze && limit.is_some() {
        return Err("--limit cannot be used with --analyze".to_owned());
    }

    let filename = filename.ok_or_else(|| "an input file is required".to_owned())?;
    Ok(Some(CliArgs {
        filename,
        limit,
        analyze,
        quiet,
    }))
}

fn report_error(error: impl std::fmt::Display) -> ! {
    eprintln!("Error: {error}");
    exit(1);
}

fn report_usage_error(error: &str) -> ! {
    eprintln!("Error: {error}");
    eprintln!("{}", usage_line());
    exit(1);
}

// The CLI is the host process, so reading its own command line is the point;
// the ban on process arguments guards the library.
#[allow(clippy::disallowed_methods)]
fn main() {
    let args = match parse_args(env::args_os().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            print!("{}", help_text());
            return;
        }
        Err(error) => report_usage_error(&error),
    };

    let source = match fs::read_to_string(&args.filename) {
        Ok(source) => source,
        Err(error) => report_error(format!("reading {}: {error}", args.filename.display())),
    };

    if args.analyze {
        match analyze_cost(&source) {
            Ok(analysis) => {
                println!("{analysis}");
                eprintln!(
                    "Warning: analyze_cost accounts only for static bytecode and excludes all data-dependent native string, pattern, and table work; it is not a runtime bound or native-cost estimate."
                );
            }
            Err(error) => report_error(format!("parsing source: {error}")),
        }
        return;
    }

    let mut state = State::new();
    if let Some(limit) = args.limit {
        state.set_cost_budget(limit);
    }

    let filename = args.filename.to_string_lossy().into_owned();
    let result = state
        .load_string_named(&source, Some(filename))
        .and_then(|()| state.call(ArgCount::Fixed(0), RetCount::Fixed(0)));

    if !args.quiet {
        eprintln!("Cost used: {}", state.cost_used());
    }

    if let Err(error) = result {
        report_error(error);
    }
}
