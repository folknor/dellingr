//! Check a linked CLI binary for platform libm transcendental symbols.
//!
//! Both imports and definitions count: compiler-builtins can provide weak libm
//! definitions locally. Exact IEEE operations (including sqrt and fmod) are
//! fine, and Rust-mangled crmath functions do not have C libm symbol names.

use object::{Object, ObjectSymbol};

const TRANSCENDENTALS: &[&str] = &[
    "acos",
    "acosh",
    "asin",
    "asinh",
    "atan",
    "atan2",
    "atanh",
    "cos",
    "cosh",
    "sin",
    "sinh",
    "tan",
    "tanh",
    "exp",
    "exp2",
    "exp10",
    "expm1",
    "log",
    "log2",
    "log10",
    "log1p",
    "pow",
    "cbrt",
    "hypot",
    "erf",
    "erfc",
    "gamma",
    "lgamma",
    "lgamma_r",
    "tgamma",
    "j0",
    "j1",
    "jn",
    "y0",
    "y1",
    "yn",
    "sincos",
    "sinpi",
    "cospi",
    "tanpi",
    "asinpi",
    "acospi",
    "atanpi",
    "atan2pi",
    "exp2m1",
    "exp10m1",
    "log2p1",
    "log10p1",
    "compoundn",
    "pown",
    "powr",
    "rootn",
    "fma",
];

/// Whether `symbol` (already stripped of version and clone suffixes) is a
/// forbidden C libm name: an optional `_` and optional `__` prefix, a
/// transcendental, an optional `f` or `l`, an optional `_finite`.
fn is_forbidden(symbol: &str) -> bool {
    let bytes = symbol.as_bytes();
    let underscores = bytes.iter().take_while(|&&b| b == b'_').count();
    // `_?(?:__)?` accepts 0, 1, 2 or 3 leading underscores.
    (0..=underscores.min(3)).any(|prefix| {
        let rest = &symbol[prefix..];
        let without_finite = rest.strip_suffix("_finite");
        [Some(rest), without_finite]
            .into_iter()
            .flatten()
            .any(|rest| {
                let without_width = rest.strip_suffix(['f', 'l']);
                [Some(rest), without_width]
                    .into_iter()
                    .flatten()
                    .any(|name| TRANSCENDENTALS.contains(&name))
            })
    })
}

/// The sorted, deduplicated forbidden names among `symbols`.
pub(crate) fn forbidden_symbols<'a>(symbols: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut rejected: Vec<String> = Vec::new();
    for raw in symbols {
        let symbol = raw.split('@').next().unwrap_or_default();
        // Link-time optimization may keep a private clone of a C definition.
        let symbol = [".llvm.", ".constprop.", ".isra."]
            .iter()
            .filter_map(|marker| symbol.find(marker))
            .min()
            .map_or(symbol, |at| &symbol[..at]);
        if is_forbidden(symbol) && !rejected.iter().any(|seen| seen == symbol) {
            rejected.push(symbol.to_owned());
        }
    }
    rejected.sort();
    rejected
}

pub(crate) fn assert_cli_binary_links_no_platform_libm_transcendental(binary: &str) {
    let data = std::fs::read(binary).expect("the CLI binary Cargo built should be readable");
    let file =
        object::File::parse(&*data).expect("the CLI binary should be a parseable object file");
    // The dynamic table remains necessary when a linker strips the static one.
    let names: Vec<&str> = file
        .symbols()
        .chain(file.dynamic_symbols())
        .map(|symbol| symbol.name().expect("ELF symbol names should be UTF-8"))
        .filter(|name| !name.is_empty())
        .collect();
    assert!(
        !names.is_empty(),
        "no symbols found in {binary}; cannot verify libm contract"
    );
    let rejected = forbidden_symbols(names);
    assert!(
        rejected.is_empty(),
        "platform libm symbols in {binary}: {}",
        rejected.join(", ")
    );
}
