//! Rewrite the checked-in save golden fixture and its text companion after a
//! reviewed save-output change. The archived released-format fixtures are
//! never regenerated.

use std::path::PathBuf;

#[path = "../tests/common/save_golden_program.rs"]
mod save_golden_program;
// This tool's destination is the shared fixture path, so the explicit
// destination parser the other regeneration tools use goes unused here.
#[allow(dead_code)]
#[path = "tools/output.rs"]
mod output;

fn main() {
    let bytes = save_golden_program::golden_bytes();
    // brokkr --commit builds this example from a worktree but runs it in the
    // caller's checkout, so find the repository from the runtime directory.
    let current_dir =
        std::env::current_dir().expect("the current working directory should be available");
    let root = current_dir
        .ancestors()
        .find(|path| {
            path.join("Cargo.toml").is_file()
                && path.join("tests/common/save_golden_program.rs").is_file()
        })
        .map(PathBuf::from)
        .expect("the current directory should be inside the dellingr repository");
    let fixture = root.join(save_golden_program::GOLDEN_FIXTURE);
    output::write_changed(&fixture, &bytes);
    output::write_changed(
        &fixture.with_extension("txt"),
        save_golden_program::golden_text(&bytes).as_bytes(),
    );
}
