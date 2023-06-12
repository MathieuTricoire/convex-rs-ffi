pub fn cargo_path() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}
