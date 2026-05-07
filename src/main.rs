fn main() {
    if let Err(error) = tp7::run() {
        eprintln!("error: {error}");
        std::process::exit(error.exit_code());
    }
}
