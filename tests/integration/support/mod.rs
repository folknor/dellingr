//! Helpers shared by the suites of this crate.
//!
//! The files under `tests/common/` are also compiled by another target (the
//! native-stack test, the release libm symbol test, the compiler's unit tests,
//! the golden-regeneration example), so they stay self-contained and are
//! included by path, here or by the suite that uses them.

#[path = "../../common/error_boundary_cases.rs"]
pub(crate) mod error_boundary_cases;
#[path = "../../common/examples.rs"]
pub(crate) mod examples;
mod lua;
pub(crate) mod process;
mod recorder;
#[cfg(feature = "snapshot")]
#[path = "../../common/save_golden_program.rs"]
pub(crate) mod save_golden_program;
#[path = "../../common/stack.rs"]
pub(crate) mod stack;

pub(crate) use lua::*;
pub(crate) use recorder::Recorder;

/// The `dellingr` binary Cargo built for this test run. Tests must use this
/// rather than a `target/` path, which may hold a stale binary from another
/// profile, checkout or commit. Cargo's uplifted path can also be overwritten
/// by another feature build, so refuse a mismatched snapshot shape here. The
/// probe runs once per test process; the corpus test asks for the path once
/// per example.
pub(crate) fn dellingr_bin() -> &'static str {
    // The binary and the harness's feature shape are fixed for the process.
    // process-global: a write-once record that the shape probe passed.
    static PROBED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let binary = env!("CARGO_BIN_EXE_dellingr");
    PROBED.get_or_init(|| process::probe_snapshot_shape(binary));
    binary
}

/// The repository's `brokkr.toml`, parsed. Suites that check workload
/// registrations or check shapes read it through this one loader.
pub(crate) fn brokkr_config() -> toml::Table {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("brokkr.toml");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    // A whole document parses as a `Table`; `Value` parses a single TOML value.
    source
        .parse::<toml::Table>()
        .unwrap_or_else(|error| panic!("brokkr.toml is not valid TOML: {error}"))
}

/// Run `check` over `items` on scoped worker threads, at most `max_workers`
/// and never more than the available parallelism or the item count, and
/// return every failure in item order, so the report does not depend on
/// scheduling.
pub(crate) fn parallel_failures<T: Sync>(
    items: &[T],
    max_workers: usize,
    check: impl Fn(&T) -> Option<String> + Sync,
) -> Vec<String> {
    parallel_map(items, max_workers, check)
        .into_iter()
        .flatten()
        .collect()
}

/// Run `map` over `items` on scoped worker threads, at most `max_workers`
/// and never more than the available parallelism or the item count, and
/// return the results in item order, so a report does not depend on
/// scheduling.
pub(crate) fn parallel_map<T: Sync, R: Send>(
    items: &[T],
    max_workers: usize,
    map: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    #[allow(
        clippy::disallowed_methods,
        reason = "The harness sizes workers to the host; results remain in item order."
    )]
    let available =
        std::thread::available_parallelism().map_or(max_workers, std::num::NonZeroUsize::get);
    let workers = available.min(max_workers).min(items.len());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut results: Vec<(usize, R)> = std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut local = Vec::new();
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(index) else { break };
                        local.push((index, map(item)));
                    }
                    local
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("worker thread panicked"))
            .collect()
    });
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}
