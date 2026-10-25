//! The CLI binary Cargo built for this test run links no platform libm
//! transcendental (reference/deterministic-math.md).
//!
//! Clippy's bans cannot see dependencies or LLVM libcalls, so this reads the
//! linked binary's symbol tables instead (static and dynamic, with the
//! `object` crate). `brokkr check` runs it once per ordinary `[[check]]`
//! feature shape, which checks the dev-profile binary of each shape, retaining
//! coverage before release optimization. The dedicated libm_symbols target
//! checks the release binaries in both release sweeps.
//! `support::dellingr_bin()` also verifies that the CLI binary has the
//! integration harness's snapshot feature shape.

#[path = "../../common/libm_symbols.rs"]
mod common;

#[test]
fn cli_binary_links_no_platform_libm_transcendental() {
    common::assert_cli_binary_links_no_platform_libm_transcendental(crate::support::dellingr_bin());
}

#[test]
fn rejects_imports_versions_and_weak_definitions() {
    assert_eq!(
        common::forbidden_symbols(["sin@GLIBC_2.2.5", "powf", "fma", "__log_finite"]),
        ["__log_finite", "fma", "powf", "sin"]
    );
}

#[test]
fn accepts_exact_operations_and_rust_names() {
    assert!(common::forbidden_symbols(["fmod", "sqrt", "floor", "_RNvCs_crmath_sin"]).is_empty());
}

#[test]
fn rejects_float_long_double_and_macho_names() {
    assert_eq!(
        common::forbidden_symbols(["_cos", "erfcf", "lgammal", "sincos"]),
        ["_cos", "erfcf", "lgammal", "sincos"]
    );
}

#[test]
fn rejects_private_lto_clones() {
    assert_eq!(common::forbidden_symbols(["sin.llvm.123"]), ["sin"]);
    assert_eq!(common::forbidden_symbols(["pow.constprop.0"]), ["pow"]);
    assert_eq!(common::forbidden_symbols(["exp.isra.1"]), ["exp"]);
}
