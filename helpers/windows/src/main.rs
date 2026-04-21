fn main() {
    if let Err(err) = sgm_windows_helper::run() {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}
