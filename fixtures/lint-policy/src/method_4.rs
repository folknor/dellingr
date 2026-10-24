pub fn probe(result: Result<u8, u8>) {
    let _ = result.unwrap_or_default();
}
