//! Rewrite the checked-in save golden fixture after an intentional serializer
//! format change.

use std::path::PathBuf;

#[path = "../tests/common/save_golden_program.rs"]
mod save_golden_program;

fn main() {
    let bytes = save_golden_program::golden_bytes();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(save_golden_program::GOLDEN_FIXTURE);
    std::fs::write(&fixture, bytes).expect("golden fixture should be writable");
    println!("wrote {}", fixture.display());
}
