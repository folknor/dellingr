pub fn probe(pointer: *mut u8) {
    let _ = pointer.expose_provenance();
}
