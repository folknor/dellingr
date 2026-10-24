pub fn probe(pointer: *const u8) {
    let _ = pointer.addr();
}
