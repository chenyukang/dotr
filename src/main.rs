fn main() {
    if let Err(err) = dotr::cli::run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}
