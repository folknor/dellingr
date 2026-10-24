pub fn probe() {
    std::panic::set_hook(Box::new(|_| {}));
}
