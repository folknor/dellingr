//! Rewrite the checked-in save golden fixture after an intentional serializer
//! format change.

use std::path::PathBuf;

#[path = "../tests/common/save_golden_program.rs"]
mod save_golden_program;

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
    let current = match std::fs::read(&fixture) {
        Ok(current) => Some(current),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("golden fixture should be readable: {error}"),
    };

    if current.as_deref() == Some(bytes.as_slice()) {
        println!("unchanged {}", fixture.display());
    } else {
        std::fs::write(&fixture, bytes).expect("golden fixture should be writable");
        println!("wrote {}", fixture.display());
    }
}
