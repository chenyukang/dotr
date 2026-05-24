fn main() {
    if let Err(err) = dotr::cli::run() {
        eprintln!("{}", dotr::terminal::red(format!("error: {err:#}")));
        std::process::exit(1);
    }
}
